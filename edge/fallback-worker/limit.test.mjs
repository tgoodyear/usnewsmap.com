// node --test edge/fallback-worker/limit.test.mjs
//
// Edge flood control on the search and MCP paths (#1541): the Worker asks a stand-in for
// the rate-limiting binding, and a stand-in for fetch() records whether a request went on
// to the tunnel. Nothing here reaches Cloudflare or the network.

import assert from "node:assert/strict";
import { test } from "node:test";

import worker, { LIMITED_PATHS, clientKey } from "./worker.mjs";

const SITE = "https://usnewsmap.com";
const IP = "203.0.113.7";

// A binding that allows `allow` calls, then refuses, and records each key.
function binding(allow = Infinity) {
  const keys = [];
  return {
    keys,
    async limit({ key }) {
      keys.push(key);
      return { success: keys.length <= allow };
    },
  };
}

async function run(request, env) {
  const calls = [];
  const realFetch = globalThis.fetch;
  globalThis.fetch = async (input) => {
    calls.push(input instanceof Request ? input.url : String(input));
    return new Response("from the tunnel", { status: 200 });
  };
  try {
    return { response: await worker.fetch(request, env), calls };
  } finally {
    globalThis.fetch = realFetch;
  }
}

const search = (path = "/v1/hits?q=gold", init = {}) =>
  new Request(`${SITE}${path}`, { ...init, headers: { "cf-connecting-ip": IP, ...init.headers } });

test("a client under the limit goes on to the tunnel, keyed by its address", async () => {
  const limit = binding();
  const { response, calls } = await run(search(), { SEARCH_LIMIT: limit });
  assert.equal(response.status, 200);
  assert.equal(await response.text(), "from the tunnel");
  assert.deepEqual(calls, [`${SITE}/v1/hits?q=gold`]);
  assert.deepEqual(limit.keys, [`4:${IP}`]);
});

test("a client over the limit gets the API's 429 at the edge, and the tunnel is not asked", async () => {
  const limit = binding(0);
  const { response, calls } = await run(search(), { SEARCH_LIMIT: limit, METRICS_MODE: "native" });
  assert.deepEqual(calls, []);
  assert.equal(response.status, 429);
  assert.equal(response.headers.get("retry-after"), "10");
  assert.equal(response.headers.get("content-type"), "application/problem+json");
  assert.equal(response.headers.get("cache-control"), "no-store");
  assert.equal(response.headers.get("x-usnm-edge-limit"), "normal");
  assert.equal(response.headers.get("x-usnm-origin"), "edge");
  assert.equal(response.headers.get("x-usnm-failover-reason"), "edge-rate-limit");
  assert.equal(response.headers.get("access-control-allow-origin"), null);
  assert.deepEqual(await response.json(), {
    type: "/errors/rate-limited",
    title: "Too many requests",
    status: 429,
    detail: "This client has sent too many requests.",
    hint: "Wait for the time in Retry-After, then try again.",
  });
});

test("every search and MCP path counts, under both prefixes and every method but OPTIONS", async () => {
  assert.deepEqual([...LIMITED_PATHS].sort(), [
    "/api/v1/aggregate", "/api/v1/hits", "/api/v1/search-report",
    "/mcp", "/mcp/", "/v1/aggregate", "/v1/hits", "/v1/search-report",
  ]);
  for (const path of LIMITED_PATHS) {
    for (const method of ["GET", "POST", "HEAD"]) {
      const { response, calls } = await run(search(path, { method }), { SEARCH_LIMIT: binding(0) });
      assert.equal(response.status, 429, `${method} ${path}`);
      assert.deepEqual(calls, []);
      if (method === "HEAD") assert.equal(await response.text(), "");
    }
    const limit = binding(0);
    const preflight = await run(search(path, { method: "OPTIONS" }), { SEARCH_LIMIT: limit });
    assert.equal(preflight.response.status, 200);
    assert.deepEqual(limit.keys, []);
  }
});

test("other paths are never counted", async () => {
  for (const path of ["/", "/search/whigs?q=gold", "/v1/datasets", "/v1/hits/extra", "/mcp/x", "/assets/app.js",
                      "/V1/hits", "/readyz", "/v1/beacon"]) {
    const limit = binding(0);
    const { response } = await run(search(path), { SEARCH_LIMIT: limit });
    assert.equal(response.status, 200, path);
    assert.deepEqual(limit.keys, [], path);
  }
});

test("SEARCH_EDGE_LIMIT chooses the limit: strict uses its own binding, off counts nothing", async () => {
  for (const [mode, used] of [[undefined, "normal"], ["normal", "normal"], ["strict", "strict"],
                              ["bogus", "normal"], ["off", null]]) {
    const normal = binding(0);
    const strict = binding(0);
    const { response } = await run(search(), { SEARCH_LIMIT: normal, SEARCH_LIMIT_STRICT: strict, SEARCH_EDGE_LIMIT: mode });
    assert.equal(response.status, used ? 429 : 200, String(mode));
    assert.equal(normal.keys.length, used === "normal" ? 1 : 0, String(mode));
    assert.equal(strict.keys.length, used === "strict" ? 1 : 0, String(mode));
    if (used) assert.equal(response.headers.get("x-usnm-edge-limit"), used);
  }
});

test("it fails open: no binding, no client address, or a limiter error lets the API decide", async () => {
  const broken = { async limit() { throw new Error("limiter unavailable"); } };
  for (const [name, request, env] of [
    ["no binding", search(), {}],
    ["no strict binding", search(), { SEARCH_LIMIT: binding(0), SEARCH_EDGE_LIMIT: "strict" }],
    ["not a binding", search(), { SEARCH_LIMIT: "x" }],
    ["no address", new Request(`${SITE}/v1/hits?q=gold`), { SEARCH_LIMIT: binding(0) }],
    ["limiter error", search(), { SEARCH_LIMIT: broken }],
    ["odd answer", search(), { SEARCH_LIMIT: { async limit() { return undefined; } } }],
  ]) {
    const { response, calls } = await run(request, env);
    assert.equal(response.status, 200, name);
    assert.equal(calls.length, 1, name);
  }
});

test("the 429 carries CORS only for the site's own origins", async () => {
  const env = { SEARCH_LIMIT: binding(0), ALLOWED_ORIGINS: "https://usnewsmap.com, https://www.usnewsmap.com" };
  const own = await run(search("/v1/hits?q=gold", { headers: { origin: "https://www.usnewsmap.com" } }), env);
  assert.equal(own.response.headers.get("access-control-allow-origin"), "https://www.usnewsmap.com");
  assert.equal(own.response.headers.get("access-control-expose-headers"), "Retry-After");
  assert.equal(own.response.headers.get("vary"), "Origin");
  const other = await run(search("/v1/hits?q=gold", { headers: { origin: "https://evil.example" } }), env);
  assert.equal(other.response.headers.get("access-control-allow-origin"), null);
});

test("MCP's 429 also allows the apex and api origins, as the MCP endpoint does", async () => {
  const env = { SEARCH_LIMIT: binding(0), ALLOWED_ORIGINS: "https://www.usnewsmap.com" };
  for (const origin of ["https://usnewsmap.com", "https://api.usnewsmap.com", "https://www.usnewsmap.com"]) {
    const mcp = await run(search("/mcp", { method: "POST", headers: { origin } }), env);
    assert.equal(mcp.response.headers.get("access-control-allow-origin"), origin, origin);
  }
  const rest = await run(search("/v1/hits?q=gold", { headers: { origin: "https://api.usnewsmap.com" } }), env);
  assert.equal(rest.response.headers.get("access-control-allow-origin"), null);
});

test("a www search and the apex request its redirect leads to are counted once each, apart", async () => {
  const limit = binding();
  const calls = [];
  const realFetch = globalThis.fetch;
  // The API's www redirect, then the apex answer, as a client following redirects sees.
  globalThis.fetch = async (input) => {
    const url = new URL(input.url);
    calls.push(url.hostname);
    return url.hostname === "www.usnewsmap.com"
      ? new Response(null, { status: 301, headers: { location: `${SITE}${url.pathname}${url.search}` } })
      : new Response("from the tunnel", { status: 200 });
  };
  try {
    let request = new Request("https://www.usnewsmap.com/v1/hits?q=gold", { headers: { "cf-connecting-ip": IP } });
    let answer = await worker.fetch(request, { SEARCH_LIMIT: limit });
    assert.equal(answer.status, 301);
    request = new Request(answer.headers.get("location"), { headers: { "cf-connecting-ip": IP } });
    answer = await worker.fetch(request, { SEARCH_LIMIT: limit });
    assert.equal(answer.status, 200);
  } finally {
    globalThis.fetch = realFetch;
  }
  assert.deepEqual(calls, ["www.usnewsmap.com", "usnewsmap.com"]);
  assert.deepEqual(limit.keys, [`www|4:${IP}`, `4:${IP}`]);
});

test("the limit comes before health-driven failover's probes", async () => {
  const { response, calls } = await run(search(), {
    SEARCH_LIMIT: binding(0), HEALTH_FAILOVER: "true", HEALTH_HOSTNAME: "usnewsmap.com", METRICS_MODE: "native",
  });
  assert.equal(response.status, 429);
  assert.deepEqual(calls, []);
});

test("the opt-in outcome log is the fixed record, with no address or search words", async () => {
  const records = [];
  const log = console.log;
  console.log = (record) => records.push(record);
  try {
    await run(search(), { SEARCH_LIMIT: binding(0), LOG_OUTCOMES: "true", METRICS_MODE: "native" });
    await run(search(), { SEARCH_LIMIT: binding(0), METRICS_MODE: "native" });
  } finally {
    console.log = log;
  }
  assert.deepEqual(records, [{ origin: "edge", reason: "edge-rate-limit", status: 429 }]);
});

test("a limited answer is counted under its own outcome, not the address", async () => {
  const points = [];
  await run(search(), { SEARCH_LIMIT: binding(0), FALLBACK_METRICS: { writeDataPoint: (p) => points.push(p) } });
  assert.deepEqual(points, [{ blobs: ["edge", "production"], doubles: [1, 429], indexes: ["production"] }]);
});

test("client keys: IPv4 whole, IPv6 by /64, IPv4-mapped as IPv4", () => {
  assert.equal(clientKey("203.0.113.7"), "4:203.0.113.7");
  assert.equal(clientKey(" 203.0.113.7 "), "4:203.0.113.7");
  assert.equal(clientKey("::ffff:203.0.113.7"), "4:203.0.113.7");
  assert.equal(clientKey("::ffff:cb00:7107"), "4:203.0.113.7");
  const one = clientKey("2001:db8:1:2:aaaa::1");
  assert.equal(one, "6:2001:db8:1:2::/64");
  for (const same of ["2001:0DB8:0001:0002:ffff:ffff:ffff:ffff", "2001:db8:1:2::", "2001:db8:1:2::9%eth0",
                      "2001:db8:1:2:0:0:0:1"]) {
    assert.equal(clientKey(same), one, same);
  }
  assert.equal(clientKey("2001:db8::1"), "6:2001:db8:0:0::/64");
  assert.equal(clientKey("::1"), "6:0:0:0:0::/64");
  assert.notEqual(clientKey("2001:db8:1:3::1"), one);
  for (const missing of [null, undefined, "", "  "]) assert.equal(clientKey(missing), null);
  // Not an address: still a key of its own, so the header can't be used to dodge the limit.
  for (const odd of ["1:2:3", "1::2::3", "2001:db8::g", "1:2:3:4:5:6:7:8:9"]) {
    assert.equal(clientKey(odd), `?:${odd}`, odd);
  }
});
