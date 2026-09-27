import type { Env } from "./auth";

export function publicOrigin(value: string): string | null {
	try {
		const url = new URL(value);
		if (url.protocol !== "https:" || url.username || url.password ||
			url.hostname === "localhost" || url.hostname.endsWith(".localhost") ||
			url.hostname === "127.0.0.1" || url.hostname === "[::1]" || url.hostname.endsWith(".internal")) return null;
		return url.origin;
	} catch { return null; }
}

export async function readApiOrigin(env: Env): Promise<string | null> {
	const row = await env.DB.prepare("SELECT origin FROM api_origin WHERE id = 1").first<{ origin: string }>();
	return row ? publicOrigin(row.origin) : null;
}

// Only call after a successful bearer-authenticated /api/ingest. Service-bound
// console queries retain the console URL and must never set this value.
export function rememberApiOrigin(request: Request, env: Env): D1PreparedStatement | null {
	const origin = publicOrigin(request.url);
	if (!origin) return null;
	return env.DB.prepare(`INSERT INTO api_origin (id, origin) VALUES (1, ?)
		ON CONFLICT(id) DO UPDATE SET origin = excluded.origin
		WHERE api_origin.origin != excluded.origin`).bind(origin);
}
