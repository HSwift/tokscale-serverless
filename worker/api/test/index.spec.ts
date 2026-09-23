import { env, SELF } from "cloudflare:test";
import { beforeAll, describe, expect, it } from "vitest";
import schema from "../migrations/0001_init.sql?raw";

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
	const body = schema
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
