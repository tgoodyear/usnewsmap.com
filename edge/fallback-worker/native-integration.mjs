import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { createServer } from "node:http";
import { setTimeout as delay } from "node:timers/promises";
import { delimiter, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

// Keep Wrangler's local runtime/cache artifacts inside the owned, ignored edge path.
process.chdir(fileURLToPath(new URL(".", import.meta.url)));

let resolved;
for (const path of process.env.PATH.split(delimiter)) {
  if (!path.includes("/_npx/") || !path.endsWith("/node_modules/.bin")) continue;
  try {
    resolved = createRequire(join(path, "../package.json")).resolve("miniflare");
    break;
  } catch (error) {
    if (error.code !== "MODULE_NOT_FOUND") throw error;
  }
}
assert.ok(resolved, "Run with npx --package=wrangler@4.154.0 --call 'node edge/fallback-worker/native-integration.mjs'");
const { Miniflare, convertV4MiniflareOptions, Response: NativeResponse } = await import(pathToFileURL(resolved));
const { MockAgent, fetch: mockFetch } = createRequire(resolved)("undici");
const mock = new MockAgent();
mock.disableNetConnect();
const origin = mock.get("https://usnewsmap.com");
const options = convertV4MiniflareOptions({
  modules: true,
  scriptPath: fileURLToPath(new URL("./worker.mjs", import.meta.url)),
  compatibilityDate: "2025-10-01",
  bindings: { ERROR_LAYER: "true", ADD_DNT: "true", ALLOWED_ORIGINS: "https://usnewsmap.com" },
  outboundService: async (request) => {
    const response = await mockFetch(request.url, { dispatcher: mock });
    return new NativeResponse(await response.arrayBuffer(), {
      status: response.status, headers: response.headers,
    });
  },
  port: 0,
  host: "127.0.0.1",
});
options.enableDevRegistry = false;
const instance = new Miniflare(options);
try {
  const shell = "<!doctype html><html><body><main>Anonymous home shell</main></body></html>";
  origin.intercept({ path: "/", method: "GET" }).reply(200, shell, {
    headers: {
      "content-type": "text/html",
      "content-length": String(Buffer.byteLength(shell)),
      etag: '"original"',
      "last-modified": "Wed, 01 Oct 2025 00:00:00 GMT",
    },
  });
  const healthy = await instance.dispatchFetch("https://usnewsmap.com/");
  assert.equal(healthy.status, 200);
  assert.equal(await healthy.text(), shell);
  assert.equal(healthy.headers.get("x-usnm-error-layer"), null);

  origin.intercept({ path: "/search/whigs?q=private-marker", method: "GET" }).reply(530, "1033");
  const degraded = await instance.dispatchFetch("https://usnewsmap.com/search/whigs?q=private-marker", {
    headers: { accept: "text/html" },
  });
  const html = await degraded.text();
  assert.equal(degraded.status, 503);
  assert.equal(degraded.headers.get("x-usnm-error-layer"), "cached-shell");
  assert.equal(degraded.headers.get("retry-after"), "30");
  assert.equal(degraded.headers.get("cache-control"), "no-store");
  assert.equal(degraded.headers.get("etag"), null);
  assert.equal(degraded.headers.get("last-modified"), null);
  assert.equal(degraded.headers.get("content-length"), null);
  assert.match(html, /<body><div role="status">Search is temporarily unavailable/);
  assert.match(html, /Anonymous home shell/);
  assert.doesNotMatch(html, /private-marker/);

  origin.intercept({ path: "/v1/hits?q=private-marker", method: "GET" }).reply(502, "bad gateway");
  const api = await instance.dispatchFetch("https://usnewsmap.com/v1/hits?q=private-marker", {
    headers: { origin: "https://usnewsmap.com" },
  });
  assert.equal(api.status, 503);
  assert.equal(api.headers.get("content-type"), "application/problem+json");
  assert.equal(api.headers.get("access-control-allow-origin"), "https://usnewsmap.com");
  assert.doesNotMatch(await api.text(), /private-marker/);
  mock.assertNoPendingInterceptors();
  console.log("Native Cache API/HTMLRewriter GET/banner/503/cache-policy/validator/CORS integration passed");
} finally {
  await instance.dispose();
  await mock.close();
}

// No outbound adapter here: real workerd TCP fetches to an owned loopback server.
// A socket close proves the Request.signal cancels I/O, not just the Promise race.
let healthMode = "headers";
let closed = false;
const calls = [];
const server = createServer((request, answer) => {
  calls.push({ url: request.url, method: request.method, headers: request.headers });
  if (request.url === "/readyz") {
    if (healthMode === "ready") return answer.end("ready");
    request.on("close", () => { closed = true; });
    if (healthMode === "body") {
      answer.writeHead(200);
      answer.write("rea");
    }
    return;
  }
  answer.writeHead(200, { "content-type": "text/plain" });
  answer.write("streamed ");
  answer.end("primary");
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const local = `http://127.0.0.1:${server.address().port}`;
const healthOptions = convertV4MiniflareOptions({
  modules: true,
  scriptPath: fileURLToPath(new URL("./worker.mjs", import.meta.url)),
  compatibilityDate: "2025-10-01",
  bindings: { HEALTH_FAILOVER: "true", AZURE_FALLBACK_ORIGIN: "", ALLOWED_ORIGINS: "https://usnewsmap.com" },
  port: 0,
  host: "127.0.0.1",
});
healthOptions.enableDevRegistry = false;
const healthInstance = new Miniflare(healthOptions);
try {
  for (const mode of ["headers", "body"]) {
    healthMode = mode;
    closed = false;
    const start = Date.now();
    const answer = await healthInstance.dispatchFetch(`${local}/v1/hits?q=not-for-probe`, {
      headers: { cookie: "not-for-probe", origin: "https://usnewsmap.com" },
    });
    assert.equal(answer.status, 503);
    assert.equal(answer.headers.get("x-usnm-origin"), "unavailable");
    assert.equal(answer.headers.get("x-usnm-failover-reason"), "azure-config-invalid");
    assert.equal(answer.headers.get("retry-after"), "10");
    assert.ok(Date.now() - start >= 2900 && Date.now() - start < 6000, "real health deadline must be 3s");
    await answer.text();
    for (let n = 0; n < 20 && !closed; n++) await delay(25);
    assert.equal(closed, true, `workerd must abort the ${mode} health socket`);
    assert.equal(calls.at(-1).url, "/readyz");
    assert.equal(calls.at(-1).headers.dnt, "1");
    assert.equal(calls.at(-1).headers.cookie, undefined);
    assert.equal(calls.at(-1).headers.origin, undefined);
  }
  healthMode = "ready";
  const answer = await healthInstance.dispatchFetch(`${local}/v1/hits?q=original`, {
    headers: { origin: "https://usnewsmap.com", "sec-fetch-site": "same-origin" },
  });
  assert.equal(answer.status, 200);
  assert.equal(answer.headers.get("x-usnm-origin"), "onprem");
  assert.equal(await answer.text(), "streamed primary");
  assert.equal(calls.at(-1).headers.origin, "https://usnewsmap.com");
  assert.equal(calls.at(-1).headers["sec-fetch-site"], "same-origin");
  assert.equal(calls.length, 4);
  console.log("Native health headers/body 3s deadlines cancel real TCP sockets; streamed primary/header isolation passed");
} finally {
  await healthInstance.dispose();
  server.closeAllConnections();
  await new Promise((resolve) => server.close(resolve));
}

const customCalls = [];
const customOptions = convertV4MiniflareOptions({
  modules: true,
  scriptPath: fileURLToPath(new URL("./worker.mjs", import.meta.url)),
  compatibilityDate: "2025-10-01",
  bindings: {
    HEALTH_FAILOVER: "true", HEALTH_HOSTNAME: "usnewsmap.com",
    HEALTHCHECK_HOSTNAME: "healthcheck.usnewsmap.com", METRICS_MODE: "native",
  },
  outboundService: async (request) => {
    customCalls.push(request.url);
    assert.equal(request.url, "https://usnewsmap.com/readyz");
    assert.equal(request.headers.get("dnt"), "1");
    assert.equal(request.headers.get("cookie"), null);
    return new NativeResponse("ready");
  },
  port: 0,
  host: "127.0.0.1",
});
customOptions.enableDevRegistry = false;
const customInstance = new Miniflare(customOptions);
try {
  const answer = await customInstance.dispatchFetch("https://healthcheck.usnewsmap.com/?q=not-forwarded", {
    headers: { cookie: "not-forwarded" },
  });
  assert.deepEqual(await answer.json(), { available: true, origin: "onprem", primary_ready: true });
  assert.equal(answer.headers.get("x-usnm-fallback-metric"), "native-aggregate");
  const readiness = await customInstance.dispatchFetch("https://healthcheck.usnewsmap.com/readyz");
  assert.equal(await readiness.text(), "ready");
  const rejected = await customInstance.dispatchFetch("https://healthcheck.usnewsmap.com/v1/hits");
  assert.equal(rejected.status, 404);
  assert.equal(customCalls.length, 2);
  console.log("Native custom health hostname routes only anonymous readiness probes to apex, never itself");
} finally {
  await customInstance.dispose();
}
