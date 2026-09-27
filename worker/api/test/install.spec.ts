import { env, SELF } from "cloudflare:test";
import { beforeAll, beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import { issueInstall, redeemInstall, releases } from "../src/install";
import { readApiOrigin } from "../src/endpoint";
import type { Env } from "../src/auth";
import initialSchema from "../migrations/0001_init.sql?raw";
import schema from "../migrations/0002_hourly_and_install.sql?raw";
import originSchema from "../migrations/0003_api_origin.sql?raw";

const installEnv = { ...env, INGEST_TOKEN: "test-token" } as Env;
const assets = ["x86_64-unknown-linux-musl.tar.gz", "aarch64-apple-darwin.tar.gz", "x86_64-apple-darwin.tar.gz", "x86_64-pc-windows-msvc.zip"].map(target => ({ name: `tokscale-client-${target}` }));
const fixture = { tag_name: "v0.2.0", draft: false, prerelease: false, assets: [...assets, { name: "SHA256SUMS" }] };
function mockRelease(data = fixture) { vi.spyOn(globalThis, "fetch").mockImplementation(async () => Response.json(data)); }
function request(platform = "linux") { return new Request("https://console.example/api/install-tickets", { method: "POST", body: JSON.stringify({ platform }) }); }

beforeAll(async () => {
	const sql = [initialSchema, schema, originSchema].join("\n").split("\n").filter(line => !line.trimStart().startsWith("--")).join("\n");
	for (const stmt of sql.split(";")) if (stmt.trim()) await env.DB.exec(stmt.replace(/\s+/g, " ").trim());
});
beforeEach(async () => {
	await env.DB.prepare("INSERT OR REPLACE INTO api_origin (id, origin) VALUES (1, ?)").bind("https://api.example").run();
});
afterEach(() => vi.restoreAllMocks());

describe("installation handoff", () => {
	it("uses latest release tag and only offers real assets", async () => {
		mockRelease();
		const res = await releases(installEnv);
		const body = await res.json<any>();
		expect(body.tag).toBe("v0.2.0");
		expect(body.platforms).toHaveLength(4);
		expect(body.platforms[0].url).toBe("https://github.com/HSwift/tokscale-serverless/releases/download/v0.2.0/tokscale-client-x86_64-unknown-linux-musl.tar.gz");
		expect(body.installReady).toBe(true);
		expect(body.apiUrl).toBe("https://api.example");
		expect(JSON.stringify(body)).not.toContain("test-token");
	});

	it("issues a secret-free command and redeems a ticket exactly once", async () => {
		mockRelease();
		const issued = await issueInstall(request(), installEnv);
		const body = await issued.json<any>();
		expect(issued.headers.get("cache-control")).toBe("no-store");
		expect(body.command).toMatch(/^curl -fsSL .* \| bash$/);
		expect(body.command).toContain("https://api.example/install/");
		expect(body.command).not.toContain("console.example");
		expect(body.command).not.toContain("test-token");
		const ticket = body.command.match(/\/install\/([a-f0-9]{64})/)[1];
		const results = await Promise.all([redeemInstall(ticket, installEnv), redeemInstall(ticket, installEnv)]);
		expect(results.map(result => result.status).sort()).toEqual([200, 410]);
		const script = await results.find(result => result.status === 200)!.text();
		expect(script).toContain("SYNC_TOKEN='test-token'");
		expect(script).toContain("SYNC_URL='https://api.example'");
		expect(script).toContain("Checksum verification failed");
		expect(script).toContain('"$install_tmp/tokscale-client" connect');
	});

	it("rejects expired tickets, missing credentials, and unpublished releases", async () => {
		mockRelease();
		const body = await (await issueInstall(request(), installEnv)).json<any>();
		const ticket = body.command.match(/\/install\/([a-f0-9]{64})/)[1];
		await env.DB.prepare("UPDATE install_tickets SET expires_at = 0").run();
		expect((await redeemInstall(ticket, installEnv)).status).toBe(410);
		expect((await issueInstall(request(), { ...installEnv, INGEST_TOKEN: undefined })).status).toBe(503);
		vi.restoreAllMocks(); mockRelease({ ...fixture, draft: true });
		expect((await releases(installEnv)).status).toBe(502);
	});

	it("learns its public origin only from successful authenticated HTTPS uploads", async () => {
		await env.DB.prepare("DELETE FROM api_origin").run();
		mockRelease();
		expect(await (await releases(installEnv)).json()).toMatchObject({ apiUrl: null, installReady: false });
		expect((await issueInstall(request(), installEnv)).status).toBe(503);
		const upload = (origin: string, token = "test-token", payload: unknown = { device: { id: "discovery-test" }, contributions: [] }) => SELF.fetch(`${origin}/api/ingest`, {
			method: "POST", headers: { Authorization: `Bearer ${token}`, "content-type": "application/json",
				"x-forwarded-host": "untrusted.example", "x-forwarded-proto": "http" }, body: JSON.stringify(payload),
		});
		expect((await upload("https://bad.example", "wrong")).status).toBe(401);
		expect((await upload("https://bad.example", "test-token", {})).status).toBe(400);
		expect((await upload("http://127.0.0.1:18787")).status).toBe(200);
		expect(await readApiOrigin(installEnv)).toBeNull();
		expect((await upload("https://api.example")).status).toBe(200);
		expect(await readApiOrigin(installEnv)).toBe("https://api.example");
		await SELF.fetch("https://console.example/api/summary", { headers: { Authorization: "Bearer test-token" } });
		expect(await readApiOrigin(installEnv)).toBe("https://api.example");
		expect(await (await releases(installEnv)).json()).toMatchObject({ apiUrl: "https://api.example", installReady: true });
		expect((await upload("https://new-api.example")).status).toBe(200);
		expect(await readApiOrigin(installEnv)).toBe("https://new-api.example");
	});

	it("provides PowerShell on Windows and escapes embedded credentials", async () => {
		mockRelease();
		const special = { ...installEnv, INGEST_TOKEN: "quoted'token$()" };
		const body = await (await issueInstall(request("windows"), special)).json<any>();
		expect(body.command).toContain("powershell -NoProfile");
		const ticket = body.command.match(/\/install\/([a-f0-9]{64})/)[1];
		const script = await (await redeemInstall(ticket, special)).text();
		expect(script).toContain("$env:SYNC_TOKEN = 'quoted''token$()'");
		expect(script).toContain("Get-FileHash");
		expect(script).toContain("$env:SYNC_TOKEN = $previousToken");
	});

	it("requires authentication even when given a forged Access header", async () => {
		for (const headers of [{}, { "cf-access-jwt-assertion": "forged.jwt" }]) {
			const response = await SELF.fetch("https://example.com/api/install-tickets", { method: "POST", headers, body: JSON.stringify({ platform: "linux" }) });
			expect(response.status).toBe(401);
		}
	});
});
