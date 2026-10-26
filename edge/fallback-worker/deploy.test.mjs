// node --test edge/fallback-worker/deploy.test.mjs
//
// Runs deploy.sh with az and npx replaced by stubs in a directory of each test's own.
// Nothing here reaches Azure, npm, Wrangler or Cloudflare.

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const DEPLOY = fileURLToPath(new URL("./deploy.sh", import.meta.url));
const CONFIG = fileURLToPath(new URL("./wrangler.jsonc", import.meta.url));
const ORIGIN = "https://ca-usnm-prod.example-env.eastus2.azurecontainerapps.io";
const TOKEN = "fake-workers-token-0123456789";
const FLAGS = {
  "--account-id": "0123456789abcdef0123456789abcdef",
  "--azure-origin": ORIGIN,
  "--wrangler-version": "4.152.0",
  "--vault": "kv-example",
  "--token-secret": "example-secret",
};
// The flags above, with changes: a value replaces one, null leaves one out.
function flags(changes = {}) {
  return Object.entries({ ...FLAGS, ...changes }).flatMap(([name, value]) => (value === null ? [] : [name, value]));
}
const COMMON = flags();

const STUBS = {
  az: `#!/bin/sh
echo "az $*" >> "$STUB_DIR/calls.log"
[ -z "\${STUB_AZ_FAIL:-}" ] || exit 1
printf '%s\\n' "\${STUB_TOKEN-${TOKEN}}"
`,
  npx: `#!/bin/sh
echo "npx $*" >> "$STUB_DIR/calls.log"
{
  echo "token=\${CLOUDFLARE_API_TOKEN:-}"
  echo "account=\${CLOUDFLARE_ACCOUNT_ID:-}"
  echo "metrics=\${WRANGLER_SEND_METRICS:-}"
} > "$STUB_DIR/env.log"
[ -z "\${STUB_NPX_FAIL:-}" ] || exit 3
`,
};

function deploy(args, extraEnv = {}) {
  const dir = mkdtempSync(join(tmpdir(), "deploy-test-"));
  try {
    mkdirSync(join(dir, "bin"));
    for (const [name, body] of Object.entries(STUBS)) {
      writeFileSync(join(dir, "bin", name), body);
      chmodSync(join(dir, "bin", name), 0o755);
    }
    const result = spawnSync("bash", [DEPLOY, ...args], {
      cwd: dir,
      encoding: "utf8",
      env: { ...process.env, PATH: `${join(dir, "bin")}:${process.env.PATH}`, STUB_DIR: dir, ...extraEnv },
    });
    const read = (name) => (existsSync(join(dir, name)) ? readFileSync(join(dir, name), "utf8") : "");
    return { ...result, calls: read("calls.log"), env: read("env.log") };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test("a production dry run prints the deployment and calls nothing", () => {
  const result = deploy([...COMMON, "--production", "--dry-run"]);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /wrangler@4\.152\.0 deploy --config \S*edge\/fallback-worker\/wrangler\.jsonc/);
  assert.ok(result.stdout.includes(`--var AZURE_FALLBACK_ORIGIN:${ORIGIN}`), result.stdout);
  assert.doesNotMatch(result.stdout, /--route|--name|ADD_DNT/);
  assert.match(result.stdout, /dry run: nothing read or sent/);
  assert.equal(result.calls, "");
});

test("a dry run needs no vault or secret", () => {
  const result = deploy([...flags({ "--vault": null, "--token-secret": null }), "--production", "--dry-run"]);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.calls, "");
});

test("a drill dry run deploys the drill Worker to its one hostname with DNT on", () => {
  const result = deploy([...COMMON, "--drill-hostname", "test.usnewsmap.com", "--dry-run"]);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /--name usnm-fallback-drill/);
  assert.match(result.stdout, /--route test\.usnewsmap\.com\/\\\*/);
  assert.match(result.stdout, /--var ADD_DNT:true/);
  assert.equal(result.calls, "");
});

test("it is not run from the repository to find its files: any directory works", () => {
  // deploy() runs in a scratch directory; the config path is still the repository's file.
  const result = deploy([...COMMON, "--production", "--dry-run"]);
  assert.ok(result.stdout.includes(CONFIG), result.stdout);
});

test("a deploy needs one of --production and --drill-hostname, not both, not neither", () => {
  for (const args of [
    [...COMMON, "--dry-run"],
    [...COMMON, "--production", "--drill-hostname", "test.usnewsmap.com", "--dry-run"],
  ]) {
    const result = deploy(args);
    assert.equal(result.status, 2);
    assert.match(result.stderr, /error:/);
    assert.equal(result.calls, "");
  }
});

test("it refuses what it cannot check, and calls nothing", () => {
  const sent = (changes) => [...flags(changes), "--production", "--dry-run"];
  const refused = [
    ["a positional argument", [...sent({}), "extra"]],
    ["an unknown flag", [...sent({}), "--force"]],
    ["a flag without its value", [...sent({}), "--vault"]],
    ["no account id", sent({ "--account-id": null })],
    ["a short account id", sent({ "--account-id": "0123" })],
    ["an uppercase account id", sent({ "--account-id": "0123456789ABCDEF0123456789ABCDEF" })],
    ["no Azure origin", sent({ "--azure-origin": null })],
    ["an http Azure origin", sent({ "--azure-origin": ORIGIN.replace("https", "http") })],
    ["an Azure origin with a path", sent({ "--azure-origin": `${ORIGIN}/v1` })],
    ["an Azure origin with a slash", sent({ "--azure-origin": `${ORIGIN}/` })],
    ["an Azure origin with a port", sent({ "--azure-origin": `${ORIGIN}:8443` })],
    ["a host that is not Azure's", sent({ "--azure-origin": "https://usnewsmap.com" })],
    ["a lookalike host", sent({ "--azure-origin": `${ORIGIN}.example.com` })],
    ["no Wrangler version", sent({ "--wrangler-version": null })],
    ["a moving Wrangler tag", sent({ "--wrangler-version": "latest" })],
    ["a major version alone", sent({ "--wrangler-version": "4" })],
    ["a version with a suffix", sent({ "--wrangler-version": "4.152.0-beta.1" })],
  ];
  for (const [what, args] of refused) {
    const result = deploy(args);
    assert.equal(result.status, 2, `${what}: ${result.stderr}`);
    assert.match(result.stderr, /error:|Usage:/, what);
    assert.equal(result.calls, "", what);
  }
});

test("the drill hostname cannot be a production name or outside the zone", () => {
  for (const host of [
    "usnewsmap.com",
    "www.usnewsmap.com",
    "api.usnewsmap.com",
    "boinc.usnewsmap.com",
    "test.example.com",
    "test.usnewsmap.com.example.com",
    "*.usnewsmap.com",
    "test.usnewsmap.com/*",
    "",
  ]) {
    const result = deploy([...COMMON, "--drill-hostname", host, "--dry-run"]);
    assert.equal(result.status, 2, `${host}: ${result.stderr}`);
    assert.equal(result.calls, "", host);
  }
});

test("a live run without a vault or secret is refused before anything is read", () => {
  const result = deploy([...flags({ "--vault": null, "--token-secret": null }), "--production"]);
  assert.equal(result.status, 2);
  assert.equal(result.calls, "");
});

test("a deploy reads the token into Wrangler's environment only", () => {
  const result = deploy([...COMMON, "--production"]);
  assert.equal(result.status, 0, result.stderr);
  const [az, npx] = result.calls.trim().split("\n");
  assert.equal(az, "az keyvault secret show --vault-name kv-example -n example-secret --query value -o tsv");
  assert.match(npx, /^npx --yes wrangler@4\.152\.0 deploy --config \S+ --var AZURE_FALLBACK_ORIGIN:https:\/\/ca-usnm-prod\./);
  assert.equal(result.calls.includes(TOKEN), false, "the token is on a command line");
  assert.equal(result.stdout.includes(TOKEN), false, "the token is printed");
  assert.equal(result.stderr.includes(TOKEN), false, "the token is printed");
  assert.match(result.env, new RegExp(`^token=${TOKEN}$`, "m"));
  assert.match(result.env, /^account=0123456789abcdef0123456789abcdef$/m);
  assert.match(result.env, /^metrics=false$/m);
  assert.match(result.stdout, /fail open/);
});

test("a drill deploy names the drill Worker, its one route and DNT", () => {
  const result = deploy([...COMMON, "--drill-hostname", "test.usnewsmap.com"]);
  assert.equal(result.status, 0, result.stderr);
  const npx = result.calls.trim().split("\n")[1];
  assert.match(npx, / --name usnm-fallback-drill /);
  assert.match(npx, / --route test\.usnewsmap\.com\/\* /);
  assert.match(npx, / --var ADD_DNT:true$/);
});

test("when the token cannot be read, or is empty, Wrangler is not run", () => {
  for (const env of [{ STUB_AZ_FAIL: "1" }, { STUB_TOKEN: "" }]) {
    const result = deploy([...COMMON, "--production"], env);
    assert.equal(result.status, 1, result.stderr);
    assert.match(result.stderr, /error:/);
    assert.doesNotMatch(result.calls, /npx/);
  }
});

test("a failed deploy is reported and the token is still not printed", () => {
  const result = deploy([...COMMON, "--production"], { STUB_NPX_FAIL: "1" });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /wrangler deploy failed \(3\)/);
  assert.equal(result.stdout.includes(TOKEN) || result.stderr.includes(TOKEN), false);
});
