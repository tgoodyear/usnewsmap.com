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
    if (!/\.script|\bmark\b|\.tight|\.letter|\.gap|\.inset/.test(selector)) continue;
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
// the stage is empty until a case is built, so ask for the Cherokee face first: the cases must be measured in it
document.fonts.load('32px "Noto Sans Cherokee"', "ᎠᎡᏒ").then((faces) => {
  if (!faces.length || faces.some((f) => f.status !== "loaded")) found.push({ code: "chr", text: "", diff: [], overlap: "the Cherokee font did not load" });
  return document.fonts.ready;
}).then(() => {
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
  const { lookalikeLabels } = new Function(`const cfg = {};\n${scriptTextSource()}\nreturn { lookalikeLabels };`)();
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

test("lookalikeLabels takes a letter's label from the language's labels, and keeps the stored name for a letter with none", () => {
  const make = (labels) => new Function(`const cfg = ${JSON.stringify({ labels })};\n${scriptTextSource()}\nreturn { lookalikeLabels };`)().lookalikeLabels;
  const sr = make(JSON.parse(readFileSync(join(here, "languages", "sr.json"), "utf8")).labels);
  const pair = (a, b) => ({ a, b, lookalikes: [{ a: 0, a_label: "cyrillic small letter a", b: 0, b_label: "cyrillic small letter b" }] });
  // two Cyrillic letters: each reading shows its Latin equivalent
  const pn = pair("пс", "нс");
  assert.deepEqual([...sr(pn, "a")], [[0, "p"]]);
  assert.deepEqual([...sr(pn, "b")], [[0, "n"]]);
  assert.deepEqual([...sr(pair("Ц", "П"), "a")], [[0, "C"]]);
  assert.deepEqual([...sr(pair("џ", "ц"), "a")], [[0, "dž"]]);
  assert.deepEqual([...sr(pair("Њ", "Н"), "a")], [[0, "Nj"]]);
  // a Cyrillic letter against a Latin look-alike: the Latin one says so, and the Cyrillic one says its script,
  // so "H" against "Н" (or h against ћ) never reads as one letter twice
  assert.deepEqual([...sr(pair("П", "H"), "a")], [[0, "Cyrillic P"]]);
  assert.deepEqual([...sr(pair("П", "H"), "b")], [[0, "Latin H"]]);
  assert.deepEqual([...sr(pair("ћ", "h"), "a")], [[0, "Cyrillic ć"]]);
  assert.deepEqual([...sr(pair("ћ", "h"), "b")], [[0, "Latin h"]]);
  assert.deepEqual([...sr(pair("х", "h"), "a")], [[0, "Cyrillic h"]]);
  assert.notEqual(sr(pair("х", "h"), "a").get(0), sr(pair("х", "h"), "b").get(0));
  // a letter the map doesn't have keeps the stored name; a language with no labels is unchanged
  assert.deepEqual([...sr(pair("ё", "н"), "a")], [[0, "cyrillic small letter a"]]);
  assert.deepEqual([...make(undefined)(pn, "a")], [[0, "cyrillic small letter a"]]);
  assert.deepEqual([...make(undefined)(pair("П", "H"), "b")], [[0, "cyrillic small letter b"]]);
  // a Cherokee letter against a Latin look-alike keeps its plain name: only a Cyrillic letter says "Cyrillic"
  const chr = make(languages.chr.labels);
  const cherokee = (a, b) => ({ a, b, lookalikes: [{ a: 0, a_label: "x", b: 0, b_label: "y" }] });
  assert.deepEqual([...chr(cherokee("Ꭰ", "D"), "a")], [[0, "a"]]);
  assert.deepEqual([...chr(cherokee("Ꭰ", "D"), "b")], [[0, "Latin D"]]);
  assert.deepEqual([...chr(cherokee("Ꭺ", "Ꭰ"), "a")], [[0, "go"]]);
  // a transposition flags both positions, each labelled from its own letter
  const swap = { a: "пн", b: "нп", lookalikes: [{ a: 0, a_label: "x", b: 0, b_label: "y" }, { a: 1, a_label: "y", b: 1, b_label: "x" }] };
  assert.deepEqual([...sr(swap, "a")], [[0, "p"], [1, "n"]]);
  assert.deepEqual([...sr(swap, "b")], [[0, "n"], [1, "p"]]);
});

test("markLabels names the marks each reading has on the differing letter", () => {
  const make = (cfg) => new Function(`const cfg = ${JSON.stringify(cfg)};\n${scriptTextSource()}\nreturn { markLabels };`)().markLabels;
  const yi = make({ marks: { "\u05b7": "pasekh", "\u05b8": "komets", "\u05bc": "dagesh", "\u05bf": "rafe" } });
  const t = { a: "\u05d0\u05b7\u05dc", b: "\u05d0\u05b8\u05dc", mark_only: [{ a: 0, a_marks: "\u05b7", b: 0, b_marks: "\u05b8", base: "\u05d0" }] };
  assert.deepEqual([...yi(t, "a")], [[0, "pasekh"]]);
  assert.deepEqual([...yi(t, "b")], [[0, "komets"]]);
  // a reading with no mark says so; a letter with two marks names both; a repeated mark is named once
  const u = { a: "\u05e4\u05bc\u05bf", b: "\u05e4", mark_only: [{ a: 0, a_marks: "\u05bc\u05bf\u05bf", b: 0, b_marks: "", base: "\u05e4" }] };
  assert.deepEqual([...yi(u, "a")], [[0, "dagesh + rafe"]]);
  assert.deepEqual([...yi(u, "b")], [[0, "no mark"]]);
  // a mark the language has no name for gets a short generic name, never the Unicode name
  const other = make({}), v = { a: "\u05d0\u05b4", b: "\u05d0", mark_only: [{ a: 0, a_marks: "\u05b4", b: 0, b_marks: "", base: "\u05d0" }] };
  assert.deepEqual([...other(v, "a")], [[0, "another mark"]]);
  // tasks built before the field existed, an index past the word and a malformed entry show nothing
  assert.equal(yi({ a: "\u05d0\u05b7", b: "\u05d0" }, "a").size, 0);
  assert.equal(yi({ a: "\u05d0", mark_only: [{ a: 3, a_marks: "\u05b7" }] }, "a").size, 0);
  assert.equal(yi({ a: "\u05d0", mark_only: [{ a: 0, a_marks: 5 }] }, "a").size, 0);
  // the same pair in another script: a Latin letter with and without an acute accent
  assert.deepEqual([...make({ marks: { "\u0301": "acute" } })({ a: "e\u0301", mark_only: [{ a: 0, a_marks: "\u0301", base: "e" }] }, "a")], [[0, "acute"]]);
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
    // Serbian with the language's labels: Latin equivalents, and the Cyrillic-against-Latin pair told apart
    { code: "sr", cfg: { dir: null, notes: {}, labels: languages.sr.labels }, task: flagged("пс", "нс", "x", "y"), expect: { a: ["p"], b: ["n"] } },
    { code: "sr", cfg: { dir: null, notes: {}, labels: languages.sr.labels }, task: flagged("Џорђе", "Цорђе", "x", "y"), expect: { a: ["Dž"], b: ["C"] } },
    { code: "sr", cfg: { dir: null, notes: { "ћ": "a note" }, labels: languages.sr.labels }, task: flagged("ћс", "hс", "x", "y"), expect: { a: ["Cyrillic ć"], b: ["Latin h"] } },
    { code: "sr", cfg: { dir: null, notes: {}, labels: languages.sr.labels }, task: flagged("Пс", "Hс", "x", "y"), expect: { a: ["Cyrillic P"], b: ["Latin H"] } },
    { code: "chr", cfg: { dir: null, notes: {} }, task: { a: "ᏒᎯ", b: "ᎡᎯ", a_diff: [0], b_diff: [0] }, expect: { a: [], b: [] } },
    // letter names showing: the label that is the same text as the name under the letter is not printed again, a label that adds
    // the script is kept, and the picture row keeps the letter either way
    { code: "chr", cfg: { dir: null, notes: {}, labels: languages.chr.labels }, named: true, task: flagged("ᏒᎯ", "ᎡᎯ", "sv", "e"), expect: { a: [], b: [] } },
    { code: "chr", cfg: { dir: null, notes: {}, labels: languages.chr.labels }, task: flagged("ᏒᎯ", "ᎡᎯ", "sv", "e"), expect: { a: ["sv"], b: ["e"] } },
    { code: "sr", cfg: { dir: null, notes: {}, labels: languages.sr.labels }, named: true, task: flagged("Пс", "Hс", "x", "y"), expect: { a: ["Cyrillic P"], b: ["Latin H"] } },
    { code: "sr", cfg: { dir: null, notes: {}, labels: languages.sr.labels }, named: true, task: flagged("пс", "нс", "x", "y"), expect: { a: [], b: [] } },
    // a mark-only pair in right-to-left text: the name of each reading's mark, and the letter enlarged beside it
    { code: "yi", cfg: { dir: "rtl", notes: {}, marks: { "\u05b7": "pasekh", "\u05b8": "komets", "\u05bc": "dagesh" } },
      task: { a: "\u05e4\u05bc\u05d0\u05b7\u05dc", b: "\u05e4\u05d0\u05b8\u05dc", a_diff: [0, 1], b_diff: [0, 1], mark_only: [
        { a: 0, a_marks: "\u05bc", b: 0, b_marks: "", base: "\u05e4" }, { a: 1, a_marks: "\u05b7", b: 1, b_marks: "\u05b8", base: "\u05d0" }] },
      expect: { a: ["dagesh", "pasekh"], b: ["no mark", "komets"] }, insets: { a: 2, b: 2 } },
    { code: "yi", cfg: { dir: "rtl", notes: { "\u05d0\u05b7": "a note" }, marks: { "\u05b7": "pasekh" } },
      task: { a: "\u05d0\u05b7", b: "\u05d0", a_diff: [0], b_diff: [0], mark_only: [{ a: 0, a_marks: "\u05b7", b: 0, b_marks: "", base: "\u05d0" }] },
      expect: { a: ["pasekh"], b: ["no mark"] }, insets: { a: 1, b: 1 } },
    // a double yod with a point (U+05F2 U+05B7) is one displayed letter: its label names its mark, and the inset shows the whole cluster
    { code: "yi", cfg: { dir: "rtl", notes: {}, marks: { "\u05b7": "pasekh", "\u05b8": "komets" } },
      task: { a: "\u05d0\u05f2\u05b7\u05d1", b: "\u05d0\u05f2\u05b8\u05d1", a_diff: [1], b_diff: [1], mark_only: [{ a: 1, a_marks: "\u05b7", b: 1, b_marks: "\u05b8", base: "\u05f2", differ: "\u05b7\u05b8" }] },
      expect: { a: ["pasekh"], b: ["komets"] }, insets: { a: 1, b: 1 }, insetText: { a: "\u05f2\u05b7", b: "\u05f2\u05b8" } },
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
document.fonts.load('32px "Noto Sans Cherokee"', "ᎠᎡᏒ").then((faces) => {
  if (!faces.length || faces.some((f) => f.status !== "loaded")) found.push({ code: "chr", side: "", problem: "the Cherokee font did not load" });
  return document.fonts.ready;
}).then(() => {
  for (const c of cases) {
    lang = c.code; cfg = c.cfg;
    for (const side of ["a", "b"]) {
      const box = examples(c.task[side], c.task[side + "_diff"], lookalikeLabels(c.task, side), markLabels(c.task, side), !!c.named);
      stage.replaceChildren(box);
      const names = [...box.querySelectorAll(".name")];
      seen.push({ code: c.code, side, names: names.map((n) => n.textContent) });
      const want = c.expect[side];
      if (JSON.stringify(names.map((n) => n.textContent)) !== JSON.stringify(want)) found.push({ code: c.code, side, problem: "label", names: names.map((n) => n.textContent) });
      const insets = [...box.querySelectorAll(".inset")];
      if (insets.length !== ((c.insets || {})[side] || 0)) found.push({ code: c.code, side, problem: "insets", count: insets.length });
      for (const e of box.querySelectorAll(".ex")) {
        // every box in a label row sits apart from the others: no two of letter, label, inset and notes overlap
        const parts = [...e.children].map((x) => x.getBoundingClientRect());
        for (let i = 0; i < parts.length; i++) for (let j = i + 1; j < parts.length; j++) {
          const p = parts[i], q = parts[j];
          if (Math.min(p.right, q.right) - Math.max(p.left, q.left) > 0.5 && Math.min(p.bottom, q.bottom) - Math.max(p.top, q.top) > 0.5) found.push({ code: c.code, side, problem: "parts overlap" });
        }
      }
      const wantText = (c.insetText || {})[side];
      if (wantText !== undefined && (insets.length !== 1 || insets[0].textContent !== wantText)) found.push({ code: c.code, side, problem: "inset is not the displayed cluster", got: insets.map((n) => n.textContent) });
      for (const n of insets) {
        if (n.getAttribute("aria-hidden") !== "true") found.push({ code: c.code, side, problem: "inset is not hidden from screen readers" });
        const r = n.getBoundingClientRect(), l = n.parentElement.querySelector(".letter").getBoundingClientRect(), s = stage.getBoundingClientRect();
        if (r.height < 1.8 * l.height - 1) found.push({ code: c.code, side, problem: "inset is not about twice the letter" });
        if (r.left < s.left - 0.5 || r.right > s.right + 0.5) found.push({ code: c.code, side, problem: "inset outside the column" });
      }
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
    assert.equal(result.seen.length, cases.length * 2);
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
  assert.match(appJs, /const live = !!task && !busy && !fontFailed && !stale;/);
  assert.match(appJs, /if \(!task \|\| busy \|\| fontFailed \|\| stale\) return;/);
});

test("each answer carries what the page knows about the bundled font", () => {
  const start = appJs.indexOf("const CHEROKEE");
  const end = appJs.indexOf("function render(");
  assert.ok(start > 0 && end > start, "app.js no longer has the markers this test slices between");
  const state = (document) => new Function("document", `${appJs.slice(start, end)}\nreturn fontState;`)(document);
  const word = { a: "ᏒᎯ", b: "ᎡᎯ" };
  const checks = [];
  const fonts = (result) => ({ fonts: { check: (spec, text) => { checks.push([spec, text]); if (result instanceof Error) throw result; return result; } } });
  assert.equal(state(fonts(true))(word), "loaded");
  assert.deepEqual(checks, [['32px "Noto Sans Cherokee"', "ᏒᎯᎡᎯ"]], "asks about the family the page declares, for the letters shown");
  assert.equal(state(fonts(false))(word), "not_loaded");
  assert.equal(state(fonts(new Error("bad font list")))(word), "unknown");
  assert.equal(state({})(word), "unknown", "a browser with no font loading API");
  // no bundled font for the word's letters: the answer says so, and nothing is asked of the browser
  checks.length = 0;
  assert.equal(state(fonts(false))({ a: "אל", b: "בל" }), "none");
  assert.equal(state(fonts(false))({ a: "пн", b: "нп" }), "none");
  assert.deepEqual(checks, []);
  // the page sends only that value, and the family it names is the one @font-face declares
  assert.match(css, /font-family:\s*"Noto Sans Cherokee"/);
  assert.doesNotMatch(appJs, /queryLocalFonts|navigator\.userAgent|document\.fonts\.forEach|document\.fonts\.values/);
});

// ---------- letter names under the letters (the "Show letter names" option) ----------

const NAMED_WIDTH = 287; // the reading's text box at a 375px viewport: page gutters, card, button padding and borders taken off
const chrLabels = languages.chr.labels;
const longChr = Object.keys(chrLabels).join(""); // all 86 letters in one word: wraps over several lines
const widest = Object.entries(chrLabels).sort((x, y) => y[1].length - x[1].length).slice(0, 6).map(([ch]) => ch).join("");
const namedCases = [
  { code: "chr", cfg: { dir: null, labels: chrLabels }, words: [...samples.chr, ...Object.keys(languages.chr.notes), widest, longChr, "Ꭰ", "ᏣᎳᎩ 1929 (3)", "ᎠᏂ-ᏴᏫ"] },
  { code: "sr", cfg: { dir: null, labels: languages.sr.labels }, words: [...samples.sr, "Џорђе Његош", "Hа"] },
  // right-to-left text with names (a language of the future; the names here only exercise the layout)
  { code: "yi", cfg: { dir: "rtl", labels: { "ש": "shin", "ל": "lamed", "ו": "vav", "ם": "final mem", "ד": "dalet" } }, words: ["שלום", "דער שלום (3)", "שלום ".repeat(12)] },
];

function runNamedHarness(chrome, body) {
  const dir = mkdtempSync(join(tmpdir(), "check-named-"));
  try {
    const page = join(dir, "harness.html");
    writeFileSync(page, `<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=375"><link rel="stylesheet" href="file://${join(publicDir, "style.css")}">
<body><div id="stage" style="width:${NAMED_WIDTH}px"></div><pre id="out"></pre>
<script>
let lang = null, cfg = null;
${examplesSource()}
const cases = ${JSON.stringify(namedCases)};
const stage = document.getElementById("stage");
const found = [];
const hit = (p, q) => Math.min(p.right, q.right) - Math.max(p.left, q.left) > 0.5 && Math.min(p.bottom, q.bottom) - Math.max(p.top, q.top) > 0.5;
${body}
</script>`);
    const dom = execFileSync(chrome, ["--headless=new", "--no-sandbox", "--disable-gpu", "--allow-file-access-from-files",
      "--virtual-time-budget=20000", "--dump-dom", `file://${page}`], { encoding: "utf8", timeout: 120000, maxBuffer: 64 * 1024 * 1024, stdio: ["ignore", "pipe", "ignore"] });
    const m = dom.match(/<pre id="out">([\s\S]*?)<\/pre>/);
    assert.ok(m && m[1].trim(), "the harness page produced no result");
    return JSON.parse(m[1].replace(/&quot;/g, '"').replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&amp;/g, "&"));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test("with letter names on, no name overlaps a letter or another name, at phone width, left to right and right to left", (t) => {
  const chrome = findChrome();
  if (!chrome) {
    if (process.env.CI) assert.fail("CI is set but no Chrome was found");
    t.skip("no Chrome or Chromium found; set CHROME=/path/to/chrome");
    return;
  }
  const result = runNamedHarness(chrome, `
document.fonts.load('32px "Noto Sans Cherokee"', "ᎠᎡᏒ").then((faces) => {
  if (!faces.length || faces.some((f) => f.status !== "loaded")) found.push({ problem: "the Cherokee font did not load" });
  return document.fonts.ready;
}).then(() => {
  let checked = 0;
  for (const c of cases) {
    cfg = c.cfg; lang = c.code;
    for (const text of c.words) {
      const n = letters(text).length;
      for (const diff of [[], [0], [n - 1], [...Array(n).keys()]]) {
        checked++;
        const el = scriptText(text, diff, true);
        stage.replaceChildren(el);
        const where = { code: c.code, text: text.slice(0, 12), diff: diff.length };
        const s = stage.getBoundingClientRect();
        const cells = [...el.querySelectorAll(".cell")];
        const want = letters(text).filter((ch) => cfg.labels[ch] !== undefined);
        if (cells.length !== want.length) found.push({ ...where, problem: "cell count", got: cells.length, want: want.length });
        const parts = [];
        cells.forEach((cell, k) => {
          const g = cell.querySelector(".glyph"), nm = cell.querySelector(".n");
          if (!g || !nm || nm.textContent !== cfg.labels[g.textContent] || g.textContent !== want[k]) { found.push({ ...where, problem: "cell content", at: k }); return; }
          const gr = g.getBoundingClientRect(), nr = nm.getBoundingClientRect(), cr = cell.getBoundingClientRect();
          if (nr.top < gr.bottom - 0.5) found.push({ ...where, problem: "name not under its letter", at: k });
          if (Math.abs((nr.left + nr.right) / 2 - (gr.left + gr.right) / 2) > 1) found.push({ ...where, problem: "name not centred", at: k });
          if (nr.left < cr.left - 0.5 || nr.right > cr.right + 0.5) found.push({ ...where, problem: "name outside its cell", at: k });
          if (cr.left < s.left - 0.5 || cr.right > s.right + 0.5) found.push({ ...where, problem: "outside the column", at: k });
          parts.push(gr, nr);
        });
        for (let i = 0; i < parts.length; i++) for (let j = i + 1; j < parts.length; j++) if (hit(parts[i], parts[j])) found.push({ ...where, problem: "boxes overlap" });
        // (not scrollWidth: a space that hangs at the end of a wrapped line counts in it, though nothing is drawn there; the cell boxes above are the check)
        // text order: each letter, then its name; the name is real text
        const spoken = cells.map((cell) => cell.textContent).join("|");
        if (spoken !== want.map((ch) => ch + cfg.labels[ch]).join("|")) found.push({ ...where, problem: "text order" });
        if (cells.length && getComputedStyle(cells[0].querySelector(".n")).userSelect !== "none") found.push({ ...where, problem: "name is copied with the text" });
        // highlighted letters: the mark is the letter's own box, on the letter and nowhere else
        const marks = [...el.querySelectorAll("mark")];
        const named = new Set(); letters(text).forEach((ch, i) => { if (diff.includes(i) && cfg.labels[ch] !== undefined) named.add(i); });
        if (marks.filter((m) => m.classList.contains("glyph")).length !== named.size) found.push({ ...where, problem: "marks" });
        for (const m of marks) for (const q of parts) { const mr = m.getBoundingClientRect(); if (hit(mr, q) && !(Math.abs(mr.left - q.left) < 0.5 && Math.abs(mr.right - q.right) < 0.5)) found.push({ ...where, problem: "mark over another box" }); }
      }
    }
  }
  document.getElementById("out").textContent = JSON.stringify({ checked, found });
});`);
  assert.ok(result.checked > 60, `only ${result.checked} cases ran`);
  assert.deepEqual(result.found.slice(0, 10), [], `${result.found.length} problems in ${result.checked} renderings`);
});

test("with letter names off, the reading is laid out exactly as before: no cells, same widths", (t) => {
  const chrome = findChrome();
  if (!chrome) {
    if (process.env.CI) assert.fail("CI is set but no Chrome was found");
    t.skip("no Chrome or Chromium found; set CHROME=/path/to/chrome");
    return;
  }
  const result = runNamedHarness(chrome, `
document.fonts.load('32px "Noto Sans Cherokee"', "ᎠᎡᏒ").then(() => document.fonts.ready).then(() => {
  let checked = 0;
  for (const c of cases) {
    for (const text of c.words) {
      const n = letters(text).length;
      for (const diff of [[], [0], [n - 1]]) {
        checked++;
        // a language without labels is the page as it was; the same language with its labels but the option off must match it
        cfg = { dir: c.cfg.dir }; lang = c.code;
        const before = scriptText(text, diff); stage.replaceChildren(before);
        const w0 = before.getBoundingClientRect().width, h0 = before.getBoundingClientRect().height, html0 = before.outerHTML;
        cfg = c.cfg;
        const off = scriptText(text, diff); stage.replaceChildren(off);
        const off2 = scriptText(text, diff, false);
        const r = off.getBoundingClientRect();
        if (off.outerHTML !== html0 || off2.outerHTML !== html0) found.push({ code: c.code, text: text.slice(0, 12), problem: "markup differs" });
        if (Math.abs(r.width - w0) > 0.01 || Math.abs(r.height - h0) > 0.01) found.push({ code: c.code, text: text.slice(0, 12), problem: "size differs", w0, h0, w: r.width, h: r.height });
        if (off.querySelector(".cell, .n") || off.classList.contains("named")) found.push({ code: c.code, text: text.slice(0, 12), problem: "names present" });
      }
    }
  }
  document.getElementById("out").textContent = JSON.stringify({ checked, found });
});`);
  assert.ok(result.checked > 50, `only ${result.checked} cases ran`);
  assert.deepEqual(result.found.slice(0, 10), []);
});

test("a look-alike label the same as the name under the letter is not printed twice; one that adds something is kept", () => {
  // the rule in the source: examples() skips a chip whose text is nameOf(ch) when names are on, and keeps everything else
  assert.match(appJs, /if \(name !== undefined && !\(named && nameOf\(ch\) === name\)\) \{/);
  assert.match(appJs, /examples\(t\[side\], t\[side \+ "_diff"\], lookalikeLabels\(t, side\), markLabels\(t, side\), named\)/);
});

test("the option is offered only for a language that has letter names", () => {
  // the control's own display rule must not override the hidden attribute
  assert.match(css, /\.toggle\[hidden\]\s*\{\s*display:\s*none;/);
  // the control starts hidden, is shown by work() only when the language has labels, and its state is read and written inside try/catch (store())
  assert.match(readFileSync(join(publicDir, "index.html"), "utf8"), /<label class="toggle" id="names-wrap" hidden><input type="checkbox" id="names"> Show letter names<\/label>/);
  assert.match(appJs, /if \(hasNames\(\)\) \{[\s\S]*?\$\("names-wrap"\)\.hidden = false;/);
  assert.match(appJs, /showNames = store\(NAMES\) === "1";/);
  assert.equal(Object.entries(languages).filter(([, c]) => c.labels).map(([k]) => k).sort().join(), "chr,chr-lines,chr-rare,sr");
});

test("a 401 that comes back late doesn't throw away a session another request has already renewed", async () => {
  const start = appJs.indexOf("let memory = {};");
  const end = appJs.indexOf("// ---------- home");
  assert.ok(start > 0 && end > start, "app.js no longer has the markers this test slices between");
  let issued = 0, release;
  const gate = new Promise((ok) => { release = ok; });
  const reply = (status, body) => ({ ok: status < 400, status, statusText: String(status), json: async () => body });
  const fetch = async (path, opts = {}) => {
    if (path === "/api/config") return reply(200, { turnstile_site_key: "k" });
    if (path === "/api/session") return reply(200, { session: "s" + ++issued });
    const tok = opts.headers.authorization.slice("Bearer ".length);
    if (tok === "old") { if (path === "/slow") await gate; return reply(401, {}); }
    return reply(200, { tok });
  };
  const window = { turnstile: { render: (el, o) => { o.callback("ts"); return 1; }, remove() {} } };
  const document = { getElementById: () => ({ classList: { toggle() {} } }) };
  const { authed, store } = new Function("fetch", "window", "document", `const $ = (id) => document.getElementById(id), SESSION = "usnm-check-session";\n${appJs.slice(start, end)}\nreturn { authed, store };`)(fetch, window, document);
  store("usnm-check-session", "old");
  const slow = authed("/slow"), quick = await authed("/quick");
  assert.equal(quick.tok, "s1");
  release();
  assert.equal((await slow).tok, "s1", "the late 401 retried with the renewed session");
  assert.equal(issued, 1, "only one new session was made");
  assert.equal(store("usnm-check-session"), "s1");
});

test("the first session waits for Turnstile's script even when window.turnstile is already something else", async () => {
  const start = appJs.indexOf("let memory = {};");
  const end = appJs.indexOf("// ---------- home");
  assert.ok(start > 0 && end > start, "app.js no longer has the markers this test slices between");
  // the page has no element with id "turnstile" (it would be window.turnstile until the script replaced it)
  assert.doesNotMatch(readFileSync(join(publicDir, "index.html"), "utf8"), /id="turnstile"/);
  const reply = (status, body) => ({ ok: status < 400, status, statusText: String(status), json: async () => body });
  const fetch = async (path) => (path === "/api/config" ? reply(200, { turnstile_site_key: "k" }) : reply(200, { session: "s1" }));
  // what a named element looks like before the script runs: truthy, no render
  const window = { turnstile: { tagName: "DIV" } };
  setTimeout(() => { window.turnstile = { render: (el, o) => { o.callback("ts"); return 1; }, remove() {} }; }, 300);
  const document = { getElementById: () => ({ classList: { toggle() {} } }) };
  const { session } = new Function("fetch", "window", "document", `const $ = (id) => document.getElementById(id), SESSION = "usnm-check-session";\n${appJs.slice(start, end)}\nreturn { session };`)(fetch, window, document);
  assert.equal(await session(), "s1");
});

test("the letter key puts the letters in question first, with their equivalents, and leaves the answer alone", () => {
  const start = appJs.indexOf("const KEY_FONTS"), end = appJs.indexOf("function renderKey(");
  assert.ok(start > 0 && end > start, "app.js no longer has the markers this test slices between");
  const make = (cfg) => new Function(`const cfg = ${JSON.stringify(cfg)};\n${scriptTextSource()}\n${appJs.slice(start, end)}\nreturn { disputed };`)().disputed;
  const de = make({ equivalents: { "ſ": "s", "ꝛ": "r" } });
  assert.deepEqual([...de({ a: "Haus", b: "Hanſ", a_diff: [2, 3], b_diff: [2, 3] })].sort(), ["n", "s", "u", "ſ"].sort());
  assert.deepEqual([...make({})({ a: "cat", b: "cot", a_diff: [1], b_diff: [1] })].sort(), ["a", "o"]);
  assert.equal(make({})({ a: "x", b: "y" }).size, 0, "a task built without diffs");
  // K opens and closes it; the toggle is a button that says whether it is open; the answer sent is unchanged by it
  assert.match(appJs, /e\.key === "k" \|\| e\.key === "K"\) toggleKey\(\)/);
  assert.match(appJs, /setAttribute\("aria-expanded"/);
  assert.match(appJs, /JSON\.stringify\(\{ task: task\.id, choice, display: DISPLAY, font: fontState\(task\), names_shown: namesState\(task\) \}\)/);
  assert.match(appJs, /const DISPLAY = 2;/);
  // a key may only name a bundled typeface
  assert.match(appJs, /if \(KEY_FONTS\.includes\(map\.font\)\)/);
});

test("Neither matches puts the cursor in the suggestion box, and key 3 doesn't type into it", () => {
  // choose() focuses the box it just rendered, whatever picked Neither (click, tap or key)
  assert.match(appJs, /if \(holds\) \{[^\n]*render\(\); const box = \$\("suggest-text"\); if \(box\) box\.focus\(\); return; \}/);
  // the keydown that opens it is cancelled, so its "3" isn't typed into the box that now has focus
  assert.match(appJs, /else if \(e\.key === "3"\) \{ e\.preventDefault\(\); choose\("neither"\); \}/);
  // leaving the word from the box (Enter, Send or Skip) gives the next word's first reading focus, as a focused button would
  assert.match(appJs, /function controlFocused\(\) \{ const a = document\.activeElement; return !!a && \(a\.tagName === "BUTTON" \|\| a\.id === "suggest-text"\); \}/);
});

test("the suggestion box offers fraction buttons that insert at the cursor and keep the box focused", () => {
  assert.match(appJs, /const INSERT_CHARS = \["½", "¼", "¾", "⅓", "⅔", "⅛", "⅜", "⅝", "⅞"\];/);
  assert.match(appJs, /const chars = Array\.isArray\(cfg\.insert_chars\) \? cfg\.insert_chars : INSERT_CHARS;/);
  assert.match(appJs, /b\.addEventListener\("pointerdown", \(e\) => e\.preventDefault\(\)\);/, "the box keeps focus (and a phone its keyboard)");
  assert.match(appJs, /input\.setRangeText\(ch, at, to, "end"\);\s*suggestDraft = input\.value;/, "inserted at the cursor and kept through a re-render");
  // a save in flight locks the box, even one drawn again during it (letter names toggled)
  assert.match(appJs, /if \(input\.readOnly \|\| suggestSending === suggestOpening\) return;/);
  assert.match(appJs, /if \(suggestSending && suggestSending === suggestOpening\) input\.readOnly = true;/);
  // a failed save unlocks the box on screen, not only the one it started from
  assert.match(appJs, /const nowSend = \$\("suggest-send"\), nowInput = \$\("suggest-text"\);\s*if \(nowSend\) nowSend\.disabled = false; if \(nowInput\) nowInput\.readOnly = false;/);
});

test("the suggestion box can start from either reading, in the card's order, replacing the draft with the cursor at the end", () => {
  // the buttons take the readings in the order the card draws them (flip), so a blind task shows nothing new
  const order = '(flip(t.id) ? ["b", "a"] : ["a", "b"])';
  assert.ok(appJs.includes(order + ".forEach((side, k) =>"), "the card's order");
  assert.ok(appJs.includes("const sides = " + order + '.map((side, k) => [k + 1, t[side]])'), "the buttons' order is the card's");
  assert.match(appJs, /b\.id = "suggest-from-" \+ k; b\.textContent = "Reading " \+ k;/);
  // a save in flight ignores it; otherwise the reading replaces the draft and the box keeps focus with the cursor at the end
  assert.match(appJs, /if \(input\.readOnly \|\| suggestSending === suggestOpening\) return;\s*input\.value = text; suggestDraft = text;\s*input\.focus\(\); input\.setSelectionRange\(text\.length, text\.length\);/);
  assert.match(appJs, /box\.append\(label, \.\.\.\(sides\.length \? \[from\] : \[\]\), input, row/);
});

test("a card drawn with another language's configuration than the page's carries that language's rules and notes", () => {
  assert.match(appJs, /function ownHelp\(card\) \{\s*if \(!cfg \|\| cfg === pageCfg\) return;/);
  assert.match(appJs, /function renderCover\(card, t, prev\) \{\n[^\n]*\n  ownHelp\(card\);/);
  assert.match(appJs, /function renderGutter\(card, t, prev\) \{\n[^\n]*\n  ownHelp\(card\);/);
  assert.match(appJs, /\$\{l\.unit \|\| "words"\}/, "the home list names lines, blocks or items, not always words");
});

test("Back to a word answered Neither shows Neither and reopens its box with its text, and brings its note back", () => {
  assert.equal((appJs.match(/history\.push\(\{ task, choice: "neither" \}\)/g) || []).length, 1, "every Neither leaves through pushNeither");
  assert.match(appJs, /function pushNeither\(\) \{\n[^\n]*\n  keepTyped\(task\.id, \{ suggest: suggestDraft \}\);/);
  assert.match(appJs, /if \(h\.choice === "neither" && !isGutter\(task\) && !isCover\(task\)\) \{ suggestFor = task\.id; suggestOpening = \{\}; suggestDraft = k\.suggest \|\| ""; \}/);
  assert.match(appJs, /noteDone\.add\(id\); keepTyped\(id, \{ saved: text \}\);/, "a saved note, apart from the draft");
  assert.match(appJs, /if \(task\) keepTyped\(task\.id, noteFor === task\.id && noteDraft\.trim\(\) \? \{ draft: noteDraft, noteOpen: true \} : \{ draft: null, noteOpen: false \}\);/, "an open draft, or none once cleared or cancelled");
  assert.match(appJs, /for \(const k of \[\.\.\.typed\.keys\(\)\]\) if \(typed\.size > 60 && !reach\.has\(k\)\) typed\.delete\(k\);/, "words Back can reach are never dropped");
  assert.match(appJs, /\$\(id\)\.classList\.toggle\("chosen", prev === c\)/, "Neither, Can't tell, Not text and Bad crop show the answer given");
});

test("the character buttons stay hidden behind a keyboard button, which this browser remembers", () => {
  assert.match(appJs, /const open = store\(CHARSOPEN\) === "1";\s*pad\.hidden = !open;/, "hidden until asked for");
  assert.match(appJs, /toggle\.setAttribute\("aria-controls", "suggest-chars"\); toggle\.setAttribute\("aria-expanded", String\(open\)\);/);
  assert.match(appJs, /toggle\.addEventListener\("pointerdown", \(e\) => e\.preventDefault\(\)\);/, "opening it keeps the box focused");
  assert.match(appJs, /pad\.hidden = !show; toggle\.setAttribute\("aria-expanded", String\(show\)\); store\(CHARSOPEN, show \? "1" : "0"\);/);
  assert.match(css, /\.suggest-chars\[hidden\] \{ display: none; \}/, "the flex rule doesn't override hidden");
  // a combining mark (a Yiddish point) is drawn on a dotted circle and named from the language's marks
  assert.match(appJs, /b\.textContent = mark \? "\\u25CC" \+ ch : ch;/);
});

test("a language's character buttons can be rows: a chart whose columns line up, each key with its name", () => {
  assert.match(appJs, /const rows = chars\.some\(Array\.isArray\) \? chars\.map\(\(r\) => \(Array\.isArray\(r\) \? r : \[r\]\)\) : null;/);
  assert.match(appJs, /pad\.style\.setProperty\("--cols", String\(Math\.max\(\.\.\.rows\.map\(\(r\) => r\.length\)\)\)\);/, "every row has as many columns as the longest");
  assert.match(appJs, /const name = rows && !mark && nameOf\(ch\);/, "a chart key shows the letter's label under it");
  assert.match(appJs, /\+ \(name \? ", " \+ name : ""\)\);/, "and says it");
  assert.match(css, /\.suggest-chars-row \{ display: grid; grid-template-columns: repeat\(var\(--cols\), minmax\(0, 1fr\)\);/, "columns shrink to fit a phone");
});

test("the enlarged word is never drawn smaller than the same word in the line above it", () => {
  assert.match(appJs, /const WORD_OVER_LINE = 1\.5, WORD_MAX_HEIGHT = 120;/);
  assert.match(appJs, /const scale = WORD_OVER_LINE \* Math\.max\(w \* li\.clientWidth \/ wi\.naturalWidth, h \* li\.clientHeight \/ wi\.naturalHeight\);/, "held to the word's box in the line on the side that needs more");
  assert.match(appJs, /const width = Math\.min\(Math\.max\(own, wi\.naturalWidth \* scale\), room\);/, "never past the strip");
  assert.match(appJs, /li\.addEventListener\("load", fit\); wi\.addEventListener\("load", fit\);/, "sized once both images are in");
  assert.match(appJs, /new ResizeObserver\(fit\)\.observe\(lv\);/, "and again when the line is drawn at another width");
});
