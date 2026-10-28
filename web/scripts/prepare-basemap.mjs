import { createHash } from "node:crypto";
import { createReadStream, existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, rename, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { createWriteStream } from "node:fs";

const web = fileURLToPath(new URL("../", import.meta.url));
const manifest = JSON.parse(await readFile(join(web, "basemap.json"), "utf8"));
const destination = join(web, "public/basemap", manifest.version);
const digest = async (path) => {
  const hash = createHash("sha256");
  await pipeline(createReadStream(path), hash);
  return hash.digest("hex");
};
async function verify(root) {
  const hashes = JSON.parse(await readFile(join(root, "checksums.json"), "utf8"));
  for (const [name, expected] of Object.entries(hashes)) {
    const path = resolve(root, name);
    if (!path.startsWith(resolve(root) + sep)) throw new Error("Invalid basemap asset path");
    if ((await digest(path)) !== expected) throw new Error(`Basemap checksum mismatch: ${name}`);
  }
}
if (existsSync(join(destination, ".verified"))) {
  const marker = await readFile(join(destination, ".verified"), "utf8");
  if (marker === manifest.sha256) {
    await verify(destination);
    console.log(`basemap verified: ${manifest.version}`);
    process.exit(0);
  }
  throw new Error("Existing basemap version differs; publish a new version instead");
}
await mkdir(dirname(destination), { recursive: true });
const temp = await mkdtemp(join(dirname(destination), ".install-"));
try {
  const archive = join(temp, "bundle.tar.gz");
  const response = await fetch(manifest.url, { signal: AbortSignal.timeout(120_000) });
  if (!response.ok || !response.body) throw new Error(`Basemap download failed: HTTP ${response.status}`);
  let bytes = 0;
  const limit = async function* (source) {
    for await (const chunk of source) {
      bytes += chunk.length;
      if (bytes > manifest.bytes) throw new Error("Basemap download exceeds pinned size");
      yield chunk;
    }
  };
  await pipeline(Readable.fromWeb(response.body), limit, createWriteStream(archive));
  if (bytes !== manifest.bytes || (await digest(archive)) !== manifest.sha256) {
    throw new Error("Basemap bundle checksum/size mismatch");
  }
  execFileSync("tar", ["-xzf", archive, "-C", temp]);
  const extracted = join(temp, manifest.version);
  await verify(extracted);
  await writeFile(join(extracted, ".verified"), manifest.sha256);
  await mkdir(dirname(destination), { recursive: true });
  await rename(extracted, destination);
  console.log(`basemap installed: ${manifest.version} (${bytes} bytes)`);
} finally {
  await rm(temp, { recursive: true, force: true });
}
