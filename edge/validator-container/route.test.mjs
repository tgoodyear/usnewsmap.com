import { test } from "node:test";
import assert from "node:assert/strict";
import { containerEnv, route, sameToken } from "./route.mjs";

const env = { WEBHOOK_TOKEN: "hook" };
const req = (path, method = "POST", auth = "Bearer hook") =>
  new Request(`https://validator.example${path}`, {
    method,
    headers: auth ? { Authorization: auth } : {},
  });

test("a validation call with the token goes to a container", () => {
  assert.deepEqual(route(req("/v1/validate/usnm_quality"), env), { forward: true });
});

test("health is forwarded for GET only", () => {
  assert.deepEqual(route(req("/healthz", "GET", null), env), { forward: true });
  assert.equal(route(req("/healthz", "POST", null), env).status, 405);
});

test("a missing or wrong token is refused before any container starts", () => {
  assert.equal(route(req("/v1/validate/usnm_gold", "POST", null), env).status, 401);
  assert.equal(route(req("/v1/validate/usnm_gold", "POST", "Bearer nope"), env).status, 401);
  assert.equal(route(req("/v1/validate/usnm_gold", "POST", "hook"), env).status, 401);
  // No token configured: nothing is let through.
  assert.equal(route(req("/v1/validate/usnm_gold"), {}).status, 401);
});

test("other paths and methods are answered here", () => {
  assert.equal(route(req("/"), env).status, 404);
  assert.equal(route(req("/v1/validate/"), env).status, 404);
  assert.equal(route(req("/v1/validate/../admin"), env).status, 404);
  assert.equal(route(req("/v1/validate/usnm_gold/x"), env).status, 404);
  assert.equal(route(req("/v1/validate/usnm_gold", "GET"), env).status, 405);
});

test("token comparison", () => {
  assert.ok(sameToken("abc", "abc"));
  assert.ok(!sameToken("abd", "abc"));
  assert.ok(!sameToken("ab", "abc"));
  assert.ok(!sameToken("abcd", "abc"));
  assert.ok(!sameToken("", ""));
  assert.ok(!sameToken(undefined, "abc"));
});

test("the container environment needs every setting", () => {
  const full = {
    WEBHOOK_TOKEN: "w",
    CBOINC_TOKEN: "a",
    CBOINC_BASE_URL: "https://boinc-dev.usnewsmap.com",
    STATE_URL: "r2://usnm-validator/state",
    GOLD_URL: "r2://usnm-validator/gold",
    R2_ENDPOINT: "https://acct.r2.cloudflarestorage.com",
    R2_ACCESS_KEY_ID: "k",
    R2_SECRET_ACCESS_KEY: "s",
    UNRELATED: "x",
  };
  const out = containerEnv(full);
  assert.equal(out.UNRELATED, undefined);
  assert.equal(Object.keys(out).length, 8);
  assert.equal(out.WEBHOOK_SIGNING_SECRET, undefined);
  assert.equal(containerEnv({ ...full, WEBHOOK_SIGNING_SECRET: "sek" }).WEBHOOK_SIGNING_SECRET, "sek");
  assert.throws(() => containerEnv({ ...full, GOLD_URL: "" }), /GOLD_URL/);
});
