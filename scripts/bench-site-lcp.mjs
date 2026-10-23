#!/usr/bin/env node
// Time the site's pages in a real browser: time to first byte, first and
// largest contentful paint, load, and what the page fetched.
//
//   node scripts/bench-site-lcp.mjs [--base-url URL] [--runs 5] [--profile desktop|mobile]
//                                   [--paths /,/datasets] [--web-dir DIR] [--json FILE]
//
// Each run opens a fresh browser context, so nothing is cached: a first
// visit. The desktop profile is unthrottled, so it shows this machine's
// network to the site. The mobile profile is a Pixel 7 on a throttled
// connection (1.6 Mbit/s, 150 ms round trip, 4x slower CPU), close to what
// PageSpeed Insights uses for "mobile", and mostly shows the page's weight.
// Run it from each network that matters, as for scripts/bench-site-speed.py
// (the same site, and the same split between from-here and from-elsewhere).
//
// Needs Playwright with a Chromium build, which the web app's tests already
// use: `npm ci` in web/, then `npx playwright install chromium`. --web-dir
// (or USNM_WEB_DIR) points at another web/ directory that has node_modules;
// PW_CHROMIUM_PATH names a Chromium to use instead of Playwright's own.
//
// Largest contentful paint is the last candidate the browser reported by 3 s
// after the load event. A pointer or key event ends the candidates early
// in real use, so a real visitor's number can be a little lower.
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { writeFileSync } from "node:fs";

const { values: a } = parseArgs({
  options: {
    "base-url": { type: "string", default: "https://usnewsmap.com" },
    runs: { type: "string", default: "5" },
    profile: { type: "string", default: "desktop" },
    paths: { type: "string", default: "/,/?q=whigs&mode=phrase&from=1834-01-01&to=1856-12-31&bucket=month,/datasets" },
    "web-dir": { type: "string", default: process.env.USNM_WEB_DIR ?? "" },
    json: { type: "string", default: "" },
  },
});
const runs = Number(a.runs);
if (!Number.isInteger(runs) || runs < 1) throw new Error("--runs must be a whole number of at least 1");
if (!["desktop", "mobile"].includes(a.profile)) throw new Error("--profile is desktop or mobile");
const base = a["base-url"].replace(/\/+$/, "");
const webDir = a["web-dir"] || path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "web");
const { chromium, devices } = createRequire(path.join(webDir, "package.json"))("@playwright/test");

const median = (xs) => {
  const s = [...xs].sort((x, y) => x - y);
  const m = Math.floor(s.length / 2);
  return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
};
const round = (x) => Math.round(x);

const browser = await chromium.launch(process.env.PW_CHROMIUM_PATH ? { executablePath: process.env.PW_CHROMIUM_PATH } : {});
const results = [];
try {
  console.log(`${base}  ${a.profile}, ${runs} runs each, a new browser context per run (ms)`);
  console.log(`${"path".padEnd(34)} ${"ttfb".padStart(6)} ${"fcp".padStart(6)} ${"lcp".padStart(6)} ${"lcp max".padStart(8)} ${"load".padStart(6)} ${"reqs".padStart(5)} ${"KB".padStart(6)}  lcp element`);
  for (const p of a.paths.split(",")) {
    const rows = [];
    for (let i = 0; i < runs; i++) {
      const context = await browser.newContext(a.profile === "mobile" ? { ...devices["Pixel 7"] } : {});
      const page = await context.newPage();
      if (a.profile === "mobile") {
        const cdp = await context.newCDPSession(page);
        await cdp.send("Network.enable");
        await cdp.send("Network.emulateNetworkConditions", {
          offline: false,
          latency: 150,
          downloadThroughput: (1.6 * 1024 * 1024) / 8,
          uploadThroughput: (750 * 1024) / 8,
        });
        await cdp.send("Emulation.setCPUThrottlingRate", { rate: 4 });
      }
      await page.addInitScript(() => {
        window.__lcp = { time: 0, el: "" };
        new PerformanceObserver((list) => {
          for (const e of list.getEntries()) window.__lcp = { time: e.startTime, el: e.element ? e.element.tagName.toLowerCase() : "" };
        }).observe({ type: "largest-contentful-paint", buffered: true });
      });
      const response = await page.goto(base + p, { waitUntil: "load", timeout: 60_000 });
      await page.waitForLoadState("networkidle", { timeout: 15_000 }).catch(() => {});
      await page.waitForTimeout(3000);
      const m = await page.evaluate(() => {
        const nav = performance.getEntriesByType("navigation")[0];
        const res = performance.getEntriesByType("resource");
        const fcp = performance.getEntriesByName("first-contentful-paint")[0];
        return {
          ttfb: nav.responseStart,
          fcp: fcp ? fcp.startTime : 0,
          lcp: window.__lcp.time,
          el: window.__lcp.el,
          load: nav.loadEventEnd,
          reqs: res.length + 1,
          bytes: nav.transferSize + res.reduce((n, r) => n + r.transferSize, 0),
        };
      });
      rows.push({ ...m, status: response.status() });
      await context.close();
    }
    const row = {
      path: p,
      status: [...new Set(rows.map((r) => r.status))],
      ttfb: round(median(rows.map((r) => r.ttfb))),
      fcp: round(median(rows.map((r) => r.fcp))),
      lcp: round(median(rows.map((r) => r.lcp))),
      lcpMax: round(Math.max(...rows.map((r) => r.lcp))),
      load: round(median(rows.map((r) => r.load))),
      reqs: median(rows.map((r) => r.reqs)),
      kb: round(median(rows.map((r) => r.bytes)) / 1024),
      lcpElement: [...new Set(rows.map((r) => r.el))].join("/"),
      runs: rows,
    };
    results.push(row);
    const label = row.path.length > 33 ? row.path.slice(0, 30) + "..." : row.path;
    console.log(
      `${label.padEnd(34)} ${String(row.ttfb).padStart(6)} ${String(row.fcp).padStart(6)} ${String(row.lcp).padStart(6)} ` +
        `${String(row.lcpMax).padStart(8)} ${String(row.load).padStart(6)} ${String(row.reqs).padStart(5)} ${String(row.kb).padStart(6)}  ${row.lcpElement}` +
        (row.status.some((s) => s !== 200) ? `  status ${row.status.join("/")}` : ""),
    );
  }
} finally {
  await browser.close();
}
if (a.json) {
  writeFileSync(a.json, JSON.stringify({ at: new Date().toISOString(), base_url: base, profile: a.profile, runs, results }, null, 2) + "\n");
}
