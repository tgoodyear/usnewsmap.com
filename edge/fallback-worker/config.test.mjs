// node --test edge/fallback-worker/config.test.mjs
//
// wrangler.jsonc holds the settings the Worker's privacy and fail-open rules rest on. This
// reads the file as it is committed and fails if one of them changes.

import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { test } from "node:test";

import worker from "./worker.mjs";
import { parseJsonc } from "./parse-jsonc.mjs";

// JSON with // and /* */ comments, as wrangler reads it. Comment markers inside strings
// (the https:// of a URL) stay. One forward pass: every branch moves the cursor ahead and
// the input is consumed once. A string or block comment that never closes throws a
// SyntaxError, so a malformed file fails the run and does not hang it.

const config = parseJsonc(readFileSync(new URL("./wrangler.jsonc", import.meta.url), "utf8"));
const healthConfig = parseJsonc(readFileSync(new URL("./wrangler.health.jsonc", import.meta.url), "utf8"));

test("heartbeat configuration is opt-in with identical runtime, routes and privacy settings", () => {
  const { triggers, vars, cache, ...base } = healthConfig;
  const { vars: legacyVars, ...legacy } = config;
  assert.deepEqual(base, legacy);
  assert.deepEqual(triggers, { crons: ["* * * * *"] });
  assert.deepEqual(cache, { enabled: false });
  assert.equal(healthConfig.placement, undefined);
  assert.equal(healthConfig.compatibility_flags, undefined);
  assert.deepEqual(vars, { ...legacyVars, HEALTH_FAILOVER: "true", HEALTH_HEARTBEAT: "true", HEALTH_HOSTNAME: "usnewsmap.com" });
  assert.equal(config.triggers, undefined);
});

test("the comment stripper keeps URLs and drops comments", () => {
  assert.deepEqual(parseJsonc('{ "a": "https://x.example/*", // gone\n "b": 1 /* gone */ }'), {
    a: "https://x.example/*",
    b: 1,
  });
});

test("the comment stripper throws on an unterminated string instead of scanning on", () => {
  for (const source of ['{ "a": "never closed', '{ "a": "ends on an escape \\"', '{ "a": "x\\']) {
    assert.throws(() => parseJsonc(source), { name: "SyntaxError", message: /unterminated string/ });
  }
});

test("the comment stripper throws on an unterminated block comment instead of looping", () => {
  for (const source of ['{ "a": 1 } /* never closed', '{ "a": 1 } /*', '{ "a": 1 } /*/']) {
    assert.throws(() => parseJsonc(source), { name: "SyntaxError", message: /unterminated block comment/ });
  }
});

test("Workers Logs, Logpush and tail consumers are off or absent", () => {
  assert.deepEqual(config.observability, { enabled: false });
  assert.ok(!config.logpush);
  assert.equal(config.tail_consumers, undefined);
});

test("the Worker has no workers.dev name and no preview URLs", () => {
  assert.equal(config.workers_dev, false);
  assert.equal(config.preview_urls, false);
});

test("the runtime date is pinned and not in the future", () => {
  assert.match(config.compatibility_date, /^\d{4}-\d{2}-\d{2}$/);
  assert.ok(config.compatibility_date <= new Date().toISOString().slice(0, 10));
});

test("the entry point exists and the file holds no setting this Worker does not use", () => {
  assert.ok(existsSync(new URL(config.main, import.meta.url)));
  assert.deepEqual(Object.keys(config).sort(), [
    "analytics_engine_datasets",
    "compatibility_date",
    "main",
    "name",
    "observability",
    "preview_urls",
    "routes",
    "vars",
    "workers_dev",
  ]);
});

test("the routes are the apex, www and api, every path, nothing else", () => {
  assert.deepEqual(
    config.routes.map((route) => [route.pattern, route.zone_name]),
    [
      ["usnewsmap.com/*", "usnewsmap.com"],
      ["www.usnewsmap.com/*", "usnewsmap.com"],
      ["api.usnewsmap.com/*", "usnewsmap.com"],
    ],
  );
  // /* includes /assets/* (ADR-0027 settled decision 10); no route is written without the Worker.
  for (const route of config.routes) assert.deepEqual(Object.keys(route).sort(), ["pattern", "zone_name"]);
});

test("vars hold no secret, drill flag or Azure name; the Phase 4 mode stays off", () => {
  assert.deepEqual(Object.keys(config.vars), ["AZURE_FALLBACK_ORIGIN", "ERROR_LAYER", "ALLOWED_ORIGINS"]);
  assert.equal(config.vars.AZURE_FALLBACK_ORIGIN, "");
  assert.equal(config.vars.ERROR_LAYER, "false");
  assert.equal(config.vars.ALLOWED_ORIGINS, "https://usnewsmap.com");
});

test("the Worker does not fall back to the empty name committed here", async () => {
  const tunnel = new Response("error code: 1033", { status: 530 });
  const realFetch = globalThis.fetch;
  let calls = 0;
  globalThis.fetch = async () => {
    calls++;
    return tunnel;
  };
  try {
    const response = await worker.fetch(new Request("https://usnewsmap.com/"), config.vars);
    assert.equal(response, tunnel);
    assert.equal(calls, 1);
  } finally {
    globalThis.fetch = realFetch;
  }
});
