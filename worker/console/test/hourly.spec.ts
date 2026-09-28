import { expect, it } from "vitest";
import { hourlyChartData, hourRange } from "../ui/hourly";

it("positions each hour at its interval midpoint and uses hour keys, even for sparse unordered data", () => {
	const points = hourlyChartData([
		{ dataKey: "first", hours: [{ hour: 9, tokens: 900 }, { hour: 8, tokens: 800 }, { hour: 23, tokens: 2300 }, { hour: 0, tokens: 10 }] },
		{ dataKey: "second", hours: [{ hour: 8, tokens: 80 }] },
	]);
	expect(points).toHaveLength(24);
	expect(points[0]).toEqual({ time: 0.5, hour: 0, first: 10, second: 0 });
	expect(points[8]).toEqual({ time: 8.5, hour: 8, first: 800, second: 80 });
	expect(points[9]).toEqual({ time: 9.5, hour: 9, first: 900, second: 0 });
	expect(points[23]).toEqual({ time: 23.5, hour: 23, first: 2300, second: 0 });
	expect(points[7].first).toBe(0);
	expect(hourRange(points[8].hour)).toBe("08:00–09:00");
	expect(hourRange(points[9].hour)).toBe("09:00–10:00");
	expect(hourRange(points[23].hour)).toBe("23:00–24:00");
	// Recharts chooses the nearest x value; both sides of the boundary
	// must select the bucket that actually covers the hovered time.
	for (const time of [0.01, 0.99, 8.01, 8.49, 8.99, 9.01, 9.99, 23.01, 23.99]) {
		const nearest = points.reduce((best, point) => Math.abs(point.time - time) < Math.abs(best.time - time) ? point : best);
		expect(nearest.hour).toBe(Math.floor(time));
	}
});
