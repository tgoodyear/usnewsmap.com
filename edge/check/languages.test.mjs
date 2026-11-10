// node --test edge/check/languages.test.mjs
// The language configurations (languages/<code>.json) that sync-languages.sh sends to the Worker
// and from_tap_data.py builds from: the fields the page reads have the types it expects, and keys
// are sorted so diffs stay small.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";

const dir = new URL("./languages/", import.meta.url);
const files = readdirSync(dir).filter((f) => f.endsWith(".json")).sort();
const KNOWN = new Set(["dir", "drop_examples", "equivalents", "intro", "min_examples", "name", "notes", "title"]);
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
    for (const key of ["notes", "equivalents"]) if (key in cfg) assert.ok(strings(cfg[key]), `${key} maps letters to text`);
    if ("drop_examples" in cfg) assert.equal(typeof cfg.drop_examples, "string");
    if ("min_examples" in cfg) assert.ok(Number.isInteger(cfg.min_examples) && cfg.min_examples > 0);
    // Keys in ascending order at every level, as written by json.dumps(sort_keys=True).
    const sorted = (v) => (v && typeof v === "object" && !Array.isArray(v)
      ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, sorted(v[k])])) : v);
    assert.deepEqual(Object.keys(cfg), Object.keys(sorted(cfg)), "top-level keys are sorted");
    for (const key of ["notes", "equivalents"]) if (key in cfg) assert.deepEqual(Object.keys(cfg[key]), Object.keys(cfg[key]).slice().sort(), `${key} keys are sorted`);
  });
}

test("a synced configuration keeps the site's examples and drops the build options", async () => {
  const { desired, canonical } = await import("./sync-languages.mjs");
  const live = { examples: { "Ꭰ": ["chr/x.png"] }, intro: "old", name: "Cherokee", title: "T" };
  const want = desired({ drop_examples: "Ꮛ", intro: "new", name: "Cherokee", title: "T" }, live);
  assert.deepEqual(want, { examples: { "Ꭰ": ["chr/x.png"] }, intro: "new", name: "Cherokee", title: "T" });
  assert.equal(canonical({ b: 1, a: [2, { d: 3, c: 4 }] }), canonical({ a: [2, { c: 4, d: 3 }], b: 1 }));
  assert.notEqual(canonical(want), canonical(live));
  assert.equal(canonical(desired({ intro: "old", name: "Cherokee", title: "T" }, live)), canonical(live));
});
