// node --test edge/check/languages.test.mjs
// The language configurations (languages/<code>.json) that sync-languages.sh sends to the Worker
// and from_tap_data.py builds from: the fields the page reads have the types it expects, and keys
// are sorted so diffs stay small.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";

const dir = new URL("./languages/", import.meta.url);
const files = readdirSync(dir).filter((f) => f.endsWith(".json")).sort();
const KNOWN = new Set(["dir", "drop_examples", "equivalents", "intro", "labels", "marks", "min_examples", "name", "notes", "title"]);
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
    if ("drop_examples" in cfg) assert.equal(typeof cfg.drop_examples, "string");
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
