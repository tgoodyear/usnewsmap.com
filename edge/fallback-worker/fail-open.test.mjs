import assert from "node:assert/strict";
import { test } from "node:test";
import { failOpen } from "./fail-open.mjs";

/** A fake routes API holding `routes`; `stick: false` ignores the failure-mode update. */
function api(t, routes, { stick = true, staleReads = 0 } = {}) {
  let stale = staleReads;
  const pending = [];
  const calls = [];
  t.mock.method(globalThis, "fetch", async (url, init = {}) => {
    const path = new URL(url).pathname.replace("/client/v4", "");
    const method = init.method ?? "GET";
    calls.push(`${method} ${path}`);
    const ok = (result) => new Response(JSON.stringify({ success: true, result }));
    if (path === "/zones") return ok([{ id: "z1" }]);
    if (method === "GET" && path === "/zones/z1/workers/routes") {
      if (stale > 0 && pending.length) {
        stale--;
        return ok(structuredClone(routes).map((r) => (pending.includes(r.id) ? { ...r, request_limit_fail_open: false } : r)));
      }
      return ok(structuredClone(routes));
    }
    const route = routes.find((r) => path === `/zones/z1/workers/routes/${r.id}`);
    if (method === "PUT" && route) {
      const body = JSON.parse(init.body);
      assert.equal(body.pattern, route.pattern);
      assert.equal(body.script, route.script);
      pending.push(route.id);
      if (stick) route.request_limit_fail_open = body.request_limit_fail_open;
      return ok(route);
    }
    return new Response(JSON.stringify({ success: false, errors: [{ message: "not found" }] }), { status: 404 });
  });
  return calls;
}

const fast = { delayMs: 0, sleep: async () => {} };
const routes = () => [
  { id: "a", pattern: "usnewsmap.com/*", script: "usnm-fallback", request_limit_fail_open: false },
  { id: "b", pattern: "www.usnewsmap.com/*", script: "usnm-fallback", request_limit_fail_open: true },
  { id: "c", pattern: "drill.usnewsmap.com/*", script: "usnm-fallback-drill", request_limit_fail_open: false },
];

test("sets only the Worker's fail-closed routes to fail open", async (t) => {
  const calls = api(t, routes());
  assert.deepEqual(await failOpen("token", "usnewsmap.com", "usnm-fallback", fast), ["usnewsmap.com/*", "www.usnewsmap.com/*"]);
  assert.deepEqual(calls.filter((c) => c.startsWith("PUT")), ["PUT /zones/z1/workers/routes/a"]);
});

test("an update that doesn't stick fails", async (t) => {
  api(t, routes(), { stick: false });
  await assert.rejects(failOpen("token", "usnewsmap.com", "usnm-fallback", fast), /still fail closed after the update and 6 read-backs: usnewsmap\.com\/\* \(request_limit_fail_open=false\)/);
});

test("a Worker with no routes fails", async (t) => {
  api(t, routes());
  await assert.rejects(failOpen("token", "usnewsmap.com", "other"), /no routes for other/);
});

test("a stale read-back is retried until it shows fail open", async (t) => {
  const calls = api(t, routes(), { staleReads: 3 });
  const waits = [];
  const patterns = await failOpen("token", "usnewsmap.com", "usnm-fallback", { delayMs: 10, sleep: async (ms) => waits.push(ms) });
  assert.deepEqual(patterns, ["usnewsmap.com/*", "www.usnewsmap.com/*"]);
  assert.deepEqual(waits, [10, 20, 40]);
  assert.equal(calls.filter((c) => c.startsWith("PUT")).length, 1);
});

test("a read-back that stays stale past the limit fails", async (t) => {
  api(t, routes(), { staleReads: 99 });
  await assert.rejects(failOpen("token", "usnewsmap.com", "usnm-fallback", { ...fast, attempts: 3 }), /after the update and 3 read-backs/);
});
