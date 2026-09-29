export interface IngestIssue { path: string; message: string }

function object(value: unknown): value is Record<string, unknown> {
	return value !== null && typeof value === "object" && !Array.isArray(value);
}

export function isValidDate(value: unknown): value is string {
	if (typeof value !== "string" || !/^\d{4}-\d{2}-\d{2}$/.test(value)) return false;
	const [year, month, day] = value.split("-").map(Number);
	const leap = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
	return month >= 1 && month <= 12 && day >= 1 &&
		day <= [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][month - 1];
}

// Shared by ingest and the validation-only endpoint. This function never uses D1
// and returns field paths rather than logging user-supplied values.
export function validateIngest(body: unknown): IngestIssue | null {
	const issue = (path: string, message: string) => ({ path, message });
	if (!object(body)) return issue("body", "must be a JSON object");
	if (!object(body.device) || typeof body.device.id !== "string" || !body.device.id.trim()) {
		return issue("device.id", "must be a nonempty string");
	}
	if (!Array.isArray(body.contributions)) return issue("contributions", "must be an array");
	if (body.meta !== undefined && !object(body.meta)) return issue("meta", "must be an object");
	const meta = body.meta as Record<string, unknown> | undefined;
	if (meta?.timelineVersion !== undefined && meta.timelineVersion !== 1) return issue("meta.timelineVersion", "unsupported timeline version");
	if (meta?.sourceTimeZone !== undefined && (typeof meta.sourceTimeZone !== "string" || !meta.sourceTimeZone || meta.sourceTimeZone.length > 100)) return issue("meta.sourceTimeZone", "must be a timezone string");
	for (const [i, day] of body.contributions.entries()) {
		const path = `contributions[${i}]`;
		if (!object(day)) return issue(path, "must be an object");
		if (!isValidDate(day.date)) return issue(`${path}.date`, "must be a valid YYYY-MM-DD date");
		if (day.totals != null && !object(day.totals)) return issue(`${path}.totals`, "must be an object");
		if (day.clients != null && !Array.isArray(day.clients)) return issue(`${path}.clients`, "must be an array");
		for (const [j, row] of (day.clients ?? []).entries()) {
			if (!object(row)) return issue(`${path}.clients[${j}]`, "must be an object");
			const field = `${path}.clients[${j}].timeline`;
			if (row.timeline !== undefined || meta?.timelineVersion === 1) {
				if (!Array.isArray(row.timeline) || row.timeline.length > 4320) return issue(field, "must be an array of UTC minute/token pairs (at most 4320)");
				if (typeof row.client !== "string" || !row.client || typeof row.modelId !== "string" || !row.modelId) return issue(field, "client and modelId are required");
				const origin = Date.parse(`${day.date}T00:00:00Z`) / 60_000;
				let previous = -1, total = 0;
				for (const [k, pair] of row.timeline.entries()) {
					if (!Array.isArray(pair) || pair.length !== 2 || !Number.isSafeInteger(pair[0]) || pair[0] <= previous || Math.abs(pair[0] - origin) > 2880 || !Number.isSafeInteger(pair[1]) || pair[1] < 0) return issue(`${field}[${k}]`, "must contain sorted unique UTC minutes near the source date and non-negative integer tokens");
					previous = pair[0]; total += pair[1];
				}
				if (!object(row.tokens)) return issue(field, "token breakdown is required");
				const count = ["input", "output", "cacheRead", "cacheWrite", "reasoning"].reduce((sum, name) => sum + Number((row.tokens as Record<string, unknown>)[name] ?? 0), 0);
				if (!Number.isSafeInteger(total) || !Number.isSafeInteger(count) || total > count) return issue(field, "timeline tokens must not exceed the row total");
			}
		}
	}
	if (body.hourly !== undefined) {
		if (!Array.isArray(body.hourly)) return issue("hourly", "must be an array");
		for (const [i, row] of body.hourly.entries()) {
			const path = `hourly[${i}]`;
			if (!object(row)) return issue(path, "must be an object");
			if (!isValidDate(row.date)) return issue(`${path}.date`, "must be a valid YYYY-MM-DD date");
			if (typeof row.hour !== "number" || !Number.isInteger(row.hour) || row.hour < 0 || row.hour > 23) {
				return issue(`${path}.hour`, "must be an integer from 0 to 23");
			}
			for (const field of ["client", "modelId"]) {
				if (typeof row[field] !== "string" || !row[field]) return issue(`${path}.${field}`, "must be a nonempty string");
			}
			if (typeof row.tokens !== "number" || !Number.isSafeInteger(row.tokens) || row.tokens < 0) {
				return issue(`${path}.tokens`, "must be an integer from 0 to 9007199254740991");
			}
		}
	}
	return null;
}
