import { initInstaller } from "./assets/installer.js";
import { renderDailyCard, formatTokens, toolLabel, SERIES_COLORS } from "./assets/daily-card.js";
import { filterBreakdown } from "./breakdown.js";

const DAY_MS = 86_400_000;
const RANGE_DAYS = { week: 7, month: 30, year: 371 };

const LEVELS = ["var(--l0)", "var(--l1)", "var(--l2)", "var(--l3)", "var(--l4)"];

const ISO_W = 7.5;
const ISO_D = 3.75;
const ISO_MARGIN = 12;
const ISO_MIN_H = 1.5;
const ISO_ACTIVE_MIN_H = 4;
const ISO_MAX_H = 64;

const BAR_COLORS = SERIES_COLORS;
const GROUP_LABELS = { devices: "设备", clients: "工具", models: "模型" };
const OTHER_COLOR = "#39435a";
const BAR_TOP_N = 5;

const state = {
	range: "month",
	group: "devices",
	selected: "all",
	view: "2d",
	numberFormat: savedNumberFormat(),
	series: { devices: null, models: null, clients: null },
	breakdown: null,
	window: null,
	selectedDate: null,
};

const $ = (sel) => document.querySelector(sel);

/* ── helpers ─────────────────────────────────────────── */

function toKey(d) {
	const p = (n) => String(n).padStart(2, "0");
	return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

function parseKey(key) {
	const [y, m, d] = key.split("-").map(Number);
	return new Date(y, m - 1, d).getTime();
}

function savedNumberFormat() {
	try { return localStorage.getItem("tokscale-number-format") === "exact" ? "exact" : "compact"; }
	catch { return "compact"; }
}

function fmtTokens(n) {
	return formatTokens(n, state.numberFormat);
}

function entityLabel(entity, group = state.group) {
	return group === "clients" ? toolLabel(entity.key) : entity.label;
}

function intensity(tokens, max) {
	if (tokens <= 0 || max <= 0) return 0;
	const r = tokens / max;
	if (r >= 0.75) return 4;
	if (r >= 0.5) return 3;
	if (r >= 0.25) return 2;
	return 1;
}

async function fetchJSON(path, options) {
	const res = await fetch(path, options);
	if (!res.ok) {
		let detail = `HTTP ${res.status}`;
		try {
			const body = await res.json();
			if (body?.error?.message) detail = body.error.message;
		} catch { /* keep status */ }
		const error = new Error(detail);
		error.status = res.status;
		throw error;
	}
	return res.json();
}

/* ── window + data ───────────────────────────────────── */

function windowFor(range) {
	const today = parseKey(toKey(new Date()));
	const days = RANGE_DAYS[range] ?? RANGE_DAYS.month;
	return { start: today - (days - 1) * DAY_MS, end: today };
}

function entityWindowTokens(entity, win = state.window) {
	let total = 0;
	for (const day of entity.days) {
		const t = parseKey(day.date);
		if (t >= win.start && t <= win.end) total += day.tokens;
	}
	return total;
}

function currentEntities() {
	return state.series[state.group]?.entities ?? [];
}

function selectedEntity() {
	if (state.selected === "all") return null;
	return currentEntities().find((e) => e.key === state.selected) ?? null;
}

// Daily token totals for the heatmap: the selected entity (or all entities in
// the current group merged) restricted to the active window.
function heatmapDaysMap() {
	const merged = new Map();
	const win = state.window;
	const add = (date, tokens) => merged.set(date, (merged.get(date) ?? 0) + tokens);
	for (const entity of currentEntities()) {
		if (state.selected !== "all" && entity.key !== state.selected) continue;
		for (const day of entity.days) {
			const t = parseKey(day.date);
			if (t >= win.start && t <= win.end) add(day.date, day.tokens);
		}
	}
	return merged;
}

function globalDaysMap() {
	const merged = new Map();
	const win = state.window;
	for (const entity of state.series.devices?.entities ?? []) {
		for (const day of entity.days) {
			const t = parseKey(day.date);
			if (t >= win.start && t <= win.end) {
				merged.set(day.date, (merged.get(day.date) ?? 0) + day.tokens);
			}
		}
	}
	return merged;
}

/* ── calendar ────────────────────────────────────────── */

function buildCalendar(daysMap) {
	const { start: winStart, end: today } = state.window;
	const start = winStart - new Date(winStart).getDay() * DAY_MS;
	const end = today + (6 - new Date(today).getDay()) * DAY_MS;

	const cells = [];
	for (let t = start; t <= end; t += DAY_MS) {
		const key = toKey(new Date(t));
		cells.push({
			date: key,
			tokens: t > today ? 0 : (daysMap.get(key) ?? 0),
			future: t > today,
			week: cells.length / 7 | 0,
			day: cells.length % 7,
		});
	}
	const weekCount = cells.length / 7;
	const maxTokens = Math.max(0, ...cells.map((c) => c.tokens));
	for (const cell of cells) cell.level = intensity(cell.tokens, maxTokens);

	const months = [];
	let lastMonth = -1;
	for (let w = 0; w < weekCount; w++) {
		const month = new Date(start + w * 7 * DAY_MS).getMonth();
		if (month !== lastMonth) {
			months.push({ week: w, label: `${month + 1}月` });
			lastMonth = month;
		}
	}
	return { cells, months, weekCount, maxTokens, start, end: today };
}

/* ── 2D grid ─────────────────────────────────────────── */

function renderGrid(calendar) {
	const grid = $("#grid");
	grid.replaceChildren();
	const months = $("#months");
	months.replaceChildren();
	months.style.gridAutoColumns = "var(--cell)";
	for (const m of calendar.months) {
		const span = document.createElement("span");
		span.style.gridColumnStart = String(m.week + 1);
		span.textContent = m.label;
		months.append(span);
	}

	const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)").matches;
	for (const cell of calendar.cells) {
		const el = document.createElement("button");
		el.type = "button";
		el.className = "cell";
		el.dataset.level = String(cell.level);
		if (cell.future) {
			el.classList.add("future");
			el.disabled = true;
			el.tabIndex = -1;
		} else {
			el.classList.add("live");
			el.dataset.date = cell.date;
			el.dataset.tokens = String(cell.tokens);
			el.tabIndex = 0;
			el.classList.toggle("selected-day", cell.date === state.selectedDate);
			el.setAttribute("aria-pressed", String(cell.date === state.selectedDate));
			el.setAttribute("aria-label", `${cell.date}: ${fmtTokens(cell.tokens)} tokens`);
		}
		if (!reduceMotion) {
			el.classList.add("animate");
			el.style.animationDelay = `${Math.min(cell.week * 12 + cell.day * 2, 600)}ms`;
		}
		grid.append(el);
	}
}

/* ── isometric 3D ────────────────────────────────────── */

function shade(color, pct) {
	return `color-mix(in srgb, ${color} ${pct}%, #000)`;
}

function renderIso(calendar) {
	const svg = $("#iso");
	svg.replaceChildren();
	const { cells, weekCount, maxTokens } = calendar;
	const finalWeek = Math.max(0, weekCount - 1);
	const originX = ISO_MARGIN + 6 * ISO_W;
	const originY = ISO_MARGIN + ISO_MAX_H;

	const ns = "http://www.w3.org/2000/svg";
	for (const cell of cells) {
		if (cell.future) continue;
		const ratio = maxTokens > 0 ? cell.tokens / maxTokens : 0;
		const height = cell.tokens > 0
			? ISO_ACTIVE_MIN_H + ratio * (ISO_MAX_H - ISO_ACTIVE_MIN_H)
			: ISO_MIN_H;
		const cx = originX + (cell.week - cell.day) * ISO_W;
		const cy = originY + (cell.week + cell.day) * ISO_D;

		const topY = cy - height;
		const leftX = cx - ISO_W;
		const rightX = cx + ISO_W;
		const midY = topY + ISO_D;
		const botTopY = topY + ISO_D * 2;
		const midBotY = cy + ISO_D;
		const botY = cy + ISO_D * 2;

		const color = LEVELS[cell.level];
		const faces = [
			[`${leftX},${midY} ${cx},${botTopY} ${cx},${botY} ${leftX},${midBotY}`, shade(color, 58), null],
			[`${rightX},${midY} ${cx},${botTopY} ${cx},${botY} ${rightX},${midBotY}`, shade(color, 72), null],
			[`${cx},${topY} ${rightX},${midY} ${cx},${botTopY} ${leftX},${midY}`, color, "top"],
		];
		const g = document.createElementNS(ns, "g");
		g.classList.add("live");
		g.dataset.date = cell.date;
		g.dataset.tokens = String(cell.tokens);
		g.classList.toggle("selected-day", cell.date === state.selectedDate);
		g.setAttribute("role", "button");
		g.setAttribute("tabindex", "0");
		g.setAttribute("aria-label", `${cell.date}: ${fmtTokens(cell.tokens)} tokens`);
		g.setAttribute("aria-pressed", String(cell.date === state.selectedDate));
		for (const [points, fill, face] of faces) {
			const poly = document.createElementNS(ns, "polygon");
			poly.setAttribute("points", points);
			poly.style.fill = fill;
			if (face === "top") {
				poly.setAttribute("stroke", "rgba(255, 255, 255, 0.08)");
				poly.setAttribute("stroke-width", "0.55");
				poly.setAttribute("vector-effect", "non-scaling-stroke");
			}
			g.append(poly);
		}
		svg.append(g);
	}
	const width = originX + finalWeek * ISO_W + ISO_W + ISO_MARGIN;
	const height = originY + (finalWeek + 6) * ISO_D + ISO_D * 2 + ISO_MARGIN;
	svg.setAttribute("viewBox", `0 0 ${width} ${height}`);
	svg.setAttribute("preserveAspectRatio", "xMidYMid meet");
}

/* ── tooltip ─────────────────────────────────────────── */

const tooltip = () => $("#tooltip");
let tooltipAnchor = null;

function showTooltip(anchor) {
	const tip = tooltip();
	const entity = selectedEntity();
	const entityName = entity ? entityLabel(entity) : null;
	tip.replaceChildren();
	if (entityName) {
		const e = document.createElement("span");
		e.className = "t-entity";
		e.textContent = entityName;
		tip.append(e);
	}
	const date = document.createElement("span");
	date.className = "t-date";
	const d = new Date(parseKey(anchor.dataset.date));
	date.textContent = `${anchor.dataset.date} 周${"日一二三四五六"[d.getDay()]}`;
	const tokens = document.createElement("span");
	tokens.className = "t-tokens";
	tokens.textContent = `${fmtTokens(Number(anchor.dataset.tokens))} tokens`;
	tip.append(date, tokens);

	const rect = anchor.getBoundingClientRect();
	const below = rect.top < 72;
	tip.classList.toggle("below", below);
	tip.classList.remove("hidden");
	const tw = tip.offsetWidth;
	const cx = Math.max(tw / 2 + 8, Math.min(innerWidth - tw / 2 - 8, rect.left + rect.width / 2));
	tip.style.left = `${cx}px`;
	tip.style.top = below ? `${rect.bottom}px` : `${rect.top}px`;
	tooltipAnchor = anchor;
}

function hideTooltip() {
	tooltip().classList.add("hidden");
	tooltipAnchor = null;
}

function bindTooltip(container) {
	container.addEventListener("pointerover", (ev) => {
		const anchor = ev.target.closest?.("[data-date]");
		if (anchor && anchor !== tooltipAnchor) showTooltip(anchor);
	});
	container.addEventListener("pointerout", (ev) => {
		if (!ev.target.closest?.("[data-date]")) return;
		if (!ev.relatedTarget?.closest?.("[data-date]")) hideTooltip();
	});
	container.addEventListener("focusin", (ev) => {
		const anchor = ev.target.closest?.("[data-date]");
		if (anchor) showTooltip(anchor);
	});
	container.addEventListener("focusout", hideTooltip);
}

/* ── render sections ─────────────────────────────────── */

function renderStats() {
	const daysMap = globalDaysMap();
	const total = [...daysMap.values()].reduce((a, t) => a + t, 0);
	const active = [...daysMap.values()].filter((t) => t > 0).length;
	const elapsed = Math.round((state.window.end - state.window.start) / DAY_MS) + 1;
	const items = [
		["今日", fmtTokens(daysMap.get(toKey(new Date())) ?? 0), fmtTokens(daysMap.get(toKey(new Date())) ?? 0), true],
		["总 tokens", fmtTokens(total), fmtTokens(total), true],
		["活跃天数", String(active), `${active} / ${elapsed}`, false],
		["日均", fmtTokens(total / Math.max(1, elapsed)), fmtTokens(total / Math.max(1, elapsed)), false],
	];
	const box = $("#stats");
	box.replaceChildren();
	for (const [label, value, title, accent] of items) {
		const wrap = document.createElement("div");
		const dt = document.createElement("dt");
		dt.textContent = label;
		const dd = document.createElement("dd");
		dd.textContent = value;
		dd.title = title;
		if (accent) dd.classList.add("accent");
		wrap.append(dt, dd);
		box.append(wrap);
	}
}

function renderChips() {
	const box = $("#chips");
	box.replaceChildren();
	const entities = currentEntities();
	const make = (key, label, tokens, isModel) => {
		const b = document.createElement("button");
		b.type = "button";
		b.className = `chip${isModel ? " model" : ""}${state.selected === key ? " active" : ""}`;
		b.dataset.key = key;
		b.setAttribute("role", "option");
		b.setAttribute("aria-selected", String(state.selected === key));
		const l = document.createElement("span");
		l.className = "label";
		l.textContent = label;
		const n = document.createElement("span");
		n.className = "n";
		n.textContent = fmtTokens(tokens);
		b.append(l, n);
		box.append(b);
	};
	const totals = entities
		.map((e) => ({ ...e, windowTokens: entityWindowTokens(e) }))
		.sort((a, b) => b.windowTokens - a.windowTokens);
	const total = totals.reduce((acc, e) => acc + e.windowTokens, 0);
	make("all", "全部", total, false);
	for (const e of totals) make(e.key, entityLabel(e), e.windowTokens, state.group === "models");
}

function renderHeatmap() {
	const daysMap = heatmapDaysMap();
	const calendar = buildCalendar(daysMap);

	const hasData = calendar.cells.some((c) => c.tokens > 0);
	$("#heatmap-empty").classList.toggle("hidden", hasData);

	renderGrid(calendar);
	renderIso(calendar);

	const fmt = (ts) => toKey(new Date(ts));
	const active = calendar.cells.filter((c) => c.tokens > 0).length;
	const entity = selectedEntity();
	const scope = entity ? ` · ${entityLabel(entity)}` : "";
	$("#range-note").textContent = `${fmt(state.window.start)} – ${fmt(state.window.end)} · ${active} 天活跃${scope}${state.selectedDate ? ` · 已选 ${state.selectedDate}` : ""}`;
	$("#clear-date").classList.toggle("hidden", !state.selectedDate);

	$("#months").classList.toggle("hidden", !hasData);
	$(".heatmap-row").classList.toggle("hidden", !hasData || state.view !== "2d");
	$("#iso-scroll").classList.toggle("hidden", !hasData || state.view !== "3d");
}

function renderBars() {
	const totals = state.breakdown ? filterBreakdown(state.breakdown, {
		group: state.group, selected: state.selected, date: state.selectedDate,
		since: toKey(new Date(state.window.start)), until: toKey(new Date(state.window.end)),
	}) : null;
	const win = state.selectedDate ? { start: parseKey(state.selectedDate), end: parseKey(state.selectedDate) } : state.window;
	for (const [id, group] of [["bar-clients", "clients"], ["bar-models", "models"], ["bar-devices", "devices"]]) {
		const panel = document.getElementById(id);
		const bar = panel.querySelector(".share-bar");
		const legend = panel.querySelector(".bar-legend");
		bar.replaceChildren();
		legend.replaceChildren();
		bar.removeAttribute("aria-label");
		const unavailable = !totals && state.selected !== "all" && group !== state.group;

		const rows = (state.series[group]?.entities ?? [])
			.filter(e => totals || state.selected === "all" || (group === state.group && e.key === state.selected))
			.map((e) => ({ label: entityLabel(e, group), tokens: totals ? (totals[group].get(e.key) ?? 0) : entityWindowTokens(e, win) }))
			.filter((r) => r.tokens > 0)
			.sort((a, b) => b.tokens - a.tokens);
		const total = rows.reduce((acc, r) => acc + r.tokens, 0);

		const totalEl = panel.querySelector(".bar-total");
		totalEl.textContent = unavailable ? "—" : fmtTokens(total);
		totalEl.title = unavailable ? "" : fmtTokens(total);

		if (total === 0) {
			const li = document.createElement("li");
			li.className = "bar-empty";
			li.textContent = unavailable ? "请更新 API 以启用关联筛选" : "这个范围内没有数据";
			legend.append(li);
			continue;
		}

		const top = rows.slice(0, BAR_TOP_N);
		const rest = rows.slice(BAR_TOP_N);
		const segs = rest.length
			? [...top, { label: "其他", tokens: rest.reduce((acc, r) => acc + r.tokens, 0) }]
			: top;

		segs.forEach((seg, i) => {
			const color = seg.label === "其他" ? OTHER_COLOR : BAR_COLORS[i % BAR_COLORS.length];
			const pct = (seg.tokens / total) * 100;

			const part = document.createElement("i");
			part.style.width = `${Math.max(pct, 0.5)}%`;
			part.style.background = color;
			bar.append(part);

			const li = document.createElement("li");
			const dot = document.createElement("span");
			dot.className = "dot";
			dot.style.background = color;
			const name = document.createElement("span");
			name.className = "name";
			name.textContent = seg.label;
			name.title = seg.label;
			const value = document.createElement("span");
			value.className = "v";
			value.textContent = fmtTokens(seg.tokens);
			value.title = fmtTokens(seg.tokens);
			const share = document.createElement("span");
			share.className = "p";
			share.textContent = `${pct < 0.1 ? "<0.1" : pct.toFixed(1)}%`;
			li.append(dot, name, value, share);
			legend.append(li);
		});
		bar.setAttribute(
			"aria-label",
			segs.map((s) => `${s.label} ${((s.tokens / total) * 100).toFixed(1)}%`).join(", "),
		);
	}
}

function renderAll() {
	renderStats();
	renderChips();
	renderHeatmap();
	renderBars();
	void renderDay();
}

function renderDay() {
	const date = state.selectedDate ?? toKey(new Date());
	const params = new URLSearchParams({ date, group: state.group });
	if (state.selected !== "all") params.set({ devices: "deviceId", models: "modelId", clients: "client" }[state.group], state.selected);
	const total = currentEntities().filter(entity => state.selected === "all" || entity.key === state.selected)
		.reduce((sum, entity) => sum + (entity.days.find(day => day.date === date)?.tokens ?? 0), 0);
	renderDailyCard({ date, today: toKey(new Date()), scope: selectedEntity() ? entityLabel(selectedEntity()) : `全部${GROUP_LABELS[state.group]}`,
		query: params.toString(), initialTotal: total, group: state.group, numberFormat: state.numberFormat });
}

function selectDay(date) {
	if (!/^\d{4}-\d{2}-\d{2}$/.test(date) || date > toKey(new Date())) return;
	if (parseKey(date) < state.window.start) return;
	state.selectedDate = date;
	hideTooltip();
	renderHeatmap();
	renderBars();
	void renderDay();
}

/* ── events ──────────────────────────────────────────── */

function activateSeg(seg, btn) {
	for (const b of seg.querySelectorAll("button")) {
		const on = b === btn;
		b.classList.toggle("active", on);
		b.setAttribute("aria-selected", String(on));
	}
}

function renderNumberFormat() {
	const button = $("#number-format");
	const exact = state.numberFormat === "exact";
	button.setAttribute("aria-pressed", String(exact));
	button.setAttribute("aria-label", exact ? "以 K / M / B 显示 token 数字" : "显示完整 token 数字");
	button.title = exact ? "当前：完整 · 点击显示简写数字" : "当前：简写 · 点击显示完整数字";
}

function bindControls() {
	renderNumberFormat();
	$("#number-format").addEventListener("click", () => {
		state.numberFormat = state.numberFormat === "compact" ? "exact" : "compact";
		try { localStorage.setItem("tokscale-number-format", state.numberFormat); } catch {}
		renderNumberFormat();
		hideTooltip();
		if (state.window) renderAll();
	});
	$("#clear-date").addEventListener("click", () => {
		state.selectedDate = null;
		hideTooltip();
		renderHeatmap();
		renderBars();
		void renderDay();
	});
	for (const container of [$("#grid"), $("#iso")]) {
		container.addEventListener("click", event => {
			const cell = event.target.closest?.("[data-date]");
			if (cell) selectDay(cell.dataset.date);
		});
	}
	$("#iso").addEventListener("keydown", event => {
		if (event.key === "Enter" || event.key === " ") {
			const cell = event.target.closest?.("[data-date]");
			if (cell) { event.preventDefault(); selectDay(cell.dataset.date); }
		}
	});
	$("#theme-toggle").addEventListener("click", () => {
		const root = document.documentElement;
		const next = root.dataset.theme === "light" ? "dark" : "light";
		root.dataset.theme = next;
		try {
			localStorage.setItem("tokscale-theme", next);
		} catch {}
	});

	$("#range-seg").addEventListener("click", (ev) => {
		const btn = ev.target.closest("button[data-range]");
		if (!btn || btn.dataset.range === state.range) return;
		state.range = btn.dataset.range;
		state.window = windowFor(state.range);
		if (state.selectedDate && parseKey(state.selectedDate) < state.window.start) state.selectedDate = null;
		activateSeg($("#range-seg"), btn);
		hideTooltip();
		renderAll();
	});

	$("#group-seg").addEventListener("click", (ev) => {
		const btn = ev.target.closest("button[data-group]");
		if (!btn || btn.dataset.group === state.group) return;
		state.group = btn.dataset.group;
		state.selected = "all";
		activateSeg($("#group-seg"), btn);
		hideTooltip();
		renderChips();
		renderHeatmap();
		renderBars();
		void renderDay();
	});

	$("#chips").addEventListener("click", (ev) => {
		const chip = ev.target.closest(".chip");
		if (!chip) return;
		state.selected = chip.dataset.key;
		for (const c of $("#chips").querySelectorAll(".chip")) {
			const on = c === chip;
			c.classList.toggle("active", on);
			c.setAttribute("aria-selected", String(on));
		}
		hideTooltip();
		renderHeatmap();
		renderBars();
		void renderDay();
	});

	$("#view-seg").addEventListener("click", (ev) => {
		const btn = ev.target.closest("button[data-view]");
		if (!btn) return;
		state.view = btn.dataset.view;
		activateSeg($("#view-seg"), btn);
		hideTooltip();
		renderHeatmap();
	});

	addEventListener("scroll", hideTooltip, { passive: true });
}

/* ── boot ────────────────────────────────────────────── */

async function boot() {
	bindControls();
	initInstaller();
	bindTooltip($("#grid"));
	bindTooltip($("#iso"));
	try {
		// One request covers all views and only the dates this UI can display.
		const visible = windowFor("year");
		const query = new URLSearchParams({ since: toKey(new Date(visible.start)), until: toKey(new Date(visible.end)) });
		let data;
		try {
			data = await fetchJSON(`/api/dashboard?${query}`);
		} catch (error) {
			// Console and API builds can finish in either order. Only an older
			// API's missing route uses the fallback; do not multiply quota errors.
			if (error.status !== 404) throw error;
			const groups = ["devices", "models", "clients"];
			const values = await Promise.all(groups.map(group => fetchJSON(`/api/series?group=${group}&${query}`)));
			data = Object.fromEntries(groups.map((group, index) => [group, values[index]]));
		}
		const { devices, models, clients } = data;
		state.series.devices = devices;
		state.series.models = models;
		state.series.clients = clients;
		state.breakdown = data.breakdown ?? null;
		state.window = windowFor(state.range);

		renderAll();

		const foot = $("#page-foot");
		const span = document.createElement("span");
		span.textContent = "tokscale-serverless";
		const right = document.createElement("span");
		right.textContent = `更新于 ${new Date().toLocaleString("zh-CN", { hour12: false })}`;
		foot.append(span, right);

		$("#overlay").classList.add("gone");
	} catch (err) {
		const overlay = $("#overlay");
		overlay.classList.add("error");
		const p = $("#overlay-text");
		p.textContent = "";
		p.append(
			"数据加载失败：",
			Object.assign(document.createElement("code"), { textContent: String(err.message ?? err) }),
			document.createElement("br"),
			"如果这是首次访问，可能是鉴权尚未配置。",
		);
	}
}

boot();
