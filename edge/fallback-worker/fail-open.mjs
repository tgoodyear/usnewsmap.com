// Sets every route of one Worker in one zone to fail open, then reads them back.
// `wrangler deploy` recreates the routes, and a new route fails closed: on Workers
// Free, once the day's requests are spent, it answers with error 1027 instead of
// skipping the Worker (README.md, Configuration). deploy.sh runs this after each deploy.
//
//   CLOUDFLARE_API_TOKEN=... node fail-open.mjs --zone usnewsmap.com --script usnm-fallback
//
// The token needs Workers Routes Write, which the deploy token has.
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

const API = "https://api.cloudflare.com/client/v4";

async function call(token, method, path, body) {
  const response = await fetch(`${API}${path}`, {
    method,
    headers: { Authorization: `Bearer ${token}`, ...(body ? { "Content-Type": "application/json" } : {}) },
    body: body ? JSON.stringify(body) : undefined,
  });
  const json = await response.json().catch(() => null);
  if (!response.ok || !json?.success) {
    const why = json?.errors?.map((e) => e.message).join("; ") || response.statusText;
    throw new Error(`${method} ${path}: ${response.status} ${why}`);
  }
  return json.result;
}

/** Sets `script`'s routes in `zone` to fail open; returns their patterns, read back. */
export async function failOpen(token, zone, script) {
  const zones = await call(token, "GET", `/zones?name=${encodeURIComponent(zone)}`);
  if (zones.length !== 1) throw new Error(`zone ${zone}: found ${zones.length}`);
  const routesPath = `/zones/${zones[0].id}/workers/routes`;
  const mine = (await call(token, "GET", routesPath)).filter((r) => r.script === script);
  if (mine.length === 0) throw new Error(`no routes for ${script} in ${zone}`);
  for (const r of mine.filter((r) => r.request_limit_fail_open !== true)) {
    await call(token, "PUT", `${routesPath}/${r.id}`, {
      pattern: r.pattern, script: r.script, request_limit_fail_open: true,
    });
  }
  const after = (await call(token, "GET", routesPath)).filter((r) => r.script === script);
  const closed = after.filter((r) => r.request_limit_fail_open !== true).map((r) => r.pattern);
  if (closed.length) throw new Error(`still fail closed after the update: ${closed.join(", ")}`);
  return after.map((r) => r.pattern);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const { values } = parseArgs({ options: { zone: { type: "string" }, script: { type: "string" } } });
  const token = process.env.CLOUDFLARE_API_TOKEN;
  if (!token || !values.zone || !values.script) {
    console.error("usage: CLOUDFLARE_API_TOKEN=... node fail-open.mjs --zone ZONE --script WORKER");
    process.exit(2);
  }
  try {
    const patterns = await failOpen(token, values.zone, values.script);
    console.log(`fail open: ${patterns.join(", ")}`);
  } catch (error) {
    console.error(`error: ${error.message}`);
    process.exit(1);
  }
}
