import { cloudflareTest } from "@cloudflare/vitest-plugin";
import { defineConfig } from "vitest/config";

export default defineConfig({
	plugins: [
		cloudflareTest({
			wrangler: { configPath: "./api/wrangler.jsonc" },
			miniflare: {
				bindings: { INGEST_TOKEN: "test-token" },
			},
		}),
	],
});
