// The font the site serves for Cherokee is the one public/fonts/README.md describes: same bytes, licence beside it.
// Run: node --test edge/check/fonts.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const fonts = join(dirname(fileURLToPath(import.meta.url)), "public", "fonts");
const sha = (file) => createHash("sha256").update(readFileSync(join(fonts, file))).digest("hex");
const readme = readFileSync(join(fonts, "README.md"), "utf8");

test("the served Cherokee font and its licence match the hashes in the README", () => {
  for (const file of ["NotoSansCherokee-Regular.woff2", "OFL.txt"]) {
    const hash = sha(file);
    assert.ok(readme.includes(hash), `${file} (sha256 ${hash}) is not the file public/fonts/README.md describes`);
  }
});

test("the licence is the SIL Open Font License 1.1 and the README names the source and version", () => {
  const ofl = readFileSync(join(fonts, "OFL.txt"), "utf8");
  assert.match(ofl, /SIL OPEN FONT LICENSE Version 1\.1/);
  assert.match(ofl, /Noto Project Authors/);
  assert.match(readme, /github\.com\/notofonts\/cherokee/);
  assert.match(readme, /NotoSansCherokee-v2\.001/);
});

test("the font file is a woff2", () => {
  assert.equal(readFileSync(join(fonts, "NotoSansCherokee-Regular.woff2")).subarray(0, 4).toString("latin1"), "wOF2");
});
