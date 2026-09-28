import { build } from "esbuild";
import { fileURLToPath } from "node:url";

let apiUrl = process.env.PUBLIC_API_URL?.trim() ?? "";
if (apiUrl) {
	let parsed;
	try { parsed = new URL(apiUrl); } catch { /* Report the expected format below. */ }
	if (!parsed || !["http:", "https:"].includes(parsed.protocol) || parsed.username || parsed.password ||
		parsed.pathname !== "/" || parsed.search || parsed.hash) {
		throw new Error("PUBLIC_API_URL must be an http(s) origin, e.g. https://api.example.com (no path, query, or credentials).");
	}
	apiUrl = parsed.origin;
}

await build({
	absWorkingDir: fileURLToPath(new URL(".", import.meta.url)),
	entryPoints: ["ui/daily-card.tsx", "ui/installer.js"],
	outdir: "public/assets",
	bundle: true,
	format: "esm",
	platform: "browser",
	target: ["es2022"],
	jsx: "automatic",
	minify: true,
	define: {
		"process.env.NODE_ENV": '"production"',
		"process.env.PUBLIC_API_URL": JSON.stringify(apiUrl),
	},
	logLevel: "info",
});
