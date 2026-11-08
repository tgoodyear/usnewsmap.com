// Layout tests for the word view. A highlighted letter must never sit on top of its neighbours, in any language
// or direction (a right-to-left line with a bracket and a digit once did). The rules are checked in two ways:
//  - the stylesheet never pulls a highlight or a letter back with a negative margin or offset (always runs);
//  - the real scriptText() is rendered in headless Chrome for every language's sample words, one highlighted
//    letter at a time, and no highlight box may overlap another character (skipped when Chrome isn't installed,
//    unless CI is set).
// Run: node --test edge/check/layout.test.mjs   (CHROME=/path/to/chrome to choose the browser)
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const publicDir = join(here, "public");
const css = readFileSync(join(publicDir, "style.css"), "utf8");
const appJs = readFileSync(join(publicDir, "app.js"), "utf8");

test("no negative margins or offsets on script text", () => {
  const bad = [];
  for (const m of css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    const selector = m[1].trim();
    if (!/\.script|\bmark\b|\.tight|\.letter|\.gap/.test(selector)) continue;
    for (const d of m[2].split(";")) {
      const [prop, value = ""] = d.split(":").map((s) => s.trim());
      if (/^(margin|padding|text-indent|left|right|inset|word-spacing)/.test(prop) && /(^|[\s(])-\d|-\.\d/.test(value)) bad.push(`${selector} { ${d.trim()} }`);
      if (prop === "transform" && /translate/.test(value)) bad.push(`${selector} { ${d.trim()} }`);
    }
  }
  assert.deepEqual(bad, [], "a negative offset overlaps the neighbouring glyph whenever its gap isn't on the expected side");
});

// ---------- headless Chrome ----------

function findChrome() {
  const candidates = [process.env.CHROME, "google-chrome", "google-chrome-stable", "chromium", "chromium-browser",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "/Applications/Chromium.app/Contents/MacOS/Chromium"].filter(Boolean);
  for (const c of candidates) {
    try {
      if (c.includes("/") ? existsSync(c) : execFileSync("which", [c], { stdio: "pipe" })) return c;
    } catch { /* not installed */ }
  }
  return null;
}

// scriptText() and what it needs, taken from the page's own source so this tests what ships.
function scriptTextSource() {
  const start = appJs.indexOf("const segmenter");
  const end = appJs.indexOf("function examples(");
  assert.ok(start > 0 && end > start, "app.js no longer has the markers this test slices between");
  return appJs.slice(start, end);
}

const languages = Object.fromEntries(readdirSync(join(here, "languages")).filter((f) => f.endsWith(".json"))
  .map((f) => [f.replace(/\.json$/, ""), JSON.parse(readFileSync(join(here, "languages", f), "utf8"))]));

// Words as the tasks carry them, plus the awkward parts of real lines: a bracket and a digit, a run of digits,
// mixed-direction text, the letters a language's notes single out and its equivalent spellings.
const common = ["3)", "3(", "(3", ")3", "(3)", "1929", "A1", "-", "«а»"];
const samples = {
  yi: ["דער סעקרעטאר", "װאָס", "ײ", "ײַ", "ווי", "דער 1929 (3)", "פּאַריז", "ר\"ל"],
  sr: ["Српски", "Cop", "сх", "Ђорђе 1906 (3)", "Amerikanski"],
  chr: ["ᏣᎳᎩ", "ᎠᏂᏴᏫᏯ", "ᏣᎳᎩ (3)"],
};

function cases() {
  const out = [];
  for (const [code, cfg] of Object.entries(languages)) {
    const words = new Set([...common, ...(samples[code] || []), ...Object.keys(cfg.notes || {}), ...(cfg.equivalents ? Object.entries(cfg.equivalents).flat() : [])]);
    out.push({ code, cfg: { dir: cfg.dir || null, equivalents: cfg.equivalents || null }, words: [...words] });
  }
  return out;
}

function harness() {
  return `<!doctype html><meta charset="utf-8"><link rel="stylesheet" href="file://${join(publicDir, "style.css")}">
<body><div id="stage" style="width:3000px;white-space:nowrap;direction:ltr"></div><pre id="out"></pre>
<script>
let lang = null, cfg = null;
${scriptTextSource()}
const cases = ${JSON.stringify(cases())};
const stage = document.getElementById("stage");
const found = [];
function charRects(node) {
  const rects = [], r = document.createRange();
  for (let i = 0; i < node.length; i++) {
    r.setStart(node, i); r.setEnd(node, i + 1);
    for (const q of r.getClientRects()) if (q.width > 0) rects.push(q);
  }
  return rects;
}
function check(code, text, diff) {
  const el = scriptText(text, diff);
  stage.replaceChildren(el);
  const marks = [...el.querySelectorAll("mark")];
  const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    if (n.parentElement.closest("mark")) continue;
    for (const q of charRects(n)) for (const m of marks) {
      const b = m.getBoundingClientRect();
      const w = Math.min(q.right, b.right) - Math.max(q.left, b.left);
      const h = Math.min(q.bottom, b.bottom) - Math.max(q.top, b.top);
      if (w > 0.5 && h > 0.5) found.push({ code, text, diff, overlap: +w.toFixed(2) });
    }
  }
  // the highlight must also not sit over another highlight or run outside the line
  for (let i = 0; i < marks.length; i++) for (let j = i + 1; j < marks.length; j++) {
    const a = marks[i].getBoundingClientRect(), b = marks[j].getBoundingClientRect();
    if (Math.min(a.right, b.right) - Math.max(a.left, b.left) > 0.5) found.push({ code, text, diff, overlap: "mark on mark" });
  }
}
document.fonts.ready.then(() => {
  let checked = 0;
  for (const c of cases) {
    cfg = c.cfg; lang = c.code;
    for (const text of c.words) {
      const n = letters(text).length;
      for (let i = 0; i < n; i++) { check(c.code, text, [i]); checked++; }
      if (n > 1) { check(c.code, text, [0, n - 1]); check(c.code, text, [...Array(n).keys()]); checked += 2; }
    }
  }
  document.getElementById("out").textContent = JSON.stringify({ checked, found });
});
</script>`;
}

test("highlights never overlap other letters (real layout, all languages)", (t) => {
  const chrome = findChrome();
  if (!chrome) {
    if (process.env.CI) assert.fail("CI is set but no Chrome was found");
    t.skip("no Chrome or Chromium found; set CHROME=/path/to/chrome");
    return;
  }
  const dir = mkdtempSync(join(tmpdir(), "check-layout-"));
  try {
    const page = join(dir, "harness.html");
    writeFileSync(page, harness());
    const dom = execFileSync(chrome, ["--headless=new", "--no-sandbox", "--disable-gpu", "--allow-file-access-from-files",
      "--virtual-time-budget=20000", "--dump-dom", `file://${page}`], { encoding: "utf8", timeout: 120000, maxBuffer: 64 * 1024 * 1024, stdio: ["ignore", "pipe", "ignore"] });
    const m = dom.match(/<pre id="out">([\s\S]*?)<\/pre>/);
    assert.ok(m && m[1].trim(), "the harness page produced no result");
    const result = JSON.parse(m[1].replace(/&quot;/g, '"').replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&amp;/g, "&"));
    assert.ok(result.checked > 100, `only ${result.checked} cases ran`);
    assert.deepEqual(result.found.slice(0, 10), [], `${result.found.length} overlaps in ${result.checked} renderings`);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
