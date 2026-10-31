import assert from "node:assert/strict";
import { setImmediate } from "node:timers/promises";
import { test } from "node:test";
import worker from "./worker.mjs";

const SITE = "https://usnewsmap.com";
const AZURE = "https://ca-usnm-prod.example-env.eastus2.azurecontainerapps.io";
const ENV = { HEALTH_FAILOVER: "true", AZURE_FALLBACK_ORIGIN: AZURE, ALLOWED_ORIGINS: SITE };
const response = (status = 200, body = "ready") => new Response(body, { status });
const get = (path = "/v1/hits?q=private", init = {}) => new Request(`${SITE}${path}`, init);
const network = () => new TypeError("network");
const stall = ({ signal }) => new Promise((resolve, reject) => {
  signal.addEventListener("abort", () => reject(signal.reason), { once: true });
});

async function run(t, request, script, env = ENV, scheduled = false) {
  t.mock.timers.enable({ apis: ["Date", "setTimeout"], now: 0 });
  const original = globalThis.fetch;
  const calls = [];
  const metrics = [];
  globalThis.fetch = async (input, init) => {
    const sent = new Request(input);
    const call = {
      url: sent.url, method: sent.method, headers: Object.fromEntries(sent.headers),
      body: sent.body ? await sent.text() : null, redirect: sent.redirect,
      signal: sent.signal, cf: init?.cf, at: Date.now(),
    };
    calls.push(call);
    const step = script[calls.length - 1];
    if (step === undefined) throw new Error("Unexpected subrequest");
    if (step instanceof Error) throw step;
    return typeof step === "function" ? step(call) : step;
  };
  try {
    let pending;
    const binding = { ...env, FALLBACK_METRICS: { writeDataPoint: (point) => metrics.push(point) } };
    if (scheduled) {
      worker.scheduled({}, binding, { waitUntil: (promise) => { pending = promise; } });
      pending ??= Promise.resolve();
    } else {
      pending = worker.fetch(request, binding);
    }
    let finished = false;
    let answer;
    let error;
    pending.then((value) => { answer = value; finished = true; }, (value) => { error = value; finished = true; });
    for (let n = 0; n < 260 && !finished; n++) {
      await setImmediate();
      if (!finished) t.mock.timers.tick(1000);
    }
    assert.equal(finished, true, "Worker request exceeded bound");
    return { answer, error, calls, metrics, elapsed: Date.now() };
  } finally {
    globalThis.fetch = original;
    t.mock.timers.reset();
  }
}

test("healthy primary preflight is query/body/header-free, bypasses cache and preserves streamed answer", async (t) => {
  const stream = new ReadableStream({ start(c) { c.enqueue(new TextEncoder().encode("stream")); c.close(); } });
  const result = await run(t, get("/v1/hits?q=private", {
    headers: { authorization: "private", cookie: "private", origin: SITE, "sec-fetch-site": "same-origin" },
  }), [response(), new Response(stream, { headers: { etag: '"stream"' } })]);
  assert.equal(result.calls.length, 2);
  const probe = result.calls[0];
  assert.equal(probe.url, `${SITE}/readyz`);
  assert.deepEqual(probe.headers, { "cache-control": "no-cache, no-store", dnt: "1" });
  assert.equal(probe.body, null);
  assert.equal(probe.redirect, "manual");
  assert.deepEqual(probe.cf, { cacheTtl: 0, cacheEverything: false });
  assert.equal(result.calls[1].headers.origin, SITE);
  assert.equal(result.answer.headers.get("x-usnm-origin"), "onprem");
  assert.equal(result.answer.headers.get("x-usnm-failover-reason"), "primary-ready");
  assert.equal(result.answer.headers.get("etag"), '"stream"');
  assert.equal(await result.answer.text(), "stream");
  assert.deepEqual(result.metrics, [{ blobs: ["onprem", "production"], doubles: [1, 200], indexes: ["production"] }]);
});

test("API/searcher/tunnel unready, redirects, wrong body and network select Azure before user delivery", async (t) => {
  for (const first of [response(503), response(530), response(302), response(200, "ready\n"), response(200, "not ready"), network()]) {
    const { answer, calls } = await run(t, get(), [first, response(), response(200, "azure")]);
    assert.equal(calls.length, 3);
    assert.equal(calls[1].url, `${AZURE}/readyz`);
    assert.deepEqual(calls[1].headers, calls[0].headers);
    assert.equal(calls[2].url, `${AZURE}/v1/hits?q=private`);
    assert.equal(answer.headers.get("x-usnm-origin"), "azure");
    assert.equal(answer.headers.get("x-usnm-failover-reason"), "primary-unhealthy");
  }
});

test("stalled primary headers and readiness body abort at 3s then select Azure", async (t) => {
  for (const first of [stall, ({ signal }) => new Response(new ReadableStream({
    start(c) { signal.addEventListener("abort", () => c.error(signal.reason), { once: true }); },
  }))]) {
    const { answer, calls } = await run(t, get(), [first, response(), response(200, "azure")]);
    assert.equal(calls[1].at, 3000);
    assert.equal(calls[0].signal.aborted, true);
    assert.equal(answer.headers.get("x-usnm-origin"), "azure");
  }
});

test("GET/HEAD 5xx/network/timeouts fail over; 4xx and redirects remain onprem", async (t) => {
  for (const method of ["GET", "HEAD"]) {
    for (const failed of [response(503), response(524), network(), stall, Object.assign(network(), { name: "TimeoutError" })]) {
      const { answer, calls } = await run(t, get("/", { method }), [response(), failed, response(), response(200, "azure")]);
      assert.equal(calls.length, 4);
      assert.equal(calls[3].method, method);
      assert.equal(answer.headers.get("x-usnm-origin"), "azure");
    }
  }
  for (const status of [301, 400, 401, 404, 429]) {
    const { answer, calls } = await run(t, get(), [response(), response(status, "unchanged")]);
    assert.equal(calls.length, 2);
    assert.equal(answer.status, status);
    assert.equal(await answer.text(), "unchanged");
  }
});

test("mutations preflight once then never replay 5xx, network or ambiguous timeout", async (t) => {
  for (const method of ["POST", "PUT", "PATCH", "DELETE", "OPTIONS"]) {
    for (const failed of [response(503), response(502), network(), stall]) {
      const request = get("/v1/beacon", {
        method, body: "original", headers: { origin: SITE, "sec-fetch-site": "same-origin" },
      });
      const { calls, answer } = await run(t, request, [response(), failed]);
      assert.equal(calls.length, 2);
      assert.equal(calls[0].body, null);
      assert.equal(calls[1].body, "original");
      assert.equal(answer.status, failed instanceof Response ? failed.status : 503);
    }
  }
});

test("Azure cold polling holds original POST until exact readiness and preserves headers/body", async (t) => {
  const request = get("/v1/beacon?x=original", {
    method: "POST", body: "original",
    headers: { origin: SITE, "sec-fetch-site": "same-origin", "content-type": "text/plain" },
  });
  const { answer, calls } = await run(t, request, [
    response(503), response(503), stall, response(), response(200, "azure"),
  ]);
  assert.equal(calls.length, 5);
  assert.equal(calls[2].signal.aborted, true);
  for (const probe of calls.slice(0, 4)) {
    assert.equal(new URL(probe.url).pathname, "/readyz");
    assert.equal(new URL(probe.url).search, "");
    assert.equal(probe.body, null);
    assert.equal(probe.method, "GET");
  }
  assert.equal(calls[2].at - calls[1].at, 3000);
  assert.equal(calls[3].at - calls[2].at, 8000);
  assert.equal(calls[4].body, "original");
  assert.equal(calls[4].headers.origin, SITE);
  assert.equal(calls[4].headers["sec-fetch-site"], "same-origin");
  assert.equal(calls[4].redirect, "manual");
  assert.equal(answer.headers.get("x-usnm-origin"), "azure");
});

test("both unready have <=48 subrequests, bounded wait, explicit no-store 503/CORS/Retry-After", async (t) => {
  for (const cold of [() => response(503), stall]) {
    const { answer, calls, elapsed } = await run(t, get("/", { headers: { origin: SITE } }),
      [response(503), ...Array.from({ length: 45 }, () => cold)]);
    assert.ok(calls.length <= 46);
    assert.ok(elapsed <= 180000);
    assert.equal(answer.status, 503);
    assert.equal(answer.headers.get("retry-after"), "10");
    assert.equal(answer.headers.get("cache-control"), "no-store");
    assert.equal(answer.headers.get("access-control-allow-origin"), SITE);
    assert.equal(answer.headers.get("x-usnm-origin"), "unavailable");
    assert.equal(answer.headers.get("x-usnm-failover-reason"), "azure-unready");
    assert.equal((await answer.json()).status, 503);
  }
});

test("invalid Azure config never probes/sends outside allowed origin", async (t) => {
  for (const origin of ["", "https://evil.test", `${AZURE}/path`, `${AZURE}:8443`, `${AZURE}.evil.test`, `https://user@${new URL(AZURE).host}`]) {
    const { answer, calls } = await run(t, get(), [response(503)], { ...ENV, AZURE_FALLBACK_ORIGIN: origin });
    assert.equal(calls.length, 1);
    assert.equal(answer.headers.get("x-usnm-failover-reason"), "azure-config-invalid");
  }
});

test("incoming api/www/dev hostname drives its own preflight; HEAD outage has no body", async (t) => {
  for (const hostname of ["api.usnewsmap.com", "www.usnewsmap.com", "dev.usnewsmap.com"]) {
    const { calls, answer } = await run(t, new Request(`https://${hostname}/assets/file?q=original`),
      [response(), response(200, "asset")]);
    assert.equal(calls[0].url, `https://${hostname}/readyz`);
    assert.equal(calls[1].url, `https://${hostname}/assets/file?q=original`);
    assert.equal(answer.headers.get("x-usnm-origin"), "onprem");
  }
  const { answer } = await run(t, get("/", { method: "HEAD" }), [response(503)],
    { ...ENV, AZURE_FALLBACK_ORIGIN: "" });
  assert.equal(answer.status, 503);
  assert.equal(answer.body, null);
});

test("configured apex health controls all production domains despite www readiness redirect", async (t) => {
  for (const hostname of ["usnewsmap.com", "www.usnewsmap.com", "api.usnewsmap.com"]) {
    const { calls, answer } = await run(t, new Request(`https://${hostname}/v1/hits?q=original`),
      [response(), response(200, "primary")], { ...ENV, HEALTH_HOSTNAME: "usnewsmap.com" });
    assert.equal(calls[0].url, `${SITE}/readyz`);
    assert.equal(calls[1].url, `https://${hostname}/v1/hits?q=original`);
    assert.equal(answer.headers.get("x-usnm-origin"), "onprem");
  }
});

test("failover drill probes its configured hostname for requests and cron", async (t) => {
  const env = { ...ENV, HEALTH_HOSTNAME: "failover-drill.usnewsmap.com", HEALTH_HEARTBEAT: "true" };
  const routed = await run(t, get(), [response(), response(200, "primary")], env);
  assert.equal(routed.calls[0].url, "https://failover-drill.usnewsmap.com/readyz");
  const heartbeat = await run(t, null, [response()], env, true);
  assert.equal(heartbeat.calls[0].url, routed.calls[0].url);
  const unhealthy = await run(t, get(), [response(301), response(), response(200, "azure")], env);
  assert.equal(unhealthy.answer.headers.get("x-usnm-origin"), "azure");
  for (const hostname of ["www.usnewsmap.com", "failover-drill.usnewsmap.com.evil.test", "usnewsmap.com/path", ""]) {
    const invalid = await run(t, get(), [], { ...env, HEALTH_HOSTNAME: hostname });
    assert.equal(invalid.calls.length, 0);
    assert.equal(invalid.answer.headers.get("x-usnm-failover-reason"), "health-config-invalid");
  }
});

test("native health metrics visibly retain origin/reason without inventing per-origin aggregates", async (t) => {
  for (const [script, origin] of [
    [[response(), response(200, "primary")], "onprem"],
    [[response(503), response(), response(200, "azure")], "azure"],
    [[response(503)], "unavailable"],
  ]) {
    const { answer, metrics } = await run(t, get(), script, {
      ...ENV, METRICS_MODE: "native", AZURE_FALLBACK_ORIGIN: origin === "unavailable" ? "" : AZURE,
    });
    assert.equal(answer.headers.get("x-usnm-fallback-metric"), "native-aggregate");
    assert.equal(answer.headers.get("x-usnm-origin"), origin);
    assert.ok(answer.headers.get("x-usnm-failover-reason"));
    assert.deepEqual(metrics, []);
  }
});
    test("explicit outcome logs contain only fixed origin/reason/status, default requests remain silent", async (t) => {
      const records = [];
      const log = console.log;
      console.log = (record) => records.push(record);
      try {
        await run(t, get("/v1/hits?q=private", {
          headers: { cookie: "private", origin: SITE, "cf-connecting-ip": "192.0.2.1" },
        }), [response(), response(200, "primary")]);
        assert.deepEqual(records, []);
        for (const [script, origin, reason, status] of [
          [[response(), response(200, "primary")], "onprem", "primary-ready", 200],
          [[response(503), response(), response(200, "azure")], "azure", "primary-unhealthy", 200],
          [[response(503)], "unavailable", "azure-config-invalid", 503],
        ]) {
          await run(t, get("/v1/hits?q=private"), script, {
            ...ENV, LOG_OUTCOMES: "true", AZURE_FALLBACK_ORIGIN: origin === "unavailable" ? "" : AZURE,
          });
          assert.deepEqual(records.at(-1), { origin, reason, status });
        }
        assert.equal(records.length, 3);
      } finally {
        console.log = log;
      }
    });
    test("custom health domain probes apex independently, never its own host or visitor data", async (t) => {
      const env = { ...ENV, HEALTH_HOSTNAME: "usnewsmap.com", HEALTHCHECK_HOSTNAME: "healthcheck.usnewsmap.com", METRICS_MODE: "native" };
      for (const path of ["/?q=private", "/readyz?q=private"]) {
        const { answer, calls } = await run(t, new Request(`https://healthcheck.usnewsmap.com${path}`, {
          headers: { cookie: "private", authorization: "private", "cf-connecting-ip": "192.0.2.1" },
        }), [response()], env);
        assert.equal(calls.length, 1);
        assert.equal(calls[0].url, `${SITE}/readyz`);
        assert.deepEqual(calls[0].headers, { "cache-control": "no-cache, no-store", dnt: "1" });
        assert.equal(answer.status, 200);
        assert.equal(answer.headers.get("cache-control"), "no-store");
        assert.equal(answer.headers.get("x-usnm-origin"), "onprem");
        assert.equal(answer.headers.get("x-usnm-fallback-metric"), "native-aggregate");
        if (path.startsWith("/readyz")) assert.equal(await answer.text(), "ready");
        else assert.deepEqual(await answer.json(), { available: true, origin: "onprem", primary_ready: true });
      }
      const healthyPublic = await run(t, get(), [response(), response(200, "original")], env);
      assert.equal(healthyPublic.calls.length, 2);
      assert.equal(healthyPublic.calls[1].url, `${SITE}/v1/hits?q=private`);
    });

    test("custom health domain wakes cold Azure only after primary failure and reports unavailable honestly", async (t) => {
      const env = { ...ENV, HEALTH_HOSTNAME: "usnewsmap.com", HEALTHCHECK_HOSTNAME: "healthcheck.usnewsmap.com" };
      const azure = await run(t, new Request("https://healthcheck.usnewsmap.com/"),
        [response(503), response(503), response()], env);
      assert.equal(azure.calls.length, 3);
      assert.ok(azure.calls.every((call) => new URL(call.url).pathname === "/readyz"));
      assert.equal(azure.calls[1].url, `${AZURE}/readyz`);
      assert.deepEqual(await azure.answer.json(), { available: true, origin: "azure", primary_ready: false });
      const unavailable = await run(t, new Request("https://healthcheck.usnewsmap.com/readyz", { method: "HEAD" }),
        [response(503)], { ...env, AZURE_FALLBACK_ORIGIN: "" });
      assert.equal(unavailable.answer.status, 503);
      assert.equal(unavailable.answer.body, null);
      assert.equal(unavailable.answer.headers.get("retry-after"), "10");
      const invalid = await run(t, new Request("https://healthcheck.usnewsmap.com/"), [],
        { ...env, HEALTH_HOSTNAME: "healthcheck.usnewsmap.com" });
      assert.equal(invalid.calls.length, 0);
      assert.equal(invalid.answer.status, 503);
    });

    test("custom health endpoint rejects other paths and mutations without forwarding", async (t) => {
      const env = { ...ENV, HEALTH_HOSTNAME: "usnewsmap.com", HEALTHCHECK_HOSTNAME: "healthcheck.usnewsmap.com" };
      for (const [path, init, status] of [
        ["/v1/hits?q=private", {}, 404], ["/readyz/", {}, 404],
        ["/", { method: "POST", body: "private" }, 405],
      ]) {
        const result = await run(t, new Request(`https://healthcheck.usnewsmap.com${path}`, init), [], env);
        assert.equal(result.calls.length, 0);
        assert.equal(result.answer.status, status);
      }
      const legacy = await run(t, new Request("https://healthcheck.usnewsmap.com/"), [response(), response(200, "legacy")], ENV);
      assert.equal(legacy.calls.length, 2);
      assert.equal(await legacy.answer.text(), "legacy");
    });
test("client aborts before preflight, during probe, original delivery or Azure wait never retry", async (t) => {
  for (const stage of ["before", "probe", "delivery", "poll"]) {
    const controller = new AbortController();
    const stop = () => { controller.abort(); throw controller.signal.reason; };
    if (stage === "before") controller.abort();
    const script = stage === "probe" ? [stop] : stage === "delivery" ? [response(), stop] :
      [response(503), () => { setTimeout(() => controller.abort(), 1000); return response(503); }];
    const { error, calls } = await run(t, get("/", { signal: controller.signal }), script);
    assert.equal(error.name, "AbortError");
    assert.equal(calls.length, stage === "before" ? 0 : stage === "probe" ? 1 : 2);
  }
  const abortError = new DOMException("Aborted", "AbortError");
  const { error, calls } = await run(t, get(), [response(), abortError]);
  assert.equal(error, abortError);
  assert.equal(calls.length, 2);
});

test("Azure delivery errors do not replay and denied origins get no CORS", async (t) => {
  for (const failed of [network(), stall]) {
    const { answer, calls } = await run(t, get("/", { headers: { origin: "https://evil.test" } }),
      [response(503), response(), failed]);
    assert.equal(calls.length, 3);
    assert.equal(answer.status, 503);
    assert.equal(answer.headers.get("access-control-allow-origin"), null);
    assert.equal(answer.headers.get("x-usnm-origin"), "unavailable");
  }
});

test("finished cron health outcomes are fixed-only and logging is explicitly opt-in", async (t) => {
  const records = [];
  const log = console.log;
  console.log = (record) => records.push(record);
  const env = { ...ENV, HEALTH_HEARTBEAT: "true", HEALTH_HOSTNAME: "usnewsmap.com" };
  try {
    await run(t, null, [response()], env, true);
    assert.deepEqual(records, []);
    for (const [script, origin, reason, status] of [
      [[response()], "onprem", "primary-ready", 200],
      [[response(503), response(503), response()], "azure", "primary-unhealthy", 200],
      [[response(503)], "unavailable", "azure-unready", 503],
    ]) {
      await run(t, null, script, {
        ...env, LOG_OUTCOMES: "true", AZURE_FALLBACK_ORIGIN: origin === "unavailable" ? "" : AZURE,
      }, true);
      assert.deepEqual(records.at(-1), { origin, reason, status, trigger: "heartbeat" });
    }
    assert.equal(records.length, 3);
  } finally {
    console.log = log;
  }
});

test("cron primary heartbeat wakes Azure only on failure, with opt-in and exact hosts", async (t) => {
  const env = { ...ENV, HEALTH_HEARTBEAT: "true", HEALTH_HOSTNAME: "dev.usnewsmap.com" };
  const healthy = await run(t, null, [response()], env, true);
  assert.equal(healthy.calls.length, 1);
  assert.equal(healthy.calls[0].url, "https://dev.usnewsmap.com/readyz");
  const cold = await run(t, null, [stall, response(503), response()], env, true);
  assert.equal(cold.calls.length, 3);
  assert.equal(cold.calls[1].url, `${AZURE}/readyz`);
  assert.equal(cold.metrics.length, 0);
  for (const disabled of [
    { ...env, HEALTH_FAILOVER: "false" }, { ...env, HEALTH_HEARTBEAT: "false" },
    { ...env, HEALTH_HOSTNAME: "evil.test" }, { ...env, ERROR_LAYER: "true" },
  ]) {
    const { calls } = await run(t, null, [], disabled, true);
    assert.equal(calls.length, 0);
  }
});
