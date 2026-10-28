// node --test edge/fallback-worker/worker.test.mjs
//
// Runs the Worker against a stand-in for fetch(): nothing here reaches Cloudflare, Azure
// or the network. The stand-in reads each request's body, as a real fetch does, so a
// request whose body was already used fails the way it would in production.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import worker from "./worker.mjs";

const AZURE = "https://ca-usnm-prod.example-env.eastus2.azurecontainerapps.io";
const ENV = { AZURE_FALLBACK_ORIGIN: AZURE, FALLBACK_METRICS: { writeDataPoint() {} } };
const SITE = "https://usnewsmap.com";
const PROBLEM = JSON.stringify({ type: "about:blank", title: "Service Unavailable", status: 503 });

// Runs the Worker on `request` with fetch() answering from `script`, one step per call: a
// Response to return, an Error to throw, or a function of the recorded call that returns
// either. Returns the Worker's response (or the error it threw) and every call made.
async function run(request, env, script) {
  const calls = [];
  const realFetch = globalThis.fetch;
  globalThis.fetch = async (input, init) => {
    const sent = input instanceof Request ? input : new Request(input, init);
    const call = {
      url: sent.url,
      method: sent.method,
      headers: Object.fromEntries(sent.headers),
      redirect: sent.redirect,
      body: sent.body === null ? null : await sent.text(),
    };
    calls.push(call);
    const step = script[calls.length - 1];
    // An extra call is a failure the test's call count shows; the Worker would otherwise
    // treat this error as a thrown fetch.
    if (step === undefined) throw new Error(`unexpected fetch ${calls.length}: ${call.method} ${call.url}`);
    const result = typeof step === "function" ? step(call) : step;
    if (result instanceof Error) throw result;
    return result;
  };
  try {
    let outcome;
    try {
      outcome = { response: await worker.fetch(request, env) };
    } catch (error) {
      outcome = { error };
    }
    return { ...outcome, calls };
  } finally {
    globalThis.fetch = realFetch;
  }
}

const get = (path, headers = {}) => new Request(`${SITE}${path}`, { headers });
const text = (status, body = "") => new Response([204, 205, 304].includes(status) ? null : body, { status });
const networkError = () => new TypeError("Network connection lost.");

test("a tunnel answer passes through untouched, with no second request", async () => {
  const answer = new Response("<html>", { status: 200, headers: { "content-type": "text/html", etag: 'W/"abc"' } });
  const { response, calls } = await run(get("/datasets?x=1"), ENV, [answer]);
  assert.equal(response, answer);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].url, `${SITE}/datasets?x=1`);
});

test("only 502, 530 and a thrown fetch fall back: every other status is an answer", async () => {
  for (const status of [200, 204, 301, 304, 307, 400, 404, 429, 500, 501, 503, 504, 520, 521, 522, 523, 524, 525, 526]) {
    const answer = text(status, "from the tunnel");
    const { response, calls } = await run(get("/v1/aggregate?q=gold"), ENV, [answer]);
    assert.equal(response, answer, `status ${status}`);
    assert.equal(calls.length, 1, `status ${status}`);
  }
});

test("the API's own problem+json 503 passes through and Azure is not asked", async () => {
  const answer = new Response(PROBLEM, {
    status: 503,
    headers: { "content-type": "application/problem+json", "retry-after": "30" },
  });
  const { response, calls } = await run(get("/v1/aggregate?q=gold"), ENV, [answer]);
  assert.equal(calls.length, 1);
  assert.equal(response.status, 503);
  assert.equal(response.headers.get("content-type"), "application/problem+json");
  assert.equal(response.headers.get("retry-after"), "30");
  assert.equal(await response.text(), PROBLEM);
});

test("a 502 is retried once at the Azure name, same path and query", async () => {
  const fromAzure = new Response("<html>azure", { status: 200, headers: { "content-type": "text/html" } });
  const { response, calls } = await run(get("/v1/aggregate?q=%22gold%20rush%22&from=1900-01-01"), ENV, [
    text(502, "bad gateway"),
    fromAzure,
  ]);
  assert.equal(response, fromAzure);
  assert.equal(calls.length, 2);
  assert.equal(calls[0].url, `${SITE}/v1/aggregate?q=%22gold%20rush%22&from=1900-01-01`);
  assert.equal(calls[1].url, `${AZURE}/v1/aggregate?q=%22gold%20rush%22&from=1900-01-01`);
  assert.equal(calls[1].method, "GET");
});

test("a 530 with Cloudflare's 1033 page is retried", async () => {
  const fromAzure = text(200, "ok");
  const { response, calls } = await run(get("/"), ENV, [text(530, "error code: 1033"), fromAzure]);
  assert.equal(response, fromAzure);
  assert.equal(calls.length, 2);
});

test("a thrown fetch is retried", async () => {
  const fromAzure = text(200, "ok");
  const { response, calls } = await run(get("/"), ENV, [networkError(), fromAzure]);
  assert.equal(response, fromAzure);
  assert.equal(calls.length, 2);
});

test("HEAD and GET are retried alike, with no body", async () => {
  for (const method of ["GET", "HEAD"]) {
    const { calls } = await run(new Request(`${SITE}/assets/index-abc123.js`, { method }), ENV, [
      text(530),
      text(200),
    ]);
    assert.equal(calls.length, 2, method);
    assert.equal(calls[1].method, method);
    assert.equal(calls[1].body, null, method);
  }
});

test("the unused tunnel answer's body is released when Azure answers", async () => {
  let cancelled = false;
  const body = new ReadableStream({
    pull(controller) {
      controller.enqueue(new TextEncoder().encode("error code: 1033"));
    },
    cancel() {
      cancelled = true;
    },
  });
  const { calls } = await run(get("/"), ENV, [new Response(body, { status: 530 }), text(200)]);
  assert.equal(calls.length, 2);
  assert.equal(cancelled, true);
});

test("an abort is not an outage: nothing is retried", async () => {
  for (const name of ["AbortError", "TimeoutError"]) {
    const error = Object.assign(new Error("gone"), { name });
    const { error: thrown, calls } = await run(get("/"), ENV, [error]);
    assert.equal(thrown, error, name);
    assert.equal(calls.length, 1, name);
  }
});

test("a request whose visitor has gone is not retried, whatever fetch threw", async () => {
  const controller = new AbortController();
  const request = new Request(`${SITE}/v1/aggregate?q=gold`, { signal: controller.signal });
  controller.abort();
  const boom = new Error("boom");
  const { error, calls } = await run(request, ENV, [boom]);
  assert.equal(error, boom);
  assert.equal(calls.length, 1);
});

test("a failure at Azure does not loop: its answer is returned after two requests", async () => {
  for (const status of [502, 530, 503, 500]) {
    const fromAzure = text(status, "azure");
    const { response, calls } = await run(get("/"), ENV, [text(502), fromAzure]);
    assert.equal(response, fromAzure, `azure ${status}`);
    assert.equal(calls.length, 2, `azure ${status}`);
  }
});

test("when Azure cannot answer either, the tunnel's own answer is returned", async () => {
  const tunnel = text(530, "error code: 1033");
  const { response, calls } = await run(get("/"), ENV, [tunnel, networkError()]);
  assert.equal(response, tunnel);
  assert.equal(await response.text(), "error code: 1033");
  assert.equal(calls.length, 2);
});

test("when the tunnel threw and Azure throws, Azure's error is thrown, after two requests", async () => {
  const first = networkError();
  const second = new TypeError("Azure is down too.");
  const { error, calls } = await run(get("/"), ENV, [first, second]);
  assert.equal(error, second);
  assert.equal(calls.length, 2);
});

// The page-view beacon (POST /v1/beacon) is the only request with a body. Its first attempt
// uses the body up, so the retry needs the copy the Worker took before it.
const BEACON = '{"route":"search","title":"Search"}';
function beacon(contentType = "application/json") {
  return new Request(`${SITE}/v1/beacon`, {
    method: "POST",
    headers: { "content-type": contentType, origin: SITE, "sec-fetch-site": "same-origin" },
    body: BEACON,
  });
}

test("a POST /v1/beacon is sent whole to Azure when the tunnel answers 502", async () => {
  const { response, calls } = await run(beacon(), ENV, [text(502), new Response(null, { status: 204 })]);
  assert.equal(response.status, 204);
  assert.equal(calls.length, 2);
  assert.equal(calls[0].method, "POST");
  assert.equal(calls[0].body, BEACON);
  assert.equal(calls[1].url, `${AZURE}/v1/beacon`);
  assert.equal(calls[1].method, "POST");
  assert.equal(calls[1].body, BEACON);
  assert.equal(calls[1].headers["content-type"], "application/json");
});

test("a POST /v1/beacon is sent whole to Azure when the tunnel answers 530 or the fetch throws", async () => {
  for (const first of [text(530, "error code: 1033"), networkError()]) {
    const { calls } = await run(beacon("text/plain;charset=UTF-8"), ENV, [first, new Response(null, { status: 204 })]);
    assert.equal(calls.length, 2);
    assert.equal(calls[0].body, BEACON);
    assert.equal(calls[1].body, BEACON);
    assert.equal(calls[1].headers["content-type"], "text/plain;charset=UTF-8");
  }
});

test("a POST /v1/beacon the tunnel took is sent once, not twice", async () => {
  for (const status of [204, 400, 413, 503, 504]) {
    const { calls } = await run(beacon(), ENV, [new Response(null, { status })]);
    assert.equal(calls.length, 1, `status ${status}`);
    assert.equal(calls[0].body, BEACON);
  }
});

test("a POST the visitor aborted is not replayed", async () => {
  const error = Object.assign(new Error("gone"), { name: "AbortError" });
  const { error: thrown, calls } = await run(beacon(), ENV, [error]);
  assert.equal(thrown, error);
  assert.equal(calls.length, 1);
});

test("Sec-Fetch-Site and Origin reach Azure as the browser sent them", async () => {
  const request = get("/v1/aggregate?q=gold", {
    origin: SITE,
    "sec-fetch-site": "same-origin",
    "sec-fetch-mode": "cors",
  });
  const { calls } = await run(request, ENV, [text(502), text(200)]);
  assert.equal(calls[1].headers["sec-fetch-site"], "same-origin");
  assert.equal(calls[1].headers.origin, SITE);
  assert.equal(calls[1].headers["sec-fetch-mode"], "cors");
});

test("a request without them is not given them", async () => {
  const { calls } = await run(get("/v1/aggregate?q=gold"), ENV, [text(502), text(200)]);
  assert.equal("sec-fetch-site" in calls[1].headers, false);
  assert.equal("origin" in calls[1].headers, false);
});

test("the other headers and the client IP headers go to Azure as received, and the Worker adds none", async () => {
  const sent = {
    accept: "application/json",
    "accept-language": "en-US",
    "cf-connecting-ip": "203.0.113.7",
    "if-none-match": 'W/"abc"',
    "user-agent": "Mozilla/5.0 (test)",
    "x-forwarded-for": "203.0.113.7",
    "dnt": "0",
  };
  const { calls } = await run(get("/v1/meta", sent), ENV, [text(502), text(200)]);
  assert.deepEqual(calls[1].headers, sent);
  // Not even the tunnel request is changed.
  assert.deepEqual(calls[0].headers, sent);
});

test("the Host header is left to the runtime, so Azure is reached by its own name", async () => {
  const { calls } = await run(get("/", { host: "usnewsmap.com" }), ENV, [text(502), text(200)]);
  assert.equal("host" in calls[1].headers, false);
});

test("a redirect from Azure goes to the visitor, not followed by the Worker", async () => {
  const { calls } = await run(get("/api/v1/meta"), ENV, [text(502), text(200)]);
  assert.equal(calls[1].redirect, "manual");
});

test("the drill Worker sends DNT: 1 to both origins", async () => {
  const drill = { ...ENV, ADD_DNT: "true" };
  for (const visitor of [{}, { dnt: "0" }, { dnt: "1" }]) {
    const { calls } = await run(get("/v1/aggregate?q=gold", visitor), drill, [text(502), text(200)]);
    assert.equal(calls.length, 2);
    assert.equal(calls[0].headers.dnt, "1");
    assert.equal(calls[1].headers.dnt, "1");
  }
});

test("the drill Worker keeps a POST body whole in both requests", async () => {
  const { calls } = await run(beacon(), { ...ENV, ADD_DNT: "1" }, [text(530), new Response(null, { status: 204 })]);
  assert.equal(calls.length, 2);
  assert.equal(calls[0].body, BEACON);
  assert.equal(calls[1].body, BEACON);
  assert.equal(calls[0].headers.dnt, "1");
  assert.equal(calls[1].headers.dnt, "1");
});

test("the production Worker adds no DNT, whatever ADD_DNT holds besides true or 1", async () => {
  for (const env of [ENV, { ...ENV, ADD_DNT: "" }, { ...ENV, ADD_DNT: "false" }, { ...ENV, ADD_DNT: "0" }]) {
    const { calls } = await run(get("/v1/aggregate?q=gold"), env, [text(502), text(200)]);
    assert.equal("dnt" in calls[0].headers, false);
    assert.equal("dnt" in calls[1].headers, false);
  }
});

test("a missing or unsuitable Azure name turns the fallback off: the tunnel's answer stands", async () => {
  const unsuitable = [
    undefined,
    "",
    "usnewsmap.com",
    "http://ca-usnm-prod.example-env.eastus2.azurecontainerapps.io",
    "https://ca-usnm-prod.example-env.eastus2.azurecontainerapps.io:8443",
    "https://ca-usnm-prod.example-env.eastus2.azurecontainerapps.io/v1",
    "https://ca-usnm-prod.example-env.eastus2.azurecontainerapps.io/?x=1",
    "https://user:pass@ca-usnm-prod.example-env.eastus2.azurecontainerapps.io",
    "https://azurecontainerapps.io",
    "https://example.com",
    "https://usnewsmap.com",
    "https://ca-usnm-prod.example-env.eastus2.azurecontainerapps.io.example.com",
    "https://evilazurecontainerapps.io",
  ];
  for (const origin of unsuitable) {
    const env = origin === undefined ? {} : { AZURE_FALLBACK_ORIGIN: origin };
    const tunnel = text(530, "error code: 1033");
    const { response, calls } = await run(get("/"), env, [tunnel]);
    assert.equal(response, tunnel, String(origin));
    assert.equal(calls.length, 1, String(origin));
    const thrown = networkError();
    const { error, calls: threw } = await run(get("/"), env, [thrown]);
    assert.equal(error, thrown, String(origin));
    assert.equal(threw.length, 1, String(origin));
  }
});

test("a path that starts with two slashes stays on the Azure name", async () => {
  const { calls } = await run(get("//evil.example/steal?q=gold"), ENV, [text(502), text(200)]);
  assert.equal(calls.length, 2);
  const target = new URL(calls[1].url);
  assert.equal(target.host, "ca-usnm-prod.example-env.eastus2.azurecontainerapps.io");
  assert.equal(target.pathname, "//evil.example/steal");
  assert.equal(target.search, "?q=gold");
});

test("the Worker logs nothing", async () => {
  const methods = ["log", "info", "warn", "error", "debug", "trace", "dir"];
  const real = Object.fromEntries(methods.map((name) => [name, console[name]]));
  const seen = [];
  for (const name of methods) console[name] = (...args) => seen.push([name, ...args]);
  try {
    const drill = { ...ENV, ADD_DNT: "true" };
    await run(get("/v1/aggregate?q=gold"), ENV, [text(200)]);
    await run(get("/v1/aggregate?q=gold"), ENV, [text(502), text(200)]);
    await run(get("/v1/aggregate?q=gold"), ENV, [networkError(), networkError()]);
    await run(get("/v1/aggregate?q=gold"), drill, [text(530), text(502)]);
    await run(get("/v1/aggregate?q=gold"), {}, [text(530)]);
    await run(beacon(), ENV, [text(502), text(204)]);
  } finally {
    Object.assign(console, real);
  }
  assert.deepEqual(seen, []);
});

test("the source has no console call, waitUntil or passThroughOnException", () => {
  const source = readFileSync(new URL("./worker.mjs", import.meta.url), "utf8")
    .split("\n")
    .filter((line) => !line.trimStart().startsWith("//"))
    .join("\n");
  assert.doesNotMatch(source, /console/);
  assert.doesNotMatch(source, /waitUntil|passThroughOnException/);
});

test("fallback accounting records only Worker-produced answers and no visitor data", async () => {
  const points = [];
  const env = { ...ENV, FALLBACK_METRICS: { writeDataPoint: (point) => points.push(point) } };
  await run(get("/?q=private-marker", { "cf-connecting-ip": "203.0.113.7" }), env, [text(200)]);
  assert.equal(points.length, 0);
  await run(get("/v1/hits?q=private-marker"), env, [text(530), text(200)]);
  await run(get("/v1/hits?q=private-marker"), { ...env, ADD_DNT: "true" }, [text(502), text(503)]);
  await run(get("/v1/hits?q=private-marker"), env, [text(502), networkError()]);
  assert.deepEqual(points, [
    { blobs: ["azure", "production"], doubles: [1, 200], indexes: ["production"] },
    { blobs: ["azure", "drill"], doubles: [1, 503], indexes: ["drill"] },
  ]);
});

test("missing or failed accounting is visible without losing Azure's actual answer", async () => {
  for (const [binding, expected] of [
    [undefined, "unavailable"],
    [{ writeDataPoint() { throw new Error("storage"); } }, "write-failed"],
  ]) {
    const { response } = await run(get("/"), { ...ENV, FALLBACK_METRICS: binding }, [text(530), text(200, "azure")]);
    assert.equal(response.status, 200);
    assert.equal(await response.text(), "azure");
    assert.equal(response.headers.get("x-usnm-fallback-metric"), expected);
  }
});

test("the explicit error layer never asks Azure and gives APIs/assets a non-cacheable problem", async () => {
  for (const path of ["/v1/hits?q=private-marker", "/api/v1", "/api/v1/meta", "/assets/cold.js"]) {
    for (const first of [text(502), text(530), networkError()]) {
      const { response, calls } = await run(get(path), { ...ENV, ERROR_LAYER: "true" }, [first]);
      assert.equal(calls.length, 1);
      assert.equal(response.status, 503);
      assert.equal(response.headers.get("content-type"), "application/problem+json");
      assert.equal(response.headers.get("retry-after"), "30");
      assert.equal(response.headers.get("cache-control"), "no-store");
      assert.doesNotMatch(await response.text(), /private-marker/);
    }
  }
  const own = new Response(PROBLEM, { status: 503 });
  const result = await run(get("/v1/hits"), { ...ENV, ERROR_LAYER: "true" }, [own]);
  assert.equal(result.response, own);
  const timeout = Object.assign(new Error("timeout"), { name: "TimeoutError" });
  assert.equal((await run(get("/"), { ...ENV, ERROR_LAYER: "true" }, [timeout])).error, timeout);
});

test("only an anonymous query-free successful home shell is cached, never search results", async () => {
  const original = globalThis.caches;
  const stored = new Map();
  globalThis.caches = { default: {
    async put(key, answer) { stored.set(key.url, answer); },
    async match(key) { return stored.get(key.url)?.clone(); },
  } };
  try {
    const env = { ...ENV, ERROR_LAYER: "true" };
    const shell = () => new Response("<html><body>Home</body></html>", {
      headers: { "content-type": "text/html", etag: '"original"' },
    });
    for (const request of [
      get("/?q=private-marker"),
      get("/search/whigs", { accept: "text/html" }),
      get("/", { cookie: "private-marker" }),
      get("/", { authorization: "private-marker" }),
    ]) await run(request, env, [shell()]);
    for (const headers of [{ "set-cookie": "private-marker" }, { "cache-control": "private" }, { "cache-control": "no-store" }]) {
      const answer = shell();
      for (const [name, value] of Object.entries(headers)) answer.headers.set(name, value);
      await run(get("/"), env, [answer]);
    }
    assert.equal(stored.size, 0);
    const cold = await run(get("/", { accept: "text/html" }), env, [text(530)]);
    assert.equal(cold.response.status, 503);
    assert.equal(cold.response.headers.get("content-type"), "application/problem+json");
    await run(get("/"), env, [shell()]);
    assert.equal(stored.size, 1);
    assert.equal([...stored.keys()][0], `${SITE}/__usnm_fallback/shell-v1`);
    const request = new Request(`${SITE}/search/whigs?q=private-marker`, {
      method: "HEAD", headers: { accept: "text/html" },
    });
    const { response, calls } = await run(request, env, [text(530)]);
    assert.equal(calls.length, 1);
    assert.equal(response.status, 503);
    assert.equal(response.headers.get("x-usnm-error-layer"), "cached-shell");
    assert.equal(response.headers.get("cache-control"), "no-store");
    assert.equal(response.headers.has("etag"), false);
    assert.equal(await response.text(), "");
  } finally {
    if (original === undefined) delete globalThis.caches;
    else globalThis.caches = original;
  }
});

test("cache failures are explicit and a healthy shell response is not lost", async () => {
  const original = globalThis.caches;
  globalThis.caches = { default: {
    async put() { throw new Error("cache"); },
    async match() { throw new Error("cache"); },
  } };
  try {
    const env = { ...ENV, ERROR_LAYER: "true" };
    const warm = await run(get("/"), env, [
      new Response("Home", { headers: { "content-type": "text/html" } }),
    ]);
    assert.equal(await warm.response.text(), "Home");
    assert.equal(warm.response.headers.get("x-usnm-error-layer"), "cache-write-failed");
    const cold = await run(get("/", { accept: "text/html" }), env, [text(530)]);
    assert.equal(cold.response.status, 503);
    assert.equal(cold.response.headers.get("x-usnm-error-layer"), "cache-read-failed");
  } finally {
    if (original === undefined) delete globalThis.caches;
    else globalThis.caches = original;
  }
});
