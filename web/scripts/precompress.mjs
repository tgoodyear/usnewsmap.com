// Write .br and .gz copies of the built text files next to them, so the
// API serves them without compressing on each request (usnm-api `site`).
// The files are compressed at once on libuv's thread pool, so the largest
// (the map bundle and its source map) don't wait for each other.
// Usage: node scripts/precompress.mjs dist
import { readdirSync, readFileSync, statSync } from "node:fs";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import { promisify } from "node:util";
import { brotliCompress, constants, gzip } from "node:zlib";

const brotli = promisify(brotliCompress);
const gz = promisify(gzip);

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
const jobs = [];
for (const path of files(root)) {
  if (!TEXT.test(path)) continue;
  const data = readFileSync(path);
  if (data.length < MIN_BYTES) continue;
  jobs.push(
    brotli(data, {
      params: { [constants.BROTLI_PARAM_QUALITY]: 11, [constants.BROTLI_PARAM_SIZE_HINT]: data.length },
    }).then((out) => writeFile(`${path}.br`, out)),
    gz(data, { level: 9 }).then((out) => writeFile(`${path}.gz`, out)),
  );
}
await Promise.all(jobs);
console.log(`precompressed ${jobs.length / 2} files in ${root}`);
