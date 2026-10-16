// Census cartographic boundaries are a public-domain US federal government work.
import { parseArgs } from "node:util";
import { readFile, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { gzipSync } from "node:zlib";
import mapshaper from "mapshaper";

const { values } = parseArgs({
  options: {
    input: { type: "string" },
    output: { type: "string" },
    names: { type: "string" },
  },
});
if (!values.input || !values.output || !values.names) {
  throw new Error("Usage: node scripts/prepare-states.mjs --input FILE.zip --output FILE.geojson --names FILE.json");
}
// Pin the source bytes as well as the tool version (package-lock.json).
const sha = createHash("sha256")
  .update(await readFile(values.input))
  .digest("hex");
if (sha !== "c9db0e395c11a1f94a8017fde4f4c7cbee1dca6eb37ba8f1ccaab927df70885f")
  throw new Error(`Unexpected Census source SHA256: ${sha}`);
await mapshaper.runCommands(
  `-i ${JSON.stringify(values.input)} -simplify 3% keep-shapes ` +
    `-filter-fields STUSPS,NAME -sort STUSPS ` +
    `-o ${JSON.stringify(values.output)} format=geojson precision=0.001`,
);
const bytes = await readFile(values.output);
const collection = JSON.parse(bytes);
if (collection.features.length !== 56) throw new Error("Expected 50 states, DC and five territories");
await writeFile(
  values.names,
  JSON.stringify(
    Object.fromEntries(collection.features.map((f) => [f.properties.STUSPS, f.properties.NAME])),
    null,
    2,
  ) + "\n",
);
const size = gzipSync(bytes, { level: 9 }).length;
if (size > 100_000) throw new Error(`Boundary exceeds 100 KB gzip: ${size}`);
console.log(`${collection.features.length} boundaries; ${size} bytes gzip`);
