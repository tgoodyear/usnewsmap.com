import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { delimiter, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

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
