import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { compare } from "./drift.mjs";

const expected = JSON.parse(readFileSync(new URL("./zone-expected.json", import.meta.url), "utf8"));

/** Live settings that match zone-expected.json, with `change` applied. */
function live(change = (s) => s) {
  return change({
    tracing: { enabled: false, sampling_ratio: 1 },
    observability: structuredClone(expected.worker_observability),
    routes: Object.entries(expected.routes).map(([pattern, r]) => ({ id: pattern, pattern, ...r })),
    domains: Object.entries(expected.custom_domains).map(([hostname, service]) => ({ hostname, service })),
  });
}

test("matching settings have no drift", () => {
  assert.deepEqual(compare(expected, live()), []);
});

test("zone tracing turned on is drift", () => {
  assert.deepEqual(compare(expected, live((s) => ({ ...s, tracing: { enabled: true } }))), [
    "tracing.enabled: expected false, found true",
  ]);
});

test("query strings no longer redacted is drift", () => {
  const s = live();
  s.observability.redact_query_string = false;
  assert.deepEqual(compare(expected, s), ["worker.observability.redact_query_string: expected true, found false"]);
});

test("null observability is drift, not a match", () => {
  const problems = compare(expected, live((s) => ({ ...s, observability: null })));
  assert.equal(problems.length, 1);
  assert.match(problems[0], /^worker\.observability: expected .* found null$/);
});

test("a route that fails closed is drift", () => {
  const s = live();
  s.routes[0].request_limit_fail_open = false;
  assert.deepEqual(compare(expected, s), [
    `route ${s.routes[0].pattern}.request_limit_fail_open: expected true, found false`,
  ]);
});

test("a missing, a moved and an extra route are drift", () => {
  const s = live();
  const [first, second] = s.routes;
  s.routes = s.routes.filter((r) => r !== first);
  second.script = "other";
  s.routes.push({ pattern: "boinc.usnewsmap.com/*", script: "usnm-fallback" });
  assert.deepEqual(compare(expected, s), [
    `route ${first.pattern}: missing`,
    `route ${second.pattern}.script: expected "usnm-fallback", found "other"`,
    "route boinc.usnewsmap.com/*: not in zone-expected.json",
  ]);
});

test("a missing or extra custom domain is drift", () => {
  const s = live((x) => ({ ...x, domains: [{ hostname: "drill.usnewsmap.com", service: "usnm-fallback" }] }));
  assert.deepEqual(compare(expected, s), [
    "custom domain healthcheck.usnewsmap.com: expected usnm-fallback, found nothing",
    "custom domain drill.usnewsmap.com: not in zone-expected.json",
  ]);
});

test("zone-expected.json keys are sorted", () => {
  const sorted = (v) => v === null || typeof v !== "object" ? true
    : Object.keys(v).every((k, i, a) => i === 0 || a[i - 1] < k) && Object.values(v).every(sorted);
  assert.ok(sorted(expected));
});
