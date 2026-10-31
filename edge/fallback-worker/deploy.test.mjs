// node --test edge/fallback-worker/deploy.test.mjs
//
// Runs deploy.sh with az and npx replaced by stubs in a directory of each test's own.
// Nothing here reaches Azure, npm, Wrangler or Cloudflare.

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
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
  previous=""
  for argument in "$@"; do
    if [ "$previous" = "--config" ]; then
      cp "$argument" "$STUB_DIR/config.json"
      node -e 'process.stdout.write(String(require("node:fs").statSync(process.argv[1]).mode & 511))' "$argument" > "$STUB_DIR/config-mode.log"
    fi
    previous="$argument"
  done
  {
  echo "token=\${CLOUDFLARE_API_TOKEN:-}"
  echo "account=\${CLOUDFLARE_ACCOUNT_ID:-}"
  echo "metrics=\${WRANGLER_SEND_METRICS:-}"
} > "$STUB_DIR/env.log"
[ -z "\${STUB_NPX_FAIL:-}" ] || exit 3
`,
};

function deploy(args, extraEnv = {}) {
  const dir = mkdtempSync(fileURLToPath(new URL("./.deploy-test-", import.meta.url)));
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
    const generated = `${result.stdout}\n${read("calls.log")}`.match(/--config (\S*\.native-metrics-\S+\.json)/)?.[1];
    return {
      ...result, calls: read("calls.log"), env: read("env.log"),
      config: read("config.json"), configMode: read("config-mode.log"),
      generated, generatedRemains: generated ? existsSync(generated) : false,
    };
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

test("health selection and minute heartbeat are independently opt-in", () => {
  const base = [...COMMON, "--production", "--health-failover", "--allowed-origins", "https://usnewsmap.com", "--dry-run"];
  const selection = deploy(base);
  assert.equal(selection.status, 0, selection.stderr);
  assert.match(selection.stdout, /wrangler\.jsonc/);
  assert.match(selection.stdout, /HEALTH_FAILOVER:true/);
  assert.match(selection.stdout, /HEALTH_HOSTNAME:usnewsmap\.com/);
  assert.doesNotMatch(selection.stdout, /HEALTH_HEARTBEAT/);
  const heartbeat = deploy([...base, "--health-heartbeat"]);
  assert.equal(heartbeat.status, 0, heartbeat.stderr);
  assert.match(heartbeat.stdout, /wrangler\.health\.jsonc/);
  assert.match(heartbeat.stdout, /HEALTH_HEARTBEAT:true/);
  assert.match(heartbeat.stdout, /HEALTH_HOSTNAME:usnewsmap\.com/);
  assert.equal(heartbeat.calls, "");
  for (const host of ["dev.usnewsmap.com", "drill.usnewsmap.com", "failover-drill.usnewsmap.com"]) {
    const drill = deploy([...COMMON, "--drill-hostname", host, "--health-failover", "--health-heartbeat",
      "--allowed-origins", "https://usnewsmap.com", "--dry-run"]);
    assert.equal(drill.status, 0, drill.stderr);
    assert.ok(drill.stdout.includes(`HEALTH_HOSTNAME:${host}`));
    const withoutCron = deploy([...COMMON, "--drill-hostname", host, "--health-failover",
      "--allowed-origins", "https://usnewsmap.com", "--dry-run"]);
    assert.equal(withoutCron.status, 0, withoutCron.stderr);
    assert.ok(withoutCron.stdout.includes(`HEALTH_HOSTNAME:${host}`));
    assert.doesNotMatch(withoutCron.stdout, /HEALTH_HEARTBEAT/);
  }
});

test("unsafe or inconsistent health deployment flags are refused", () => {
  for (const extra of [
    ["--health-heartbeat"],
    ["--health-failover"],
    ["--health-failover", "--error-layer", "--allowed-origins", "https://usnewsmap.com"],
    ["--health-hostname", "usnewsmap.com"],
    ...["evil.test", "api.usnewsmap.com", "usnewsmap.com/path", "usnewsmap.com.evil.test"].map((host) =>
      ["--health-failover", "--health-heartbeat", "--allowed-origins", "https://usnewsmap.com", "--health-hostname", host]),
  ]) {
    const result = deploy([...COMMON, "--production", "--dry-run", ...extra]);
    assert.equal(result.status, 2, result.stderr);
    assert.equal(result.calls, "");
  }
});

test("native metrics explicitly removes binding in private full config, preserves cron and cleans up", () => {
  const args = [...COMMON, "--drill-hostname", "failover-drill.usnewsmap.com",
    "--health-failover", "--health-heartbeat", "--health-hostname", "failover-drill.usnewsmap.com",
    "--allowed-origins", "https://usnewsmap.com", "--native-metrics"];
  for (const extraEnv of [{}, { STUB_NPX_FAIL: "1" }]) {
    const result = deploy(args, extraEnv);
    assert.equal(result.status, extraEnv.STUB_NPX_FAIL ? 1 : 0, result.stderr);
    const config = JSON.parse(result.config);
    assert.deepEqual(config.analytics_engine_datasets, []);
    assert.deepEqual(config.triggers, { crons: ["* * * * *"] });
    assert.deepEqual(config.cache, { enabled: false });
    assert.equal(config.placement, undefined);
    assert.equal(config.compatibility_date, "2025-10-01");
    assert.equal(config.compatibility_flags, undefined);
    assert.equal(config.main, fileURLToPath(new URL("./worker.mjs", import.meta.url)));
    assert.equal(config.vars.METRICS_MODE, "native");
    assert.equal(config.observability.enabled, false);
    assert.equal(config.routes.length, 3);
    assert.equal(result.configMode, String(0o600));
    assert.match(result.calls, /--var METRICS_MODE:native/);
    assert.match(result.calls, /HEALTH_HOSTNAME:failover-drill\.usnewsmap\.com/);
    assert.equal(result.config.includes(TOKEN), false);
    assert.equal(result.generatedRemains, false);
  }
  for (const suffix of [["--dry-run"], []]) {
    const result = deploy([...args, ...suffix], suffix.length ? {} : { STUB_AZ_FAIL: "1" });
    assert.equal(result.status, suffix.length ? 0 : 1, result.stderr);
    assert.ok(result.generated);
    assert.equal(result.generatedRemains, false);
    assert.doesNotMatch(result.calls, /npx /);
  }
});

test("native metrics also supports legacy config without introducing a cron", () => {
  const result = deploy([...COMMON, "--production", "--native-metrics"]);
  assert.equal(result.status, 0, result.stderr);
  const config = JSON.parse(result.config);
  assert.deepEqual(config.analytics_engine_datasets, []);
  assert.equal(config.triggers, undefined);
  assert.equal(config.vars.ERROR_LAYER, "false");
  assert.equal(result.generatedRemains, false);
});

test("custom health domain retains production routes, cron and explicit metrics selection", () => {
  for (const native of [false, true]) {
    const args = [...COMMON, "--production", "--health-failover", "--health-heartbeat",
      "--health-hostname", "usnewsmap.com", "--healthcheck-hostname", "healthcheck.usnewsmap.com",
      "--allowed-origins", "https://usnewsmap.com", ...(native ? ["--native-metrics"] : [])];
    const result = deploy(args);
    assert.equal(result.status, 0, result.stderr);
    const config = JSON.parse(result.config);
    assert.equal(config.routes.length, 4);
    assert.deepEqual(config.routes.at(-1), { pattern: "healthcheck.usnewsmap.com", custom_domain: true });
    assert.deepEqual(config.routes.slice(0, 3).map((route) => route.pattern), [
      "usnewsmap.com/*", "www.usnewsmap.com/*", "api.usnewsmap.com/*",
    ]);
    assert.equal(config.main, fileURLToPath(new URL("./worker.mjs", import.meta.url)));
    assert.deepEqual(config.triggers, { crons: ["* * * * *"] });
    assert.equal(config.vars.HEALTHCHECK_HOSTNAME, "healthcheck.usnewsmap.com");
    assert.equal(config.analytics_engine_datasets.length, native ? 0 : 1);
    assert.match(result.calls, /HEALTHCHECK_HOSTNAME:healthcheck\.usnewsmap\.com/);
    assert.equal(result.generatedRemains, false);
  }
});

test("custom health domain is exact and requires production health mode", () => {
  for (const args of [
    ["--production"],
    ["--drill-hostname", "failover-drill.usnewsmap.com", "--health-failover"],
    ["--production", "--health-failover", "--healthcheck-hostname", "evil.test"],
  ]) {
    const result = deploy([...COMMON, "--allowed-origins", "https://usnewsmap.com",
      "--healthcheck-hostname", "healthcheck.usnewsmap.com", ...args, "--dry-run"]);
    assert.equal(result.status, 2, result.stderr);
    assert.equal(result.calls, "");
  }
});

test("invocation logs are explicit, redacted and independent of native metrics/custom domain", () => {
  for (const extra of [[], ["--native-metrics", "--health-failover", "--health-heartbeat",
    "--healthcheck-hostname", "healthcheck.usnewsmap.com", "--allowed-origins", "https://usnewsmap.com"]]) {
    const result = deploy([...COMMON, "--production", "--worker-logs", ...extra]);
    assert.equal(result.status, 0, result.stderr);
    const config = JSON.parse(result.config);
    assert.deepEqual(config.observability, {
      enabled: true, logs: { enabled: true, invocation_logs: true },
      traces: { enabled: false }, redact_query_string: true,
    });
    assert.equal(config.vars.LOG_OUTCOMES, "true");
    assert.equal(config.logpush, undefined);
    assert.equal(config.tail_consumers, undefined);
    assert.equal(result.generatedRemains, false);
  }
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

test("the Phase 4 mode requires an explicit deploy flag", () => {
  const result = deploy([...COMMON, "--drill-hostname", "test.usnewsmap.com", "--error-layer",
    "--allowed-origins", "https://usnewsmap.com", "--dry-run"]);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /--var ERROR_LAYER:true/);
  assert.match(result.stdout, /--var ADD_DNT:true/);
  assert.match(result.stdout, /--var ALLOWED_ORIGINS:https:\/\/usnewsmap\.com/);
  assert.equal(result.calls, "");
});

test("the error layer cannot deploy without an explicit valid origin policy", () => {
  for (const value of [null, "*", "https://usnewsmap.com/path", "https://user:pass@usnewsmap.com", "https://usnewsmap.com,"]) {
    const args = [...COMMON, "--drill-hostname", "test.usnewsmap.com", "--error-layer", "--dry-run"];
    if (value !== null) args.push("--allowed-origins", value);
    const result = deploy(args);
    assert.equal(result.status, 2, String(value));
    assert.match(result.stderr, /error:/);
    assert.equal(result.calls, "");
  }
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
