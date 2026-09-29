import type { Env } from "./auth";
import { isValidDate } from "./validate";

type Point = [number, number]; // Unix minute, total tokens
interface Row {
    device_id: string; device_label: string; client: string; model_id: string;
    date: string; tokens: number; timeline: string | null;
}
interface DayRow extends Row { legacyTokens: number }
const json = (body: unknown, status = 200) => Response.json(body, { status });
const bad = (message: string) => json({ error: { code: "bad_request", message } }, 400);

function shift(date: string, days: number): string {
    return new Date(Date.parse(`${date}T00:00:00Z`) + days * 86_400_000).toISOString().slice(0, 10);
}

class Calendar {
    private formatter: Intl.DateTimeFormat;
    private partsCache = new Map<number, { date: string; hour: number; minute: number }>();
    private boundsCache = new Map<string, [number, number]>();
    private dayRanges = new Map<number, { date: string; start: number; end: number }[]>();
    constructor(readonly timeZone: string) {
        this.formatter = new Intl.DateTimeFormat("en-CA", {
            timeZone, year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hourCycle: "h23",
        });
    }
    parts(minute: number) {
        let result = this.partsCache.get(minute);
        if (!result) {
            const parts = Object.fromEntries(this.formatter.formatToParts(minute * 60_000).map(p => [p.type, p.value]));
            result = { date: `${parts.year}-${parts.month}-${parts.day}`, hour: Number(parts.hour), minute: Number(parts.minute) };
            this.partsCache.set(minute, result);
        }
        return result;
    }
    bounds(date: string): [number, number] {
        let result = this.boundsCache.get(date);
        if (!result) {
            // Find calendar midnights rather than assuming all days are 24h.
            const midnight = (key: string) => {
                const nominal = Date.parse(`${key}T00:00:00Z`) / 60_000;
                const observed = this.parts(nominal);
                const offset = Date.parse(`${observed.date}T00:00:00Z`) / 60_000 + observed.hour * 60 + observed.minute - nominal;
                const candidate = nominal - offset;
                const actual = this.parts(candidate);
                if (actual.date === key && actual.hour === 0 && actual.minute === 0 && this.parts(candidate - 1).date < key) return candidate;
                // Midnight itself may be skipped/repeated during an offset change.
                let lo = nominal - 2160, hi = nominal + 2160;
                while (lo < hi) {
                    const mid = Math.floor((lo + hi) / 2);
                    if (this.parts(mid).date < key) lo = mid + 1; else hi = mid;
                }
                return lo;
            };
            result = [midnight(date), midnight(shift(date, 1))];
            this.boundsCache.set(date, result);
        }
        return result;
    }
    days(points: Point[]) {
        const days = new Map<string, number>();
        let current = "", start = 0, end = 0;
        for (const [minute, tokens] of points) {
            if (!current || minute < start || minute >= end) {
                const key = Math.floor(minute / 1440);
                const ranges = this.dayRanges.get(key) ?? [];
                let range = ranges.find(range => minute >= range.start && minute < range.end);
                if (!range) {
                    current = this.parts(minute).date;
                    [start, end] = this.bounds(current);
                    range = { date: current, start, end };
                    ranges.push(range);
                    this.dayRanges.set(key, ranges);
                }
                ({ date: current, start, end } = range);
            }
            days.set(current, (days.get(current) ?? 0) + tokens);
        }
        return days;
    }
}

function calendar(url: URL): Calendar | Response {
    const timeZone = url.searchParams.get("timeZone") ?? "";
    try {
        if (!timeZone || timeZone.length > 100) return bad("timeZone must be an IANA timezone");
        return new Calendar(timeZone);
    } catch { return bad("timeZone must be an IANA timezone"); }
}

async function readRows(url: URL, env: Env, since?: string, until?: string): Promise<Row[]> {
    const clauses: string[] = [], binds: string[] = [];
    // Existing keys are collector-local dates. A bounded two-day margin covers
    // the largest source/display timezone difference without a table scan.
    if (since) { clauses.push("r.date >= ?"); binds.push(shift(since, -2)); }
    if (until) { clauses.push("r.date <= ?"); binds.push(shift(until, 2)); }
    for (const [parameter, column] of [["deviceId", "device_id"], ["client", "client"], ["modelId", "model_id"]]) {
        const value = url.searchParams.get(parameter);
        if (value) { clauses.push(`r.${column} = ?`); binds.push(value); }
    }
    const rows = await env.DB.prepare(`SELECT r.device_id, r.client, r.model_id, r.date, r.timeline,
        COALESCE(d.name, d.hostname, r.device_id) AS device_label,
        (r.input + r.output + r.cache_read + r.cache_write + r.reasoning) AS tokens
        FROM daily_rows r LEFT JOIN devices d ON d.id = r.device_id
        ${clauses.length ? `WHERE ${clauses.join(" AND ")}` : ""} ORDER BY r.date`)
        .bind(...binds).all<Row>();
    return rows.results;
}

function points(row: Row): Point[] {
    return row.timeline === null ? [] : JSON.parse(row.timeline) as Point[];
}

function dailyRows(rows: Row[], clock: Calendar, since?: string, until?: string): DayRow[] {
    const output: DayRow[] = [];
    for (const row of rows) {
        const days = clock.days(points(row));
        const located = [...days.values()].reduce((sum, n) => sum + n, 0);
        const legacy = Math.max(0, row.tokens - located);
        // Retain totals whose timestamps cannot be recovered, but never invent
        // hours for them. The response exposes this uncertainty to the console.
        if (legacy) days.set(row.date, (days.get(row.date) ?? 0) + legacy);
        for (const [date, tokens] of days) {
            if ((since && date < since) || (until && date > until)) continue;
            output.push({ ...row, date, tokens, legacyTokens: date === row.date ? legacy : 0 });
        }
    }
    return output;
}

export async function zonedDashboard(url: URL, env: Env): Promise<Response> {
    const clock = calendar(url);
    if (clock instanceof Response) return clock;
    const since = url.searchParams.get("since") ?? undefined;
    const until = url.searchParams.get("until") ?? undefined;
    if ((since && !isValidDate(since)) || (until && !isValidDate(until))) return bad("since/until must be YYYY-MM-DD");
    if (since && until && since > until) return bad("since must not be after until");
    const rows = dailyRows(await readRows(url, env, since, until), clock, since, until);
    const groups = { devices: new Map<string, Entity>(), clients: new Map<string, Entity>(), models: new Map<string, Entity>() };
    for (const row of rows) {
        for (const [group, key, label] of [["devices", row.device_id, row.device_label], ["clients", row.client, row.client], ["models", row.model_id, row.model_id]] as const) {
            const entity = groups[group].get(key) ?? { key, label, tokens: 0, days: new Map<string, number>() };
            entity.tokens += row.tokens;
            entity.days.set(row.date, (entity.days.get(row.date) ?? 0) + row.tokens);
            groups[group].set(key, entity);
        }
    }
    return json({ timezone: clock.timeZone, legacyTokens: rows.reduce((sum, row) => sum + row.legacyTokens, 0),
        ...Object.fromEntries(Object.entries(groups).map(([group, map]) => [group, { group,
            entities: [...map.values()].map(entity => ({ ...entity, days: [...entity.days].sort(([a], [b]) => a.localeCompare(b)).map(([date, tokens]) => ({ date, tokens })) }))
                .sort((a, b) => b.tokens - a.tokens || a.key.localeCompare(b.key)),
        }])),
        breakdown: rows.map(row => ({ deviceId: row.device_id, client: row.client, modelId: row.model_id, date: row.date, tokens: row.tokens })),
    });
}

interface Entity { key: string; label: string; tokens: number; days: Map<string, number> }

export async function zonedHourly(url: URL, env: Env): Promise<Response> {
    const clock = calendar(url);
    if (clock instanceof Response) return clock;
    const date = url.searchParams.get("date") ?? "";
    if (!isValidDate(date)) return bad("date must be YYYY-MM-DD");
    const group = url.searchParams.get("group") ?? "devices";
    if (!["devices", "clients", "models"].includes(group)) return bad("group must be 'devices', 'clients' or 'models'");
    const blankHours = () => Array.from({ length: 24 }, (_, hour) => ({ hour, tokens: 0 }));
    const hours = blankHours();
    const entities = new Map<string, { key: string; label: string; tokens: number; hours: typeof hours }>();
    const rows = await readRows(url, env, date, date);
    const [start, end] = clock.bounds(date);
    for (const row of rows) {
        const key = group === "devices" ? row.device_id : group === "clients" ? row.client : row.model_id;
        const label = group === "devices" ? row.device_label : key;
        for (const [minute, tokens] of points(row)) {
            if (minute < start || minute >= end) continue;
            const entity = entities.get(key) ?? { key, label, tokens: 0, hours: blankHours() };
            const hour = clock.parts(minute).hour;
            entity.tokens += tokens;
            entity.hours[hour].tokens += tokens;
            hours[hour].tokens += tokens;
            entities.set(key, entity);
        }
    }
    const days = dailyRows(rows, clock, date, date);
    const totalTokens = days.reduce((sum, row) => sum + row.tokens, 0);
    const legacyTokens = days.reduce((sum, row) => sum + row.legacyTokens, 0);
    const hourlyTokens = hours.reduce((sum, row) => sum + row.tokens, 0);
    return json({ date, timezone: clock.timeZone, totalTokens, hourlyTokens, legacyTokens,
        hasHourlyData: entities.size > 0, complete: hourlyTokens === totalTokens,
        hours, group, entities: [...entities.values()].sort((a, b) => b.tokens - a.tokens || a.key.localeCompare(b.key)),
    });
}
