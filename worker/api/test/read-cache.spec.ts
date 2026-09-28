import { afterEach, describe, expect, it, vi } from "vitest";
import { cachedRead, invalidateReads } from "../src/read-cache";
import type { Env } from "../src/auth";

afterEach(() => vi.restoreAllMocks());

describe("statistics read cache", () => {
	it("coalesces concurrent reads, normalizes parameters and expires without querying D1 for a version", async () => {
		const env = { DB: {} } as Env;
		const clock = vi.spyOn(Date, "now").mockReturnValue(1000);
		const load = vi.fn(async () => Response.json({ tokens: 123 }));
		const urls = [new URL("https://api.example/api/hourly?date=2026-09-22&group=devices"),
			new URL("https://api.example/api/hourly?group=devices&date=2026-09-22")];
		const responses = await Promise.all(urls.map(url => cachedRead(env, url, load)));
		expect(load).toHaveBeenCalledTimes(1);
		for (const response of responses) expect(await response.json()).toEqual({ tokens: 123 });
		clock.mockReturnValue(1000 + 5 * 60 * 1000);
		await cachedRead(env, urls[0], load);
		expect(load).toHaveBeenCalledTimes(2);
		invalidateReads(env);
		await cachedRead(env, urls[0], load);
		expect(load).toHaveBeenCalledTimes(3);
	});

	it("isolates databases and does not cache errors", async () => {
		const env = { DB: {} } as Env;
		const url = new URL("https://api.example/api/dashboard");
		const error = vi.fn(async () => new Response("unavailable", { status: 503 }));
		await cachedRead(env, url, error);
		await cachedRead(env, url, error);
		expect(error).toHaveBeenCalledTimes(2);
		const load = vi.fn(async () => Response.json({ tokens: 1 }));
		await cachedRead(env, url, load);
		await cachedRead({ DB: {} } as Env, url, load);
		expect(load).toHaveBeenCalledTimes(2);
	});
});
