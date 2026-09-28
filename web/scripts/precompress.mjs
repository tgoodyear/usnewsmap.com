// Write .br and .gz copies of the built text files next to them, so the
// API serves them without compressing on each request (usnm-api `site`).
// Usage: node scripts/precompress.mjs dist
import { readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { brotliCompressSync, constants, gzipSync } from "node:zlib";

const TEXT = /\.(html|js|mjs|css|svg|json|map|txt|webmanifest)$/;
const MIN_BYTES = 1024;

function* files(dir) {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) yield* files(path);
    else yield path;
  }
}

const root = process.argv[2] ?? "dist";
let count = 0;
for (const path of files(root)) {
  if (!TEXT.test(path)) continue;
  const data = readFileSync(path);
  if (data.length < MIN_BYTES) continue;
  writeFileSync(`${path}.br`, brotliCompressSync(data, {
    params: { [constants.BROTLI_PARAM_QUALITY]: 11, [constants.BROTLI_PARAM_SIZE_HINT]: data.length },
  }));
  writeFileSync(`${path}.gz`, gzipSync(data, { level: 9 }));
  count++;
}
console.log(`precompressed ${count} files in ${root}`);
