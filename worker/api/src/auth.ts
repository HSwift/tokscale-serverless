import { createRemoteJWKSet, jwtVerify } from "jose";

export interface Env {
	DB: D1Database;
	INGEST_TOKEN?: string;
	ACCESS_TEAM_DOMAIN?: string;
	ACCESS_AUD?: string;
}

export interface AuthActor {
	id: string;
	method: "access" | "bearer";
}

/** Constant-time string comparison for the shared bearer token. */
export function secureEqual(a: string, b: string): boolean {
	const x = new TextEncoder().encode(a);
	const y = new TextEncoder().encode(b);
	if (x.length !== y.length) return false;
	let diff = 0;
	for (let i = 0; i < x.length; i++) diff |= x[i] ^ y[i];
	return diff === 0;
}

/**
 * Verify a Cloudflare Access assertion against the team JWKS (issuer + AUD).
 * Never trust header presence alone: this worker is reachable without Access
 * in front of it, so the signature must check out cryptographically.
 */
export async function authenticateAccess(request: Request, env: Env): Promise<AuthActor | null> {
	const token = request.headers.get("cf-access-jwt-assertion");
	if (!token) return null;
	const teamDomain = (env.ACCESS_TEAM_DOMAIN ?? "").replace(/\/$/, "");
	if (
		!teamDomain.startsWith("https://") ||
		!env.ACCESS_AUD ||
		env.ACCESS_AUD.startsWith("replace-")
	) {
		return null;
	}
	try {
		const jwks = createRemoteJWKSet(new URL(`${teamDomain}/cdn-cgi/access/certs`));
		const { payload } = await jwtVerify(token, jwks, {
			issuer: teamDomain,
			audience: env.ACCESS_AUD,
		});
		const email = typeof payload.email === "string" ? payload.email : null;
		return { id: email ?? payload.sub ?? "access-user", method: "access" };
	} catch {
		return null;
	}
}

export async function authenticateBearer(request: Request, env: Env): Promise<AuthActor | null> {
	const authorization = request.headers.get("authorization") ?? "";
	if (!authorization.startsWith("Bearer ") || !env.INGEST_TOKEN) return null;
	if (!secureEqual(authorization.slice("Bearer ".length), env.INGEST_TOKEN)) return null;
	return { id: "api-token", method: "bearer" };
}
