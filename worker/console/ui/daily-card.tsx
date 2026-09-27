import { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { Area, AreaChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis } from "recharts";

interface HourlyData {
	date: string;
	totalTokens: number;
	hourlyTokens: number;
	hasHourlyData: boolean;
	complete: boolean;
	hours: { hour: number; tokens: number }[];
}

interface Props {
	date: string;
	today: string;
	scope: string;
	query: string;
	initialTotal: number;
	refreshKey: number;
	onSelectDate: (date: string) => void;
}

const exact = (value: number) => Math.round(value).toLocaleString("en-US");
const compact = (value: number) => new Intl.NumberFormat("en-US", { notation: "compact", maximumFractionDigits: 1 }).format(value);
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
	}, [props.query, props.refreshKey]);
	const current = result?.query === props.query ? result : undefined;
	const data = current?.data;
	const missing = data && !data.hasHourlyData && data.totalTokens > 0;
	const note = current?.error ?? (missing
		? "升级采集器并完成同步后，可从保留的本地会话补齐小时记录。"
		: data && !data.complete ? `已记录 ${exact(data.hourlyTokens)} tokens 的小时明细；部分用量尚未补齐。`
			: "按采集主机本地时间汇总 · 点击方块查看其他日期");
	return <div aria-busy={loading}>
		<div className="daily-head">
			<div>
				<h2 id="daily-label">{props.date === props.today ? "今日 token 用量" : "当日 token 用量"}</h2>
				<div className="daily-number" id="daily-total">{exact(data?.totalTokens ?? props.initialTotal)}<span> tokens</span></div>
				<p className="daily-scope">{props.date} · {props.scope}</p>
			</div>
			<div className="daily-controls">
				<label className="sr-only" htmlFor="selected-date">查看日期</label>
				<input id="selected-date" type="date" value={props.date} max={props.today} onChange={event => props.onSelectDate(event.target.value)} />
				<button className="text-btn" id="back-today" type="button" disabled={props.date === props.today} onClick={() => props.onSelectDate(props.today)}>今天</button>
			</div>
		</div>
		<div className="hourly-chart" id="hourly-chart" aria-label={`${props.date} 每小时 token 用量`}>
			{!data || missing ? <p className="chart-placeholder">{loading ? "加载中…" : current?.error ? "暂时无法加载小时明细" : "这一天还没有小时明细"}</p> :
				<ResponsiveContainer width="100%" height="100%" minWidth={0} initialDimension={{ width: 800, height: 230 }}>
					<AreaChart data={data.hours} margin={{ top: 12, right: 14, bottom: 0, left: 0 }} accessibilityLayer>
						<defs><linearGradient id="hourly-gradient" x1="0" y1="0" x2="0" y2="1">
							<stop offset="0%" stopColor="var(--accent)" stopOpacity={.28} />
							<stop offset="100%" stopColor="var(--accent)" stopOpacity={.015} />
						</linearGradient></defs>
						<CartesianGrid vertical={false} stroke="var(--border)" strokeDasharray="3 5" />
						<XAxis dataKey="hour" type="number" domain={[0, 23]} ticks={[0, 4, 8, 12, 16, 20, 23]} tickFormatter={hourLabel} minTickGap={20} axisLine={false} tickLine={false} tick={{ fill: "var(--muted)", fontSize: 11 }} />
						<YAxis width={48} tickFormatter={compact} allowDecimals={false} axisLine={false} tickLine={false} tick={{ fill: "var(--muted)", fontSize: 11 }} />
						<Tooltip labelFormatter={value => `${hourLabel(Number(value))}–${hourLabel(Number(value) + 1)}`} formatter={value => [exact(Number(value)), "tokens"]} contentStyle={{ background: "var(--tooltip-bg)", border: "1px solid var(--border-strong)", borderRadius: 8, color: "var(--tooltip-text)", fontSize: 12 }} itemStyle={{ color: "var(--tooltip-entity)" }} cursor={{ stroke: "var(--muted)", strokeDasharray: "3 3" }} />
						<Area type="linear" dataKey="tokens" stroke="var(--accent)" strokeWidth={2.5} fill="url(#hourly-gradient)" isAnimationActive={false} activeDot={{ r: 4, stroke: "var(--surface)", strokeWidth: 2 }} />
					</AreaChart>
				</ResponsiveContainer>}
		</div>
		<p className="chart-note" id="hourly-note" role="status">{note}</p>
	</div>;
}

let root: ReturnType<typeof createRoot> | undefined;
export function renderDailyCard(props: Props) {
	root ??= createRoot(document.getElementById("daily-panel")!);
	root.render(<DailyCard {...props} />);
}
