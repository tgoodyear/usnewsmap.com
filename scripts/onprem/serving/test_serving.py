"""Tests for the on-prem serving scripts (#955): python3 -m unittest -v
scripts/onprem/serving/test_serving.py. Needs jq; shellcheck is used when
it's installed. Nothing here reaches GitHub, Azure or Cloudflare, or
changes the machine: every host tool the scripts call is a stub in a
directory of each test's own, which also stands in for / (the scripts'
USNM_SERVING_TEST_ROOT).

The tests that run update-api.sh's deploy and install.sh for real need
GNU tools (flock, realpath -e, stat -c, base64 -w0), so they run on Linux
only, as in CI."""

import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
LINUX = sys.platform.startswith("linux")
SHA = {n: c * 40 for n, c in (("old", "a"), ("mid", "b"), ("new", "c"))}
REGISTRY = "crexampletest"
MANIFEST = '{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[]}'

# curl, as the scripts call it: the GitHub API (with ETags), the Arc
# agent's identity endpoint, the registry's token exchange, the searcher's
# and the colors' health, and Caddy's admin API, which reports the port
# upstream.caddy names. Every call is logged to STUB_LOG.
STUB_CURL = r'''#!/usr/bin/env python3
import json, os, re, sys
args = sys.argv[1:]
root = os.environ["USNM_SERVING_TEST_ROOT"]
stubs = os.environ["STUB_DIR"]
def opt(name):
    return args[args.index(name) + 1] if name in args else None
headers = [args[i + 1] for i, a in enumerate(args) if a == "-H"]
url = next(a for a in reversed(args) if a.startswith("http"))
with open(os.environ["STUB_LOG"], "a") as log:
    log.write("curl %s %s\n" % (url, " ".join(h for h in headers if not h.startswith("@"))))
ready = os.environ.get("STUB_READY_PORTS", "").split()
code, body, out_headers = 200, "", {}
def upstream():
    try:
        return re.search(r"127\.0\.0\.1:(\d+)", open(root + "/etc/usnm-serving/upstream.caddy").read()).group(1)
    except (OSError, AttributeError):
        return None
if url.startswith("https://api.github.com/"):
    path = url.split("/repos/tgoodyear/usnewsmap.com/")[1]
    m = re.match(r"deployments/(\d+)/statuses", path)
    name = ("status-%s" % m.group(1)) if m else ("deployments" if path.startswith("deployments") else None)
    if path.startswith("attestations/"):
        name = "attestations"
    f = os.path.join(stubs, name + ".json") if name else None
    if not f or not os.path.exists(f):
        code = 404
    else:
        body = open(f).read()
        etag = '"%s-%d"' % (name, hash(body) & 0xffff)
        out_headers["ETag"] = etag
        if "If-None-Match: " + etag in headers:
            code, body = 304, ""
elif url.startswith("http://localhost:40342/"):
    if opt("-D") == "-":
        sys.stdout.write("HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=/var/opt/azcmagent/tokens/agent.key\r\n\r\n")
        sys.exit(0)
    body = json.dumps({"access_token": "arm-token"})
elif url.endswith("/oauth2/exchange"):
    body = json.dumps({"refresh_token": "registry-refresh-token"})
elif url == "http://127.0.0.1:7280/health/readyz":
    code = 503 if os.environ.get("STUB_SEARCHER_DOWN") else 200
elif url == "http://127.0.0.1:2019/config/apps/http/servers":
    port = os.environ.get("STUB_CADDY_STUCK") or upstream()
    body = json.dumps({"srv0": {"routes": [{"handle": [{"handler": "reverse_proxy",
        "upstreams": [{"dial": "127.0.0.1:%s" % port}]}]}]}})
elif re.match(r"http://127\.0\.0\.1:(\d+)/readyz$", url):
    port = re.match(r"http://127\.0\.0\.1:(\d+)/", url).group(1)
    if port == "8080":
        port = os.environ.get("STUB_CADDY_STUCK") or upstream()
    code = 200 if port in ready else 503
else:
    code = 404
if opt("-D") and opt("-D") != "-":
    with open(opt("-D"), "w") as f:
        f.write("HTTP/1.1 %d\r\n" % code + "".join("%s: %s\r\n" % kv for kv in out_headers.items()) + "\r\n")
if opt("-o") and opt("-o") != "/dev/null":
    with open(opt("-o"), "w") as f:
        f.write(body)
elif not opt("-o"):
    sys.stdout.write(body)
if opt("-w"):
    sys.stdout.write(str(code))
if code >= 400 and any(a.startswith("-") and not a.startswith("--") and "f" in a for a in args):
    sys.exit(22)
'''

# Logs its arguments and succeeds, with a few answers the scripts read.
STUB_TOOL = r'''#!/usr/bin/env bash
echo "$(basename "$0") $*" >> "$STUB_LOG"
case "$(basename "$0") $1" in
  "systemctl is-active")
    for u in ${STUB_ACTIVE-usnm-searcher.service usnm-api@blue.service usnm-api@green.service}; do
      [ "$u" != "$3" ] || exit 0
    done
    exit 3 ;;
  "skopeo inspect")
    [ -z "${STUB_NO_IMAGE:-}" ] || { echo "manifest unknown" >&2; exit 1; }
    printf '%s' "$STUB_MANIFEST" ;;
  "gh attestation") [ -z "${STUB_BAD_ATTESTATION:-}" ] || exit 1 ;;
  "podman images") printf '%s\n' ${STUB_IMAGES:-} ;;
  "getent group") echo "himds:x:4242:" ;;
  "getent passwd") exit 2 ;;
esac
exit 0
'''


def write(path, text, mode=0o644):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)
    os.chmod(path, mode)


class Stubbed(unittest.TestCase):
    """A directory with stub tools first on PATH and a stand-in for /."""

    TOOLS = ("systemctl", "skopeo", "gh", "podman", "getent", "useradd", "chown",
             "systemd-tmpfiles", "caddy", "cloudflared", "mountpoint")

    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp)
        self.bin = os.path.join(self.tmp, "bin")
        self.root = os.path.join(self.tmp, "root")
        self.stubs = os.path.join(self.tmp, "stubs")
        self.log = os.path.join(self.tmp, "log")
        os.makedirs(self.root)
        os.makedirs(self.stubs)
        write(os.path.join(self.bin, "curl"), STUB_CURL, 0o755)
        for tool in self.TOOLS:
            write(os.path.join(self.bin, tool), STUB_TOOL, 0o755)
        write(os.path.join(self.bin, "sleep"), "#!/bin/sh\n", 0o755)
        open(self.log, "w").close()
        self.env = {**os.environ, "PATH": self.bin + os.pathsep + os.environ["PATH"],
                    "USNM_SERVING_TEST_ROOT": self.root, "STUB_DIR": self.stubs, "STUB_LOG": self.log,
                    "STUB_MANIFEST": MANIFEST, "TMPDIR": self.tmp}
        self.env.pop("JOURNAL_STREAM", None)

    def calls(self):
        with open(self.log) as f:
            return f.read().splitlines()


class UpdateApi(Stubbed):
    SCRIPT = os.path.join(HERE, "update-api.sh")

    def deployments(self, *rows):
        """rows: (id, sha, state) newest first."""
        write(os.path.join(self.stubs, "deployments.json"),
              json.dumps([{"id": i, "sha": s, "task": "deploy", "environment": "prod"} for i, s, _ in rows]))
        for i, _, state in rows:
            write(os.path.join(self.stubs, "status-%d.json" % i), json.dumps([{"state": state}]))

    def serving(self, color, sha):
        port = {"blue": 8081, "green": 8082}[color]
        write(os.path.join(self.root, "var/lib/usnm-serving/active"), color + "\n")
        write(os.path.join(self.root, "etc/usnm-serving/api-%s.env" % color),
              "API_IMAGE=%s.azurecr.io/usnewsmap-api@sha256:%s\nAPI_COMMIT=%s\nUSNM_BIND=127.0.0.1:%d\n"
              % (REGISTRY, "0" * 64, sha, port))
        write(os.path.join(self.root, "etc/usnm-serving/upstream.caddy"), "reverse_proxy 127.0.0.1:%d\n" % port)

    def run_update(self, *extra, **env):
        return subprocess.run(["bash", self.SCRIPT, "--registry", REGISTRY, *extra],
                              env={**self.env, **env}, capture_output=True, text=True)

    def dry(self, *extra, **env):
        r = self.run_update("--dry-run", *extra, **env)
        self.assertEqual(r.returncode, 0, r.stderr)
        return r.stdout

    def test_first_run_deploys_the_newest_success_to_blue(self):
        self.deployments((3, SHA["new"], "in_progress"), (2, SHA["mid"], "success"), (1, SHA["old"], "success"))
        self.assertIn("would deploy %s to blue (now nothing)" % SHA["mid"], self.dry())

    def test_a_new_deployment_goes_to_the_idle_color(self):
        self.deployments((2, SHA["new"], "success"), (1, SHA["old"], "success"))
        self.serving("blue", SHA["old"])
        self.assertIn("would deploy %s to green (now %s on blue)" % (SHA["new"], SHA["old"]), self.dry())

    def test_up_to_date_says_nothing(self):
        self.deployments((2, SHA["new"], "failure"), (1, SHA["old"], "success"))
        self.serving("green", SHA["old"])
        self.assertEqual(self.dry(), "")

    def test_a_rejected_commit_is_skipped_but_never_below_the_served_one(self):
        self.deployments((3, SHA["new"], "success"), (2, SHA["mid"], "success"), (1, SHA["old"], "success"))
        write(os.path.join(self.root, "var/lib/usnm-serving/rejected"), SHA["new"] + "\n")
        self.serving("blue", SHA["old"])
        self.assertIn("would deploy %s to green" % SHA["mid"], self.dry())
        self.serving("blue", SHA["mid"])
        self.assertEqual(self.dry(), "")

    def test_hold_and_searcher_down_do_nothing(self):
        self.deployments((2, SHA["new"], "success"))
        self.serving("blue", SHA["old"])
        write(os.path.join(self.root, "etc/usnm-serving/hold"), "")
        self.assertIn("held:", self.dry())
        os.remove(os.path.join(self.root, "etc/usnm-serving/hold"))
        self.assertIn("searcher isn't ready", self.dry(STUB_SEARCHER_DOWN="1"))
        self.assertIn("searcher isn't ready", self.dry(STUB_ACTIVE=""))

    def test_requests_are_conditional_on_the_last_etag(self):
        self.deployments((2, SHA["new"], "success"))
        self.serving("blue", SHA["new"])
        self.dry()
        self.assertFalse(any("If-None-Match" in c for c in self.calls()))
        open(self.log, "w").close()
        self.dry()
        github = [c for c in self.calls() if "api.github.com" in c]
        self.assertEqual(len(github), 2, github)
        self.assertTrue(all("If-None-Match:" in c for c in github), github)

    def test_flags_are_checked(self):
        for args in (["--registry", "UPPER"], ["--registry", REGISTRY, "--commit", "abc"],
                     ["--registry", REGISTRY, "--ready-secs", "0"], ["--registry", REGISTRY, "extra"]):
            r = subprocess.run(["bash", self.SCRIPT, *args, "--dry-run"], env=self.env, capture_output=True, text=True)
            self.assertEqual(r.returncode, 2, (args, r.stderr))


@unittest.skipUnless(LINUX, "needs GNU flock, realpath, stat and base64")
class UpdateApiDeploy(UpdateApi):
    def setUp(self):
        super().setUp()
        tokens = os.path.join(self.root, "var/opt/azcmagent/tokens")
        write(os.path.join(tokens, "agent.key"), "challenge", 0o600)
        write(os.path.join(self.stubs, "attestations.json"), json.dumps({"attestations": [{"bundle": {"b": 1}}]}))

    def deploy(self, *extra, **env):
        return self.run_update("--drain-secs", "0", "--ready-secs", "1", *extra, **env)

    def read(self, rel):
        with open(os.path.join(self.root, rel)) as f:
            return f.read()

    def test_first_deploy_starts_blue_and_points_caddy_at_it(self):
        self.deployments((1, SHA["new"], "success"))
        r = self.deploy(STUB_READY_PORTS="8081")
        self.assertEqual(r.returncode, 0, r.stderr + r.stdout)
        digest = "sha256:" + __import__("hashlib").sha256(MANIFEST.encode()).hexdigest()
        self.assertEqual(self.read("etc/usnm-serving/api-blue.env"),
                         "API_IMAGE=%s.azurecr.io/usnewsmap-api@%s\nAPI_COMMIT=%s\nUSNM_BIND=127.0.0.1:8081\n"
                         % (REGISTRY, digest, SHA["new"]))
        self.assertEqual(self.read("etc/usnm-serving/upstream.caddy"), "reverse_proxy 127.0.0.1:8081\n")
        self.assertEqual(self.read("var/lib/usnm-serving/active"), "blue\n")
        calls = self.calls()
        verify = next(c for c in calls if c.startswith("gh attestation verify"))
        for part in ("--source-digest " + SHA["new"], "--source-ref refs/heads/main",
                     "--signer-workflow tgoodyear/usnewsmap.com/.github/workflows/ci.yml"):
            self.assertIn(part, verify)
        order = [c for c in calls if c.startswith(("systemctl restart", "systemctl reload", "systemctl enable",
                                                     "podman pull"))]
        self.assertEqual([c.split()[1] for c in order], ["pull", "restart", "reload", "enable"], order)
        self.assertFalse(any("arm-token" in c or "registry-refresh-token" in c for c in calls))
        self.assertFalse(os.path.exists(os.path.join(self.root, "etc/usnm-serving/hold")))

    def test_switch_drains_and_stops_the_old_color(self):
        self.deployments((2, SHA["new"], "success"), (1, SHA["old"], "success"))
        self.serving("blue", SHA["old"])
        r = self.deploy(STUB_READY_PORTS="8081 8082")
        self.assertEqual(r.returncode, 0, r.stderr + r.stdout)
        self.assertEqual(self.read("etc/usnm-serving/upstream.caddy"), "reverse_proxy 127.0.0.1:8082\n")
        self.assertEqual(self.read("var/lib/usnm-serving/active"), "green\n")
        calls = self.calls()
        self.assertIn("systemctl disable --quiet usnm-api@blue.service", calls)
        self.assertIn("systemctl stop usnm-api@blue.service", calls)
        self.assertLess(calls.index("systemctl reload caddy.service"), calls.index("systemctl stop usnm-api@blue.service"))

    def test_a_color_that_never_gets_ready_is_rejected_and_blue_keeps_serving(self):
        self.deployments((2, SHA["new"], "success"), (1, SHA["old"], "success"))
        self.serving("blue", SHA["old"])
        r = self.deploy(STUB_READY_PORTS="8081")
        self.assertEqual(r.returncode, 1)
        self.assertIn("is rejected", r.stdout + r.stderr)
        self.assertEqual(self.read("var/lib/usnm-serving/rejected"), SHA["new"] + "\n")
        self.assertEqual(self.read("etc/usnm-serving/upstream.caddy"), "reverse_proxy 127.0.0.1:8081\n")
        self.assertEqual(self.read("var/lib/usnm-serving/active"), "blue\n")
        self.assertIn("systemctl stop usnm-api@green.service", self.calls())
        self.assertNotIn("systemctl reload caddy.service", self.calls())

    def test_nothing_ready_means_the_host_not_the_commit(self):
        self.deployments((2, SHA["new"], "success"), (1, SHA["old"], "success"))
        self.serving("blue", SHA["old"])
        r = self.deploy(STUB_READY_PORTS="")
        self.assertEqual(r.returncode, 1)
        self.assertIn("isn't rejected", r.stdout + r.stderr)
        self.assertFalse(os.path.exists(os.path.join(self.root, "var/lib/usnm-serving/rejected")))

    def test_a_switch_that_doesnt_take_goes_back(self):
        self.deployments((2, SHA["new"], "success"), (1, SHA["old"], "success"))
        self.serving("blue", SHA["old"])
        r = self.deploy(STUB_READY_PORTS="8081 8082", STUB_CADDY_STUCK="8081")
        self.assertEqual(r.returncode, 1)
        self.assertEqual(self.read("etc/usnm-serving/upstream.caddy"), "reverse_proxy 127.0.0.1:8081\n")
        self.assertEqual(self.read("var/lib/usnm-serving/active"), "blue\n")
        self.assertIn(SHA["new"], self.read("var/lib/usnm-serving/rejected"))

    def test_bad_or_missing_attestation_rejects_before_pulling(self):
        self.deployments((1, SHA["new"], "success"))
        r = self.deploy(STUB_READY_PORTS="8081", STUB_BAD_ATTESTATION="1")
        self.assertEqual(r.returncode, 1)
        self.assertFalse(any(c.startswith("podman pull") for c in self.calls()))
        self.assertIn(SHA["new"], self.read("var/lib/usnm-serving/rejected"))
        os.remove(os.path.join(self.root, "var/lib/usnm-serving/rejected"))
        write(os.path.join(self.stubs, "attestations.json"), json.dumps({"attestations": []}))
        r = self.deploy(STUB_READY_PORTS="8081")
        self.assertEqual(r.returncode, 1)
        self.assertIn("has no attestation", r.stdout + r.stderr)

    def test_an_image_not_pushed_yet_waits(self):
        self.deployments((1, SHA["new"], "success"))
        r = self.deploy(STUB_READY_PORTS="8081", STUB_NO_IMAGE="1")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("yet", r.stdout)
        self.assertFalse(os.path.exists(os.path.join(self.root, "var/lib/usnm-serving/rejected")))

    def test_commit_rolls_back_and_holds(self):
        self.deployments((2, SHA["new"], "success"), (1, SHA["old"], "success"))
        self.serving("green", SHA["new"])
        r = self.deploy("--commit", SHA["old"], STUB_READY_PORTS="8081 8082")
        self.assertEqual(r.returncode, 0, r.stderr + r.stdout)
        self.assertIn("API_COMMIT=" + SHA["old"], self.read("etc/usnm-serving/api-blue.env"))
        self.assertTrue(os.path.exists(os.path.join(self.root, "etc/usnm-serving/hold")))
        r = self.deploy(STUB_READY_PORTS="8081 8082")
        self.assertIn("held:", r.stdout)

    def test_unused_images_are_removed(self):
        self.deployments((2, SHA["new"], "success"), (1, SHA["old"], "success"))
        self.serving("blue", SHA["old"])
        keep = "%s.azurecr.io/usnewsmap-api@sha256:%s" % (REGISTRY, "0" * 64)
        stale = "%s.azurecr.io/usnewsmap-api@sha256:%s" % (REGISTRY, "9" * 64)
        other = "docker.io/library/other@sha256:%s" % ("8" * 64)
        r = self.deploy(STUB_READY_PORTS="8081 8082", STUB_IMAGES=" ".join((keep, stale, other)))
        self.assertEqual(r.returncode, 0, r.stderr + r.stdout)
        removed = [c for c in self.calls() if c.startswith("podman rmi")]
        self.assertEqual(removed, ["podman rmi " + stale])


class GenEnv(unittest.TestCase):
    SCRIPT = os.path.join(HERE, "gen-env.sh")
    LOCAL = {"STORAGE_ACCOUNT": "stexampletest", "COSMOS_ENDPOINT": "https://cosmos-example.documents.azure.com:443/",
             "APPLICATIONINSIGHTS_CONNECTION_STRING": "InstrumentationKey=k;IngestionEndpoint=https://in.example/",
             "API_APP": "ca-example", "AZURE_RESOURCE_GROUP": "rg-example"}

    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp)
        env_dir = os.path.join(self.tmp, "env")
        os.makedirs(env_dir)
        shutil.copy(os.path.join(ROOT, "infra", "env", "schema.json"), env_dir)
        committed = {"$schema": "./schema.json", "USNM_BROWSE_PAGES": True, "USNM_CONGRESS_SEARCH": False,
                     "USNM_DNS_ZONE": "usnewsmap.com", "USNM_INGEST_CRON": "0 6 * * 1", "USNM_INGEST_JOBS": True,
                     "USNM_SEARCH_BACKEND": "quickwit", "USNM_USE_ACR": True}
        write(os.path.join(env_dir, "prod.json"), json.dumps(dict(sorted(committed.items())), indent=2) + "\n")
        settings = os.path.join(self.tmp, "settings")
        write(os.path.join(settings, "prod", ".env"), "".join('%s="%s"\n' % kv for kv in self.LOCAL.items()))
        self.bin = os.path.join(self.tmp, "bin")
        write(os.path.join(self.bin, "az"), '#!/bin/sh\necho "az $*" >> "$STUB_LOG"\ncat "$STUB_AZ"\n', 0o755)
        self.env = {**os.environ, "PATH": self.bin + os.pathsep + os.environ["PATH"], "HOME": self.tmp,
                    "USNM_ENV_DIR": env_dir, "USNM_SETTINGS_HOME": settings, "TMPDIR": self.tmp,
                    "STUB_LOG": os.path.join(self.tmp, "log"), "STUB_AZ": os.path.join(self.tmp, "az.json")}

    def gen(self, *extra):
        return subprocess.run(["bash", self.SCRIPT, "--env", "prod", "--index-dir", "/srv/usnm/runs/e/qw", *extra],
                              env=self.env, capture_output=True, text=True, cwd=self.tmp)

    def files(self, out):
        parts = re.split(r"^== (\S+)\n", out, flags=re.M)[1:]
        return dict(zip(parts[::2], parts[1::2]))

    def test_api_env_follows_the_settings_and_leaves_the_search_log_unset(self):
        r = self.gen("--dry-run")
        self.assertEqual(r.returncode, 0, r.stderr)
        api = dict(line.split("=", 1) for line in self.files(r.stdout)["api.env"].splitlines())
        self.assertEqual(api["USNM_REFERENCE_URL"], "https://stexampletest.blob.core.windows.net/reference")
        self.assertEqual(api["USNM_QUICKWIT_URL"], "http://127.0.0.1:7280")
        self.assertEqual(api["USNM_RESPONSE_CACHE_URL"], "/srv/usnm-serving/cache")
        self.assertEqual(api["USNM_ALLOWED_ORIGINS"], "https://usnewsmap.com,https://www.usnewsmap.com")
        self.assertEqual(api["USNM_CONGRESS_SEARCH"], "false")
        self.assertEqual(api["USNM_BROWSE_PAGES"], "true")
        self.assertEqual(api["USNM_INGEST_CRON"], "0 6 * * 1")
        self.assertEqual(api["USNM_COMPUTE_CONCURRENCY"], "2")
        self.assertEqual((api["USNM_PREWARM_STARTUP_BUDGET_SECS"], api["USNM_READY_CAP_SECS"]), ("300", "360"))
        self.assertEqual(api["IDENTITY_ENDPOINT"], "http://localhost:40342/metadata/identity/oauth2/token")
        for unset in ("USNM_SEARCH_LOG_URL", "USNM_OCR_REQUESTS_URL", "AZURE_CLIENT_ID", "IDENTITY_HEADER",
                      "USNM_AMERICAN_STORIES_SEARCH", "USNM_BIND"):
            self.assertNotIn(unset, api)

    def test_searcher_yaml_reads_the_local_index_with_azures_limits(self):
        files = self.files(self.gen("--dry-run").stdout)
        yaml = files["searcher.yaml"]
        self.assertIn("metastore_uri: file:///srv/usnm/runs/e/qw#polling_interval=30s\n", yaml)
        self.assertIn("default_index_root_uri: file:///srv/usnm/runs/e/qw\n", yaml)
        self.assertIn("data_dir: /var/lib/usnm-searcher\n", yaml)
        self.assertNotIn("azure", yaml)
        self.assertNotIn("__STORAGE_ACCOUNT__", yaml)
        with open(os.path.join(ROOT, "infra/quickwit/searcher.yaml")) as f:
            source = f.read()
        for line in source.splitlines():
            if line.startswith("  ") and not line.lstrip().startswith("#") and line.strip() not in ("azure:",) \
                    and "account:" not in line:
                self.assertIn(line + "\n", yaml)

    def test_searcher_env_matches_the_azure_sidecar(self):
        env = dict(line.split("=", 1) for line in self.files(self.gen("--dry-run").stdout)["searcher.env"].splitlines())
        with open(os.path.join(ROOT, "infra/modules/containerapp.bicep")) as f:
            bicep = f.read()
        block = bicep[bicep.index("var quickwitContainer = {"):]
        block = block[block.index("env: ["):block.index("probes:")]
        azure = dict(re.findall(r"name: '([A-Z_]+)'\s*,?\s*value: '([^']*)'", block))
        self.assertEqual(env, azure)

    def test_out_writes_private_files_once(self):
        out = os.path.join(self.tmp, "out")
        r = self.gen("--out", out)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(stat.S_IMODE(os.stat(out).st_mode), 0o700)
        for f in ("api.env", "searcher.env", "searcher.yaml"):
            self.assertEqual(stat.S_IMODE(os.stat(os.path.join(out, f)).st_mode), 0o600, f)
        self.assertNotEqual(self.gen("--out", out).returncode, 0)

    def azure_app(self, **changes):
        api = dict(line.split("=", 1) for line in self.files(self.gen("--dry-run").stdout)["api.env"].splitlines())
        for k in ("IDENTITY_ENDPOINT", "IMDS_ENDPOINT"):
            del api[k]
        api.update({"USNM_QUICKWIT_URL": "http://usnm-search-ca-prod", "USNM_RESPONSE_CACHE_URL": "https://x/cache",
                    "USNM_TRUSTED_PROXY_HOPS": "1", "USNM_COMPUTE_CONCURRENCY": "1", "AZURE_CLIENT_ID": "id",
                    "USNM_SEARCH_LOG_URL": "https://x/searches", "USNM_OCR_REQUESTS_URL": "https://x/ocr-requests",
                    "USNM_ALLOWED_ORIGINS": "https://usnewsmap.com,https://www.usnewsmap.com,https://ca.example"})
        api.update(changes)
        with open(self.env["STUB_AZ"], "w") as f:
            json.dump([{"name": k, "value": v} for k, v in api.items() if v is not None], f)

    def test_check_app_allows_only_the_listed_differences(self):
        self.azure_app()
        r = self.gen("--dry-run", "--check-app")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("matches ca-example", r.stderr)
        self.azure_app(USNM_CONGRESS_SEARCH=None, USNM_PAGE_LANGUAGE_FILTER="match")
        r = self.gen("--dry-run", "--check-app")
        self.assertEqual(r.returncode, 1)
        self.assertIn("USNM_CONGRESS_SEARCH: only here", r.stderr)
        self.assertIn("USNM_PAGE_LANGUAGE_FILTER: only in the Azure app", r.stderr)

    def test_bad_index_dir_is_refused(self):
        for bad in ("relative/qw", "/srv/../etc", "/srv/qw/", "/srv/q w"):
            r = subprocess.run(["bash", self.SCRIPT, "--env", "prod", "--index-dir", bad, "--dry-run"],
                               env=self.env, capture_output=True, text=True)
            self.assertEqual(r.returncode, 2, bad)


@unittest.skipUnless(LINUX, "needs GNU stat and install")
class Install(Stubbed):
    SCRIPT = os.path.join(HERE, "install.sh")

    def setUp(self):
        super().setUp()
        self.config = os.path.join(self.tmp, "config")
        write(os.path.join(self.config, "api.env"), "USNM_BACKEND=quickwit\n")
        write(os.path.join(self.config, "searcher.env"), "QW_DISABLE_TELEMETRY=1\n")
        write(os.path.join(self.config, "searcher.yaml"), "default_index_root_uri: file:///srv/usnm/runs/e/qw\n")

    def install(self, *extra):
        return subprocess.run(["bash", self.SCRIPT, "--registry", REGISTRY, "--config-dir", self.config,
                               "--skip-quickwit", *extra], env=self.env, capture_output=True, text=True)

    def test_dry_run_changes_nothing(self):
        r = self.install("--dry-run")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("would install %s/etc/systemd/system/usnm-api@.service" % self.root, r.stdout)
        self.assertEqual(os.listdir(self.root), [])

    def test_install_then_nothing_to_change(self):
        r = self.install()
        self.assertEqual(r.returncode, 0, r.stderr)
        etc = os.path.join(self.root, "etc")
        with open(os.path.join(etc, "usnm-serving/host.env")) as f:
            self.assertEqual(f.read(), "HIMDS_GID=4242\nUSNM_SERVING_REGISTRY=%s\nUSNM_SERVING_ENVIRONMENT=prod\n"
                             % REGISTRY)
        self.assertEqual(stat.S_IMODE(os.stat(os.path.join(etc, "usnm-serving/api.env")).st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(os.stat(os.path.join(self.root, "usr/local/sbin/usnm-serving-update")).st_mode),
                         0o755)
        with open(os.path.join(etc, "usnm-serving/upstream.caddy")) as f:
            self.assertEqual(f.read(), "reverse_proxy 127.0.0.1:8081\n")
        for unit in ("usnm-searcher.service", "usnm-api@.service", "cloudflared-usnm-serving.service",
                     "usnm-serving-update.service", "usnm-serving-update.timer"):
            self.assertTrue(os.path.isfile(os.path.join(etc, "systemd/system", unit)), unit)
        self.assertTrue(os.path.isfile(os.path.join(etc, "systemd/journald.conf.d/usnm-serving.conf")))
        self.assertIn("chown 65532:65532 %s/srv/usnm-serving/cache" % self.root, self.calls())
        open(self.log, "w").close()
        r = self.install()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertNotIn("installed", r.stdout)
        self.assertFalse(any(c.startswith(("systemctl restart", "systemctl daemon-reload")) for c in self.calls()))

    def test_search_log_is_refused_before_the_cutover(self):
        write(os.path.join(self.config, "api.env"), "USNM_SEARCH_LOG_URL=https://x/searches\n")
        r = self.install("--dry-run")
        self.assertEqual(r.returncode, 1)
        self.assertIn("Phase 3", r.stderr)


class Static(unittest.TestCase):
    def read(self, name):
        with open(os.path.join(HERE, name)) as f:
            return f.read()

    def test_shell_syntax_and_shellcheck(self):
        scripts = sorted(f for f in os.listdir(HERE) if f.endswith(".sh"))
        for s in scripts:
            subprocess.run(["bash", "-n", os.path.join(HERE, s)], check=True)
        if shutil.which("shellcheck"):
            r = subprocess.run(["shellcheck", "-x", "-S", "warning",
                                *("scripts/onprem/serving/" + s for s in scripts)],
                               cwd=ROOT, capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, r.stdout)

    def test_every_installed_file_is_here(self):
        install = self.read("install.sh")
        for name in re.findall(r'"\$HERE/([^"]+)"', install) + re.findall(r"(?<![\w-])((?:cloudflared-)?usnm-[a-z@.-]+\.(?:service|timer))\b", install):
            if name.startswith("../") or "$" in name:
                continue
            self.assertTrue(os.path.isfile(os.path.join(HERE, name)), name)

    def test_ports_agree(self):
        caddy = self.read("Caddyfile")
        self.assertIn("http://:8080 {\n\tbind 127.0.0.1\n", caddy)
        self.assertIn("admin 127.0.0.1:2019", caddy)
        self.assertIn("import /etc/usnm-serving/upstream.caddy", caddy)
        update = self.read("update-api.sh")
        self.assertIn("blue) echo 8081 ;; green) echo 8082", update)
        self.assertIn("http://127.0.0.1:8080/readyz", update)
        self.assertIn("http://127.0.0.1:7280/health/readyz", update)
        unit = self.read("usnm-api@.service")
        self.assertIn("-e USNM_BIND=${USNM_BIND}", unit)
        self.assertIn("--stop-timeout 40", unit)
        self.assertIn("--network host", unit)

    def test_tunnel_ingress_routes_only_the_site_to_caddy(self):
        rules = json.loads(self.read("tunnel-ingress.json"))["config"]["ingress"]
        self.assertEqual(rules[-1], {"service": "http_status:404"})
        for rule in rules[:-1]:
            self.assertRegex(rule["hostname"], r"^([a-z0-9-]+\.)?usnewsmap\.com$")
            self.assertEqual(rule["service"], "http://127.0.0.1:8080")

    def test_logs_keep_no_addresses_or_queries(self):
        cloudflared = self.read("cloudflared-usnm-serving.service")
        self.assertIn("--loglevel info", cloudflared)
        self.assertNotIn("debug", cloudflared.split("[Service]")[1])
        caddy = self.read("Caddyfile")
        self.assertIsNone(re.search(r"^\s*log\b", caddy, re.M))
        self.assertIn("MaxRetentionSec=30day", self.read("journald-usnm-serving.conf"))
        self.assertIn("e /srv/usnm-serving/cache - - - mM:14d", self.read("usnm-serving.tmpfiles"))

    def test_no_private_addresses_or_internal_names(self):
        private = re.compile(r"(^|[^0-9.])(10\.\d+\.\d+\.\d+|172\.(1[6-9]|2\d|3[01])\.\d+\.\d+|192\.168\.\d+\.\d+)")
        for name in os.listdir(HERE):
            with open(os.path.join(HERE, name), errors="replace") as f:
                text = f.read()
            if name == os.path.basename(__file__):
                continue
            self.assertIsNone(private.search(text), name)
            self.assertNotIn("tjg.fyi", text, name)
            self.assertIsNone(re.search(r"\bpve(-big)?\b", text), name)


if __name__ == "__main__":
    unittest.main()
