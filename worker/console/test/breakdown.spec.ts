import { describe, expect, it } from "vitest";
import { filterBreakdown } from "../public/breakdown.js";

const rows = [
	{ date: "2026-09-28", deviceId: "a", client: "codex", modelId: "shared", tokens: 100 },
	{ date: "2026-09-28", deviceId: "b", client: "qoder", modelId: "shared", tokens: 200 },
	{ date: "2026-09-29", deviceId: "a", client: "qoder", modelId: "other", tokens: 400 },
	{ date: "2026-09-29", deviceId: "b", client: "codex", modelId: "shared", tokens: 800 },
	{ date: "2026-08-01", deviceId: "a", client: "codex", modelId: "shared", tokens: 1600 },
];
const scope = { group: "devices", selected: "all", since: "2026-09-01", until: "2026-09-29", date: null };
const sum = (values: Map<string, number>) => [...values.values()].reduce((a, b) => a + b, 0);

describe("dashboard breakdown filters", () => {
	it.each([
		["devices", "a", { a: 500 }, { codex: 100, qoder: 400 }, { shared: 100, other: 400 }],
		["clients", "qoder", { a: 400, b: 200 }, { qoder: 600 }, { shared: 200, other: 400 }],
		["models", "shared", { a: 100, b: 1000 }, { codex: 900, qoder: 200 }, { shared: 1100 }],
	])("cross-filters all three cards by %s", (group, selected, devices, clients, models) => {
		const result = filterBreakdown(rows, { ...scope, group, selected });
		expect(Object.fromEntries(result.devices)).toEqual(devices);
		expect(Object.fromEntries(result.clients)).toEqual(clients);
		expect(Object.fromEntries(result.models)).toEqual(models);
	});

	it("intersects date and entity, then restores the selected range when date is cleared", () => {
		const selection = { ...scope, group: "clients", selected: "qoder" };
		const day = filterBreakdown(rows, { ...selection, date: "2026-09-28" });
		expect(Object.fromEntries(day.devices)).toEqual({ b: 200 });
		for (const totals of Object.values(day)) expect(sum(totals)).toBe(200);
		for (const totals of Object.values(filterBreakdown(rows, selection))) expect(sum(totals)).toBe(600);
		for (const totals of Object.values(filterBreakdown(rows, scope))) expect(sum(totals)).toBe(1500);
	});

	it("keeps empty selections empty and honors both range boundaries", () => {
		for (const date of ["2026-09-27", "2026-08-01", "2026-09-30"]) {
			for (const totals of Object.values(filterBreakdown(rows, { ...scope, date }))) expect(totals.size).toBe(0);
		}
		for (const totals of Object.values(filterBreakdown(rows, { ...scope, since: "2026-09-28", until: "2026-09-28" }))) {
			expect(sum(totals)).toBe(300);
		}
	});
});
