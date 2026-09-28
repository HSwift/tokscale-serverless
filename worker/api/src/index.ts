/**
 * tokscale-serverless-api: cloud aggregation point for tokscale-client agents.
 *
 * Collectors push aggregated usage (TsExport shape) to
 * POST /api/ingest with a shared bearer token; rows upsert idempotently per
 * (device, date, client, model), so any device may resend full history at any
 * time. Read endpoints accept the bearer token (scripts) or a verified
 * Cloudflare Access JWT (browser console, verified against the team JWKS).
 * Qoder plan credits travel day-level and stay out of USD cost columns.
 */

import {
	authenticateAccess,
	authenticateBearer,
	type AuthActor,
	type Env,
} from "./auth";
import { installerScript, releases } from "./install";
import { cachedRead, invalidateReads } from "./read-cache";
import { isValidDate, validateIngest, type IngestIssue } from "./validate";

interface TsTokenBreakdown {
	input?: number;
	output?: number;
	cacheRead?: number;
	cacheWrite?: number;
	reasoning?: number;
}

interface TsSourceContribution {
	client?: string;
	modelId?: string;
	providerId?: string;
	tokens?: TsTokenBreakdown;
	cost?: number;
	messages?: number;
}

interface TsDailyTotals {
	tokens?: number;
	cost?: number;
	messages?: number;
	costIsComplete?: boolean;
	credits?: number;
}

interface TsDailyContribution {
	date?: string;
	totals?: TsDailyTotals;
	clients?: TsSourceContribution[];
}

interface TsDevice {
	id?: string;
	name?: string;
	hostname?: string;
	os?: string;
	arch?: string;
}

interface TsExport {
	device?: TsDevice;
	contributions?: TsDailyContribution[];
	hourly?: { date: string; hour: number; client: string; modelId: string; tokens: number }[];
}

function json(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { "content-type": "application/json" },
	});
}

function error(status: number, code: string, message: string): Response {
	return json({ error: { code, message } }, status);
}

async function requireBearer(request: Request, env: Env): Promise<Response | null> {
	if (!env.INGEST_TOKEN) {
		return error(503, "auth_not_configured", "INGEST_TOKEN secret is not set");
	}
	if (!(await authenticateBearer(request, env))) {
		return error(401, "unauthorized", "missing or invalid bearer token");
	}
	return null;
}

/**
 * Read endpoints: verified Access JWT (console browser) or bearer (scripts).
 * Returns the actor on success so handlers can surface it (/api/me).
 */
async function authenticateRead(
	request: Request,
	env: Env,
): Promise<{ actor: AuthActor } | { failure: Response }> {
	const [accessActor, bearerActor] = await Promise.all([
		authenticateAccess(request, env),
		authenticateBearer(request, env),
	]);
	const actor = accessActor ?? bearerActor;
	if (!actor) {
		if (!env.INGEST_TOKEN) {
			return { failure: error(503, "auth_not_configured", "INGEST_TOKEN secret is not set") };
		}
		return {
			failure: error(
				401,
				"unauthorized",
				"a valid Cloudflare Access assertion or bearer token is required",
			),
		};
	}
	return { actor };
}

function num(value: unknown): number {
	const n = Number(value);
	return Number.isFinite(n) ? n : 0;
}

function rejectIngest(issue: IngestIssue): Response {
	console.warn("ingest validation failed", issue);
	return json({ error: { code: "bad_request", path: issue.path, message: `${issue.path}: ${issue.message}` } }, 400);
}

async function ingest(request: Request, env: Env, validationOnly = false): Promise<Response> {
	let body: TsExport;
	try {
		body = (await request.json()) as TsExport;
	} catch {
		return rejectIngest({ path: "body", message: "must be valid JSON" });
	}
	const issue = validateIngest(body);
	if (issue) return rejectIngest(issue);
	if (validationOnly) {
		return json({ ok: true, validationOnly: true, days: body.contributions!.length,
			rows: body.contributions!.reduce((n, day) => n + (day.clients ?? []).filter(row => row.client && row.modelId).length, 0),
			hourly: body.hourly?.length ?? 0 });
	}
	const device = body.device!;

	const stmts: D1PreparedStatement[] = [
		env.DB.prepare(
			`INSERT INTO devices (id, name, hostname, os, arch, last_seen)
			 VALUES (?, ?, ?, ?, ?, datetime('now'))
			 ON CONFLICT(id) DO UPDATE SET
			   name = excluded.name,
			   hostname = excluded.hostname,
			   os = excluded.os,
			   arch = excluded.arch,
			   last_seen = datetime('now')
			 WHERE devices.name IS NOT excluded.name
			    OR devices.hostname IS NOT excluded.hostname
			    OR devices.os IS NOT excluded.os
			    OR devices.arch IS NOT excluded.arch
			    OR devices.last_seen <= datetime('now', '-1 hour')`,
		).bind(
			device.id,
			device.name ?? null,
			device.hostname ?? null,
			device.os ?? null,
			device.arch ?? null,
		),
	];

	let days = 0;
	let rows = 0;
	for (const day of body.contributions!) {
		const date = day.date ?? "";
		days += 1;
		const costIsComplete = day.totals?.costIsComplete;
		for (const row of day.clients ?? []) {
			if (!row.client || !row.modelId) continue;
			rows += 1;
			stmts.push(
				env.DB.prepare(
					`INSERT INTO daily_rows
					   (device_id, date, client, model_id, provider_id,
					    input, output, cache_read, cache_write, reasoning,
					    cost, messages, cost_is_complete, updated_at)
					 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, datetime('now'))
					 ON CONFLICT(device_id, date, client, model_id) DO UPDATE SET
					   provider_id = excluded.provider_id,
					   input = excluded.input,
					   output = excluded.output,
					   cache_read = excluded.cache_read,
					   cache_write = excluded.cache_write,
					   reasoning = excluded.reasoning,
					   cost = excluded.cost,
					   messages = excluded.messages,
					   cost_is_complete = excluded.cost_is_complete,
					   updated_at = datetime('now')
					 WHERE daily_rows.provider_id IS NOT excluded.provider_id
					    OR daily_rows.input IS NOT excluded.input
					    OR daily_rows.output IS NOT excluded.output
					    OR daily_rows.cache_read IS NOT excluded.cache_read
					    OR daily_rows.cache_write IS NOT excluded.cache_write
					    OR daily_rows.reasoning IS NOT excluded.reasoning
					    OR daily_rows.cost IS NOT excluded.cost
					    OR daily_rows.messages IS NOT excluded.messages
					    OR daily_rows.cost_is_complete IS NOT excluded.cost_is_complete`,
				).bind(
					device.id,
					date,
					row.client,
					row.modelId,
					row.providerId ?? null,
					num(row.tokens?.input),
					num(row.tokens?.output),
					num(row.tokens?.cacheRead),
					num(row.tokens?.cacheWrite),
					num(row.tokens?.reasoning),
					num(row.cost),
					num(row.messages),
					costIsComplete === undefined ? null : costIsComplete ? 1 : 0,
				),
			);
		}
		if (day.totals?.credits !== undefined) {
			stmts.push(
				env.DB.prepare(
					`INSERT INTO daily_credits (device_id, date, credits, updated_at)
					 VALUES (?, ?, ?, datetime('now'))
					 ON CONFLICT(device_id, date) DO UPDATE SET
					   credits = excluded.credits,
					   updated_at = datetime('now')
					 WHERE daily_credits.credits IS NOT excluded.credits`,
				).bind(device.id, date, num(day.totals.credits)),
			);
		}
	}

	if (body.hourly !== undefined) {
		for (const row of body.hourly) {
			stmts.push(env.DB.prepare(`INSERT INTO hourly_rows
				(device_id, date, hour, client, model_id, tokens) VALUES (?, ?, ?, ?, ?, ?)
				ON CONFLICT(device_id, date, hour, client, model_id) DO UPDATE SET
				tokens = excluded.tokens, updated_at = datetime('now')
				WHERE hourly_rows.tokens IS NOT excluded.tokens`)
				.bind(device.id, row.date, row.hour, row.client, row.modelId, row.tokens));
		}
	}

	// Bound batch size for full-history resends.
	let rowsRead = 0, rowsWritten = 0;
	try {
		for (let i = 0; i < stmts.length; i += 100) {
			const results = await env.DB.batch(stmts.slice(i, i + 100));
			for (const result of results) {
				rowsRead += result.meta.rows_read;
				rowsWritten += result.meta.rows_written;
			}
		}
	} catch (error) {
		// A failed response may still follow a committed earlier batch.
		invalidateReads(env);
		const message = String(error).split(env.INGEST_TOKEN || "\0").join("[redacted]").slice(0, 1024);
		console.error("ingest database failed", { message });
		return json({ error: { code: "database_error", message } }, 500);
	}
	if (rowsWritten > 0) invalidateReads(env);
	return json({ ok: true, days, rows, rowsRead, rowsWritten });
}

interface Filters {
	where: string;
	binds: string[];
}

// On-demand only: inspect a few schema records, not usage tables. Auth is checked
// before reaching this handler. Never include credentials or device statistics.
async function diagnostics(env: Env): Promise<Response> {
	const started = Date.now();
	const base = { apiVersion: "0.3.0", serverTime: new Date().toISOString() };
	try {
		const result = await env.DB.prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name IN ('devices', 'daily_rows', 'daily_credits', 'hourly_rows')")
			.all<{ name: string }>();
		const found = new Set(result.results.map(row => row.name));
		const missingTables = ["devices", "daily_rows", "daily_credits", "hourly_rows"].filter(name => !found.has(name));
		const ok = missingTables.length === 0;
		return json({ ...base, ok, database: { ok, missingTables, elapsedMs: Date.now() - started,
			rowsRead: result.meta.rows_read, rowsWritten: result.meta.rows_written } }, ok ? 200 : 503);
	} catch (error) {
		const message = String(error).split(env.INGEST_TOKEN || "\0").join("[redacted]").slice(0, 1024);
		return json({ ...base, ok: false, database: { ok: false, elapsedMs: Date.now() - started, error: message } }, 503);
	}
}

function filters(url: URL): Filters | Response {
	const clauses: string[] = [];
	const binds: string[] = [];
	for (const [key, column] of [
		["since", "date"],
		["until", "date"],
	] as const) {
		const value = url.searchParams.get(key);
		if (value !== null) {
			if (!isValidDate(value)) {
				return error(400, "bad_request", `invalid ${key} date (expected YYYY-MM-DD)`);
			}
			clauses.push(`${column} ${key === "since" ? ">=" : "<="} ?`);
			binds.push(value);
		}
	}
	const deviceId = url.searchParams.get("deviceId");
	if (deviceId) {
		clauses.push("device_id = ?");
		binds.push(deviceId);
	}
	return {
		where: clauses.length ? `WHERE ${clauses.join(" AND ")}` : "",
		binds,
	};
}

async function summary(url: URL, env: Env): Promise<Response> {
	const f = filters(url);
	if (f instanceof Response) return f;
	const totals = await env.DB.prepare(
		`SELECT COALESCE(SUM(input), 0) AS input,
		        COALESCE(SUM(output), 0) AS output,
		        COALESCE(SUM(cache_read), 0) AS cacheRead,
		        COALESCE(SUM(cache_write), 0) AS cacheWrite,
		        COALESCE(SUM(reasoning), 0) AS reasoning,
		        COALESCE(SUM(cost), 0) AS cost,
		        COALESCE(SUM(messages), 0) AS messages,
		        COUNT(DISTINCT date) AS days,
		        COUNT(DISTINCT device_id) AS devices
		 FROM daily_rows ${f.where}`,
	)
		.bind(...f.binds)
		.first();
	const credits = await env.DB.prepare(
		`SELECT COALESCE(SUM(credits), 0) AS credits FROM daily_credits ${f.where}`,
	)
		.bind(...f.binds)
		.first();
	const byClient = await env.DB.prepare(
		`SELECT client,
		        SUM(input + output + cache_read + cache_write + reasoning) AS tokens,
		        SUM(cost) AS cost,
		        SUM(messages) AS messages
		 FROM daily_rows ${f.where}
		 GROUP BY client ORDER BY tokens DESC`,
	)
		.bind(...f.binds)
		.all();
	return json({ summary: totals, credits: credits?.credits ?? 0, clients: byClient.results });
}

async function daily(url: URL, env: Env): Promise<Response> {
	const f = filters(url);
	if (f instanceof Response) return f;
	const days = await env.DB.prepare(
		`SELECT date,
		        SUM(input) AS input,
		        SUM(output) AS output,
		        SUM(cache_read) AS cacheRead,
		        SUM(cache_write) AS cacheWrite,
		        SUM(reasoning) AS reasoning,
		        SUM(cost) AS cost,
		        SUM(messages) AS messages
		 FROM daily_rows ${f.where}
		 GROUP BY date ORDER BY date`,
	)
		.bind(...f.binds)
		.all();
	const credits = await env.DB.prepare(
		`SELECT date, SUM(credits) AS credits FROM daily_credits ${f.where} GROUP BY date`,
	)
		.bind(...f.binds)
		.all();
	const creditsByDate = new Map(
		(credits.results as unknown as { date: string; credits: number }[]).map((r) => [
			r.date,
			r.credits,
		]),
	);
	const contributions = (days.results as unknown as Record<string, unknown>[]).map((r) => ({
		...r,
		credits: creditsByDate.get(r.date as string) ?? 0,
	}));
	return json({ contributions });
}

async function hourly(url: URL, env: Env): Promise<Response> {
	const date = url.searchParams.get("date") ?? "";
	if (!isValidDate(date)) return error(400, "bad_request", "date must be YYYY-MM-DD");
	const group = url.searchParams.get("group") ?? "devices";
	if (group !== "devices" && group !== "clients" && group !== "models") {
		return error(400, "bad_request", "group must be 'devices', 'clients' or 'models'");
	}
	const key = { devices: "device_id", clients: "client", models: "model_id" }[group];
	const label = group === "devices" ? "COALESCE(devices.name, devices.hostname, device_id)" : key;
	const join = group === "devices" ? "LEFT JOIN devices ON devices.id = hourly_rows.device_id" : "";
	const clauses = ["date = ?"];
	const binds = [date];
	for (const [parameter, column] of [["deviceId", "device_id"], ["modelId", "model_id"], ["client", "client"]]) {
		const value = url.searchParams.get(parameter);
		if (value) { clauses.push(`${column} = ?`); binds.push(value); }
	}
	const where = `WHERE ${clauses.join(" AND ")}`;
	const [rows, dailyTotal] = await Promise.all([
		env.DB.prepare(`SELECT ${key} AS key, ${label} AS label, hour, SUM(tokens) AS tokens
			FROM hourly_rows ${join} ${where} GROUP BY key, hour ORDER BY hour`)
			.bind(...binds).all<{ key: string; label: string; hour: number; tokens: number }>(),
		env.DB.prepare(`SELECT COALESCE(SUM(input + output + cache_read + cache_write + reasoning), 0) AS tokens FROM daily_rows ${where}`).bind(...binds).first<{ tokens: number }>(),
	]);
	const hours = Array.from({ length: 24 }, (_, hour) => ({ hour, tokens: 0 }));
	const entities = new Map<string, { key: string; label: string; tokens: number; hours: typeof hours }>();
	for (const row of rows.results) {
		let entity = entities.get(row.key);
		if (!entity) {
			entity = { key: row.key, label: row.label, tokens: 0,
				hours: Array.from({ length: 24 }, (_, hour) => ({ hour, tokens: 0 })) };
			entities.set(row.key, entity);
		}
		entity.hours[row.hour].tokens += row.tokens;
		entity.tokens += row.tokens;
		hours[row.hour].tokens += row.tokens;
	}
	const hourlyTokens = hours.reduce((sum, row) => sum + row.tokens, 0);
	const totalTokens = dailyTotal?.tokens ?? 0;
	return json({ date, totalTokens, hourlyTokens, hasHourlyData: rows.results.length > 0,
		complete: hourlyTokens === totalTokens, timezone: "collector-local",
		hours, group, entities: [...entities.values()].sort((a, b) => b.tokens - a.tokens || a.key.localeCompare(b.key)) });
}

async function models(url: URL, env: Env): Promise<Response> {
	const f = filters(url);
	if (f instanceof Response) return f;
	const rows = await env.DB.prepare(
		`SELECT model_id AS modelId,
		        client,
		        SUM(input) AS input,
		        SUM(output) AS output,
		        SUM(cache_read) AS cacheRead,
		        SUM(cache_write) AS cacheWrite,
		        SUM(reasoning) AS reasoning,
		        SUM(input + output + cache_read + cache_write + reasoning) AS tokens,
		        SUM(cost) AS cost,
		        SUM(messages) AS messages
		 FROM daily_rows ${f.where}
		 GROUP BY model_id, client ORDER BY tokens DESC`,
	)
		.bind(...f.binds)
		.all();
	return json({ models: rows.results });
}

async function devices(env: Env): Promise<Response> {
	const result = await env.DB.prepare(
		`SELECT d.id, d.name, d.hostname, d.os, d.arch, d.first_seen, d.last_seen,
		        COUNT(r.date) AS rows,
		        MAX(r.date) AS lastDate
		 FROM devices d LEFT JOIN daily_rows r ON r.device_id = d.id
		 GROUP BY d.id ORDER BY d.last_seen DESC`,
	).all();
	return json({ devices: result.results });
}

interface SeriesEntity {
	key: string;
	label: string;
	tokens: number;
	days: { date: string; tokens: number }[];
}

// Read each daily row once, then derive all three console views in memory.
async function dashboard(url: URL, env: Env): Promise<Response> {
	const f = filters(url);
	if (f instanceof Response) return f;
	const result = await env.DB.prepare(`SELECT r.device_id, r.client, r.model_id, r.date,
		COALESCE(d.name, d.hostname, r.device_id) AS device_label,
		(r.input + r.output + r.cache_read + r.cache_write + r.reasoning) AS tokens
		FROM daily_rows r LEFT JOIN devices d ON d.id = r.device_id
		${f.where} ORDER BY r.date`).bind(...f.binds)
		.all<{ device_id: string; device_label: string; client: string; model_id: string; date: string; tokens: number }>();
	const groups = {
		devices: new Map<string, { label: string; tokens: number; days: Map<string, number> }>(),
		clients: new Map<string, { label: string; tokens: number; days: Map<string, number> }>(),
		models: new Map<string, { label: string; tokens: number; days: Map<string, number> }>(),
	};
	for (const row of result.results) {
		for (const [group, key, label] of [
			["devices", row.device_id, row.device_label],
			["clients", row.client, row.client],
			["models", row.model_id, row.model_id],
		] as const) {
			let entity = groups[group].get(key);
			if (!entity) { entity = { label, tokens: 0, days: new Map() }; groups[group].set(key, entity); }
			entity.tokens += row.tokens;
			entity.days.set(row.date, (entity.days.get(row.date) ?? 0) + row.tokens);
		}
	}
	return json(Object.fromEntries(Object.entries(groups).map(([group, entities]) => [group, {
		group,
		entities: [...entities].map(([key, value]) => ({ key, label: value.label, tokens: value.tokens,
			days: [...value.days].map(([date, tokens]) => ({ date, tokens })),
		})).sort((a, b) => b.tokens - a.tokens || a.key.localeCompare(b.key)),
	}])));
}

// Per-day token series per device or per model — feeds the console heatmap's
// entity switcher without shipping raw rows to the browser.
async function series(url: URL, env: Env): Promise<Response> {
	const f = filters(url);
	if (f instanceof Response) return f;
	const group = url.searchParams.get("group") ?? "devices";
	if (group !== "devices" && group !== "models" && group !== "clients") {
		return error(400, "bad_request", "group must be 'devices', 'models' or 'clients'");
	}
	const select =
		group === "devices"
			? `daily_rows.device_id AS key,
			   COALESCE(devices.name, devices.hostname, daily_rows.device_id) AS label`
			: group === "clients"
				? "client AS key, client AS label"
				: "model_id AS key, model_id AS label";
	const join =
		group === "devices"
			? "LEFT JOIN devices ON devices.id = daily_rows.device_id"
			: "";
	const rows = await env.DB.prepare(
		`SELECT ${select}, date,
		        SUM(input + output + cache_read + cache_write + reasoning) AS tokens
		 FROM daily_rows ${join} ${f.where}
		 GROUP BY key, date ORDER BY date`,
	)
		.bind(...f.binds)
		.all();
	const entities = new Map<string, SeriesEntity>();
	for (const row of rows.results as unknown as Record<string, unknown>[]) {
		const key = String(row.key);
		let entity = entities.get(key);
		if (!entity) {
			entity = { key, label: String(row.label ?? key), tokens: 0, days: [] };
			entities.set(key, entity);
		}
		const tokens = num(row.tokens);
		entity.tokens += tokens;
		entity.days.push({ date: String(row.date), tokens });
	}
	return json({
		group,
		entities: [...entities.values()].sort((a, b) => b.tokens - a.tokens),
	});
}

export default {
	async fetch(request, env): Promise<Response> {
		const url = new URL(request.url);
		if (url.pathname === "/health") {
			return json({ status: "ok" });
		}
		if ((url.pathname === "/install.sh" || url.pathname === "/install.ps1") && request.method === "GET") {
			return installerScript(url, env);
		}
		if (!url.pathname.startsWith("/api/")) {
			return error(404, "not_found", "unknown route");
		}
		if (["/api/ingest", "/api/ingest/validate"].includes(url.pathname) && request.method === "POST") {
			const authFailure = await requireBearer(request, env);
			if (authFailure) return authFailure;
			return ingest(request, env, url.pathname === "/api/ingest/validate");
		}
		const auth = await authenticateRead(request, env);
		if ("failure" in auth) return auth.failure;
		if (request.method !== "GET") {
			return error(405, "method_not_allowed", "unsupported method");
		}
		switch (url.pathname) {
			case "/api/diagnostics":
				return diagnostics(env);
			case "/api/releases":
				return releases(env);
			case "/api/me":
				return json({ actor: auth.actor });
			case "/api/summary":
				return cachedRead(env, url, () => summary(url, env));
			case "/api/daily":
				return cachedRead(env, url, () => daily(url, env));
			case "/api/hourly":
				return cachedRead(env, url, () => hourly(url, env));
			case "/api/models":
				return cachedRead(env, url, () => models(url, env));
			case "/api/series":
				return cachedRead(env, url, () => series(url, env));
			case "/api/dashboard":
				return cachedRead(env, url, () => dashboard(url, env));
			case "/api/devices":
				return cachedRead(env, url, () => devices(env));
			default:
				return error(404, "not_found", "unknown route");
		}
	},
} satisfies ExportedHandler<Env>;
