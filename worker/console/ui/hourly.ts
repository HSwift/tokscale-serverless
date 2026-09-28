interface HourlySeries {
	dataKey: string;
	hours: { hour: number; tokens: number }[];
}

export function hourlyChartData(series: HourlySeries[]) {
	const points: Record<string, number>[] = Array.from({ length: 24 }, (_, hour) => ({
		hour,
		// A bucket covers [hour, hour + 1). Its midpoint makes nearest-point
		// tooltips switch at the hour boundary, rather than half an hour early.
		time: hour + 0.5,
	}));
	for (const entity of series) {
		for (const point of points) point[entity.dataKey] = 0;
		for (const row of entity.hours) {
			if (Number.isInteger(row.hour) && row.hour >= 0 && row.hour < 24) {
				points[row.hour][entity.dataKey] += row.tokens;
			}
		}
	}
	return points;
}

export const hourLabel = (value: number) => `${String(value).padStart(2, "0")}:00`;
export const hourRange = (hour: number) => `${hourLabel(hour)}–${hourLabel(hour + 1)}`;
