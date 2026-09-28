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

	it("does not forward installation scripts or collector uploads", async () => {
		let apiCalled = false;
		const env = makeEnv(() => {
			apiCalled = true;
			return new Response("{}");
		});
		env.ASSETS.fetch = () => new Response("Not found", { status: 404 });
		for (const [path, method] of [
			["/install.sh", "GET"], ["/install.ps1", "GET"],
			["/api/ingest", "POST"], ["/api/ingest", "GET"], ["/api/summary", "PUT"],
		]) {
			const res = await routeConsoleRequest(new Request(`https://console.example${path}`, { method }), env);
			expect(res.status).toBe(404);
		}
		expect(apiCalled).toBe(false);
	});

	it("never substitutes the console token for /api/me credentials", async () => {
		for (const authorization of [null, "Bearer collector-token"]) {
			let seen: string | null = "unset";
			await routeConsoleRequest(new Request("https://console.example/api/me", {
				headers: authorization ? { authorization } : {},
			}), makeEnv(req => {
				seen = req.headers.get("authorization");
				return new Response("{}");
			}));
			expect(seen).toBe(authorization);
		}
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
