import type { Env } from "./auth";

const TTL_MS = 5 * 60 * 1000;
const MAX_ENTRIES = 32;
interface CachedResponse { body: string; status: number; headers: [string, string][] }
interface Entry { expires: number; response: Promise<CachedResponse> }

// Best-effort cache within a Worker instance. No extra storage binding or D1
// query is needed. Other instances may retain a result for up to five minutes.
const caches = new WeakMap<D1Database, Map<string, Entry>>();

export function invalidateReads(env: Env): void {
	caches.delete(env.DB);
}

// Call only AFTER authentication. Never cache credentials, /api/me or errors.
export async function cachedRead(env: Env, url: URL, load: () => Promise<Response>): Promise<Response> {
	let cache = caches.get(env.DB);
	if (!cache) { cache = new Map(); caches.set(env.DB, cache); }
	const now = Date.now();
	for (const [key, entry] of cache) if (entry.expires <= now) cache.delete(key);
	const query = new URLSearchParams(url.searchParams);
	query.sort();
	const key = `${url.pathname}?${query}`;
	let entry = cache.get(key);
	if (!entry) {
		while (cache.size >= MAX_ENTRIES) cache.delete(cache.keys().next().value!);
		const target = cache;
		// Workers I/O objects belong to one request. Only share plain data;
		// consume the stream in the request that performed the database query.
		const next: Entry = { expires: now + TTL_MS, response: Promise.resolve().then(load).then(async response => {
			const data = { body: await response.text(), status: response.status, headers: [...response.headers] };
			if (!response.ok && target.get(key) === next) target.delete(key);
			return data;
		}).catch(error => {
			if (target.get(key) === next) target.delete(key);
			throw error;
		}) };
		cache.set(key, next);
		entry = next;
	}
	const data = await entry.response;
	const response = new Response(data.body, { status: data.status, headers: data.headers });
	response.headers.set("cache-control", "private, no-store");
	return response;
}
