import { build } from "esbuild";
import { fileURLToPath } from "node:url";

await build({
	absWorkingDir: fileURLToPath(new URL(".", import.meta.url)),
	entryPoints: ["ui/daily-card.tsx"],
	outdir: "public/assets",
	bundle: true,
	format: "esm",
	platform: "browser",
	target: ["es2022"],
	jsx: "automatic",
	minify: true,
	define: { "process.env.NODE_ENV": '"production"' },
	logLevel: "info",
});
