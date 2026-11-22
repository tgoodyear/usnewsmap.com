// What the validator Worker does with a request before any container is woken (README.md). Kept
// apart from worker.mjs so that it runs under `node --test` without the Workers runtime.

/** Container instances the calls are spread over. State is in R2, so any instance can answer. */
export const INSTANCES = 2;

/** Compares two strings in time that depends only on their lengths. */
export function sameToken(given, want) {
  if (typeof given !== "string" || typeof want !== "string" || want.length === 0) return false;
  const a = new TextEncoder().encode(given);
  const b = new TextEncoder().encode(want);
  let diff = a.length ^ b.length;
  for (let i = 0; i < b.length; i++) diff |= (a[i] ?? 0) ^ b[i];
  return diff === 0;
}

/**
 * Decides a request: `{ forward: true }` to pass it to a container unchanged, or `{ status, body }`
 * to answer here. Only `POST /v1/validate/<app>` with the webhook's bearer token, and
 * `GET /healthz`, reach a container; anything else is answered without waking one.
 */
export function route(request, env) {
  const url = new URL(request.url);
  if (url.pathname === "/healthz") {
    return request.method === "GET" ? { forward: true } : { status: 405, body: "GET only\n" };
  }
  if (!/^\/v1\/validate\/[a-z0-9_]{1,64}$/.test(url.pathname)) {
    return { status: 404, body: "not found\n" };
  }
  if (request.method !== "POST") return { status: 405, body: "POST only\n" };
  const auth = request.headers.get("Authorization") ?? "";
  const token = auth.startsWith("Bearer ") ? auth.slice(7) : "";
  if (!sameToken(token, env.WEBHOOK_TOKEN)) {
    return { status: 401, body: "missing or wrong bearer token\n" };
  }
  return { forward: true };
}

/** The environment the container starts with, from the Worker's vars and secrets. */
export function containerEnv(env) {
  const names = [
    "WEBHOOK_TOKEN",
    "CBOINC_TOKEN",
    "CBOINC_BASE_URL",
    "STATE_URL",
    "GOLD_URL",
    "R2_ENDPOINT",
    "R2_ACCESS_KEY_ID",
    "R2_SECRET_ACCESS_KEY",
  ];
  const out = {};
  for (const n of names) {
    if (typeof env[n] !== "string" || env[n] === "") throw new Error(`${n} is not set`);
    out[n] = env[n];
  }
  // Optional: with it, validatord verifies X-CB-Signature; without it, it warns and does not.
  if (typeof env.WEBHOOK_SIGNING_SECRET === "string" && env.WEBHOOK_SIGNING_SECRET !== "") {
    out.WEBHOOK_SIGNING_SECRET = env.WEBHOOK_SIGNING_SECRET;
  }
  return out;
}
