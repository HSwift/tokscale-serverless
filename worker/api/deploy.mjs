import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const cwd = fileURLToPath(new URL("..", import.meta.url));
const wrangler = fileURLToPath(new URL("../node_modules/wrangler/bin/wrangler.js", import.meta.url));
const temp = mkdtempSync(join(tmpdir(), "tokscale-deploy-"));
const output = join(temp, "deploy.jsonl");
function run(args, env = process.env) {
	const result = spawnSync(process.execPath, [wrangler, ...args, "--config", "api/wrangler.jsonc"], { cwd, env, stdio: "inherit" });
	if (result.error) throw result.error;
	if (result.status !== 0) throw new Error(`Wrangler ${args[0]} failed`);
}
try {
	run(["deploy"], { ...process.env, WRANGLER_OUTPUT_FILE_PATH: output });
	const deployed = readFileSync(output, "utf8").trim().split("\n").map(line => JSON.parse(line)).findLast(row => row.type === "deploy");
	const target = deployed?.targets?.find(url => /^https:\/\/[^/]+\.workers\.dev\/?$/.test(url));
	if (!target) throw new Error("API workers.dev URL missing from deployment output");
	const origin = new URL(target).origin;
	// Register the address during deployment so installation works on an empty database.
	run(["d1", "execute", "DB", "--remote", "--command",
		`INSERT INTO api_origin (id, origin) VALUES (1, '${origin.replaceAll("'", "''")}') ON CONFLICT(id) DO UPDATE SET origin = excluded.origin`]);
} finally {
	rmSync(temp, { recursive: true, force: true });
}
