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
	it("never injects credentials into installer requests and requires a same-origin Access session", async () => {
		let forwarded = 0;
		const env = makeEnv(req => {
			forwarded++;
			expect(req.headers.get("authorization")).toBeNull();
			expect(req.headers.get("cf-access-jwt-assertion")).toBe("assertion");
			return new Response("{}");
		});
		for (const headers of [{}, { origin: "https://evil.example", "cf-access-jwt-assertion": "assertion" }]) {
			const response = await routeConsoleRequest(new Request("https://console.example/api/install-tickets", { method: "POST", headers }), env);
			expect(response.status).toBe(403);
		}
		expect(forwarded).toBe(0);
		const response = await routeConsoleRequest(new Request("https://console.example/api/install-tickets", { method: "POST", headers: { origin: "https://console.example", "cf-access-jwt-assertion": "assertion", authorization: "Bearer ignored" } }), env);
		expect(response.status).toBe(200);
		expect(forwarded).toBe(1);
	});
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

	it("never proxies /api/ingest or non-GET methods", async () => {
		let apiCalled = false;
		const env = makeEnv(() => {
			apiCalled = true;
			return new Response("{}");
		});
		const ingest = await routeConsoleRequest(
			new Request("https://console.example/api/ingest", { method: "POST" }),
			env,
		);
		expect(ingest.status).toBe(404);
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
