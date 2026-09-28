const FIELDS = { devices: "deviceId", clients: "client", models: "modelId" };

// Each row is a daily device/tool/model total, not an individual conversation.
export function filterBreakdown(rows, { group, selected, since, until, date }) {
	const totals = { devices: new Map(), clients: new Map(), models: new Map() };
	for (const row of rows) {
		if (row.date < since || row.date > until || (date && row.date !== date)) continue;
		if (selected !== "all" && row[FIELDS[group]] !== selected) continue;
		for (const [dimension, field] of Object.entries(FIELDS)) {
			const key = row[field];
			totals[dimension].set(key, (totals[dimension].get(key) ?? 0) + row.tokens);
		}
	}
	return totals;
}
