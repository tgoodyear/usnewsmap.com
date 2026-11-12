// node --test edge/check/deploy.test.mjs
//
// Runs deploy.sh in a throwaway git repository with az, npx, curl, gh and sleep replaced by
// stubs, and sync-languages.mjs by a recorder. Nothing here reaches Azure, npm, Wrangler or
// Cloudflare, and the repository's own edge/check is never touched.

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const HERE = (name) => fileURLToPath(new URL(name, import.meta.url));
const ACCOUNT = "0123456789abcdef0123456789abcdef";
const TOKEN = "fake-workers-token-0123456789";
const ADMIN = "fake-admin-token-9876543210";
const OLD_TREE = "0000000000000000000000000000000000000000";

const STUBS = {
  az: `#!/bin/sh
echo "az $*" >> "$STUB_DIR/calls.log"
case "$*" in
  *usnm-prod-cloudflare-account-id*) echo "${ACCOUNT}" ;;
  *usnm-prod-workers-ci-token*) echo "${TOKEN}" ;;
  *usnm-prod-check-admin-token*) echo "\${STUB_ADMIN-${ADMIN}}" ;;
  *) exit 1 ;;
esac
`,
  // Before the deploy the URL is /build.txt, after it /build.txt?<random>.
  curl: `#!/bin/sh
url=""
for argument in "$@"; do case "$argument" in http*) url=$argument ;; esac; done
echo "curl $url" >> "$STUB_DIR/calls.log"
case "$url" in
  */api/languages) printf '%s' "\${STUB_LANGUAGES_CODE:-200}" ;;
  */build.txt\\?*) if [ -n "\${STUB_NEVER_SHOWS:-}" ]; then echo "${OLD_TREE}"; else cat "$STUB_DIR/tree"; fi ;;
  */build.txt) [ -z "\${STUB_LIVE_FAIL:-}" ] || exit 22; printf '%s\\n' "\${STUB_LIVE:-}" ;;
  *) exit 6 ;;
esac
`,
  npx: `#!/bin/sh
echo "npx $*" >> "$STUB_DIR/calls.log"
echo "cwd=$(basename "$(dirname "$PWD")")/$(basename "$PWD")" > "$STUB_DIR/npx.log"
{
  echo "token=\${CLOUDFLARE_API_TOKEN:-}"
  echo "account=\${CLOUDFLARE_ACCOUNT_ID:-}"
  echo "metrics=\${WRANGLER_SEND_METRICS:-}"
  echo "build=$(cat public/build.txt 2>/dev/null)"
} >> "$STUB_DIR/npx.log"
[ -z "\${STUB_NPX_FAIL:-}" ] || exit 3
`,
  gh: `#!/bin/sh
echo "gh $*" >> "$STUB_DIR/calls.log"
echo "\${STUB_MAIN_HEAD:-}"
`,
  sleep: `#!/bin/sh
echo "sleep $*" >> "$STUB_DIR/calls.log"
`,
};
const SYNC = `import { appendFileSync } from "node:fs";
appendFileSync(process.env.STUB_DIR + "/calls.log",
  "sync " + process.argv.slice(2).join(" ") + " admin=" + (process.env.CHECK_ADMIN_TOKEN ?? "") + "\\n");
`;

function git(cwd, ...args) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim();
}

// Runs deploy.sh in a fresh repository holding a copy of it, the real head-of-main.sh and a
// small edge/check. The options: live (what /build.txt serves before the deploy, "tree" for the
// tree's own hash), env (extra environment), actions (run as GitHub Actions does, with
// DEPLOY_SHA at "head" or "newer" than main's head), dirty (an uncommitted change in edge/check).
function deploy(args, { live = "", env = {}, actions = null, dirty = false } = {}) {
  const dir = mkdtempSync(join(tmpdir(), "check-deploy-test-"));
  try {
    const repo = join(dir, "repo");
    mkdirSync(join(repo, "edge/check/public"), { recursive: true });
    mkdirSync(join(repo, "scripts/ci"), { recursive: true });
    copyFileSync(HERE("./deploy.sh"), join(repo, "edge/check/deploy.sh"));
    copyFileSync(HERE("../../scripts/ci/head-of-main.sh"), join(repo, "scripts/ci/head-of-main.sh"));
    writeFileSync(join(repo, "edge/check/sync-languages.mjs"), SYNC);
    writeFileSync(join(repo, "edge/check/public/index.html"), "<p>check</p>\n");
    writeFileSync(join(repo, "edge/check/.gitignore"), "public/build.txt\n");
    mkdirSync(join(dir, "bin"));
    for (const [name, body] of Object.entries(STUBS)) {
      writeFileSync(join(dir, "bin", name), body);
      chmodSync(join(dir, "bin", name), 0o755);
    }
    git(repo, "init", "-q");
    git(repo, "-c", "user.name=t", "-c", "user.email=t@example.com", "add", ".");
    git(repo, "-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "-m", "x");
    const tree = git(repo, "rev-parse", "HEAD:edge/check");
    const sha = git(repo, "rev-parse", "HEAD");
    writeFileSync(join(dir, "tree"), `${tree}\n`);
    if (dirty) writeFileSync(join(repo, "edge/check/public/index.html"), "<p>changed</p>\n");
    const ci = actions
      ? { GITHUB_ACTIONS: "true", GITHUB_REPOSITORY: "example/repo", DEPLOY_SHA: sha,
          STUB_MAIN_HEAD: actions === "head" ? sha : "f".repeat(40) }
      : { GITHUB_ACTIONS: "false" };
    const result = spawnSync("bash", [join(repo, "edge/check/deploy.sh"), ...args], {
      cwd: dir,
      encoding: "utf8",
      env: {
        ...process.env, PATH: `${join(dir, "bin")}:${process.env.PATH}`, STUB_DIR: dir,
        STUB_LIVE: live === "tree" ? tree : live, CHECK_DEPLOY_WAIT_SECONDS: "0", ...ci, ...env,
      },
    });
    const read = (path) => (existsSync(path) ? readFileSync(path, "utf8") : "");
    const calls = read(join(dir, "calls.log"));
    return {
      ...result, tree, calls, npx: read(join(dir, "npx.log")),
      buildTxtRemains: existsSync(join(repo, "edge/check/public/build.txt")),
      deployed: /^npx /m.test(calls), synced: /^sync /m.test(calls),
    };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test("a live build.txt matching the tree skips the deploy and still syncs the languages", () => {
  const result = deploy([], { live: "tree" });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /already runs edge\/check/);
  assert.equal(result.deployed, false);
  assert.doesNotMatch(result.calls, /usnm-prod-cloudflare|usnm-prod-workers-ci-token/);
  assert.match(result.calls, /^sync --site https:\/\/check\.usnewsmap\.com admin=fake-admin-token-9876543210$/m);
  assert.equal(result.buildTxtRemains, false);
});

test("a different live build.txt deploys, verifies and syncs", () => {
  const result = deploy([], { live: OLD_TREE });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.deployed, true);
  assert.ok(result.stdout.includes(`deploying edge/check ${result.tree} (live: ${OLD_TREE})`), result.stdout);
  assert.ok(result.stdout.includes(`verified check.usnewsmap.com serves edge/check ${result.tree}`), result.stdout);
  assert.match(result.npx, /^cwd=edge\/check$/m);
  assert.ok(result.npx.includes(`build=${result.tree}`), "build.txt held the tree while Wrangler ran");
  assert.match(result.calls, /^npx --yes wrangler@4\.154\.0 deploy --message edge\/check [0-9a-f]{40}$/m);
  assert.match(result.npx, new RegExp(`^token=${TOKEN}$`, "m"));
  assert.match(result.npx, new RegExp(`^account=${ACCOUNT}$`, "m"));
  assert.match(result.npx, /^metrics=false$/m);
  assert.equal(result.calls.includes(TOKEN), false, "the Workers token is on a command line");
  assert.equal(result.calls.includes(ADMIN), true, "the sync gets the admin token in its environment");
  assert.ok(result.calls.indexOf("npx ") < result.calls.indexOf("api/languages"));
  assert.ok(result.calls.indexOf("api/languages") < result.calls.indexOf("sync "));
  assert.equal(result.synced, true);
  assert.equal(result.buildTxtRemains, false);
});

test("a build.txt that can't be read, or is empty, deploys", () => {
  for (const [what, options] of [["missing", { env: { STUB_LIVE_FAIL: "1" } }], ["empty", { live: "" }]]) {
    const result = deploy([], options);
    assert.equal(result.status, 0, `${what}: ${result.stderr}`);
    assert.equal(result.deployed, true, what);
    assert.match(result.stdout, /\(live: unknown\)/, what);
    assert.equal(result.buildTxtRemains, false, what);
  }
});

test("--force deploys although the live build.txt matches", () => {
  const result = deploy(["--force"], { live: "tree" });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.deployed, true);
  assert.doesNotMatch(result.stdout, /already runs/);
  assert.equal(result.synced, true);
  assert.equal(result.buildTxtRemains, false);
});

test("in Actions, a commit that is no longer main's head stands down before touching anything", () => {
  const result = deploy([], { live: OLD_TREE, actions: "newer" });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /not main's head: the newer commit's run deploys/);
  assert.match(result.calls, /^gh api /m);
  assert.doesNotMatch(result.calls, /^(az|curl|npx|sync) /m);
  assert.equal(result.buildTxtRemains, false);
});

test("in Actions, main's head deploys and masks both tokens", () => {
  const result = deploy([], { live: OLD_TREE, actions: "head" });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.deployed, true);
  assert.ok(result.stdout.includes(`::add-mask::${TOKEN}`));
  assert.ok(result.stdout.includes(`::add-mask::${ADMIN}`));
  assert.equal(result.buildTxtRemains, false);
});

test("in Actions, a missing DEPLOY_SHA is an error", () => {
  const result = deploy([], { live: OLD_TREE, actions: "head", env: { DEPLOY_SHA: "" } });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /DEPLOY_SHA is required in CI/);
  assert.equal(result.deployed, false);
});

test("a failed wrangler deploy fails the run, verifies nothing and removes build.txt", () => {
  const result = deploy([], { live: OLD_TREE, env: { STUB_NPX_FAIL: "1" } });
  assert.notEqual(result.status, 0);
  assert.equal(result.deployed, true);
  assert.doesNotMatch(result.calls, /build\.txt\?|api\/languages/);
  assert.equal(result.synced, false);
  assert.equal(result.buildTxtRemains, false);
});

test("a build.txt that never shows the new tree fails and removes build.txt", () => {
  const result = deploy([], { live: OLD_TREE, env: { STUB_NEVER_SHOWS: "1" } });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /doesn't serve build [0-9a-f]{40} ten minutes after the deploy/);
  assert.doesNotMatch(result.calls, /api\/languages/);
  assert.equal(result.synced, false);
  assert.equal(result.buildTxtRemains, false);
});

test("a non-200 from /api/languages fails and removes build.txt", () => {
  for (const code of ["500", "404", "000"]) {
    const result = deploy([], { live: OLD_TREE, env: { STUB_LANGUAGES_CODE: code } });
    assert.equal(result.status, 1, code);
    assert.match(result.stderr, new RegExp(`/api/languages answered ${code} after the deploy`), code);
    assert.equal(result.synced, false, code);
    assert.equal(result.buildTxtRemains, false, code);
  }
});

test("a deploy whose secrets can't be read stops before Wrangler, and an empty admin token stops the sync", () => {
  const empty = deploy([], { live: OLD_TREE, env: { STUB_ADMIN: "" } });
  assert.equal(empty.status, 1);
  assert.match(empty.stderr, /check admin token is empty/);
  assert.equal(empty.buildTxtRemains, false);
  const skipped = deploy([], { live: "tree", env: { STUB_ADMIN: "" } });
  assert.equal(skipped.status, 1);
  assert.equal(skipped.deployed, false);
  assert.equal(skipped.synced, false);
});

test("uncommitted changes in edge/check are refused before any request", () => {
  const result = deploy([], { live: OLD_TREE, dirty: true });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /edge\/check has uncommitted changes/);
  assert.doesNotMatch(result.calls, /^(az|curl|npx|sync) /m);
});

test("it refuses what it cannot check, and calls nothing", () => {
  for (const args of [["--bogus"], ["--vault", "Bad Vault"], ["--vault", ""], ["extra"]]) {
    const result = deploy(args, { live: OLD_TREE });
    assert.equal(result.status, 1, args.join(" "));
    assert.match(result.stderr, /error:/, args.join(" "));
    assert.equal(result.calls, "", args.join(" "));
  }
});

test("--vault names the vault every secret is read from", () => {
  const result = deploy(["--vault", "kv-example"], { live: OLD_TREE });
  assert.equal(result.status, 0, result.stderr);
  const reads = result.calls.split("\n").filter((line) => line.startsWith("az "));
  assert.equal(reads.length, 3);
  for (const line of reads) assert.match(line, /--vault-name kv-example /);
});
