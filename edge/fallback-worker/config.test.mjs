// node --test edge/fallback-worker/config.test.mjs
//
// wrangler.jsonc holds the settings the Worker's privacy and fail-open rules rest on. This
// reads the file as it is committed and fails if one of them changes.

import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { test } from "node:test";

import worker from "./worker.mjs";

// JSON with // and /* */ comments, as wrangler reads it. Comment markers inside strings
// (the https:// of a URL) stay.
function parseJsonc(source) {
  let out = "";
  for (let i = 0; i < source.length; i++) {
    const c = source[i];
    if (c === '"') {
      let j = i + 1;
      while (source[j] !== '"') j += source[j] === "\\" ? 2 : 1;
      out += source.slice(i, j + 1);
      i = j;
    } else if (c === "/" && source[i + 1] === "/") {
      while (i < source.length && source[i] !== "\n") i++;
      out += "\n";
    } else if (c === "/" && source[i + 1] === "*") {
      i = source.indexOf("*/", i + 2) + 1;
    } else {
      out += c;
    }
  }
  return JSON.parse(out);
}

const config = parseJsonc(readFileSync(new URL("./wrangler.jsonc", import.meta.url), "utf8"));

test("the comment stripper keeps URLs and drops comments", () => {
  assert.deepEqual(parseJsonc('{ "a": "https://x.example/*", // gone\n "b": 1 /* gone */ }'), {
    a: "https://x.example/*",
    b: 1,
  });
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

test("vars hold no secret, and neither the drill flag nor an Azure name", () => {
  assert.deepEqual(Object.keys(config.vars), ["AZURE_FALLBACK_ORIGIN"]);
  assert.equal(config.vars.AZURE_FALLBACK_ORIGIN, "");
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
