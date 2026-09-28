import { env, SELF } from "cloudflare:test";
import { beforeAll, describe, expect, it } from "vitest";
import schema from "../migrations/0001_init.sql?raw";
import hourlySchema from "../migrations/0002_hourly_and_install.sql?raw";
import originSchema from "../migrations/0003_api_origin.sql?raw";

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
					modelId: "kmodel_latest",
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
	const body = (schema + "\n" + hourlySchema + "\n" + originSchema)
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
		expect(await ingest.json()).toEqual({ ok: true, days: 2, rows: 3 });

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
		expect(byId["kmodel_latest"].messages).toBe(1);
		const filtered = await (
			await SELF.fetch("https://example.com/api/models?since=2026-09-22", {
				headers: AUTH,
			})
		).json();
		expect(filtered.models).toHaveLength(1);
		expect(filtered.models[0].modelId).toBe("kmodel_latest");
	});

	it("serves per-day series per device and per model via /api/series", async () => {
		await postIngest({
			device: { id: "dev_test2", hostname: "wsl" },
			contributions: [
				{
					date: "2026-09-22",
					totals: {},
					clients: [
						{ client: "qoder", modelId: "qmodel", tokens: { input: 50 }, messages: 1 },
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
		const kmodel = byModel.entities.find((e: { key: string }) => e.key === "kmodel_latest");
		expect(kmodel.days).toEqual([{ date: "2026-09-22", tokens: 100 }]);

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
