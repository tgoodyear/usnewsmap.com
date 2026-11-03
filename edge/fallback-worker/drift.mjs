// Compares the Cloudflare settings that sit outside wrangler.jsonc with
// zone-expected.json: zone request tracing, the Worker's observability, its routes
// (script and quota failure mode) and its custom domains. Read-only.
//
//   CLOUDFLARE_API_TOKEN=... CLOUDFLARE_ACCOUNT_ID=... node drift.mjs
//
// The token needs Zone Observability Read, Workers Routes Read and Workers Scripts
// Read. Exits 1 and names each difference when the live settings drift.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const API = "https://api.cloudflare.com/client/v4";

/** Every `path: expected X, found Y` where `actual` differs from `expected`. */
function differences(expected, actual, path) {
  if (expected !== null && typeof expected === "object") {
    if (actual === null || typeof actual !== "object") {
      return [`${path}: expected ${JSON.stringify(expected)}, found ${JSON.stringify(actual ?? null)}`];
    }
    return Object.keys(expected).flatMap((k) => differences(expected[k], actual[k], `${path}.${k}`));
  }
  return expected === actual
    ? []
    : [`${path}: expected ${JSON.stringify(expected)}, found ${JSON.stringify(actual ?? null)}`];
}

/**
 * The drift between `expected` (zone-expected.json) and `live`, the settings as read:
 * `{ tracing, observability, routes: [{pattern, script, request_limit_fail_open}],
 * domains: [{hostname, service}] }`. Routes and domains must match exactly: one the
 * file doesn't name is drift too.
 */
export function compare(expected, live) {
  const problems = [
    ...differences(expected.tracing, live.tracing, "tracing"),
    ...differences(expected.worker_observability, live.observability, "worker.observability"),
  ];
  const routes = Object.fromEntries((live.routes ?? []).map((r) => [r.pattern, r]));
  for (const pattern of Object.keys(expected.routes)) {
    if (!routes[pattern]) problems.push(`route ${pattern}: missing`);
    else problems.push(...differences(expected.routes[pattern], routes[pattern], `route ${pattern}`));
  }
  for (const pattern of Object.keys(routes)) {
    if (!(pattern in expected.routes)) problems.push(`route ${pattern}: not in zone-expected.json`);
  }
  const domains = Object.fromEntries((live.domains ?? []).map((d) => [d.hostname, d.service]));
  for (const [host, service] of Object.entries(expected.custom_domains)) {
    if (domains[host] !== service) {
      problems.push(`custom domain ${host}: expected ${service}, found ${domains[host] ?? "nothing"}`);
    }
  }
  for (const host of Object.keys(domains)) {
    if (!(host in expected.custom_domains)) problems.push(`custom domain ${host}: not in zone-expected.json`);
  }
  return problems;
}

async function get(token, path, query = {}) {
  const url = new URL(`${API}${path}`);
  for (const [k, v] of Object.entries(query)) url.searchParams.set(k, v);
  const response = await fetch(url, { headers: { Authorization: `Bearer ${token}` } });
  const body = await response.json().catch(() => null);
  if (!response.ok || !body?.success) {
    const why = body?.errors?.map((e) => e.message).join("; ") || response.statusText;
    throw new Error(`GET ${path}: ${response.status} ${why}`);
  }
  return body.result;
}

/** The live settings `compare` takes. */
export async function read(token, account, expected) {
  const zones = await get(token, "/zones", { name: expected.zone });
  if (zones.length !== 1) throw new Error(`zone ${expected.zone}: found ${zones.length}`);
  const zone = zones[0].id;
  const [tracing, settings, routes, domains] = await Promise.all([
    get(token, `/zones/${zone}/observability/tracing/settings`),
    get(token, `/accounts/${account}/workers/scripts/${expected.worker}/script-settings`),
    get(token, `/zones/${zone}/workers/routes`),
    get(token, `/accounts/${account}/workers/domains`, { zone_id: zone }),
  ]);
  return { tracing, observability: settings.observability, routes, domains };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const token = process.env.CLOUDFLARE_API_TOKEN;
  const account = process.env.CLOUDFLARE_ACCOUNT_ID;
  if (!token || !/^[0-9a-f]{32}$/.test(account ?? "")) {
    console.error("error: set CLOUDFLARE_API_TOKEN and CLOUDFLARE_ACCOUNT_ID (32 hex digits)");
    process.exit(2);
  }
  const expected = JSON.parse(readFileSync(new URL("./zone-expected.json", import.meta.url), "utf8"));
  const problems = compare(expected, await read(token, account, expected));
  if (problems.length) {
    console.error(`Cloudflare settings differ from edge/fallback-worker/zone-expected.json:`);
    for (const p of problems) console.error(`- ${p}`);
    process.exit(1);
  }
  console.log("Cloudflare settings match edge/fallback-worker/zone-expected.json");
}
