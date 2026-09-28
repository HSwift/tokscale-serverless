import { describe, expect, it } from "vitest";
import { routeConsoleRequest, type ConsoleEnv } from "../src/worker";

function makeEnv(apiFetch: (req: Request) => Response): ConsoleEnv {
	return {
		API: { fetch: apiFetch },
		ASSETS: {
			fetch: () => new Response("<html>page</html>", {
				headers: { "content-type": "text/html" },
			}),
		},
		INGEST_TOKEN: "secret-token",
	};
}

describe("console router", () => {
	it("serves assets for non-api paths", async () => {
		const res = await routeConsoleRequest(
			new Request("https://console.example/"),
			makeEnv(() => new Response("api reached", { status: 500 })),
		);
		expect(res.status).toBe(200);
		expect(await res.text()).toBe("<html>page</html>");
	});

	it("injects the bearer token for read-only /api calls without credentials", async () => {
		let seen: string | null = null;
		const res = await routeConsoleRequest(
			new Request("https://console.example/api/summary"),
			makeEnv((req) => {
				seen = req.headers.get("authorization");
				return new Response("{}");
			}),
		);
		expect(res.status).toBe(200);
		expect(seen).toBe("Bearer secret-token");
	});

	it("forwards an Access assertion untouched (no injection)", async () => {
		let auth: string | null = "unset";
		let assertion: string | null = null;
		await routeConsoleRequest(
			new Request("https://console.example/api/summary", {
				headers: { "Cf-Access-Jwt-Assertion": "real.jwt.here" },
			}),
			makeEnv((req) => {
				auth = req.headers.get("authorization");
				assertion = req.headers.get("cf-access-jwt-assertion");
				return new Response("{}");
			}),
		);
		expect(auth).toBeNull();
		expect(assertion).toBe("real.jwt.here");
	});

	it("forwards collector and installer requests with their original URL, body, and credentials", async () => {
		for (const authorization of [null, "Bearer collector-token"]) {
			for (const path of ["/api/ingest", "/api/me", "/install.sh?token=collector-token&type=linux&version=v0.2", "/install.ps1?token=collector-token&type=windows&version=v0.2"]) {
				const ingest = path === "/api/ingest";
				const request = new Request(`https://console.example${path}`, {
					method: ingest ? "POST" : "GET",
					headers: authorization ? { authorization } : {},
					body: ingest ? '{"device":{"id":"test"}}' : undefined,
				});
				let forwarded: Request | undefined;
				const response = await routeConsoleRequest(request, makeEnv(req => {
					forwarded = req;
					return new Response("api reached");
				}));
				expect(await response.text()).toBe("api reached");
				expect(forwarded?.url).toBe(request.url);
				expect(forwarded?.headers.get("authorization")).toBe(authorization);
				if (ingest) expect(await forwarded?.text()).toBe('{"device":{"id":"test"}}');
			}
		}
	});

	it("rejects unsupported methods before reaching the API", async () => {
		let apiCalled = false;
		const env = makeEnv(() => {
			apiCalled = true;
			return new Response("{}");
		});
		const install = await routeConsoleRequest(
			new Request("https://console.example/install.sh", { method: "POST" }),
			env,
		);
		expect(install.status).toBe(404);
		const put = await routeConsoleRequest(
			new Request("https://console.example/api/summary", { method: "PUT" }),
			env,
		);
		expect(put.status).toBe(404);
		const getIngest = await routeConsoleRequest(
			new Request("https://console.example/api/ingest"),
			env,
		);
		expect(getIngest.status).toBe(404);
		expect(apiCalled).toBe(false);
	});

	it("passes /api reads through when no token is configured", async () => {
		let seen: string | null = "unset";
		const env = makeEnv((req) => {
			seen = req.headers.get("authorization");
			return new Response("{}");
		});
		env.INGEST_TOKEN = undefined;
		await routeConsoleRequest(new Request("https://console.example/api/daily"), env);
		expect(seen).toBeNull();
	});
});
