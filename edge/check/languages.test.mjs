// node --test edge/check/languages.test.mjs
// The language configurations (languages/<code>.json) that sync-languages.sh sends to the Worker
// and from_tap_data.py builds from: the fields the page reads have the types it expects, and keys
// are sorted so diffs stay small.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";

const dir = new URL("./languages/", import.meta.url);
const files = readdirSync(dir).filter((f) => f.endsWith(".json")).sort();
const KNOWN = new Set(["by_paper", "confusions", "dir", "drop_examples", "equivalents", "glyph_font", "glyph_ligatures", "glyphs", "guidance", "insert_chars", "intro", "labels", "links", "marks", "min_examples", "name", "notes", "question", "rules", "title"]);
// The period typefaces public/style.css bundles for letter keys, and public/app.js accepts in a key (KEY_FONTS there).
const KEY_FONTS = ["UnifrakturMaguntia", "IM Fell English"];
const strings = (obj) => obj && typeof obj === "object" && !Array.isArray(obj) && Object.values(obj).every((v) => typeof v === "string" && v);

test("there are language configurations", () => assert.ok(files.length > 0));

for (const file of files) {
  test(`languages/${file}`, () => {
    const code = file.replace(/\.json$/, "");
    // The same code pattern as the Worker's /api/languages/<code> and /admin/languages/<code> routes.
    assert.match(code, /^[a-z]{2,3}(?:-[A-Za-z0-9]{2,8})*$/);
    const text = readFileSync(new URL(file, dir), "utf8");
    const cfg = JSON.parse(text);
    for (const key of Object.keys(cfg)) assert.ok(KNOWN.has(key), `unknown field ${key}`);
    assert.ok(!("examples" in cfg), "examples are built by from_tap_data.py, not written here");
    for (const key of ["name", "title", "intro"]) assert.ok(typeof cfg[key] === "string" && cfg[key].trim(), `${key} is required`);
    if ("dir" in cfg) assert.equal(cfg.dir, "rtl");
    for (const key of ["notes", "equivalents", "labels", "marks"]) if (key in cfg) assert.ok(strings(cfg[key]), `${key} maps letters to text`);
    // a mark is one combining character, named by a short plain word or two
    for (const [mark, name] of Object.entries(cfg.marks || {})) {
      assert.equal(mark.length, 1, `marks key ${JSON.stringify(mark)} is one character`);
      assert.match(mark, /^\p{M}$/u, `marks key ${JSON.stringify(mark)} is a combining mark`);
      assert.match(name, /^[\p{L} ]{1,20}$/u, `the name of ${JSON.stringify(mark)} is short, plain words`);
      assert.ok(!/\bhebrew\b|\bpoint\b|\bletter\b|\bpoint\b/i.test(name), "not the Unicode character name");
    }
    // a label is the short plain text shown under a flagged letter: one letter (with its marks) named by letters, no Unicode names
    for (const [letter, label] of Object.entries(cfg.labels || {})) {
      assert.ok([...new Intl.Segmenter().segment(letter)].length === 1, `labels key ${JSON.stringify(letter)} is one letter`);
      assert.match(label, /^[\p{L} ]{1,20}$/u, `the label of ${JSON.stringify(letter)} is short, plain words`);
      assert.ok(!/\bcyrillic\b|\bhebrew\b|\bletter\b|\bsmall\b|\bcapital\b/i.test(label), "not the Unicode character name");
    }
    for (const key of ["question", "rules"]) if (key in cfg) assert.ok(typeof cfg[key] === "string" && cfg[key].trim(), `${key} is text`);
    if ("guidance" in cfg) assert.ok(Array.isArray(cfg.guidance) && cfg.guidance.every((g) => typeof g === "string" && g.trim()), "guidance is a list of text");
    // Further reading shown under the intro: https links with plain text.
    if ("links" in cfg) {
      assert.ok(Array.isArray(cfg.links) && cfg.links.length > 0, "links is a list");
      for (const l of cfg.links) {
        assert.deepEqual(Object.keys(l).sort(), ["href", "text"], "a link has href and text");
        assert.match(l.href, /^https:\/\/[^\s"<>]+$/, `${l.href} is an https address`);
        assert.ok(typeof l.text === "string" && l.text.trim(), "a link has text");
      }
    }
    // One newspaper at a time (README.md): a flag, and the letter pairs the paper's letter guide shows side by side.
    if ("by_paper" in cfg) assert.equal(cfg.by_paper, true, "by_paper is true or absent");
    if ("confusions" in cfg) {
      assert.ok(Array.isArray(cfg.confusions) && cfg.confusions.length > 0, "confusions is a list of pairs");
      for (const pair of cfg.confusions) {
        assert.ok(Array.isArray(pair) && pair.length === 2 && pair[0] !== pair[1], `${JSON.stringify(pair)} is a pair of two letters`);
        for (const g of pair) assert.ok(typeof g === "string" && [...new Intl.Segmenter().segment(g)].length === 1, `${JSON.stringify(g)} is one letter`);
      }
    }
    if ("drop_examples" in cfg) assert.equal(typeof cfg.drop_examples, "string");
    // Build options for tools/letter_key.py: the letters of the language's letter key, and the bundled period typeface
    // (public/style.css, @font-face) that draws a letter no page of the paper gave a clean picture of.
    if ("glyphs" in cfg) {
      assert.ok(Array.isArray(cfg.glyphs) && cfg.glyphs.length > 0, "glyphs is a list of letters");
      for (const g of cfg.glyphs) assert.ok(typeof g === "string" && [...new Intl.Segmenter().segment(g)].length === 1, `glyph ${JSON.stringify(g)} is one letter`);
      assert.equal(new Set(cfg.glyphs).size, cfg.glyphs.length, "glyphs has no repeats");
    }
    if ("glyph_ligatures" in cfg) assert.ok(Array.isArray(cfg.glyph_ligatures) && cfg.glyph_ligatures.every((l) => typeof l === "string" && [...l].length >= 1), "glyph_ligatures is a list of letter runs (one letter: never cut out)");
    if ("glyph_font" in cfg) assert.ok(KEY_FONTS.includes(cfg.glyph_font), `glyph_font is one of the bundled ${KEY_FONTS.join(", ")}`);
    if ("insert_chars" in cfg) {
      assert.ok(Array.isArray(cfg.insert_chars), "insert_chars is a list");
      // a list of characters, or of rows (lists of characters) laid out as a chart
      for (const r of cfg.insert_chars) if (Array.isArray(r)) assert.ok(r.length > 0 && r.every((c) => typeof c === "string"), "an insert_chars row is a non-empty list of characters");
      for (const c of cfg.insert_chars.flat()) {
        assert.ok(typeof c === "string" && [...new Intl.Segmenter().segment(c)].length === 1, `insert_chars entry ${JSON.stringify(c)} is one character as written`);
        assert.equal(c, c.normalize("NFC"), `insert_chars entry ${JSON.stringify(c)} is NFC, as the Worker stores suggestions`);
      }
      assert.equal(new Set(cfg.insert_chars.flat()).size, cfg.insert_chars.flat().length, "insert_chars has no repeats");
    }
    // Cherokee: the 85 letters of the syllabary (Ᏽ mv had dropped out of print before the Phoenix), each once, as a chart with a
    // consonant per row and the vowels a e i o u v in the first six columns; the extra syllables (ka, hna, nah, s, dla) follow.
    if (code.startsWith("chr")) {
      const rows = cfg.insert_chars, flat = rows.flat();
      assert.ok(rows.every(Array.isArray), "the Cherokee keys are rows");
      assert.equal(flat.length, 85);
      assert.deepEqual(flat.slice().sort(), Object.keys(cfg.labels).filter((c) => c !== "Ᏽ").sort(), "every letter but Ᏽ, each with its syllable");
      for (const r of rows) r.slice(0, 6).forEach((c, i) => assert.ok(cfg.labels[c].endsWith("aeiouv"[i]), `${c} (${cfg.labels[c]}) is in the ${"aeiouv"[i]} column`));
    }
    if ("min_examples" in cfg) assert.ok(Number.isInteger(cfg.min_examples) && cfg.min_examples > 0);
    // Keys in ascending order at every level, as written by json.dumps(sort_keys=True).
    const sorted = (v) => (v && typeof v === "object" && !Array.isArray(v)
      ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, sorted(v[k])])) : v);
    assert.deepEqual(Object.keys(cfg), Object.keys(sorted(cfg)), "top-level keys are sorted");
    for (const key of ["notes", "equivalents", "labels", "marks"]) if (key in cfg) assert.deepEqual(Object.keys(cfg[key]), Object.keys(cfg[key]).slice().sort(), `${key} keys are sorted`);
  });
}

test("a synced configuration keeps the site's examples and drops the build options", async () => {
  const { desired, canonical, sent } = await import("./sync-languages.mjs");
  const live = { examples: { "Ꭰ": ["chr/x.png"] }, intro: "old", name: "Cherokee", title: "T" };
  const want = desired({ drop_examples: "Ꮛ", intro: "new", name: "Cherokee", title: "T" }, live);
  assert.deepEqual(want, { examples: { "Ꭰ": ["chr/x.png"] }, intro: "new", name: "Cherokee", title: "T" });
  assert.equal(canonical({ b: 1, a: [2, { d: 3, c: 4 }] }), canonical({ a: [2, { c: 4, d: 3 }], b: 1 }));
  assert.notEqual(canonical(want), canonical(live));
  assert.equal(canonical(desired({ intro: "old", name: "Cherokee", title: "T" }, live)), canonical(live));
  // The Worker keeps the stored examples itself, so sync never sends a copy of them.
  assert.deepEqual(sent({ drop_examples: "Ꮛ", examples: {}, intro: "new", name: "Cherokee" }), { intro: "new", name: "Cherokee" });
  assert.deepEqual(sent({ glyph_font: "UnifrakturMaguntia", glyph_ligatures: ["ch"], glyphs: ["a"], intro: "new", name: "German" }), { intro: "new", name: "German" });
});

test("Serbian labels give the Latin equivalent of every Cyrillic letter, both cases", () => {
  const sr = JSON.parse(readFileSync(new URL("sr.json", dir), "utf8"));
  const table = "а a,б b,в v,г g,д d,ђ đ,е e,ж ž,з z,и i,ј j,к k,л l,љ lj,м m,н n,њ nj,о o,п p,р r,с s,т t,ћ ć,у u,ф f,х h,ц c,ч č,џ dž,ш š";
  const want = {};
  for (const pair of table.split(",")) {
    const [cyr, lat] = pair.split(" ");
    want[cyr] = lat;
    want[cyr.toUpperCase()] = lat.length > 1 ? lat[0].toUpperCase() + lat.slice(1) : lat.toUpperCase();
  }
  assert.equal(Object.keys(want).length, 60);
  assert.deepEqual(sr.labels, Object.fromEntries(Object.keys(want).sort().map((k) => [k, want[k]])));
});

test("the three Cherokee files carry the same letter names, one for each syllabary letter U+13A0 to U+13F5", () => {
  const read = (f) => JSON.parse(readFileSync(new URL(f, dir), "utf8")).labels;
  const chr = read("chr.json");
  assert.deepEqual(read("chr-lines.json"), chr);
  assert.deepEqual(read("chr-rare.json"), chr);
  const letters = Array.from({ length: 0x13f6 - 0x13a0 }, (_, i) => String.fromCodePoint(0x13a0 + i));
  assert.equal(letters.length, 86);
  assert.deepEqual(Object.keys(chr), letters, "every syllabary letter, in code point order (which is the sorted order)");
  for (const [letter, name] of Object.entries(chr)) assert.match(name, /^[a-z]{1,3}$/, `the name of ${letter} is a syllable`);
  assert.equal(new Set(Object.values(chr)).size, 86, "no two letters share a name");
  // tools/test_cherokee_labels.py checks the names against the Unicode character names
});
