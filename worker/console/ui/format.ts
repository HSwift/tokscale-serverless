export type NumberFormat = "compact" | "exact";

const compact = new Intl.NumberFormat("en-US", { notation: "compact", maximumFractionDigits: 1 });
const exact = new Intl.NumberFormat("en-US", { maximumFractionDigits: 0 });

export function formatTokens(value: number, mode: NumberFormat): string {
	return (mode === "exact" ? exact : compact).format(value);
}

const TOOL_NAMES: Record<string, string> = {
	claude: "Claude Code",
	codex: "Codex",
	qoder: "Qoder CLI",
	qodercli: "Qoder CLI",
};

export function toolLabel(key: string): string {
	return TOOL_NAMES[key] ?? key;
}

export const SERIES_COLORS = ["#2f8fff", "#57a5ff", "#79b8ff", "#a8d0ff", "#3d5a80", "#5c6779"];
