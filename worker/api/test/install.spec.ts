import { env, SELF } from "cloudflare:test";
import { afterEach, describe, expect, it, vi } from "vitest";
import { installerScript, releases } from "../src/install";
import type { Env } from "../src/auth";
import { installCommand } from "../../console/ui/installer.js";

const installEnv = { ...env, INGEST_TOKEN: "test-token" } as Env;
const fixture = { tag_name: "v0.2", draft: false, prerelease: false, assets: [
	"x86_64-unknown-linux-musl.tar.gz", "aarch64-apple-darwin.tar.gz", "x86_64-apple-darwin.tar.gz", "x86_64-pc-windows-msvc.zip",
].map(target => ({ name: `tokscale-client-${target}` })) };

function installerUrl(type = "linux", token = "test-token", version = "v0.2") {
	return new URL(`https://api.custom.example/install.${type === "windows" ? "ps1" : "sh"}?${new URLSearchParams({ token, type, version })}`);
}

afterEach(() => vi.restoreAllMocks());

describe("collector installation", () => {
	it("uses the configured API domain for every platform without a database lookup or previous upload", async () => {
		vi.spyOn(globalThis, "fetch").mockResolvedValue(Response.json(fixture));
		const prepare = vi.fn(() => { throw new Error("Installation must not depend on D1"); });
		const res = await releases({ ...installEnv, DB: { prepare } } as unknown as Env);
		const body = await res.json<any>();
		expect(body.tag).toBe("v0.2");
		expect(body.platforms).toHaveLength(4);
		expect(prepare).not.toHaveBeenCalled();
		expect(res.headers.get("cache-control")).toBe("no-store");
		expect(body.platforms[0].url).toBe("https://github.com/HSwift/tokscale-serverless/releases/download/v0.2/tokscale-client-x86_64-unknown-linux-musl.tar.gz");
		for (const platform of body.platforms) {
			const url = installerUrl(platform.id);
			expect(platform.installPath).toBe(url.pathname + url.search);
			const command = installCommand(platform, url.origin);
			expect(command).toBe(platform.id === "windows" ? `irm '${url.href}' | iex` : `curl -fsSL '${url.href}' | bash`);
			expect(command).not.toContain("SYNC_");
			const script = installerScript(url, installEnv);
			expect(script.status).toBe(200);
			expect(await script.text()).toContain(platform.asset);
		}
	});

	it("embeds the API origin and token in reusable scripts fetched directly from the API", async () => {
		for (let i = 0; i < 2; i++) {
			const res = await SELF.fetch(installerUrl().href);
			expect(res.status).toBe(200);
			expect(res.headers.get("cache-control")).toBe("no-store");
			const script = await res.text();
			expect(script).toContain(`SYNC_URL='https://api.custom.example' SYNC_TOKEN='test-token' "$install_tmp/tokscale-client" connect`);
			expect(script).toContain("SHA256SUMS");
		}
	});

	it("does not generate a command without a configured API URL", () => {
		const platform = { id: "linux", installPath: "/install.sh?token=test-token&type=linux&version=v0.2" };
		expect(installCommand(platform, "")).toBe("");
		expect(installCommand(platform, undefined)).toBe("");
		expect(installCommand(platform, "http://127.0.0.1:18787")).toContain("http://127.0.0.1:18787/install.sh?");
	});

	it("never returns a script or token without valid credentials", async () => {
		for (const type of ["linux", "windows"]) {
			for (const token of ["", "wrong-token"]) {
				const url = installerUrl(type, token);
				if (!token) url.searchParams.delete("token");
				const res = await SELF.fetch(url.href);
				expect(res.status).toBe(401);
				expect(res.headers.get("cache-control")).toBe("no-store");
				expect(await res.text()).not.toContain("test-token");
			}
		}
		expect((await SELF.fetch("https://api.custom.example/api/releases")).status).toBe(401);
		expect(installerScript(installerUrl(), { ...installEnv, INGEST_TOKEN: undefined }).status).toBe(503);
	});

	it("rejects invalid platforms, versions, and script extensions", () => {
		for (const url of [installerUrl("unsupported"), installerUrl("linux", "test-token", "v1';echo bad"), installerUrl("linux", "test-token", "")]) {
			expect(installerScript(url, installEnv).status).toBe(400);
		}
		const url = installerUrl();
		url.pathname = "/install.ps1";
		expect(installerScript(url, installEnv).status).toBe(400);
	});

	it("encodes query parameters and quotes credentials inside Bash and PowerShell", async () => {
		const token = "quoted'token$()&+%/#= value";
		vi.spyOn(globalThis, "fetch").mockResolvedValue(Response.json(fixture));
		const body = await (await releases({ ...installEnv, INGEST_TOKEN: token })).json<any>();
		for (const platform of body.platforms) {
			const url = new URL(platform.installPath, "https://api.custom.example");
			expect(url.searchParams.get("token")).toBe(token);
			expect(url.hash).toBe("");
			expect([...url.searchParams.keys()]).toEqual(["token", "type", "version"]);
			const command = installCommand(platform, url.origin);
			expect(command).toContain("%27");
			expect(command).not.toContain(token);
			const script = await installerScript(url, { ...installEnv, INGEST_TOKEN: token }).text();
			if (platform.id === "windows") {
				expect(script).toContain("$env:SYNC_TOKEN = 'quoted''token$()&+%/#= value'");
				expect(script).toContain("$env:SYNC_TOKEN = $previousSyncToken");
			} else {
				expect(script).toContain("SYNC_TOKEN='quoted'\\''token$()&+%/#= value'");
			}
		}
	});
});
