import assert from "node:assert/strict";
import { test } from "node:test";
import { mcp } from "./mcp.mjs";
import worker from "./worker.mjs";

const SITE = "https://usnewsmap.com";
const HEADERS = { "content-type": "application/json", accept: "application/json, text/event-stream" };
const init = (version = "2025-11-25") => ({
  jsonrpc: "2.0", id: 0, method: "initialize",
  params: { protocolVersion: version, capabilities: {}, clientInfo: { name: "test", version: "1" } },
});
const call = (name, args = {}) => ({ jsonrpc: "2.0", id: "call", method: "tools/call", params: { name, arguments: args } });
const request = (message, headers = {}, site = SITE) => new Request(`${site}/mcp`, {
  method: "POST", headers: { ...HEADERS, ...headers }, body: JSON.stringify(message),
});
const json = (body, status = 200, headers = {}) => Response.json(body, { status, headers });
const noFetch = () => { throw new Error("Protocol requests must not contact the origin"); };
const aggregate = () => ({
  index_version: "fixture-v1", synthetic: true, query: { canonical: "q=whigs", ast: "whigs" },
  total: { hits: 31, papers: 25, first: { links: { viewer: "https://www.loc.gov/item/example/" } }, last: null },
  bucket: { unit: "year", from: "1840-01-01", to: "1841-12-31", count: 2 },
  series: { hits: [12, 19], baseline: [100, 100] }, baseline: null, coarsened: false,
  papers: {
    lccn: Array.from({ length: 25 }, (_, i) => `sn${i}`),
    hits: Array(25).fill(1), title: Array(25).fill("Fixture newspaper"), place_id: Array(25).fill("fixture-place"),
  },
  cube: { deliberately: "not sent to agent" },
});

test("both endpoints initialize without waking origins, negotiate versions and list read-only tools", async () => {
  for (const site of [SITE, "https://api.usnewsmap.com"]) {
    for (const version of ["2025-11-25", "2025-06-18", "2025-03-26", "unknown"]) {
      const answer = await worker.fetch(request(init(version), {}, site));
      assert.equal(answer.status, 200);
      assert.equal(answer.headers.get("cache-control"), "no-store");
      assert.equal(answer.headers.get("mcp-session-id"), null);
      const { result, id } = await answer.json();
      assert.equal(id, 0);
      assert.equal(result.protocolVersion, version === "unknown" ? "2025-11-25" : version);
      assert.deepEqual(result.capabilities, { tools: { listChanged: false } });
      assert.match(result.instructions, /untrusted source data/);
    }
  }
  const answer = await mcp(request({ jsonrpc: "2.0", id: 1, method: "tools/list" }), {}, noFetch);
  const { result } = await answer.json();
  assert.deepEqual(result.tools.map((tool) => tool.name), ["search", "search_pages", "get_corpus_info"]);
  for (const tool of result.tools) {
    assert.equal(tool.annotations.readOnlyHint, true);
    assert.equal(tool.inputSchema.additionalProperties, false);
  }
});

test("Origin validation precedes every method; non-browser agents need no Origin", async () => {
  for (const method of ["POST", "GET", "OPTIONS", "DELETE"]) {
    for (const origin of ["null", "https://evil.example", "https://usnewsmap.com.evil.example"]) {
      const answer = await mcp(new Request(`${SITE}/mcp`, { method, headers: { origin } }), {}, noFetch);
      assert.equal(answer.status, 403);
      assert.equal(answer.headers.get("access-control-allow-origin"), null);
    }
  }
  for (const origin of [SITE, "https://api.usnewsmap.com", "https://www.usnewsmap.com"]) {
    const answer = await mcp(request(init(), { origin }), { ALLOWED_ORIGINS: "https://www.usnewsmap.com" }, noFetch);
    assert.equal(answer.status, 200);
    assert.equal(answer.headers.get("access-control-allow-origin"), origin);
    assert.equal(answer.headers.get("vary"), "Origin");
  }
});

test("CORS preflight and unsupported SSE/session methods are explicit", async () => {
  const options = await mcp(new Request(`${SITE}/mcp`, { method: "OPTIONS", headers: { origin: SITE } }), {}, noFetch);
  assert.equal(options.status, 204);
  assert.match(options.headers.get("access-control-allow-headers"), /MCP-Protocol-Version/);
  for (const method of ["GET", "DELETE", "PUT"]) {
    const answer = await mcp(new Request(`${SITE}/mcp`, { method }), {}, noFetch);
    assert.equal(answer.status, 405);
    assert.equal(answer.headers.get("allow"), "POST, OPTIONS");
  }
});

test("content negotiation, protocol headers, JSON and message envelopes are checked", async () => {
  for (const [headers, status] of [
    [{ accept: "application/json" }, 406],
    [{ accept: "application/json, text/event-stream;q=0" }, 406],
    [{ "content-type": "text/plain" }, 415],
    [{ "mcp-protocol-version": "wrong" }, 400],
  ]) {
    assert.equal((await mcp(request(init(), headers), {}, noFetch)).status, status);
  }
  for (const message of [[], null, {}, { ...init(), id: null }, { ...init(), params: [] }]) {
    const answer = await mcp(request(message), {}, noFetch);
    assert.equal(answer.status, 400);
    assert.equal((await answer.json()).error.code, -32600);
  }
  const malformed = new Request(`${SITE}/mcp`, { method: "POST", headers: HEADERS, body: "{" });
  assert.equal((await (await mcp(malformed, {}, noFetch)).json()).error.code, -32700);
  const invalidInit = await mcp(request({ ...init(), params: {} }), {}, noFetch);
  assert.equal((await invalidInit.json()).error.code, -32602);
});

test("request size is bounded for declared and chunked bodies", async () => {
  assert.equal((await mcp(request(init(), { "content-length": "32769" }), {}, noFetch)).status, 413);
  let cancelled = false;
  const stream = new ReadableStream({
    start(c) { c.enqueue(new Uint8Array(32769)); },
    cancel() { cancelled = true; },
  });
  const answer = await mcp(new Request(`${SITE}/mcp`, {
    method: "POST", headers: HEADERS, body: stream, duplex: "half",
  }), {}, noFetch);
  assert.equal(answer.status, 413);
  assert.equal(cancelled, true);
});

test("notifications are empty 202, tools cannot run as notifications, and unknown methods are errors", async () => {
  const answer = await mcp(request({ jsonrpc: "2.0", method: "notifications/initialized" }), {}, noFetch);
  assert.equal(answer.status, 202);
  assert.equal(await answer.text(), "");
  const notification = call("get_corpus_info");
  delete notification.id;
  assert.equal((await mcp(request(notification), {}, noFetch)).status, 400);
  for (const [message, code] of [
    [{ jsonrpc: "2.0", id: 1, method: "nonexistent" }, -32601],
    [call("nonexistent"), -32602],
    [{ jsonrpc: "2.0", id: 1, method: "tools/list", params: { cursor: "bad" } }, -32602],
  ]) {
    assert.equal((await (await mcp(request(message), {}, noFetch)).json()).error.code, code);
  }
  assert.deepEqual((await (await mcp(request({ jsonrpc: "2.0", id: 1, method: "ping" }), {}, noFetch)).json()).result, {});
});

test("schema validation rejects invalid tool arguments without calling the API", async () => {
  for (const [name, args] of [
    ["search", {}], ["search", { q: "" }], ["search", { q: "a".repeat(257) }],
    ["search", { q: "whigs", mode: "semantic" }], ["search", { q: "whigs", mode: "near" }],
    ["search", { q: "whigs", fuzzy: 3 }], ["search", { q: "whigs", from: "1840" }],
    ["search", { q: "whigs", front: "false" }], ["search", { q: "whigs", url: "https://evil.example" }],
    ["search_pages", { q: "whigs" }], ["search_pages", { q: "whigs", state: "GA,SC" }],
    ["search_pages", { q: "whigs", state: "GA", limit: 0 }],
    ["search_pages", { q: "whigs", state: "GA", limit: 1.5 }],
    ["search_pages", { q: "whigs", state: "GA", limit: 51 }],
    ["get_corpus_info", { anything: true }], ["get_corpus_info", null],
  ]) {
    const { result } = await (await mcp(request(call(name, args)), {}, noFetch)).json();
    assert.equal(result.isError, true, JSON.stringify({ name, args }));
  }
});

test("global search retains counts, timeline, citations and provenance without the map cube", async () => {
  let sent;
  const answer = await mcp(request(call("search", { q: "whigs & suffrage", from: "1840-01-01" }), {
    "cf-connecting-ip": "192.0.2.1", "x-forwarded-for": "192.0.2.1", origin: SITE,
    authorization: "private", cookie: "private", "sec-fetch-site": "same-origin",
  }), {}, async (value) => { sent = value; return json(aggregate()); });
  const url = new URL(sent.url);
  assert.equal(url.origin, SITE);
  assert.equal(url.pathname, "/v1/aggregate");
  assert.equal(url.searchParams.get("q"), "whigs & suffrage");
  assert.equal(url.searchParams.get("bucket"), "year");
  assert.equal(sent.method, "GET");
  assert.equal(sent.redirect, "manual");
  assert.deepEqual(Object.fromEntries(sent.headers), {
    accept: "application/json", "cf-connecting-ip": "192.0.2.1", dnt: "1", "x-forwarded-for": "192.0.2.1",
  });
  const { result } = await answer.json();
  const data = result.structuredContent;
  assert.equal(result.isError, false);
  assert.deepEqual(JSON.parse(result.content[0].text), data);
  assert.deepEqual(data.total, aggregate().total);
  assert.deepEqual(data.series, aggregate().series);
  assert.equal(data.cube, undefined);
  assert.equal(data.top_newspapers.length, 20);
  assert.equal(data.newspapers_omitted, 5);
  assert.equal(data.top_newspapers[0].lccn, "sn0");
});

test("page searches forward pagination and preserve snippets, source links and OCR credit", async () => {
  const pages = {
    index_version: "fixture-v1", synthetic: true, total: 3, next_cursor: "next",
    items: [{ doc_id: "fixture", snippets: ["<mark>whigs</mark>"], ocr: { source: "fixture", engine: "fixture" },
      matched_in: ["loc"], links: { viewer: "https://www.loc.gov/item/example/" } }],
  };
  for (const scope of [{ state: "GA" }, { lccn: "sn123" }, { place: "fixture-place" }]) {
    const answer = await mcp(request(call("search_pages", {
      q: "whigs", ...scope, cursor: "opaque/+=", v: "fixture-v1",
    })), {}, async (sent) => {
      const url = new URL(sent.url);
      assert.equal(url.pathname, "/v1/hits");
      assert.equal(url.searchParams.get("cursor"), "opaque/+=");
      assert.equal(url.searchParams.get("v"), "fixture-v1");
      assert.equal(url.searchParams.get("limit"), "10");
      assert.equal(url.searchParams.get("sort"), "relevant");
      return json(pages);
    });
    assert.deepEqual((await answer.json()).result.structuredContent, pages);
  }
});

test("pending computations are not empty results, rate limits and failures are tool errors", async () => {
  for (const status of ["computing", "queued"]) {
    const answer = await mcp(request(call("search", { q: "whigs" })), {}, async () => json({ status, retry_after: 2 }, 202));
    const { result } = await answer.json();
    assert.equal(result.structuredContent.status, status);
    assert.match(result.structuredContent.guidance, /not an empty result/);
    assert.equal(result.structuredContent.total, undefined);
  }
  for (const status of [400, 404, 422, 429, 503]) {
    const answer = await mcp(request(call("search", { q: "whigs" })), {}, async () =>
      json({ detail: "API explanation" }, status, { "retry-after": "5" }));
    const { result } = await answer.json();
    assert.equal(result.isError, true);
    assert.match(result.content[0].text, new RegExp(`HTTP ${status}`));
    assert.match(result.content[0].text, /API explanation.*Retry after 5/);
  }
  for (const serve of [
    async () => { throw new Error("sensitive origin detail"); },
    async () => new Response("<html>outage</html>", { status: 503 }),
    async () => json({}),
    async () => json({ status: "invented" }, 202),
    async () => new Response("{", { headers: { "content-type": "application/json" } }),
  ]) {
    const { result } = await (await mcp(request(call("search", { q: "whigs" })), {}, serve)).json();
    assert.equal(result.isError, true);
    assert.doesNotMatch(result.content[0].text, /sensitive origin detail|<html>/);
  }
});

test("oversized upstream responses and results fail explicitly and stop reading", async () => {
  let cancelled = false;
  const upstream = new ReadableStream({
    start(c) { c.enqueue(new Uint8Array(8 * 1024 * 1024 + 1)); },
    cancel() { cancelled = true; },
  });
  const answer = await mcp(request(call("get_corpus_info")), {}, async () =>
    new Response(upstream, { headers: { "content-type": "application/json" } }));
  assert.equal((await answer.json()).result.isError, true);
  assert.equal(cancelled, true);
  const large = await mcp(request(call("get_corpus_info")), {}, async () =>
    json({ index_version: "fixture", large: "a".repeat(256 * 1024) }));
  assert.match((await large.json()).result.content[0].text, /too large/);
});

test("MCP reads use the existing health-selected origin and do not forward POST or replay MCP", async (t) => {
  const calls = [];
  t.mock.method(globalThis, "fetch", async (sent) => {
    calls.push(sent);
    if (new URL(sent.url).pathname === "/readyz") return new Response("ready");
    return json({ index_version: "fixture-v1" });
  });
  const answer = await worker.fetch(request(call("get_corpus_info")), {
    HEALTH_FAILOVER: "true", HEALTH_HOSTNAME: "usnewsmap.com",
  });
  assert.equal((await answer.json()).result.structuredContent.index_version, "fixture-v1");
  assert.deepEqual(calls.map((sent) => [sent.method, new URL(sent.url).pathname]), [
    ["GET", "/readyz"], ["GET", "/v1/meta"],
  ]);
});

test("the dedicated health hostname never exposes MCP", async () => {
  const answer = await worker.fetch(request(init(), {}, "https://healthcheck.usnewsmap.com"), {
    HEALTHCHECK_HOSTNAME: "healthcheck.usnewsmap.com",
  });
  assert.equal(answer.status, 404);
});
