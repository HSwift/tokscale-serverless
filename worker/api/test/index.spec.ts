import { env, SELF } from "cloudflare:test";
import { beforeAll, describe, expect, it } from "vitest";
import schema from "../migrations/0001_init.sql?raw";
import hourlySchema from "../migrations/0002_hourly_and_install.sql?raw";
import timelineSchema from "../migrations/0005_utc_timeline.sql?raw";
import worker from "../src/index";

const AUTH = { Authorization: "Bearer test-token" };

const EXPORT_PAYLOAD = {
	meta: { generatedAt: "2026-09-22T00:00:00Z", version: "0.1.0" },
	device: { id: "dev_test1", hostname: "mbp", os: "macos", arch: "aarch64" },
	summary: {},
	contributions: [
		{
			date: "2026-09-21",
			totals: { tokens: 9205, cost: 0.5, messages: 12, costIsComplete: false, credits: 837.5 },
			clients: [
				{
					client: "claude",
					modelId: "claude-sonnet-4-5",
					tokens: { input: 3000, output: 500, cacheRead: 5000, cacheWrite: 500 },
					cost: 0.5,
					messages: 10,
				},
				{
					client: "codex",
					modelId: "gpt-5.4",
					providerId: "openai",
					tokens: { input: 125, output: 40, cacheRead: 25, reasoning: 15 },
					cost: 0,
					messages: 2,
				},
			],
		},
		{
			date: "2026-09-22",
			totals: { tokens: 100, cost: 0.01, messages: 1, credits: 59.5 },
			clients: [
				{
					client: "qoder",
					modelId: "example-qoder-model-b",
					providerId: "qoder",
					tokens: { input: 100 },
					cost: 0,
					messages: 1,
				},
			],
		},
	],
};

beforeAll(async () => {
	// workerd's D1 exec is line-oriented: feed one statement per exec call,
	// collapsed to a single line, comments stripped.
	const body = (schema + "\n" + hourlySchema + "\n" + timelineSchema)
		.split("\n")
		.filter((line) => !line.trimStart().startsWith("--"))
		.join("\n");
	for (const stmt of body.split(";")) {
		const oneLine = stmt.replace(/\s+/g, " ").trim();
		if (oneLine) await env.DB.exec(oneLine);
	}
});

async function postIngest(payload: unknown, token = "test-token") {
	return SELF.fetch("https://example.com/api/ingest", {
		method: "POST",
		headers: { "content-type": "application/json", Authorization: `Bearer ${token}` },
		body: JSON.stringify(payload),
	});
}

describe("tokscale-serverless worker", () => {
	it("/health is open", async () => {
		const res = await SELF.fetch("https://example.com/health");
		expect(res.status).toBe(200);
		expect(await res.json()).toEqual({ status: "ok" });
	});

	it("rejects /api/* without or with wrong token", async () => {
		expect((await SELF.fetch("https://example.com/api/summary")).status).toBe(401);
		expect((await postIngest(EXPORT_PAYLOAD, "wrong")).status).toBe(401);
	});

	it("ingests an export and aggregates it back", async () => {
		const ingest = await postIngest(EXPORT_PAYLOAD);
		expect(ingest.status).toBe(200);
		expect(await ingest.json()).toMatchObject({ ok: true, days: 2, rows: 3 });

		const summary = await (
			await SELF.fetch("https://example.com/api/summary", { headers: AUTH })
		).json();
		expect(summary.summary.input).toBe(3225);
		expect(summary.summary.cacheRead).toBe(5025);
		expect(summary.summary.messages).toBe(13);
		expect(summary.summary.days).toBe(2);
		expect(summary.summary.devices).toBe(1);
		expect(summary.credits).toBeCloseTo(897.0);
		expect(summary.clients.map((c: { client: string }) => c.client)).toEqual([
			"claude",
			"codex",
			"qoder",
		]);

		const daily = await (
			await SELF.fetch("https://example.com/api/daily", { headers: AUTH })
		).json();
		expect(daily.contributions).toHaveLength(2);
		expect(daily.contributions[0].date).toBe("2026-09-21");
		expect(daily.contributions[0].credits).toBeCloseTo(837.5);
		expect(daily.contributions[1].credits).toBeCloseTo(59.5);

		const devices = await (
			await SELF.fetch("https://example.com/api/devices", { headers: AUTH })
		).json();
		expect(devices.devices).toHaveLength(1);
		expect(devices.devices[0].hostname).toBe("mbp");
	});

	it("re-ingesting the same payload does not double-count", async () => {
		await postIngest(EXPORT_PAYLOAD);
		const summary = await (
			await SELF.fetch("https://example.com/api/summary", { headers: AUTH })
		).json();
		expect(summary.summary.input).toBe(3225);
		expect(summary.summary.messages).toBe(13);
		expect(summary.credits).toBeCloseTo(897.0);
	});

	it("aggregates per model via /api/models", async () => {
		const models = await (
			await SELF.fetch("https://example.com/api/models", { headers: AUTH })
		).json();
		const byId = Object.fromEntries(
			models.models.map((m: { modelId: string }) => [m.modelId, m]),
		);
		expect(byId["claude-sonnet-4-5"].tokens).toBe(9000);
		expect(byId["claude-sonnet-4-5"].client).toBe("claude");
		expect(byId["gpt-5.4"].tokens).toBe(205);
		expect(byId["example-qoder-model-b"].messages).toBe(1);
		const filtered = await (
			await SELF.fetch("https://example.com/api/models?since=2026-09-22", {
				headers: AUTH,
			})
		).json();
		expect(filtered.models).toHaveLength(1);
		expect(filtered.models[0].modelId).toBe("example-qoder-model-b");
	});

	it("serves per-day series per device and per model via /api/series", async () => {
		await postIngest({
			device: { id: "dev_test2", hostname: "wsl" },
			contributions: [
				{
					date: "2026-09-22",
					totals: {},
					clients: [
						{ client: "qoder", modelId: "example-qoder-model-a", tokens: { input: 50 }, messages: 1 },
					],
				},
			],
		});

		const byDevice = await (
			await SELF.fetch("https://example.com/api/series?group=devices", { headers: AUTH })
		).json();
		expect(byDevice.entities).toHaveLength(2);
		expect(byDevice.entities[0].key).toBe("dev_test1");
		expect(byDevice.entities[0].label).toBe("mbp");
		expect(byDevice.entities[0].tokens).toBe(9305);
		expect(byDevice.entities[0].days).toEqual([
			{ date: "2026-09-21", tokens: 9205 },
			{ date: "2026-09-22", tokens: 100 },
		]);
		expect(byDevice.entities[1].key).toBe("dev_test2");
		expect(byDevice.entities[1].days).toEqual([{ date: "2026-09-22", tokens: 50 }]);

		const byModel = await (
			await SELF.fetch("https://example.com/api/series?group=models", { headers: AUTH })
		).json();
		expect(byModel.entities[0].key).toBe("claude-sonnet-4-5");
		const qoderModel = byModel.entities.find((e: { key: string }) => e.key === "example-qoder-model-b");
		expect(qoderModel.days).toEqual([{ date: "2026-09-22", tokens: 100 }]);

		const byClient = await (
			await SELF.fetch("https://example.com/api/series?group=clients", { headers: AUTH })
		).json();
		expect(byClient.entities[0].key).toBe("claude");
		expect(byClient.entities[0].days).toEqual([{ date: "2026-09-21", tokens: 9000 }]);
		const qoder = byClient.entities.find((e: { key: string }) => e.key === "qoder");
		expect(qoder.days).toEqual([{ date: "2026-09-22", tokens: 150 }]);
		expect(
			(await SELF.fetch("https://example.com/api/series?group=nope", { headers: AUTH }))
				.status,
		).toBe(400);
	});

	it("admits reads via bearer, forged Access JWT is rejected, /api/me reports actor", async () => {
		const forged = { "Cf-Access-Jwt-Assertion": "forged.jwt.value" };
		expect(
			(await SELF.fetch("https://example.com/api/summary", { headers: forged })).status,
		).toBe(401);
		const me = await (
			await SELF.fetch("https://example.com/api/me", { headers: AUTH })
		).json();
		expect(me.actor).toEqual({ id: "api-token", method: "bearer" });
		const ingest = await SELF.fetch("https://example.com/api/ingest", {
			method: "POST",
			headers: { "content-type": "application/json", ...forged },
			body: JSON.stringify(EXPORT_PAYLOAD),
		});
		expect(ingest.status).toBe(401);
	});

	it("validates dates and device id", async () => {
		expect((await postIngest({ device: {}, contributions: [] })).status).toBe(400);
		const badDate = structuredClone(EXPORT_PAYLOAD);
		badDate.contributions[0].date = "2026-13-99";
		expect((await postIngest(badDate)).status).toBe(400);
		expect(
			(await SELF.fetch("https://example.com/api/summary?since=2026-13-99", { headers: AUTH }))
				.status,
		).toBe(400);
	});
});

describe("D1 usage reduction", () => {
	it("skips unchanged writes, including timestamps, and still accepts corrections and heartbeats", async () => {
		const payload = { ...structuredClone(EXPORT_PAYLOAD), device: { id: "dev_efficient", hostname: "host", name: "original" },
			hourly: [{ date: "2026-09-22", hour: 8, client: "qoder", modelId: "example-qoder-model-b", tokens: 100 }] };
		const first = await (await postIngest(payload)).json();
		expect(first.rowsWritten).toBeGreaterThan(0);
		for (const table of ["daily_rows", "daily_credits", "hourly_rows"]) {
			await env.DB.prepare(`UPDATE ${table} SET updated_at = 'baseline' WHERE device_id = ?`).bind(payload.device.id).run();
		}
		const repeat = await (await postIngest(payload)).json();
		expect(repeat.rowsWritten).toBe(0);
		expect(repeat.rowsRead).toBeGreaterThanOrEqual(0);
		for (const table of ["daily_rows", "daily_credits", "hourly_rows"]) {
			const row = await env.DB.prepare(`SELECT COUNT(*) AS changed FROM ${table} WHERE device_id = ? AND updated_at <> 'baseline'`).bind(payload.device.id).first();
			expect(row!.changed).toBe(0);
		}
		// NULL comparisons must detect both filling and clearing optional values.
		const changed = structuredClone(payload);
		changed.contributions[1].clients[0].tokens.input = 90;
		changed.contributions[1].clients[0].providerId = "changed-provider";
		changed.contributions[1].totals.credits = 0;
		changed.hourly[0].tokens = 90;
		changed.device.name = "renamed";
		expect((await (await postIngest(changed)).json()).rowsWritten).toBeGreaterThan(0);
		const daily = await env.DB.prepare("SELECT input, provider_id FROM daily_rows WHERE device_id = ? AND date = '2026-09-22'").bind(payload.device.id).first();
		expect(daily).toMatchObject({ input: 90, provider_id: "changed-provider" });
		expect((await env.DB.prepare("SELECT credits FROM daily_credits WHERE device_id = ? AND date = '2026-09-22'").bind(payload.device.id).first())!.credits).toBe(0);
		delete changed.contributions[1].clients[0].providerId;
		expect((await (await postIngest(changed)).json()).rowsWritten).toBeGreaterThan(0);
		expect((await env.DB.prepare("SELECT provider_id FROM daily_rows WHERE device_id = ? AND date = '2026-09-22'").bind(payload.device.id).first())!.provider_id).toBeNull();
		await env.DB.prepare("UPDATE devices SET last_seen = '2000-01-01 00:00:00' WHERE id = ?").bind(payload.device.id).run();
		expect((await (await postIngest({ device: changed.device, contributions: [] })).json()).rowsWritten).toBeGreaterThan(0);
	});

	it("returns the same three series from one dashboard query and honors date/device filters", async () => {
		const deviceId = "dev_dashboard";
		await postIngest({ ...EXPORT_PAYLOAD, device: { id: deviceId, name: "Dashboard host" } });
		const query = `deviceId=${deviceId}&since=2026-09-22&until=2026-09-22`;
		const combined = await (await SELF.fetch(`https://example.com/api/dashboard?${query}`, { headers: AUTH })).json();
		for (const group of ["devices", "clients", "models"]) {
			const separate = await (await SELF.fetch(`https://example.com/api/series?group=${group}&${query}`, { headers: AUTH })).json();
			expect(combined[group]).toEqual(separate);
		}
		expect(combined.devices.entities[0].tokens).toBe(100);
		expect(combined.breakdown).toEqual([{ deviceId, client: "qoder", modelId: "example-qoder-model-b", date: "2026-09-22", tokens: 100 }]);
		// Cached results still require authentication on every request.
		expect((await SELF.fetch(`https://example.com/api/dashboard?${query}`)).status).toBe(401);
		const corrected = structuredClone(EXPORT_PAYLOAD);
		corrected.device.id = deviceId;
		corrected.contributions[1].clients[0].tokens.input = 80;
		await postIngest(corrected);
		const updated = await (await SELF.fetch(`https://example.com/api/dashboard?${query}`, { headers: AUTH })).json();
		expect(updated.devices.entities[0].tokens).toBe(80);
		expect(updated.breakdown).toEqual([{ deviceId, client: "qoder", modelId: "example-qoder-model-b", date: "2026-09-22", tokens: 80 }]);
		expect((await SELF.fetch("https://example.com/api/dashboard?since=bad", { headers: AUTH })).status).toBe(400);
	});
});

describe("diagnostics", () => {
	it("validates without touching D1 and returns the same field error as ingest", async () => {
		const forbiddenDB = { prepare() { throw new Error("D1 must not be touched"); } } as unknown as D1Database;
		const validate = (body: unknown, path = "/api/ingest/validate", token = "test-token") => worker.fetch(new Request(`https://example.com${path}`, {
			method: "POST", headers: { Authorization: `Bearer ${token}` }, body: JSON.stringify(body),
		}), { ...env, DB: forbiddenDB });
		expect((await validate(EXPORT_PAYLOAD, undefined, "wrong")).status).toBe(401);
		const result = await validate(EXPORT_PAYLOAD);
		expect(await result.json()).toMatchObject({ ok: true, validationOnly: true, days: 2, rows: 3 });
		const valid = { date: "2026-09-28", hour: 8, client: "qoder", modelId: "example-model", tokens: 100 };
		for (const [field, value] of [["date", "2026-02-30"], ["hour", 24], ["modelId", ""], ["client", ""], ["tokens", -1], ["tokens", 2 ** 53]]) {
			const bad = { ...EXPORT_PAYLOAD, hourly: [{ ...valid, [field]: value }] };
			const validation = await validate(bad);
			const ingest = await validate(bad, "/api/ingest");
			expect(validation.status).toBe(400);
			const error = await validation.json();
			expect(error).toMatchObject({ error: { code: "bad_request", path: `hourly[0].${field}` } });
			expect(await ingest.json()).toEqual(error);
		}
		for (const malformed of [null, [], { device: { id: "test" }, contributions: [null] }]) {
			expect((await validate(malformed)).status).toBe(400);
		}
	});

	it("authenticates health checks and diagnoses schema and D1 failures", async () => {
		expect((await SELF.fetch("https://example.com/api/diagnostics")).status).toBe(401);
		const response = await SELF.fetch("https://example.com/api/diagnostics", { headers: AUTH });
		expect(await response.json()).toMatchObject({ ok: true, apiVersion: "0.4.0", database: { ok: true, missingTables: [], rowsWritten: 0 } });
		const request = () => new Request("https://example.com/api/diagnostics", { headers: AUTH });
		const broken = { prepare() { throw new Error("D1 quota exceeded test-token"); } } as unknown as D1Database;
		const failure = await worker.fetch(request(), { ...env, DB: broken });
		expect(failure.status).toBe(503);
		const text = await failure.text();
		expect(text).toContain("quota exceeded");
		expect(text).not.toContain("test-token");
		const empty = { prepare() { return { all: async () => ({ results: [], meta: { rows_read: 0, rows_written: 0 } }) }; } } as unknown as D1Database;
		const missing = await worker.fetch(request(), { ...env, DB: empty });
		expect(missing.status).toBe(503);
		expect((await missing.json()).database.missingTables).toContain("daily_rows");
	});
});

describe("hourly usage", () => {
	it("groups hourly curves by device, tool, or model and intersects filters", async () => {
		const date = "2026-09-25";
		for (const [id, name, records] of [
			["hourly_group_a", "Laptop", [
				{ client: "codex", modelId: "model.with.dot", hour: 8, tokens: 100 },
				{ client: "claude", modelId: "model.with.dot", hour: 8, tokens: 20 },
				{ client: "qoder", modelId: "other-model", hour: 12, tokens: 60 },
			]],
			["hourly_group_b", "Server", [
				{ client: "codex", modelId: "model.with.dot", hour: 8, tokens: 50 },
				{ client: "qoder", modelId: "other-model", hour: 20, tokens: 40 },
			]],
		] as const) {
			expect((await postIngest({
				device: { id, name, hostname: "fallback-host" },
				contributions: [{ date, clients: records.map(row => ({
					client: row.client, modelId: row.modelId, tokens: { input: row.tokens },
				})) }],
				hourly: records.map(row => ({ date, ...row })),
			})).status).toBe(200);
		}
		const query = async (params: string) => (await SELF.fetch(
			`https://example.com/api/hourly?date=${date}&${params}`, { headers: AUTH },
		)).json();
		for (const [group, expected] of [
			["devices", [["hourly_group_a", 180], ["hourly_group_b", 90]]],
			["clients", [["codex", 150], ["qoder", 100], ["claude", 20]]],
			["models", [["model.with.dot", 170], ["other-model", 100]]],
		] as const) {
			const result = await query(`group=${group}`);
			expect(result).toMatchObject({ group, totalTokens: 270, hourlyTokens: 270, complete: true });
			expect(result.entities.map((entity: { key: string; tokens: number }) => [entity.key, entity.tokens])).toEqual(expected);
			for (const entity of result.entities) {
				expect(entity.hours).toHaveLength(24);
				expect(entity.hours[0]).toEqual({ hour: 0, tokens: 0 });
			}
			for (const hour of result.hours) {
				expect(hour.tokens).toBe(result.entities.reduce((sum: number, entity: { hours: { tokens: number }[] }) => sum + entity.hours[hour.hour].tokens, 0));
			}
		}
		const devices = await query("group=devices&client=qoder");
		expect(devices.totalTokens).toBe(100);
		expect(devices.entities.map((entity: { label: string }) => entity.label)).toEqual(["Laptop", "Server"]);
		expect(devices.entities[0].hours[12].tokens).toBe(60);
		expect(devices.entities[1].hours[20].tokens).toBe(40);
		const filtered = await query("group=clients&deviceId=hourly_group_a&modelId=model.with.dot&client=codex");
		expect(filtered).toMatchObject({ totalTokens: 100, hourlyTokens: 100, complete: true });
		expect(filtered.entities).toHaveLength(1);
		expect(filtered.entities[0].key).toBe("codex");
		expect(filtered.hours[8].tokens).toBe(100);
		expect((await query("group=models&client=missing-tool")).entities).toEqual([]);
		expect((await SELF.fetch(`https://example.com/api/hourly?date=${date}&group=nope`, { headers: AUTH })).status).toBe(400);
	});

	it("keeps resends idempotent, fills 24 hours, and filters device/model", async () => {
		const payload = {
			device: { id: "dev_hourly" },
			contributions: [{ date: "2026-09-28", clients: [
				{ client: "codex", modelId: "model-a", tokens: { input: 130 } },
				{ client: "claude", modelId: "model-b", tokens: { output: 20 } },
			] }],
			hourly: [
				{ date: "2026-09-28", hour: 0, client: "codex", modelId: "model-a", tokens: 100 },
				{ date: "2026-09-28", hour: 23, client: "codex", modelId: "model-a", tokens: 30 },
				{ date: "2026-09-28", hour: 23, client: "claude", modelId: "model-b", tokens: 20 },
			],
		};
		for (let i = 0; i < 2; i++) expect((await postIngest(payload)).status).toBe(200);
		const result = await (await SELF.fetch("https://example.com/api/hourly?date=2026-09-28&deviceId=dev_hourly", { headers: AUTH })).json();
		expect(result.totalTokens).toBe(150);
		expect(result.hourlyTokens).toBe(150);
		expect(result.complete).toBe(true);
		expect(result.hours).toHaveLength(24);
		expect(result.hours[0].tokens).toBe(100);
		expect(result.hours[1].tokens).toBe(0);
		expect(result.hours[23].tokens).toBe(50);
		const filtered = await (await SELF.fetch("https://example.com/api/hourly?date=2026-09-28&deviceId=dev_hourly&modelId=model-b", { headers: AUTH })).json();
		expect(filtered.totalTokens).toBe(20);
		expect(filtered.hourlyTokens).toBe(20);
		expect(filtered.hours[23].tokens).toBe(20);
		const bad = structuredClone(payload);
		bad.hourly[0].hour = 24;
		bad.contributions[0].clients[0].tokens.input = 999;
		expect((await postIngest(bad)).status).toBe(400);
		const unchanged = await (await SELF.fetch("https://example.com/api/hourly?date=2026-09-28&deviceId=dev_hourly", { headers: AUTH })).json();
		expect(unchanged.totalTokens).toBe(150);
	});

	it("distinguishes legacy daily-only data from an empty day", async () => {
		await postIngest({ device: { id: "dev_legacy_hourly" }, contributions: [
			{ date: "2026-09-27", clients: [{ client: "codex", modelId: "legacy", tokens: { input: 40 } }] },
		] });
		const legacy = await (await SELF.fetch("https://example.com/api/hourly?date=2026-09-27&deviceId=dev_legacy_hourly", { headers: AUTH })).json();
		expect(legacy).toMatchObject({ totalTokens: 40, hasHourlyData: false, complete: false });
		const empty = await (await SELF.fetch("https://example.com/api/hourly?date=2026-09-26&deviceId=dev_legacy_hourly", { headers: AUTH })).json();
		expect(empty).toMatchObject({ totalTokens: 0, hasHourlyData: false, complete: true });
		expect((await SELF.fetch("https://example.com/api/hourly?date=2026-02-30", { headers: AUTH })).status).toBe(400);
		expect((await SELF.fetch("https://example.com/api/hourly?date=2026-09-28")).status).toBe(401);
	});
});

const minute = (time: string) => Date.parse(time) / 60_000;

describe("UTC timelines", () => {
    const payload = (id: string, date: string, entries: [number, number][], zone = "Asia/Tokyo") => ({
        meta: { timelineVersion: 1, sourceTimeZone: zone },
        device: { id }, contributions: [{ date, clients: [{ client: "codex", modelId: "test-model",
            tokens: { input: entries.reduce((sum, point) => sum + point[1], 0) }, timeline: entries }] }],
    });
    const get = async (path: string) => {
        const response = await SELF.fetch(`https://example.com/api/${path}`, { headers: AUTH });
        expect(response.status).toBe(200);
        return response.json() as Promise<any>;
    };

    it("replaces legacy hours without duplication and converts Tokyo noon to Shanghai 11am", async () => {
        const id = "dev_utc_upgrade";
        const legacy = { device: { id }, contributions: [{ date: "2026-09-29", clients: [{
            client: "codex", modelId: "test-model", tokens: { input: 100 },
        }] }], hourly: [{ date: "2026-09-29", hour: 12, client: "codex", modelId: "test-model", tokens: 100 }] };
        expect((await postIngest(legacy)).status).toBe(200);
        const query = `date=2026-09-29&deviceId=${id}&timeZone=Asia%2FShanghai`;
        const old = await get(`hourly?${query}`);
        expect(old.totalTokens).toBe(100);
        expect(old.legacyTokens).toBe(100);
        expect(old.hourlyTokens).toBe(0);
        const upgraded = payload(id, "2026-09-29", [[minute("2026-09-29T03:15:00Z"), 100]]);
        const first = await postIngest({ ...upgraded, hourly: legacy.hourly });
        expect(await first.json()).toMatchObject({ ok: true, timelineVersion: 1 });
        const current = await get(`hourly?${query}`);
        expect(current).toMatchObject({ timezone: "Asia/Shanghai", totalTokens: 100, hourlyTokens: 100, legacyTokens: 0, complete: true });
        expect(current.hours[11].tokens).toBe(100);
        expect(current.hours[12].tokens).toBe(0);
        const dashboard = await get(`dashboard?since=2026-09-29&until=2026-09-29&deviceId=${id}&timeZone=Asia%2FShanghai`);
        expect(dashboard.devices.entities[0].tokens).toBe(100);
        expect(dashboard.breakdown).toHaveLength(1);
        expect((await (await postIngest(upgraded)).json()).rowsWritten).toBe(0);
        // A delayed older collector cannot erase the absolute timestamps.
        await postIngest(legacy);
        expect((await get(`hourly?${query}`)).legacyTokens).toBe(0);
    });

    it("rebuckets all dashboard dimensions across midnight and keeps timezone caches separate", async () => {
        const id = "dev_utc_midnight";
        const modern = payload(id, "2026-09-29", [[minute("2026-09-28T15:30:00Z"), 20], [minute("2026-09-28T16:30:00Z"), 30]]);
        expect((await postIngest(modern)).status).toBe(200);
        const query = `since=2026-09-28&until=2026-09-29&deviceId=${id}`;
        const shanghai = await get(`dashboard?${query}&timeZone=Asia%2FShanghai`);
        for (const group of ["devices", "clients", "models"]) {
            expect(shanghai[group].entities[0].days).toEqual([{ date: "2026-09-28", tokens: 20 }, { date: "2026-09-29", tokens: 30 }]);
        }
        const tokyo = await get(`dashboard?${query}&timeZone=Asia%2FTokyo`);
        expect(tokyo.devices.entities[0].days).toEqual([{ date: "2026-09-29", tokens: 50 }]);
        const hourly = await get(`hourly?date=2026-09-29&deviceId=${id}&client=codex&modelId=test-model&group=models&timeZone=Asia%2FShanghai`);
        expect(hourly.hours[0].tokens).toBe(30);
        expect(hourly.totalTokens).toBe(30);
        expect(hourly.entities[0].key).toBe("test-model");
    });

    it("handles fractional offsets and both occurrences of a DST hour", async () => {
        const id = "dev_utc_fractional";
        await postIngest(payload(id, "2026-09-29", [[minute("2026-09-29T03:15:00Z"), 10], [minute("2026-09-29T03:45:00Z"), 20]]));
        const india = await get(`hourly?date=2026-09-29&deviceId=${id}&timeZone=Asia%2FKolkata`);
        expect(india.hours[8].tokens).toBe(10);
        expect(india.hours[9].tokens).toBe(20);
        await postIngest(payload(id, "2026-11-01", [[minute("2026-11-01T05:30:00Z"), 30], [minute("2026-11-01T06:30:00Z"), 40]], "America/New_York"));
        const ny = await get(`hourly?date=2026-11-01&deviceId=${id}&timeZone=America%2FNew_York`);
        expect(ny.hours[1].tokens).toBe(70);
        expect(ny.hourlyTokens).toBe(ny.totalTokens);
    });

    it("rejects invalid timezone or UTC timelines before any D1 work", async () => {
        for (const zone of ["", "Not/A_Zone"]) {
            const response = await SELF.fetch(`https://example.com/api/hourly?date=2026-09-29&timeZone=${zone}`, { headers: AUTH });
            expect(response.status).toBe(400);
        }
        const t = minute("2026-09-29T03:15:00Z");
        for (const entries of [[[t, -1]], [[t, 1], [t, 1]], [[t, 1.5]], [[t - 10000, 1]]] as [number, number][][]) {
            expect((await postIngest(payload("dev_invalid_timeline", "2026-09-29", entries))).status).toBe(400);
        }
    });
});
