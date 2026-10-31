"""Tests for the on-prem serving scripts (#955): python3 -m unittest -v
scripts/onprem/serving/test_serving.py. Needs jq; shellcheck is used when
it's installed. Nothing here reaches GitHub, Azure or Cloudflare, or
changes the machine: every host tool the scripts call is a stub in a
directory of each test's own, which also stands in for / (the scripts'
USNM_SERVING_TEST_ROOT).

update-api.sh signs in to the registry on every run, and that and
install.sh need GNU tools (flock, realpath -e, stat -c, base64 -w0), so
their tests run on Linux only, as in CI."""

import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
LINUX = sys.platform.startswith("linux")
SHA = {n: c * 40 for n, c in (("old", "a"), ("mid", "b"), ("new", "c"))}
REGISTRY = "crexampletest"
HOST = REGISTRY + ".azurecr.io"


def manifest(name):
    return '{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","name":"%s"}' % name


def digest(text):
    return "sha256:" + hashlib.sha256(text.encode()).hexdigest()


# curl, as the scripts call it: the attestations API, the Arc agent's
# identity endpoint, the registry's token exchange, the searcher's and the
# colors' health, and Caddy's admin API, which reports the port
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
code, body = 200, ""
def upstream():
    try:
        return re.search(r"127\.0\.0\.1:(\d+)", open(root + "/etc/usnm-serving/upstream.caddy").read()).group(1)
    except (OSError, AttributeError):
        return None
if url.startswith("https://api.github.com/repos/tgoodyear/usnewsmap.com/attestations/"):
    f = os.path.join(stubs, "attestations.json")
    if os.path.exists(f):
        body = open(f).read()
    else:
        code = 404
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
    servers = {"srv0": {"listen": ["127.0.0.1:8080"], "routes": [{"handle": [{"handler": "reverse_proxy",
        "upstreams": [{"dial": "127.0.0.1:%s" % port}]}]}]}}
    if os.environ.get("STUB_EXTRA_SERVER"):
        servers["dev"] = {"listen": ["127.0.0.1:8088"], "routes": [{"handle": [
            {"handler": "reverse_proxy", "upstreams": [{"dial": "127.0.0.1:8091"}]}]}]}
    body = json.dumps(servers)
elif re.match(r"http://127\.0\.0\.1:(\d+)/readyz$", url):
    port = re.match(r"http://127\.0\.0\.1:(\d+)/", url).group(1)
    if port == "8080":
        port = os.environ.get("STUB_CADDY_STUCK") or upstream()
    code = 200 if port in ready else 503
else:
    code = 404
if opt("-o") and opt("-o") != "/dev/null":
    with open(opt("-o"), "w") as f:
        f.write(body)
elif not opt("-o"):
    sys.stdout.write(body)
if code >= 400 and any(a.startswith("-") and not a.startswith("--") and "f" in a for a in args):
    sys.exit(22)
'''

# Logs its arguments and succeeds, with the answers the scripts read: the
# registry's manifests from STUB_DIR/manifest-<tag>, and gh's verified
# attestation naming STUB_SOURCE as its source commit.
STUB_TOOL = r'''#!/usr/bin/env bash
echo "$(basename "$0") $*" >> "$STUB_LOG"
case "$(basename "$0") $1" in
  "systemctl is-active")
    for u in ${STUB_ACTIVE-usnm-searcher.service usnm-api@blue.service usnm-api@green.service}; do
      [ "$u" != "$3" ] || exit 0
    done
    exit 3 ;;
  "skopeo inspect")
    for a in "$@"; do ref=$a; done
    [ "${ref##*:}" != "${STUB_SKOPEO_DOWN:-}" ] || { echo "pinging container registry: connection reset" >&2; exit 1; }
    f="$STUB_DIR/manifest-${ref##*:}"
    [ -f "$f" ] || { echo "reading manifest: manifest unknown" >&2; exit 1; }
    cat "$f" ;;
  "gh attestation")
    [ -z "${STUB_BAD_ATTESTATION:-}" ] || exit 1
    printf '[{"verificationResult":{"signature":{"certificate":{"sourceRepositoryDigest":"%s"}}}}]\n' "$STUB_SOURCE" ;;
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
                    "STUB_SOURCE": SHA["new"], "TMPDIR": self.tmp}
        self.env.pop("JOURNAL_STREAM", None)

    def calls(self):
        with open(self.log) as f:
            return f.read().splitlines()


class UpdateApiFlags(Stubbed):
    def test_flags_are_checked(self):
        script = os.path.join(HERE, "update-api.sh")
        for args in (["--registry", "UPPER"], ["--registry", REGISTRY, "--commit", "abc"],
                     ["--registry", REGISTRY, "--ready-secs", "0"], ["--registry", REGISTRY, "--tag", "a b"],
                     ["--registry", REGISTRY, "extra"]):
            r = subprocess.run(["bash", script, *args, "--dry-run"], env=self.env, capture_output=True, text=True)
            self.assertEqual(r.returncode, 2, (args, r.stderr))


@unittest.skipUnless(LINUX, "needs GNU flock, realpath, stat and base64")
class UpdateApi(Stubbed):
    SCRIPT = os.path.join(HERE, "update-api.sh")

    def setUp(self):
        super().setUp()
        write(os.path.join(self.root, "var/opt/azcmagent/tokens/agent.key"), "challenge", 0o600)
        self.attested(True)

    def image(self, tag, name):
        """The registry's tag points at the manifest called name; its digest."""
        write(os.path.join(self.stubs, "manifest-" + tag), manifest(name))
        return digest(manifest(name))

    def release(self, commit, name):
        """A commit CI published: main and the commit's tag at one manifest."""
        self.image(commit, name)
        return self.image("main", name)

    def attested(self, yes):
        f = os.path.join(self.stubs, "attestations.json")
        if yes:
            write(f, json.dumps({"attestations": [{"bundle": {"b": 1}}]}))
        elif os.path.exists(f):
            os.remove(f)

    def serving(self, color, commit, name):
        port = {"blue": 8081, "green": 8082}[color]
        self.image(commit, name)
        write(os.path.join(self.root, "var/lib/usnm-serving/active"), color + "\n")
        write(os.path.join(self.root, "etc/usnm-serving/api-%s.env" % color),
              "API_IMAGE=%s/usnewsmap-api@%s\nAPI_COMMIT=%s\nUSNM_BIND=127.0.0.1:%d\n"
              % (HOST, digest(manifest(name)), commit, port))
        write(os.path.join(self.root, "etc/usnm-serving/upstream.caddy"), "reverse_proxy 127.0.0.1:%d\n" % port)

    def update(self, *extra, **env):
        return subprocess.run(["bash", self.SCRIPT, "--registry", REGISTRY, "--drain-secs", "0", "--ready-secs", "1",
                               *extra], env={**self.env, **env}, capture_output=True, text=True)

    def read(self, rel):
        with open(os.path.join(self.root, rel)) as f:
            return f.read()

    def exists(self, rel):
        return os.path.exists(os.path.join(self.root, rel))

    def github_calls(self):
        return [c for c in self.calls() if c.startswith("curl https://api.github.com/")]

    def test_first_deploy_starts_blue_and_points_caddy_at_it(self):
        d = self.release(SHA["new"], "new")
        r = self.update(STUB_READY_PORTS="8081")
        self.assertEqual(r.returncode, 0, r.stderr + r.stdout)
        self.assertEqual(self.read("etc/usnm-serving/api-blue.env"),
                         "API_IMAGE=%s/usnewsmap-api@%s\nAPI_COMMIT=%s\nUSNM_BIND=127.0.0.1:8081\n" % (HOST, d, SHA["new"]))
        self.assertEqual(self.read("etc/usnm-serving/upstream.caddy"), "reverse_proxy 127.0.0.1:8081\n")
        self.assertEqual(self.read("var/lib/usnm-serving/active"), "blue\n")
        calls = self.calls()
        verify = next(c for c in calls if c.startswith("gh attestation verify"))
        for part in ("--source-ref refs/heads/main", "--signer-workflow tgoodyear/usnewsmap.com/.github/workflows/ci.yml",
                     "--format json"):
            self.assertIn(part, verify)
        self.assertNotIn("--source-digest", verify)
        order = [c for c in calls if c.startswith(("systemctl restart", "systemctl reload", "systemctl enable",
                                                     "podman pull"))]
        self.assertEqual([c.split()[1] for c in order], ["pull", "restart", "reload", "enable"], order)
        self.assertFalse(any("arm-token" in c or "registry-refresh-token" in c for c in calls))
        self.assertFalse(self.exists("etc/usnm-serving/hold") or self.exists("var/lib/usnm-serving/pending"))

    def test_the_image_serving_asks_github_nothing(self):
        self.release(SHA["old"], "old")
        self.serving("blue", SHA["old"], "old")
        r = self.update()
        self.assertEqual((r.returncode, r.stdout), (0, ""), r.stderr)
        self.assertEqual(self.github_calls(), [])

    def test_switch_verifies_only_production_listener(self):
        self.serving("blue", SHA["old"], "old")
        self.release(SHA["new"], "new")
        r = self.update(STUB_READY_PORTS="8081 8082", STUB_EXTRA_SERVER="1")
        self.assertEqual(r.returncode, 0, r.stderr + r.stdout)
        self.assertEqual(self.read("var/lib/usnm-serving/active"), "green\n")

    def test_a_new_image_waits_for_its_attestation_then_gives_up(self):
        self.serving("blue", SHA["old"], "old")
        d = self.release(SHA["new"], "new")
        self.attested(False)
        r = self.update()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("waiting for the attestation", r.stdout)
        self.assertEqual(self.read("var/lib/usnm-serving/pending").split()[0], d)
        self.assertEqual(len(self.github_calls()), 1)
        # Past the first half hour, asked at most every 10 minutes.
        now = int(time.time())
        write(os.path.join(self.root, "var/lib/usnm-serving/pending"), "%s %d %d\n" % (d, now - 3600, now - 60))
        open(self.log, "w").close()
        self.assertEqual(self.update().returncode, 0)
        self.assertEqual(self.github_calls(), [])
        write(os.path.join(self.root, "var/lib/usnm-serving/pending"), "%s %d %d\n" % (d, now - 3600, now - 700))
        self.update()
        self.assertEqual(len(self.github_calls()), 1)
        # After 3 hours, rejected, and then never asked about again.
        write(os.path.join(self.root, "var/lib/usnm-serving/pending"), "%s %d %d\n" % (d, now - 4 * 3600, now - 700))
        r = self.update()
        self.assertEqual(r.returncode, 1)
        self.assertEqual(self.read("var/lib/usnm-serving/rejected"), d + "\n")
        open(self.log, "w").close()
        self.attested(True)
        r = self.update(STUB_READY_PORTS="8081 8082")
        self.assertEqual((r.returncode, self.github_calls()), (0, []))
        self.assertEqual(self.read("var/lib/usnm-serving/active"), "blue\n")

    def test_switch_drains_and_stops_the_old_color(self):
        self.serving("blue", SHA["old"], "old")
        self.release(SHA["new"], "new")
        r = self.update(STUB_READY_PORTS="8081 8082")
        self.assertEqual(r.returncode, 0, r.stderr + r.stdout)
        self.assertEqual(self.read("etc/usnm-serving/upstream.caddy"), "reverse_proxy 127.0.0.1:8082\n")
        self.assertEqual(self.read("var/lib/usnm-serving/active"), "green\n")
        calls = self.calls()
        self.assertIn("systemctl disable --quiet usnm-api@blue.service", calls)
        self.assertIn("systemctl stop usnm-api@blue.service", calls)
        self.assertLess(calls.index("systemctl reload caddy.service"), calls.index("systemctl stop usnm-api@blue.service"))

    def test_a_color_that_never_gets_ready_is_rejected_and_blue_keeps_serving(self):
        self.serving("blue", SHA["old"], "old")
        d = self.release(SHA["new"], "new")
        r = self.update(STUB_READY_PORTS="8081")
        self.assertEqual(r.returncode, 1)
        self.assertIn("is rejected", r.stdout + r.stderr)
        self.assertEqual(self.read("var/lib/usnm-serving/rejected"), d + "\n")
        self.assertEqual(self.read("etc/usnm-serving/upstream.caddy"), "reverse_proxy 127.0.0.1:8081\n")
        self.assertEqual(self.read("var/lib/usnm-serving/active"), "blue\n")
        self.assertIn("systemctl stop usnm-api@green.service", self.calls())
        self.assertNotIn("systemctl reload caddy.service", self.calls())

    def test_nothing_ready_means_the_host_not_the_image(self):
        self.serving("blue", SHA["old"], "old")
        self.release(SHA["new"], "new")
        r = self.update(STUB_READY_PORTS="")
        self.assertEqual(r.returncode, 1)
        self.assertIn("isn't rejected", r.stdout + r.stderr)
        self.assertFalse(self.exists("var/lib/usnm-serving/rejected"))

    def test_a_first_deploy_that_never_gets_ready_is_tried_again(self):
        self.release(SHA["new"], "new")
        r = self.update(STUB_READY_PORTS="")
        self.assertEqual(r.returncode, 1)
        self.assertIn("no color serves to compare", r.stdout + r.stderr)
        self.assertFalse(self.exists("var/lib/usnm-serving/rejected"))
        self.assertIn("systemctl stop usnm-api@blue.service", self.calls())

    def test_a_switch_whose_rollback_cant_be_confirmed_holds_with_both_up(self):
        self.serving("blue", SHA["old"], "old")
        d = self.release(SHA["new"], "new")
        r = self.update(STUB_READY_PORTS="8081 8082", STUB_CADDY_STUCK="8099")
        self.assertEqual(r.returncode, 1)
        self.assertIn("can't be confirmed", r.stdout + r.stderr)
        self.assertTrue(self.exists("etc/usnm-serving/hold"))
        self.assertIn(d, self.read("var/lib/usnm-serving/rejected"))
        self.assertEqual(self.read("var/lib/usnm-serving/active"), "blue\n")
        self.assertFalse([c for c in self.calls() if c.startswith("systemctl stop usnm-api@")])

    def test_a_switch_that_doesnt_take_goes_back(self):
        self.serving("blue", SHA["old"], "old")
        d = self.release(SHA["new"], "new")
        r = self.update(STUB_READY_PORTS="8081 8082", STUB_CADDY_STUCK="8081")
        self.assertEqual(r.returncode, 1)
        self.assertEqual(self.read("etc/usnm-serving/upstream.caddy"), "reverse_proxy 127.0.0.1:8081\n")
        self.assertEqual(self.read("var/lib/usnm-serving/active"), "blue\n")
        self.assertIn(d, self.read("var/lib/usnm-serving/rejected"))
        self.assertIn("systemctl stop usnm-api@green.service", self.calls())
        self.assertFalse(self.exists("etc/usnm-serving/hold"))

    def test_bad_attestation_or_mismatched_tag_rejects_before_pulling(self):
        d = self.release(SHA["new"], "new")
        r = self.update(STUB_READY_PORTS="8081", STUB_BAD_ATTESTATION="1")
        self.assertEqual(r.returncode, 1)
        self.assertIn(d, self.read("var/lib/usnm-serving/rejected"))
        # main moved on, but the attestation's commit tags another manifest.
        d2 = self.image("main", "newer")
        r = self.update(STUB_READY_PORTS="8081")
        self.assertEqual(r.returncode, 1)
        self.assertIn("isn't the same manifest", r.stdout + r.stderr)
        self.assertIn(d2, self.read("var/lib/usnm-serving/rejected"))
        self.assertFalse(any(c.startswith("podman pull") for c in self.calls()))

    def test_a_registry_error_on_the_commit_tag_is_tried_again(self):
        self.serving("blue", SHA["old"], "old")
        d = self.release(SHA["new"], "new")
        r = self.update(STUB_READY_PORTS="8081 8082", STUB_SKOPEO_DOWN=SHA["new"])
        self.assertEqual(r.returncode, 1)
        self.assertIn("connection reset", r.stdout + r.stderr)
        self.assertFalse(self.exists("var/lib/usnm-serving/rejected"))
        # The registry answering that there's no such tag is final.
        os.remove(os.path.join(self.stubs, "manifest-" + SHA["new"]))
        r = self.update(STUB_READY_PORTS="8081 8082")
        self.assertEqual(r.returncode, 1)
        self.assertEqual(self.read("var/lib/usnm-serving/rejected"), d + "\n")

    def test_dry_run_never_records_a_rejection(self):
        self.release(SHA["new"], "new")
        r = self.update("--dry-run", STUB_BAD_ATTESTATION="1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("dry run:", r.stdout + r.stderr)
        self.assertFalse(self.exists("var/lib/usnm-serving/rejected") or self.exists("var/lib/usnm-serving/pending"))

    def test_the_default_wait_covers_the_apis_start_budget(self):
        with open(self.SCRIPT) as f:
            default = int(re.search(r"READY_SECS=(\d+)", f.read()).group(1))
        with open(os.path.join(ROOT, "infra/modules/containerapp.bicep")) as f:
            bicep = f.read()
        load = int(re.search(r"var loadSecs = (\d+)", bicep).group(1))
        warm = int(re.search(r"var startupWarmUpSecs = (\d+)", bicep).group(1))
        self.assertGreaterEqual(default, load + warm + 60)

    def test_commit_rolls_back_and_holds(self):
        self.release(SHA["old"], "old")
        self.serving("green", SHA["new"], "new")
        self.image("main", "new")
        r = self.update("--commit", SHA["old"], STUB_READY_PORTS="8081 8082", STUB_SOURCE=SHA["old"])
        self.assertEqual(r.returncode, 0, r.stderr + r.stdout)
        self.assertIn("API_COMMIT=" + SHA["old"], self.read("etc/usnm-serving/api-blue.env"))
        verify = next(c for c in self.calls() if c.startswith("gh attestation verify"))
        self.assertIn("--source-digest " + SHA["old"], verify)
        self.assertTrue(self.exists("etc/usnm-serving/hold"))
        r = self.update(STUB_READY_PORTS="8081 8082")
        self.assertIn("held:", r.stdout)

    def test_commit_already_serving_still_holds(self):
        self.serving("blue", SHA["old"], "old")
        r = self.update("--commit", SHA["old"])
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(self.exists("etc/usnm-serving/hold"))
        self.assertFalse(any(c.startswith("podman pull") for c in self.calls()))

    def test_dry_run_changes_nothing(self):
        self.serving("blue", SHA["old"], "old")
        self.release(SHA["new"], "new")
        r = self.update("--dry-run")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("would deploy %s to green (now %s on blue)" % (SHA["new"], SHA["old"]), r.stdout)
        self.assertFalse(self.exists("var/lib/usnm-serving/pending") or self.exists("etc/usnm-serving/api-green.env"))

    def test_hold_and_searcher_down_do_nothing(self):
        self.serving("blue", SHA["old"], "old")
        self.release(SHA["new"], "new")
        write(os.path.join(self.root, "etc/usnm-serving/hold"), "")
        self.assertIn("held:", self.update().stdout)
        os.remove(os.path.join(self.root, "etc/usnm-serving/hold"))
        self.assertIn("searcher isn't ready", self.update(STUB_SEARCHER_DOWN="1").stdout)
        self.assertIn("searcher isn't ready", self.update(STUB_ACTIVE="").stdout)
        self.assertFalse(any(c.startswith("skopeo") for c in self.calls()))

    def test_unused_images_are_removed(self):
        self.serving("blue", SHA["old"], "old")
        self.release(SHA["new"], "new")
        keep = "%s/usnewsmap-api@%s" % (HOST, digest(manifest("old")))
        stale = "%s/usnewsmap-api@sha256:%s" % (HOST, "9" * 64)
        other = "docker.io/library/other@sha256:%s" % ("8" * 64)
        r = self.update(STUB_READY_PORTS="8081 8082", STUB_IMAGES=" ".join((keep, stale, other)))
        self.assertEqual(r.returncode, 0, r.stderr + r.stdout)
        removed = [c for c in self.calls() if c.startswith("podman rmi")]
        self.assertEqual(removed, ["podman rmi " + stale])


class GenEnv(unittest.TestCase):
    SCRIPT = os.path.join(HERE, "gen-env.sh")
    LOCAL = {"STORAGE_ACCOUNT": "stexampletest", "COSMOS_ENDPOINT": "https://cosmos-example.documents.azure.com:443/",
             "APPLICATIONINSIGHTS_CONNECTION_STRING": "IngestionEndpoint=https://in.example/;ApplicationId=app",
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

    def test_production_enables_shared_log_and_configured_ocr_requests(self):
        env_file = os.path.join(self.env["USNM_ENV_DIR"], "prod.json")
        with open(env_file) as stream:
            settings = json.load(stream)
        settings["USNM_OCR_LOCAL"] = True
        write(env_file, json.dumps(dict(sorted(settings.items())), indent=2) + "\n")
        r = self.gen("--dry-run", "--production")
        self.assertEqual(r.returncode, 0, r.stderr)
        api = dict(line.split("=", 1) for line in self.files(r.stdout)["api.env"].splitlines())
        self.assertEqual(api["USNM_SEARCH_LOG_URL"], "https://stexampletest.blob.core.windows.net/searches")
        self.assertEqual(api["USNM_OCR_REQUESTS_URL"], "https://stexampletest.blob.core.windows.net/ocr-requests")

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
        self.assertEqual(api["USNM_SITEMAP_STATE_URL"], "/srv/usnm-serving/sitemap-state")
        self.assertEqual(api["USNM_ALLOWED_ORIGINS"], "https://usnewsmap.com,https://www.usnewsmap.com")
        self.assertEqual(api["USNM_CONGRESS_SEARCH"], "false")
        self.assertEqual(api["USNM_BROWSE_PAGES"], "true")
        self.assertEqual(api["USNM_INGEST_CRON"], "0 6 * * 1")
        self.assertEqual(api["USNM_COMPUTE_CONCURRENCY"], "2")
        self.assertEqual((api["USNM_PREWARM_STARTUP_BUDGET_SECS"], api["USNM_READY_CAP_SECS"]), ("300", "360"))
        self.assertEqual(api["IDENTITY_ENDPOINT"], "http://localhost:40342/metadata/identity/oauth2/token")
        self.assertEqual(api["USNM_TRUST_CF_CONNECTING_IP"], "true")
        self.assertEqual(api["USNM_READY_REQUIRES_PUBLISHED_INDEXES"], "true")
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
        for k in ("IDENTITY_ENDPOINT", "IMDS_ENDPOINT", "USNM_SITEMAP_STATE_URL", "USNM_TRUST_CF_CONNECTING_IP",
                  "USNM_READY_REQUIRES_PUBLISHED_INDEXES"):
            del api[k]
        api.update({"USNM_QUICKWIT_URL": "http://usnm-search-ca-prod", "USNM_RESPONSE_CACHE_URL": "https://x/cache",
                    "USNM_TRUSTED_PROXY_HOPS": "1", "USNM_COMPUTE_CONCURRENCY": "1", "AZURE_CLIENT_ID": "id",
                    "USNM_SEARCH_LOG_URL": "https://x/searches", "USNM_OCR_REQUESTS_URL": "https://x/ocr-requests",
                    "USNM_ALLOWED_ORIGINS": "https://usnewsmap.com,https://www.usnewsmap.com,https://ca.example"})
        api.update(changes)
        with open(self.env["STUB_AZ"], "w") as f:
            json.dump([{"name": k, "value": v} for k, v in api.items() if v is not None], f)

    def test_check_app_accepts_declared_cold_standby_but_rejects_readiness_drift(self):
        env_file = os.path.join(self.env["USNM_ENV_DIR"], "prod.json")
        with open(env_file) as stream:
            settings = json.load(stream)
        settings["USNM_API_MIN_REPLICAS"] = 0
        write(env_file, json.dumps(dict(sorted(settings.items())), indent=2) + "\n")
        self.azure_app(USNM_PREWARM_STARTUP_BUDGET_SECS="0", USNM_READY_CAP_SECS="60")
        result = self.gen("--dry-run", "--check-app")
        self.assertEqual(result.returncode, 0, result.stderr)
        api = dict(line.split("=", 1) for line in self.files(result.stdout)["api.env"].splitlines())
        self.assertEqual(api["USNM_PREWARM_STARTUP_BUDGET_SECS"], "300")
        self.assertEqual(api["USNM_READY_CAP_SECS"], "360")
        self.azure_app(USNM_PREWARM_STARTUP_BUDGET_SECS="0", USNM_READY_CAP_SECS="360")
        result = self.gen("--dry-run", "--check-app")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("expected Azure 60", result.stderr)

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
            self.assertEqual(f.read(), "HIMDS_GID=4242\nUSNM_SERVING_REGISTRY=%s\n" % REGISTRY)
        self.assertEqual(stat.S_IMODE(os.stat(os.path.join(etc, "usnm-serving/api.env")).st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(os.stat(os.path.join(self.root, "usr/local/sbin/usnm-serving-update")).st_mode),
                         0o755)
        with open(os.path.join(etc, "usnm-serving/upstream.caddy")) as f:
            self.assertEqual(f.read(), "reverse_proxy 127.0.0.1:8081\n")
        for unit in ("usnm-searcher.service", "usnm-api@.service", "cloudflared-usnm-serving.service",
                     "usnm-serving-update.service", "usnm-serving-update.timer"):
            self.assertTrue(os.path.isfile(os.path.join(etc, "systemd/system", unit)), unit)
        self.assertTrue(os.path.isfile(os.path.join(etc, "systemd/journald.conf.d/usnm-serving.conf")))
        self.assertIn("chown 1000:1000 %s/srv/usnm-serving/cache" % self.root, self.calls())
        self.assertIn("chown 1000:1000 %s/srv/usnm-serving/sitemap-state" % self.root, self.calls())
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

    def test_production_explicitly_preserves_shared_writers(self):
        write(os.path.join(self.config, "api.env"),
              "USNM_SEARCH_LOG_URL=https://x/searches\nUSNM_OCR_REQUESTS_URL=https://x/ocr-requests\n")
        r = self.install("--production")
        self.assertEqual(r.returncode, 0, r.stderr + r.stdout)
        with open(os.path.join(self.root, "etc/usnm-serving/api.env")) as stream:
            self.assertIn("USNM_SEARCH_LOG_URL=https://x/searches", stream.read())


class Static(unittest.TestCase):
    def test_metrics_listener_rejects_non_loopback_and_invalid_ports(self):
        for value in ("0.0.0.0:20241", "127.0.0.1:0", "127.0.0.1:65536"):
            result = subprocess.run(
                ["bash", os.path.join(HERE, "run-cloudflared.sh")],
                env={**os.environ, "TUNNEL_METRICS": value},
                capture_output=True, text=True)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("TUNNEL_METRICS", result.stderr)
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

    def test_flood_rule_is_recorded_off_and_blocks_only_the_searches(self):
        # ADR-0027 settled decision 9. Keys are sorted, as in every keyed data file.
        def sorted_keys(pairs):
            keys = [k for k, _ in pairs]
            self.assertEqual(keys, sorted(keys))
            return dict(pairs)

        text = self.read("flood-rule.json")
        rules = json.loads(text, object_pairs_hook=sorted_keys)["rules"]
        # Free allows one rate-limit rule.
        self.assertEqual(len(rules), 1)
        rule = rules[0]
        # Written and off until a flood.
        self.assertIs(rule["enabled"], False)
        # Block sets no cookie; every challenge action would set cf_clearance, and
        # /privacy says the site sets none.
        self.assertEqual(rule["action"], "block")
        self.assertNotIn("action_parameters", rule)
        self.assertNotIn("challenge", text)
        # The searches only, under both prefixes the API serves. They are checked
        # against the API's own routes so a rename can't leave the rule matching nothing.
        match = re.fullmatch(r'http\.request\.uri\.path in \{((?:"[^"]+" ?)+)\}', rule["expression"])
        self.assertIsNotNone(match, rule["expression"])
        self.assertEqual(sorted(re.findall(r'"([^"]+)"', match.group(1))),
                         ["/api/v1/aggregate", "/api/v1/hits", "/v1/aggregate", "/v1/hits"])
        with open(os.path.join(ROOT, "crates", "usnm-api", "src", "lib.rs")) as f:
            api = f.read()
        for route in ('("/aggregate", get(routes::aggregate))', '("/hits", get(routes::hits))',
                      '.nest("/v1", v1.clone())', '.nest("/api/v1", v1)'):
            self.assertIn(route, api)
        # Free counts an IP over 10 seconds and blocks for 10 seconds. "IP with NAT
        # support" would set a _cfuvid cookie, so the counters are the data center
        # (the API requires it) and the plain IP.
        limit = rule["ratelimit"]
        self.assertEqual(limit["characteristics"], ["cf.colo.id", "ip.src"])
        self.assertEqual(limit["period"], 10)
        self.assertEqual(limit["mitigation_timeout"], 10)
        self.assertEqual(limit["requests_per_period"], 50)
        # It only bites past the API's own limit (so the API's 429s stay the normal case).
        with open(os.path.join(ROOT, "crates", "usnm-api", "src", "config.rs")) as f:
            per_minute = int(re.search(r'"USNM_RATE_PER_MIN", (\d+)\)', f.read()).group(1))
        self.assertGreater(limit["requests_per_period"] * 60, per_minute * limit["period"])
        # An account or zone id belongs to the operator's run, not the repository.
        self.assertIsNone(re.search(r"\b[0-9a-f]{32}\b", text))

    def test_logs_keep_no_addresses_or_queries(self):
        self.assertIn("ExecStart=/usr/local/libexec/usnm-serving/run-cloudflared",
                      self.read("cloudflared-usnm-serving.service"))
        run = self.read("run-cloudflared.sh")
        self.assertIn("--loglevel info", run)
        self.assertNotIn("debug", run.split("set -euo pipefail")[1])
        caddy = self.read("Caddyfile")
        # No site logs access; the default log drops each line's request.
        self.assertEqual(caddy.split("http://:8080")[1].count("log"), 0)
        self.assertIn("\tlog default {\n\t\tformat filter {\n\t\t\twrap json\n\t\t\tfields {\n"
                      "\t\t\t\trequest delete\n", caddy)
        self.assertIn("MaxRetentionSec=30day", self.read("journald-usnm-serving.conf"))
        tmpfiles = self.read("usnm-serving.tmpfiles")
        for color in ("blue", "green"):
            self.assertIn("d /srv/usnm-serving/cache/%s 0750 1000 1000 mM:14d" % color, tmpfiles)
        self.assertIn("-v /srv/usnm-serving/cache/%i:/srv/usnm-serving/cache", self.read("usnm-api@.service"))
        self.assertIn("-v /srv/usnm-serving/sitemap-state:/srv/usnm-serving/sitemap-state",
                      self.read("usnm-api@.service"))
        self.assertIn("d /srv/usnm-serving/sitemap-state 0750 1000 1000 -", tmpfiles)

    @unittest.skipUnless(LINUX, "needs GNU sed")
    def test_cloudflared_lines_lose_their_query_strings(self):
        program = re.search(r"sed -u -E '([^']+)'", self.read("run-cloudflared.sh")).group(1)
        line = ('ERR  error="Unable to reach the origin service: Get \\"http://127.0.0.1:8080/v1/aggregate'
                '?q=secret+words&from=1900\\": dial tcp 127.0.0.1:8080: connect: connection refused" '
                'dest=https://usnewsmap.com/v1/hits?q=other+secret&page=2 ip=198.41.200.13 type=http')
        out = subprocess.run(["sed", "-E", program], input=line + "\nINF Registered tunnel connection\n",
                             capture_output=True, text=True, check=True).stdout
        self.assertNotIn("secret", out)
        self.assertIn('/v1/aggregate?[query removed]\\": dial tcp', out)
        self.assertIn("dest=https://usnewsmap.com/v1/hits?[query removed] ip=198.41.200.13", out)
        self.assertIn("INF Registered tunnel connection\n", out)

    def test_no_private_addresses_or_internal_names(self):
        private = re.compile(r"(^|[^0-9.])(10\.\d+\.\d+\.\d+|172\.(1[6-9]|2\d|3[01])\.\d+\.\d+|192\.168\.\d+\.\d+)")
        for name in os.listdir(HERE):
            path = os.path.join(HERE, name)
            if name == os.path.basename(__file__) or not os.path.isfile(path):
                continue
            with open(path, errors="replace") as f:
                text = f.read()
            self.assertIsNone(private.search(text), name)
            # The home network's domain, from its parts, so it isn't written here either.
            self.assertNotIn("tjg" + "." + "fyi", text, name)
            self.assertIsNone(re.search(r"\bpve(-big)?\b", text), name)


if __name__ == "__main__":
    unittest.main()
