#!/usr/bin/env node
// Render the OCR dashboard's social card: an SVG to a 1200x630 PNG with
// Chromium, so the PNG can be rebuilt from its source.
//
//   node scripts/ocr-dash/render-og.mjs --svg FILE --out FILE --node-dir DIR
//
// The source is og.svg and the output og.png, in the assets directory of the
// dashboard crate (design 12 names both). --node-dir names a directory whose
// node_modules has Playwright with a Chromium build, as for
// scripts/bench-site-lcp.mjs; PW_CHROMIUM_PATH names a Chromium to use
// instead of Playwright's own.
import { createRequire } from "node:module";
import path from "node:path";
import { parseArgs } from "node:util";
import { readFileSync } from "node:fs";

const { values: a } = parseArgs({
  options: {
    svg: { type: "string" },
    out: { type: "string" },
    "node-dir": { type: "string" },
  },
});
for (const k of ["svg", "out", "node-dir"]) {
  if (!a[k]) {
    console.error(`missing --${k}`);
    process.exit(2);
  }
}
const require = createRequire(path.join(path.resolve(a["node-dir"]), "package.json"));
const { chromium } = require("@playwright/test");
const browser = await chromium.launch({
  executablePath: process.env.PW_CHROMIUM_PATH || undefined,
});
const page = await browser.newPage({ viewport: { width: 1200, height: 630 }, deviceScaleFactor: 1 });
await page.setContent(
  `<!doctype html><body style="margin:0">${readFileSync(a.svg, "utf8")}</body>`,
);
await page.screenshot({ path: a.out, type: "png", clip: { x: 0, y: 0, width: 1200, height: 630 } });
await browser.close();
console.log(`wrote ${a.out}`);
