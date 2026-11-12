#!/usr/bin/env node
// Render the OCR dashboard's social card: an SVG to a 1200x630 PNG with
// Chromium, so the PNG can be rebuilt from its source.
//
//   node scripts/ocr-dash/render-og.mjs [--svg FILE] [--out FILE] [--web-dir DIR]
//
// Defaults: --svg crates/usnm-ocr-dash/assets/og.svg and --out
// crates/usnm-ocr-dash/assets/og.png, both relative to the repository.
// Needs Playwright with a Chromium build: --web-dir (or USNM_WEB_DIR)
// names a directory whose node_modules has it, as for
// scripts/bench-site-lcp.mjs. PW_CHROMIUM_PATH names a Chromium to use
// instead of Playwright's own.
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { readFileSync } from "node:fs";

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const { values: a } = parseArgs({
  options: {
    svg: { type: "string", default: path.join(repo, "crates/usnm-ocr-dash/assets/og.svg") },
    out: { type: "string", default: path.join(repo, "crates/usnm-ocr-dash/assets/og.png") },
    "web-dir": { type: "string", default: process.env.USNM_WEB_DIR ?? path.join(repo, "web") },
  },
});
const require = createRequire(path.join(path.resolve(a["web-dir"]), "package.json"));
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
