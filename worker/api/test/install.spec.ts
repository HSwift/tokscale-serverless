import { env, SELF } from "cloudflare:test";
import { beforeAll, afterEach, describe, expect, it, vi } from "vitest";
import { installCommand, installerScript, releases } from "../src/install";
import type { Env } from "../src/auth";
import schema from "../migrations/0003_api_origin.sql?raw";

const installEnv = { ...env, INGEST_TOKEN: "test-token" } as Env;
const fixture = { tag_name: "v0.2.0", draft: false, prerelease: false, assets: [
	"x86_64-unknown-linux-musl.tar.gz", "aarch64-apple-darwin.tar.gz", "x86_64-apple-darwin.tar.gz", "x86_64-pc-windows-msvc.zip",
].map(target => ({ name: `tokscale-client-${target}` })) };

beforeAll(async () => {
	const sql = schema.split("\n").filter(line => !line.trimStart().startsWith("--")).join(" ");
	await env.DB.exec(sql.replace(/\s+/g, " ").trim());
	// Deployment supplies the address; there are no devices or usage records.
	await env.DB.prepare("INSERT OR REPLACE INTO api_origin (id, origin) VALUES (1, ?)").bind("https://api.example.workers.dev").run();
});
afterEach(() => vi.restoreAllMocks());

describe("collector installation", () => {
	it("provides commands with the token for the first collector, without an upload", async () => {
		vi.spyOn(globalThis, "fetch").mockResolvedValue(Response.json(fixture));
		const res = await releases(installEnv);
		const body = await res.json<any>();
		expect(body.tag).toBe("v0.2.0");
		expect(body.platforms).toHaveLength(4);
		expect(res.headers.get("cache-control")).toBe("no-store");
		expect(body.platforms[0].url).toBe("https://github.com/HSwift/tokscale-serverless/releases/download/v0.2.0/tokscale-client-x86_64-unknown-linux-musl.tar.gz");
		expect(body.platforms[0].command).toContain("SYNC_TOKEN='test-token' bash");
		expect(body.platforms[0].command).toContain("https://api.example.workers.dev/install.sh?platform=linux&tag=v0.2.0");
		expect(body.platforms[3].command).toContain("$env:SYNC_TOKEN='test-token'");
	});

	it("serves reusable installation scripts without embedding credentials", async () => {
		for (let i = 0; i < 2; i++) {
			const res = await SELF.fetch("https://api.example/install.sh?platform=linux&tag=v0.2.0");
			expect(res.status).toBe(200);
			const script = await res.text();
			expect(script).toContain('"$install_tmp/tokscale-client" connect');
			expect(script).not.toContain("test-token");
		}
		expect((await SELF.fetch("https://api.example/api/releases")).status).toBe(401);
	});

	it("handles command quoting and both shell formats", () => {
		const token = "quoted'token$()";
		expect(installCommand("linux", "v0.2.0", "https://api.example", token)).toContain("SYNC_TOKEN='quoted'\\''token$()'");
		expect(installCommand("windows", "v0.2.0", "https://api.example", token)).toContain("$env:SYNC_TOKEN='quoted''token$()'");
		expect(installerScript(new URL("https://api.example/install.ps1?platform=linux&tag=v1")).status).toBe(400);
		expect(installerScript(new URL("https://api.example/install.sh?platform=linux&tag='invalid")).status).toBe(400);
	});
});
