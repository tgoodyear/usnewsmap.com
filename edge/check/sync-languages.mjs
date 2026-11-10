// Sends languages/<code>.json to check.usnewsmap.com for every language the site already has, so a
// merged change to a name, intro or letter note goes live with the deploy (deploy.sh runs this).
// The site's `examples` (letter pictures that from_tap_data.py builds from the clips) are kept, and
// the build-only options are left out, as from_tap_data.py does. A language the site doesn't have
// yet is skipped: it needs its tasks and clips, through load.sh.
//
//   CHECK_ADMIN_TOKEN=... node edge/check/sync-languages.mjs [--site https://check.usnewsmap.com] [--dry-run]
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";

const BUILD_ONLY = ["drop_examples", "min_examples"];

// What the site should hold for a language: the repository's fields, the site's examples.
export function desired(repo, live) {
  const cfg = Object.fromEntries(Object.entries(repo).filter(([k]) => !BUILD_ONLY.includes(k)));
  if (live && "examples" in live) cfg.examples = live.examples;
  return cfg;
}

// Key order doesn't matter to the page, so two configurations compare with their keys sorted.
export function canonical(v) {
  if (Array.isArray(v)) return `[${v.map(canonical).join(",")}]`;
  if (v && typeof v === "object") return `{${Object.keys(v).sort().map((k) => `${JSON.stringify(k)}:${canonical(v[k])}`).join(",")}}`;
  return JSON.stringify(v);
}

async function main(argv) {
  let site = "https://check.usnewsmap.com", dryRun = false;
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--site") site = argv[++i];
    else if (argv[i] === "--dry-run") dryRun = true;
    else throw new Error(`unknown option ${argv[i]}`);
  }
  if (!/^https:\/\/[a-z0-9.-]+$/.test(site || "")) throw new Error("bad --site");
  const token = process.env.CHECK_ADMIN_TOKEN;
  if (!token && !dryRun) throw new Error("CHECK_ADMIN_TOKEN is required");
  const dir = new URL("./languages/", import.meta.url);
  for (const file of readdirSync(dir).filter((f) => f.endsWith(".json")).sort()) {
    const code = file.replace(/\.json$/, "");
    const repo = JSON.parse(readFileSync(new URL(file, dir), "utf8"));
    const res = await fetch(`${site}/api/languages/${code}?sync=${Date.now()}`, { signal: AbortSignal.timeout(20000) });
    if (res.status === 404) { console.log(`${code}: not on the site yet (load it with load.sh), skipped`); continue; }
    if (!res.ok) throw new Error(`${code}: GET answered ${res.status}`);
    const live = await res.json();
    const want = desired(repo, live);
    if (canonical(want) === canonical(live)) { console.log(`${code}: up to date`); continue; }
    const changed = [...new Set([...Object.keys(want), ...Object.keys(live)])].filter((k) => canonical(want[k]) !== canonical(live[k]));
    if (dryRun) { console.log(`${code}: would update ${changed.join(", ")}`); continue; }
    const put = await fetch(`${site}/admin/languages/${code}`, {
      method: "PUT",
      headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
      body: JSON.stringify(want),
      signal: AbortSignal.timeout(20000),
    });
    if (!put.ok) throw new Error(`${code}: PUT answered ${put.status}`);
    console.log(`${code}: updated ${changed.join(", ")}`);
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).catch((e) => { console.error(`error: ${e.message}`); process.exit(1); });
}
