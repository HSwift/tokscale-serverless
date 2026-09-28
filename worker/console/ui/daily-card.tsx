import { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { Area, AreaChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis } from "recharts";
import { formatTokens, toolLabel, SERIES_COLORS, type NumberFormat } from "./format";
export { formatTokens, toolLabel, SERIES_COLORS } from "./format";

interface HourlyEntity {
	key: string;
	label: string;
	tokens: number;
	hours: { hour: number; tokens: number }[];
}

interface HourlyData {
	date: string;
	totalTokens: number;
	hourlyTokens: number;
	hasHourlyData: boolean;
	complete: boolean;
	hours: { hour: number; tokens: number }[];
	entities?: HourlyEntity[];
}

interface Props {
	date: string;
	today: string;
	scope: string;
	query: string;
	initialTotal: number;
	group: "devices" | "clients" | "models";
	numberFormat: NumberFormat;
}

const hourLabel = (value: number) => `${String(value).padStart(2, "0")}:00`;

function DailyCard(props: Props) {
	const [result, setResult] = useState<{ query: string; data?: HourlyData; error?: string }>();
	const [loading, setLoading] = useState(true);
	useEffect(() => {
		const controller = new AbortController();
		setLoading(true);
		fetch(`/api/hourly?${props.query}`, { signal: controller.signal }).then(async response => {
			if (!response.ok) throw new Error(`小时明细加载失败（${response.status}）`);
			return response.json() as Promise<HourlyData>;
		}).then(data => {
			if (!controller.signal.aborted) setResult({ query: props.query, data });
		}).catch(error => {
			if (!controller.signal.aborted) setResult({ query: props.query, error: String(error.message) });
		}).finally(() => {
			if (!controller.signal.aborted) setLoading(false);
		});
		return () => controller.abort();
	}, [props.query]);
	const current = result?.query === props.query ? result : undefined;
	const data = current?.data;
	const fmt = (value: number) => formatTokens(value, props.numberFormat);
	// Keep the total-only response usable while the API deployment catches up.
	const entities = data?.entities ?? (data ? [{ key: "total", label: props.scope, tokens: data.hourlyTokens, hours: data.hours }] : []);
	const series = entities.map((entity, index) => ({
		...entity, dataKey: `series_${index}`, color: SERIES_COLORS[index % SERIES_COLORS.length],
		label: props.group === "clients" && entity.key !== "total" ? toolLabel(entity.key) : entity.label,
	}));
	const chartData = Array.from({ length: 24 }, (_, hour) => {
		const point: Record<string, number> = { hour };
		for (const entity of series) point[entity.dataKey] = entity.hours[hour]?.tokens ?? 0;
		return point;
	});
	const maxTokens = series.reduce((max, entity) => Math.max(max, ...entity.hours.map(row => row.tokens)), 0);
	const axisWidth = Math.max(48, Math.min(140, fmt(maxTokens * 1.2).length * 7 + 14));
	const missing = data && !data.hasHourlyData && data.totalTokens > 0;
	const note = current?.error ?? (missing
		? "升级采集器并完成同步后，可从保留的本地会话补齐小时记录。"
		: data && !data.complete ? `已记录 ${fmt(data.hourlyTokens)} tokens 的小时明细；部分用量尚未补齐。`
			: "按采集主机本地时间汇总 · 点击方块查看其他日期");
	return <div aria-busy={loading}>
		<div className="daily-head">
			<div>
				<h2 id="daily-label">{props.date === props.today ? "今日 token 用量" : "当日 token 用量"}</h2>
				<div className="daily-number" id="daily-total">{fmt(data?.totalTokens ?? props.initialTotal)}<span> tokens</span></div>
				<p className="daily-scope">{props.date} · {props.scope}</p>
			</div>
		</div>
		<div className="hourly-chart" id="hourly-chart" aria-label={`${props.date} · ${props.scope} · 每小时 token 用量`}>
			{!data || missing ? <p className="chart-placeholder">{loading ? "加载中…" : current?.error ? "暂时无法加载小时明细" : "这一天还没有小时明细"}</p> :
				<ResponsiveContainer width="100%" height="100%" minWidth={0} initialDimension={{ width: 800, height: 230 }}>
					<AreaChart data={chartData} margin={{ top: 12, right: 14, bottom: 0, left: 0 }} accessibilityLayer>
						<defs><linearGradient id="hourly-gradient" x1="0" y1="0" x2="0" y2="1">
							<stop offset="0%" stopColor="var(--accent)" stopOpacity={.28} />
							<stop offset="100%" stopColor="var(--accent)" stopOpacity={.015} />
						</linearGradient></defs>
						<CartesianGrid vertical={false} stroke="var(--border)" strokeDasharray="3 5" />
						<XAxis dataKey="hour" type="number" domain={[0, 23]} ticks={[0, 4, 8, 12, 16, 20, 23]} tickFormatter={hourLabel} minTickGap={20} axisLine={false} tickLine={false} tick={{ fill: "var(--muted)", fontSize: 11 }} />
						<YAxis width={axisWidth} tickFormatter={fmt} allowDecimals={false} axisLine={false} tickLine={false} tick={{ fill: "var(--muted)", fontSize: 11 }} />
						<Tooltip cursor={{ stroke: "var(--muted)", strokeDasharray: "3 3" }} content={({ active, payload, label }) => active && payload?.length ?
							<div className="hourly-tooltip">
								<p>{hourLabel(Number(label))}–{hourLabel(Number(label) + 1)}</p>
								<ul>{payload.map(item => <li key={String(item.dataKey)}>
									<span className="name" style={{ color: item.color }}>{item.name}</span>
									<span className="value">{fmt(Number(item.value))} tokens</span>
								</li>)}</ul>
							</div> : null} />
						{series.map((entity, index) => <Area key={entity.key} type="linear" dataKey={entity.dataKey} name={entity.label}
							stroke={entity.color} strokeWidth={2} strokeDasharray={index >= SERIES_COLORS.length ? "5 3" : undefined}
							fill={series.length === 1 ? "url(#hourly-gradient)" : "none"}
							isAnimationActive={false} activeDot={{ r: 4, stroke: "var(--surface)", strokeWidth: 2 }} />)}
					</AreaChart>
				</ResponsiveContainer>}
		</div>
		{!missing && series.length > 0 && <ul className="hourly-legend" aria-label="小时用量明细">
			{series.map(entity => <li key={entity.key} data-key={entity.key}>
				<span className="dot" style={{ background: entity.color }} aria-hidden="true" />
				<span className="name" title={entity.label}>{entity.label}</span>
				<span className="value">{fmt(entity.tokens)}</span>
			</li>)}
		</ul>}
		<p className="chart-note" id="hourly-note" role="status">{note}</p>
	</div>;
}

let root: ReturnType<typeof createRoot> | undefined;
export function renderDailyCard(props: Props) {
	root ??= createRoot(document.getElementById("daily-panel")!);
	root.render(<DailyCard {...props} />);
}
