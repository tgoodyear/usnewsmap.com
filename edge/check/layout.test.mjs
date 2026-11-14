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

test("the frame on the printed line is drawn outside the word's box", () => {
  const rule = css.match(/\.lineview \.hl\s*\{([^{}]*)\}/);
  assert.ok(rule, "style.css has a .lineview .hl rule");
  const decl = Object.fromEntries(rule[1].split(";").map((d) => d.split(":").map((s) => s.trim())).filter((d) => d.length === 2));
  // a border (box-sizing: border-box) or an inset shadow paints inside the box, on top of the first and last letters
  assert.ok(!Object.keys(decl).some((p) => p.startsWith("border") && p !== "border-radius"), "no border: it paints inside the word's box");
  assert.ok(!/inset/.test(decl["box-shadow"] || ""), "no inset shadow");
  assert.match(decl.outline || "", /^\d+px solid /, "the frame is an outline");
  assert.ok(parseFloat(decl["outline-offset"]) >= 2, "with a gap between the letters and the line");
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
  // highlighting only paints: the word must be exactly as wide as unmarked, so spacing stays even, unspaced runs stay
  // unspaced and nothing wraps earlier (a pair split across the box loses its kerning: ~1px)
  const plain = scriptText(text, []);
  stage.replaceChildren(plain);
  const base = plain.getBoundingClientRect().width;
  stage.replaceChildren(el);
  const wide = el.getBoundingClientRect().width;
  if (Math.abs(wide - base) > 1.5) found.push({ code, text, diff, overlap: "width " + base.toFixed(2) + " -> " + wide.toFixed(2) });
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

// ---------- labels under the readings of a flagged pair (#1939) ----------

function examplesSource() {
  const start = appJs.indexOf("const segmenter");
  const end = appJs.indexOf("function render(");
  assert.ok(start > 0 && end > start, "app.js no longer has the markers this test slices between");
  return appJs.slice(start, end);
}

test("lookalikeLabels maps flagged letter positions to names, and ignores tasks with no flags", () => {
  const { lookalikeLabels } = new Function(`${scriptTextSource()}\nreturn { lookalikeLabels };`)();
  const t = { a: "ᏒᎯ", b: "ᎡᎯ", lookalikes: [{ a: 0, a_label: "sv", b: 0, b_label: "e" }] };
  assert.deepEqual([...lookalikeLabels(t, "a")], [[0, "sv"]]);
  assert.deepEqual([...lookalikeLabels(t, "b")], [[0, "e"]]);
  // a transposition flags both positions
  const swap = { a: "ᎡᏒ", b: "ᏒᎡ", lookalikes: [{ a: 0, a_label: "e", b: 0, b_label: "sv" }, { a: 1, a_label: "sv", b: 1, b_label: "e" }] };
  assert.deepEqual([...lookalikeLabels(swap, "a")], [[0, "e"], [1, "sv"]]);
  assert.equal(lookalikeLabels({ a: "ᏒᎯ", b: "ᎡᎯ" }, "a").size, 0);
  assert.equal(lookalikeLabels({ a: "ᏒᎯ", lookalikes: [{ a: 7, a_label: "x" }] }, "a").size, 0, "an index past the word is ignored");
  // a letter with marks is one letter, as in a_diff
  assert.deepEqual([...lookalikeLabels({ a: "אַל", lookalikes: [{ a: 0, a_label: "alef patah" }] }, "a")], [[0, "alef patah"]]);
});

test("labels render under each reading, in left-to-right and right-to-left languages, without overlap or overflow", (t) => {
  const chrome = findChrome();
  if (!chrome) {
    if (process.env.CI) assert.fail("CI is set but no Chrome was found");
    t.skip("no Chrome or Chromium found; set CHROME=/path/to/chrome");
    return;
  }
  const flagged = (a, b, a_label, b_label) => ({ a, b, a_diff: [0], b_diff: [0], lookalikes: [{ a: 0, a_label, b: 0, b_label }] });
  const cases = [
    { code: "chr", cfg: { dir: null, notes: { "Ꮢ": "a note" } }, task: flagged("ᏒᎯ", "ᎡᎯ", "sv", "e"), expect: { a: ["sv"], b: ["e"] } },
    { code: "chr", cfg: { dir: null, notes: {} }, task: { a: "ᎡᏒ", b: "ᏒᎡ", a_diff: [0], b_diff: [1], lookalikes: [{ a: 0, a_label: "e", b: 0, b_label: "sv" }, { a: 1, a_label: "sv", b: 1, b_label: "e" }] },
      expect: { a: ["e", "sv"], b: ["sv", "e"] } },
    { code: "yi", cfg: { dir: "rtl", notes: {} }, task: flagged("אַל", "אָל", "hebrew letter alef + hebrew point patah", "hebrew letter alef + hebrew point qamats"),
      expect: { a: ["hebrew letter alef + hebrew point patah"], b: ["hebrew letter alef + hebrew point qamats"] } },
    { code: "sr", cfg: { dir: null, notes: {} }, task: flagged("пс", "нс", "cyrillic small letter pe", "cyrillic small letter en"), expect: { a: ["cyrillic small letter pe"], b: ["cyrillic small letter en"] } },
    { code: "chr", cfg: { dir: null, notes: {} }, task: { a: "ᏒᎯ", b: "ᎡᎯ", a_diff: [0], b_diff: [0] }, expect: { a: [], b: [] } },
  ];
  const dir = mkdtempSync(join(tmpdir(), "check-labels-"));
  try {
    const page = join(dir, "harness.html");
    writeFileSync(page, `<!doctype html><meta charset="utf-8"><link rel="stylesheet" href="file://${join(publicDir, "style.css")}">
<body><div id="stage" style="width:300px"></div><pre id="out"></pre>
<script>
let lang = null, cfg = null;
${examplesSource()}
const cases = ${JSON.stringify(cases)};
const stage = document.getElementById("stage");
const found = [], seen = [];
document.fonts.ready.then(() => {
  for (const c of cases) {
    lang = c.code; cfg = c.cfg;
    for (const side of ["a", "b"]) {
      const box = examples(c.task[side], c.task[side + "_diff"], lookalikeLabels(c.task, side));
      stage.replaceChildren(box);
      const names = [...box.querySelectorAll(".name")];
      seen.push({ code: c.code, side, names: names.map((n) => n.textContent) });
      const want = c.expect[side];
      if (JSON.stringify(names.map((n) => n.textContent)) !== JSON.stringify(want)) found.push({ code: c.code, side, problem: "label", names: names.map((n) => n.textContent) });
      for (const n of names) {
        if (n.dir !== "ltr") found.push({ code: c.code, side, problem: "label is not left to right" });
        const r = n.getBoundingClientRect(), l = n.parentElement.querySelector(".letter").getBoundingClientRect();
        if (Math.min(r.right, l.right) - Math.max(r.left, l.left) > 0.5 && Math.min(r.bottom, l.bottom) - Math.max(r.top, l.top) > 0.5) found.push({ code: c.code, side, problem: "label overlaps its letter" });
        const s = stage.getBoundingClientRect();
        if (r.left < s.left - 0.5 || r.right > s.right + 0.5) found.push({ code: c.code, side, problem: "label outside the column" });
      }
      if (stage.scrollWidth > stage.clientWidth) found.push({ code: c.code, side, problem: "overflow" });
    }
  }
  document.getElementById("out").textContent = JSON.stringify({ found, seen });
});
</script>`);
    const dom = execFileSync(chrome, ["--headless=new", "--no-sandbox", "--disable-gpu", "--allow-file-access-from-files",
      "--virtual-time-budget=20000", "--dump-dom", `file://${page}`], { encoding: "utf8", timeout: 120000, maxBuffer: 64 * 1024 * 1024, stdio: ["ignore", "pipe", "ignore"] });
    const m = dom.match(/<pre id="out">([\s\S]*?)<\/pre>/);
    assert.ok(m && m[1].trim(), "the harness page produced no result");
    const result = JSON.parse(m[1].replace(/&quot;/g, '"').replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&amp;/g, "&"));
    assert.equal(result.seen.length, 10);
    assert.deepEqual(result.found, []);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("the page serves a Cherokee font first in the stack, for the Cherokee blocks only", () => {
  const face = css.match(/@font-face\s*\{([^}]*)\}/);
  assert.ok(face, "style.css has an @font-face");
  assert.match(face[1], /font-family:\s*"Noto Sans Cherokee"/);
  assert.match(face[1], /url\("fonts\/NotoSansCherokee-Regular\.woff2"\)/);
  assert.match(face[1], /unicode-range:\s*U\+13A0-13FF,\s*U\+AB70-ABBF/);
  assert.match(css, /--font-script:\s*"Noto Sans Cherokee",/);
  assert.ok(existsSync(join(publicDir, "fonts", "NotoSansCherokee-Regular.woff2")));
});

test("a Cherokee word is answerable only once the served font has loaded", async () => {
  const start = appJs.indexOf("const CHEROKEE");
  const end = appJs.indexOf("function render(");
  assert.ok(start > 0 && end > start, "app.js no longer has the markers this test slices between");
  const ready = (document) => new Function("document", `${appJs.slice(start, end)}\nreturn fontReady;`)(document);
  const word = { a: "ᏒᎯ", b: "ᎡᎯ" };
  const loads = [];
  const fonts = (result) => ({ fonts: { load: async (spec, text) => { loads.push([spec, text]); if (result instanceof Error) throw result; return result; } } });
  assert.equal(await ready(fonts([{ status: "loaded" }]))(word), true);
  assert.deepEqual(loads, [['32px "Noto Sans Cherokee"', "ᏒᎯᎡᎯ"]]);
  assert.equal(await ready(fonts([{ status: "error" }]))(word), false, "a font that failed to load");
  assert.equal(await ready(fonts([]))(word), false, "no face matched");
  assert.equal(await ready(fonts(new Error("network")))(word), false, "the load threw");
  // nothing to wait for without Cherokee letters, or in a browser with no font loading API
  loads.length = 0;
  assert.equal(await ready(fonts([]))({ a: "אל", b: "בל" }), true);
  assert.deepEqual(loads, []);
  assert.equal(await ready({})(word), true);
  assert.equal(await ready(fonts([]))(null), true);
  // and the page keeps its answer controls off, and refuses the keyboard shortcuts, while the font has failed
  assert.match(appJs, /const live = !!task && !busy && !fontFailed;/);
  assert.match(appJs, /if \(!task \|\| busy \|\| fontFailed\) return;/);
});
