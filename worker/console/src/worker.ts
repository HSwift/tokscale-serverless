/**
 * tokscale-serverless-console: static stats page + API proxy.
 *
 * Assets serve the page; GET/HEAD /api/* (never /api/ingest) is forwarded to
 * the api worker over a service binding. Until Cloudflare Access fronts this
 * worker the browser carries no credential, so the console injects the shared
 * bearer token server-side for those read-only calls. Once Access is on, each
 * request arrives with Cf-Access-Jwt-Assertion, which is forwarded untouched —
 * the api worker verifies the JWT itself and no injection happens.
 * Issuing installation tickets additionally requires Access and a same-origin
 * POST; that endpoint never uses the console's fallback bearer credential.
 */

export type RequestForwarder = {
	fetch(request: Request): Response | Promise<Response>;
};

export interface ConsoleEnv {
	API: RequestForwarder;
	ASSETS: RequestForwarder;
	INGEST_TOKEN?: string;
}

function notFound(): Response {
	return new Response(
		JSON.stringify({ error: { code: "not_found", message: "unknown route" } }),
		{ status: 404, headers: { "content-type": "application/json" } },
	);
}

export function routeConsoleRequest(
	request: Request,
	env: ConsoleEnv,
): Response | Promise<Response> {
	const pathname = new URL(request.url).pathname;
	if (pathname === "/api/install-tickets") {
		// This endpoint distributes machine credentials. Never inject the
		// console's token: API authentication must validate the user's JWT.
		if (request.method !== "POST") return notFound();
		const origin = request.headers.get("origin");
		if (origin !== new URL(request.url).origin || !request.headers.has("cf-access-jwt-assertion")) {
			return new Response(JSON.stringify({ error: { message: "请先通过 Access 登录后生成安装命令。" } }), {
				status: 403, headers: { "content-type": "application/json", "cache-control": "no-store" },
			});
		}
		const forwarded = new Request(request);
		forwarded.headers.delete("authorization");
		return env.API.fetch(forwarded);
	}
	if (pathname === "/api" || pathname.startsWith("/api/")) {
		const readOnly = request.method === "GET" || request.method === "HEAD";
		if (!readOnly || pathname === "/api/ingest") {
			return notFound();
		}
		const hasCredential =
			request.headers.has("cf-access-jwt-assertion") ||
			request.headers.has("authorization");
		if (!env.INGEST_TOKEN || hasCredential) {
			return env.API.fetch(request);
		}
		const forwarded = new Request(request);
		forwarded.headers.set("authorization", `Bearer ${env.INGEST_TOKEN}`);
		return env.API.fetch(forwarded);
	}
	return env.ASSETS.fetch(request);
}

export default {
	fetch(request, env) {
		return routeConsoleRequest(request, env);
	},
} satisfies ExportedHandler<ConsoleEnv>;
