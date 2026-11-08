"""Tests for the feed monitor's bundle, updater and installer (#1441):
python3 -m unittest -v boinc/scripts/onprem/feed-monitor/test_feed_monitor.py.
Needs git and jq; shellcheck is used when it's installed. Nothing here
reaches GitHub or changes the machine: curl, gh and systemctl are stubs in a
directory of each test's own, which also stands in for / (the scripts'
USNM_FEED_MONITOR_TEST_ROOT). Bundles are real ones, built by bundle.sh from
commits in a scratch repository.

The scripts need GNU tools (tar --sort, find -printf, stat -c, mv -T), so
those tests run on Linux only, as in CI."""

import ast
import hashlib
import io
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", "..", "..", ".."))
SCRIPTS = os.path.join(ROOT, "boinc", "scripts")
LINUX = sys.platform.startswith("linux")
REPO = "tgoodyear/usnewsmap.com"
DOWNLOAD = "https://github.com/%s/releases/download/" % REPO
API = "https://api.github.com/repos/%s/attestations/sha256:" % REPO
BUNDLED = ["acceptance_client.py", "client_account.py", "feed_collect.py", "feed_status.py"]


def shell_list(path, name):
    """The words of NAME="..." in a shell script."""
    for line in open(path):
        if line.startswith(name + '="'):
            return line.split('"')[1].split()
    raise AssertionError("no %s in %s" % (name, path))


# curl, as the scripts call it: release downloads served from STUB_DIR/releases/<tag>/,
# the attestations API (an attestation exists when STUB_DIR/attested/<digest> does), and
# gh's own release tarball for install.sh. Every call is logged to STUB_LOG.
STUB_CURL = r'''#!/usr/bin/env python3
import json, os, shutil, sys
args = sys.argv[1:]
stubs = os.environ["STUB_DIR"]
url = next(a for a in reversed(args) if a.startswith("http"))
out = args[args.index("-o") + 1] if "-o" in args else None
with open(os.environ["STUB_LOG"], "a") as log:
    log.write("curl %s\n" % url)
dl = "https://github.com/tgoodyear/usnewsmap.com/releases/download/"
api = "https://api.github.com/repos/tgoodyear/usnewsmap.com/attestations/sha256:"
if url.startswith(dl):
    path = os.path.join(stubs, "releases", url[len(dl):])
    if not os.path.isfile(path):
        sys.stderr.write("curl: (22) The requested URL returned error: 404\n")
        sys.exit(22)
    shutil.copyfile(path, out)
elif url.startswith(api):
    d = url[len(api):]
    if not os.path.exists(os.path.join(stubs, "attested", d)):
        sys.exit(22)
    print(json.dumps({"attestations": [{"bundle": {"digest": d}}]}))
elif url.startswith("https://github.com/cli/cli/releases/download/"):
    shutil.copyfile(os.path.join(stubs, "gh.tar.gz"), out)
else:
    sys.exit(7)
'''

# gh: --version, and attestation verify, which passes unless STUB_DIR/bad-signature exists
# and names the commit STUB_DIR/attested/<digest> holds.
STUB_GH = r'''#!/usr/bin/env python3
import hashlib, json, os, sys
args = sys.argv[1:]
stubs = os.environ["STUB_DIR"]
with open(os.environ["STUB_LOG"], "a") as log:
    log.write("gh %s\n" % " ".join(args))
if args == ["--version"]:
    print("gh version 2.103.3 (2026-10-26)")
    sys.exit(0)
assert args[:2] == ["attestation", "verify"], args
for flag, want in (("--repo", "tgoodyear/usnewsmap.com"),
                   ("--signer-workflow", "tgoodyear/usnewsmap.com/.github/workflows/feed-monitor-release.yml"),
                   ("--source-ref", "refs/heads/main")):
    assert args[args.index(flag) + 1] == want, (flag, args)
assert "GH_TOKEN" not in os.environ and "GITHUB_TOKEN" not in os.environ
if os.path.exists(os.path.join(stubs, "bad-signature")):
    sys.stderr.write("verification failed\n")
    sys.exit(1)
d = hashlib.sha256(open(args[2], "rb").read()).hexdigest()
commit = open(os.path.join(stubs, "attested", d)).read().strip()
print(json.dumps([{"verificationResult": {"signature": {"certificate": {"sourceRepositoryDigest": commit}}}}]))
'''

# systemctl: list-units prints STUB_DIR/busy's lines while it exists (each call takes one
# away when STUB_DIR/busy-countdown does), and every other call succeeds.
STUB_SYSTEMCTL = r'''#!/usr/bin/env python3
import os, sys
stubs = os.environ["STUB_DIR"]
with open(os.environ["STUB_LOG"], "a") as log:
    log.write("systemctl %s\n" % " ".join(sys.argv[1:]))
busy = os.path.join(stubs, "busy")
if sys.argv[1:2] == ["list-units"] and os.path.exists(busy):
    lines = open(busy).read().splitlines()
    if lines:
        print(lines[0])
    if os.path.exists(os.path.join(stubs, "busy-countdown")):
        open(busy, "w").write("\n".join(lines[1:]))
'''

STUB_APT = r'''#!/bin/sh
echo "apt-get $*" >> "$STUB_LOG"
'''


def git(repo, *args, when=None):
    env = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.com",
               GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.com")
    if when is not None:
        env["GIT_AUTHOR_DATE"] = env["GIT_COMMITTER_DATE"] = "@%d +0000" % when
    return subprocess.run(["git", "-C", repo, *args], check=True, capture_output=True,
                          text=True, env=env).stdout.strip()


class Imports(unittest.TestCase):
    """The bundle holds every local module its scripts import."""

    def test_bundle_and_updater_name_the_same_files(self):
        self.assertEqual(shell_list(os.path.join(HERE, "bundle.sh"), "FILES"), BUNDLED)
        self.assertEqual(shell_list(os.path.join(HERE, "update.sh"), "FILES"), ["VERSION.json"] + BUNDLED)

    def test_every_local_import_is_bundled(self):
        local = {name[:-3] for name in os.listdir(SCRIPTS) if name.endswith(".py")}
        for name in BUNDLED:
            tree = ast.parse(open(os.path.join(SCRIPTS, name)).read())
            for node in ast.walk(tree):
                if isinstance(node, ast.Import):
                    mods = [a.name.split(".")[0] for a in node.names]
                elif isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
                    mods = [node.module.split(".")[0]]
                else:
                    continue
                for mod in mods:
                    if mod in local:
                        self.assertIn(mod + ".py", BUNDLED, "%s imports %s" % (name, mod))

    def test_the_units_run_the_bundled_scripts_from_the_live_path(self):
        units = os.path.join(SCRIPTS, "onprem")
        for unit in ("usnm-boinc-feed-source@.service", "usnm-boinc-feed-health.service"):
            with open(os.path.join(units, unit)) as f:
                text = f.read()
            run = [w for w in text.split() if w.startswith("/opt/")]
            self.assertTrue(run, unit)
            for path in run:
                self.assertTrue(path.startswith("/opt/usnm-feed-monitor/"), path)
                self.assertIn(os.path.basename(path), BUNDLED)

    def test_the_workflow_triggers_on_every_bundled_file(self):
        with open(os.path.join(ROOT, ".github", "workflows", "feed-monitor-release.yml")) as f:
            text = f.read()
        for name in BUNDLED:
            self.assertIn("- boinc/scripts/" + name + "\n", text)
        self.assertIn("- boinc/scripts/onprem/feed-monitor/bundle.sh\n", text)

    @unittest.skipUnless(shutil.which("shellcheck"), "shellcheck isn't installed")
    def test_shellcheck(self):
        paths = [os.path.join(HERE, n) for n in ("bundle.sh", "update.sh", "install.sh")]
        paths.append(os.path.join(ROOT, "scripts", "ci", "feed-monitor-release.sh"))
        subprocess.run(["shellcheck", "-x", "-S", "warning", *paths], check=True)

    def test_ci_shellchecks_the_scripts(self):
        ci = open(os.path.join(ROOT, ".github", "workflows", "ci.yml")).read()
        for name in ("boinc/scripts/onprem/feed-monitor/*.sh", "scripts/ci/feed-monitor-release.sh"):
            self.assertIn(name, ci)


@unittest.skipUnless(LINUX, "the scripts need GNU tools; CI runs these on Linux")
class FeedMonitor(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.root = os.path.join(self.tmp, "root")
        self.stubs = os.path.join(self.tmp, "stubs")
        self.repo = os.path.join(self.tmp, "repo")
        self.log = os.path.join(self.tmp, "log")
        for d in (self.root, self.stubs, os.path.join(self.stubs, "attested"),
                  os.path.join(self.stubs, "releases"), os.path.join(self.stubs, "bin")):
            os.makedirs(d)
        for name, text in (("curl", STUB_CURL), ("systemctl", STUB_SYSTEMCTL), ("apt-get", STUB_APT)):
            self._stub(os.path.join(self.stubs, "bin", name), text)
        self._stub(os.path.join(self.root, "usr/local/libexec/usnm-feed-monitor/gh"), STUB_GH)
        open(self.log, "w").close()
        # A scratch repository with the real scripts and bundle.sh.
        os.makedirs(os.path.join(self.repo, "boinc/scripts/onprem/feed-monitor"))
        for name in BUNDLED:
            shutil.copy(os.path.join(SCRIPTS, name), os.path.join(self.repo, "boinc/scripts", name))
        shutil.copy(os.path.join(HERE, "bundle.sh"), os.path.join(self.repo, "boinc/scripts/onprem/feed-monitor"))
        git(self.repo, "init", "-q", "-b", "main")
        self.when = 1_790_000_000
        git(self.repo, "add", ".")
        git(self.repo, "commit", "-qm", "first", when=self.when)

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def _stub(self, path, text):
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(text)
        os.chmod(path, 0o755)

    def env(self):
        return dict(os.environ, USNM_FEED_MONITOR_TEST_ROOT=self.root, STUB_DIR=self.stubs,
                    STUB_LOG=self.log, TMPDIR=self.tmp,
                    PATH=os.path.join(self.stubs, "bin") + os.pathsep + os.environ["PATH"])

    def commit(self, marker, when):
        with open(os.path.join(self.repo, "boinc/scripts/feed_status.py"), "a") as f:
            f.write("# %s\n" % marker)
        git(self.repo, "commit", "-qam", marker, when=when)
        return git(self.repo, "rev-parse", "HEAD")

    def build(self, commit="HEAD"):
        out = os.path.join(self.tmp, "dist-" + git(self.repo, "rev-parse", "--short", commit))
        shutil.rmtree(out, ignore_errors=True)
        tag = subprocess.run([os.path.join(self.repo, "boinc/scripts/onprem/feed-monitor/bundle.sh"),
                              "--out", out, "--commit", commit],
                             check=True, capture_output=True, text=True).stdout.strip()
        return tag, out

    def publish(self, out, tag, latest=True, attest=True, commit=None):
        """The release (and feed-monitor-latest), and the attestation of its archive."""
        data = open(os.path.join(out, "usnm-feed-monitor.tar.gz"), "rb").read()
        digest = hashlib.sha256(data).hexdigest()
        for t in [tag] + (["feed-monitor-latest"] if latest else []):
            d = os.path.join(self.stubs, "releases", t)
            os.makedirs(d, exist_ok=True)
            for f in ("usnm-feed-monitor.tar.gz", "SHA256SUMS"):
                shutil.copy(os.path.join(out, f), d)
        if attest:
            meta = json.load(tarfile.open(os.path.join(out, "usnm-feed-monitor.tar.gz")).extractfile("VERSION.json"))
            with open(os.path.join(self.stubs, "attested", digest), "w") as f:
                f.write(commit or meta["commit"])
        return digest

    def release(self, marker=None, when=None, **kw):
        if marker:
            self.when = when or self.when + 3600
            self.commit(marker, self.when)
        tag, out = self.build()
        return tag, self.publish(out, tag, **kw)

    def update(self, *args, ok=True):
        p = subprocess.run(["bash", os.path.join(HERE, "update.sh"), *args], env=self.env(),
                           capture_output=True, text=True)
        if ok is not None:
            self.assertEqual(p.returncode == 0, ok, p.stdout + p.stderr)
        return p.stdout + p.stderr

    def path(self, *parts):
        return os.path.join(self.root, *parts)

    def live(self):
        link = self.path("opt/usnm-feed-monitor")
        return os.path.basename(os.readlink(link)) if os.path.islink(link) else None

    def rejected(self):
        try:
            return open(self.path("var/lib/usnm-feed-monitor-update/rejected")).read().split()
        except FileNotFoundError:
            return []

    def calls(self, prefix):
        return [line for line in open(self.log).read().splitlines() if line.startswith(prefix)]

    # The bundle.

    def test_the_bundle_is_reproducible_and_holds_exactly_the_scripts(self):
        tag, out = self.build()
        first = open(os.path.join(out, "usnm-feed-monitor.tar.gz"), "rb").read()
        shutil.rmtree(out)
        self.assertEqual(self.build()[0], tag)
        self.assertEqual(open(os.path.join(out, "usnm-feed-monitor.tar.gz"), "rb").read(), first)
        sha = git(self.repo, "rev-parse", "HEAD")
        self.assertEqual(tag, "feed-monitor-%s-%s" % (time.strftime("%Y%m%d", time.gmtime(self.when)), sha[:7]))
        with tarfile.open(os.path.join(out, "usnm-feed-monitor.tar.gz")) as t:
            members = t.getmembers()
            self.assertEqual(sorted(m.name for m in members), ["VERSION.json"] + BUNDLED)
            for m in members:
                self.assertTrue(m.isreg())
                self.assertEqual((m.uid, m.gid, m.mode, m.mtime), (0, 0, 0o644, self.when))
            meta = json.load(t.extractfile("VERSION.json"))
            for name in BUNDLED:
                self.assertEqual(meta["files"][name], hashlib.sha256(t.extractfile(name).read()).hexdigest())
        self.assertEqual((meta["version"], meta["commit"], meta["committed_at"]), (tag, sha, self.when))
        sums = open(os.path.join(out, "SHA256SUMS")).read().split()
        self.assertEqual(sums, [hashlib.sha256(first).hexdigest(), "usnm-feed-monitor.tar.gz"])

    def test_the_bundle_is_the_commit_s_not_the_working_tree_s(self):
        with open(os.path.join(self.repo, "boinc/scripts/feed_collect.py"), "a") as f:
            f.write("# uncommitted\n")
        _, out = self.build()
        with tarfile.open(os.path.join(out, "usnm-feed-monitor.tar.gz")) as t:
            self.assertNotIn(b"uncommitted", t.extractfile("feed_collect.py").read())

    # The updater.

    def test_first_install_moves_hand_copied_files_aside_and_links_the_release(self):
        hand = self.path("opt/usnm-feed-monitor")
        os.makedirs(hand)
        open(os.path.join(hand, "feed_collect.py"), "w").write("# hand-copied\n")
        tag, digest = self.release()
        out = self.update()
        self.assertIn("usnm-feed-monitor deployed: hand-copied files -> %s" % tag, out)
        self.assertEqual(self.live(), tag)
        self.assertEqual(os.readlink(hand), self.path("opt/usnm-feed-monitor-releases", tag))
        aside = [n for n in os.listdir(self.path("opt")) if n.startswith("usnm-feed-monitor.pre-ci-")]
        self.assertEqual(len(aside), 1)
        self.assertEqual(open(self.path("opt", aside[0], "feed_collect.py")).read(), "# hand-copied\n")
        for name in BUNDLED:
            self.assertEqual(open(os.path.join(hand, name)).read(),
                             git(self.repo, "show", "HEAD:boinc/scripts/" + name) + "\n")
        self.assertEqual(open(self.path("opt/usnm-feed-monitor-releases", tag, ".bundle-sha256")).read().strip(), digest)
        self.assertFalse(os.path.exists(self.path("etc/usnm-feed-monitor-update/hold")))

    def test_a_run_that_finds_the_installed_bundle_asks_the_api_nothing(self):
        self.release()
        self.update()
        open(self.log, "w").close()
        self.assertEqual(self.update(), "")
        self.assertEqual(self.calls("curl"), ["curl " + DOWNLOAD + "feed-monitor-latest/SHA256SUMS"])
        self.assertEqual(self.calls("gh"), [])

    def test_a_new_bundle_replaces_the_old_and_the_old_is_kept_for_rollback(self):
        first, _ = self.release()
        self.update()
        second, _ = self.release("second")
        self.assertIn("deployed: %s -> %s" % (first, second), self.update())
        self.assertEqual(self.live(), second)
        self.assertEqual(open(self.path("var/lib/usnm-feed-monitor-update/previous")).read().strip(), first)
        self.assertTrue(os.path.isdir(self.path("opt/usnm-feed-monitor-releases", first)))

    def test_old_releases_are_pruned_but_live_and_previous_stay(self):
        tags = []
        for i in range(5):
            tags.append(self.release("r%d" % i)[0])
            self.update()
            # Distinct directory times, as installs minutes apart have.
            os.utime(self.path("opt/usnm-feed-monitor-releases", tags[-1]), (1000 + i, 1000 + i))
        kept = sorted(n for n in os.listdir(self.path("opt/usnm-feed-monitor-releases")) if not n.startswith("."))
        self.assertEqual(kept, sorted(tags[-3:]))

    def test_rollback_returns_to_the_previous_release_and_holds(self):
        first, _ = self.release()
        self.update()
        second, _ = self.release("second")
        self.update()
        self.assertIn("rolled back: %s -> %s" % (second, first), self.update("--rollback"))
        self.assertEqual(self.live(), first)
        self.assertTrue(os.path.exists(self.path("etc/usnm-feed-monitor-update/hold")))
        self.assertIn("held", self.update())
        self.assertEqual(self.live(), first)
        os.remove(self.path("etc/usnm-feed-monitor-update/hold"))
        self.update()
        self.assertEqual(self.live(), second)

    def test_rollback_refuses_a_release_that_changed_on_disk(self):
        first, _ = self.release()
        self.update()
        self.release("second")
        self.update()
        with open(self.path("opt/usnm-feed-monitor-releases", first, "feed_collect.py"), "a") as f:
            f.write("# edited\n")
        self.assertIn("doesn't match its VERSION.json", self.update("--rollback", ok=False))

    def test_version_pins_an_older_release_and_holds(self):
        first, _ = self.release()
        self.update()
        self.release("second")
        self.update()
        out = self.update("--version", first)
        self.assertIn("-> %s" % first, out)
        self.assertEqual(self.live(), first)
        self.assertTrue(os.path.exists(self.path("etc/usnm-feed-monitor-update/hold")))
        self.assertIn("installed already", self.update("--version", first))

    def test_an_unattested_bundle_waits_then_is_rejected(self):
        tag, digest = self.release(attest=False)
        self.assertIn("waiting for the attestation", self.update())
        self.assertIsNone(self.live())
        pending = self.path("var/lib/usnm-feed-monitor-update/pending")
        first = int(open(pending).read().split()[1])
        # Past the eager window, a run soon after the last ask stays quiet.
        open(pending, "w").write("%s %d %d\n" % (digest, first - 700, int(time.time())))
        open(self.log, "w").close()
        self.update()
        self.assertEqual([c for c in self.calls("curl") if "attestations" in c], [])
        open(pending, "w").write("%s %d 0\n" % (digest, first - 4 * 3600))
        self.assertIn("is rejected", self.update(ok=False))
        self.assertEqual(self.rejected(), [digest])
        self.assertIsNone(self.live())

    def test_an_unattested_pinned_release_is_refused_at_once(self):
        tag, _ = self.release(latest=False, attest=False)
        self.assertIn("has no attestation", self.update("--version", tag, ok=False))
        self.assertIsNone(self.live())

    def test_a_bundle_whose_attestation_fails_is_rejected_and_not_retried(self):
        _, digest = self.release()
        open(os.path.join(self.stubs, "bad-signature"), "w").close()
        self.assertIn("doesn't verify", self.update(ok=False))
        self.assertEqual(self.rejected(), [digest])
        open(self.log, "w").close()
        self.update()
        self.assertEqual(self.calls("gh"), [])
        self.assertIsNone(self.live())

    def test_an_attestation_of_another_commit_is_rejected(self):
        _, digest = self.release(commit="f" * 40)
        self.assertIn("another commit", self.update(ok=False))
        self.assertEqual(self.rejected(), [digest])

    def test_a_checksum_mismatch_waits_without_rejecting(self):
        self.release()
        sums = os.path.join(self.stubs, "releases/feed-monitor-latest/SHA256SUMS")
        open(sums, "w").write("%s  usnm-feed-monitor.tar.gz\n" % ("0" * 64))
        self.assertIn("doesn't match its SHA256SUMS yet", self.update())
        self.assertEqual(self.rejected(), [])
        self.assertIsNone(self.live())

    def test_an_older_bundle_on_latest_is_rejected(self):
        old_tag, old_out = self.build()
        self.when += 3600
        self.commit("newer", self.when)
        self.release()
        self.update()
        digest = self.publish(old_out, old_tag)
        self.assertIn("is older than the installed", self.update(ok=False))
        self.assertEqual(self.rejected(), [digest])

    def _forged(self, members):
        """A gzip tar archive of (name, bytes, type) with an attestation of HEAD."""
        buf = io.BytesIO()
        with tarfile.open(fileobj=buf, mode="w:gz") as t:
            for name, data, kind in members:
                info = tarfile.TarInfo(name)
                info.type = kind
                if kind == tarfile.SYMTYPE:
                    info.linkname = "/etc/passwd"
                else:
                    info.size = len(data)
                t.addfile(info, io.BytesIO(data) if kind == tarfile.REGTYPE else None)
        data = buf.getvalue()
        digest = hashlib.sha256(data).hexdigest()
        d = os.path.join(self.stubs, "releases/feed-monitor-latest")
        os.makedirs(d, exist_ok=True)
        open(os.path.join(d, "usnm-feed-monitor.tar.gz"), "wb").write(data)
        open(os.path.join(d, "SHA256SUMS"), "w").write("%s  usnm-feed-monitor.tar.gz\n" % digest)
        open(os.path.join(self.stubs, "attested", digest), "w").write(git(self.repo, "rev-parse", "HEAD"))
        return digest

    def _real_members(self):
        _, out = self.build()
        with tarfile.open(os.path.join(out, "usnm-feed-monitor.tar.gz")) as t:
            return [(m.name, t.extractfile(m).read(), tarfile.REGTYPE) for m in t.getmembers()]

    def test_an_archive_with_other_files_is_rejected(self):
        for extra in (("../evil.py", b"x", tarfile.REGTYPE), ("extra.py", b"x", tarfile.REGTYPE)):
            digest = self._forged(self._real_members() + [extra])
            self.assertIn("doesn't hold exactly", self.update(ok=False))
            self.assertIn(digest, self.rejected())
        self.assertIsNone(self.live())
        self.assertFalse(os.path.exists(os.path.join(self.tmp, "evil.py")))

    def test_an_archive_with_a_symlink_is_rejected(self):
        members = [m if m[0] != "client_account.py" else (m[0], b"", tarfile.SYMTYPE) for m in self._real_members()]
        self._forged(members)
        self.assertIn("other than regular files", self.update(ok=False))
        self.assertIsNone(self.live())

    def test_an_archive_whose_files_differ_from_version_json_is_rejected(self):
        members = [m if m[0] != "feed_collect.py" else (m[0], m[1] + b"# changed\n", m[2]) for m in self._real_members()]
        self._forged(members)
        self.assertIn("don't match its VERSION.json", self.update(ok=False))
        self.assertIsNone(self.live())

    def test_a_bundle_that_doesn_t_start_is_rejected(self):
        with open(os.path.join(self.repo, "boinc/scripts/feed_collect.py"), "a") as f:
            f.write("def broken(:\n")
        self.when += 60
        git(self.repo, "commit", "-qam", "broken", when=self.when)
        _, digest = self.release()
        self.assertIn("feed_collect.py from", self.update(ok=False))
        self.assertEqual(self.rejected(), [digest])
        self.assertIsNone(self.live())

    def test_a_dry_run_verifies_and_changes_nothing(self):
        tag, _ = self.release()
        before = sorted(os.listdir(self.root))
        out = self.update("--dry-run")
        self.assertIn("dry run: would install %s" % tag, out)
        self.assertEqual(sorted(os.listdir(self.root)), before)
        self.assertEqual(len(self.calls("gh attestation verify")), 1)

    def test_the_switch_waits_for_feed_runs_to_end(self):
        tag, _ = self.release()
        open(os.path.join(self.stubs, "busy"), "w").write("usnm-boinc-feed-source@server.service loaded activating start\n" * 2)
        open(os.path.join(self.stubs, "busy-countdown"), "w").close()
        self.update()
        self.assertEqual(self.live(), tag)
        os.remove(os.path.join(self.stubs, "busy-countdown"))
        open(os.path.join(self.stubs, "busy"), "w").write("usnm-boinc-feed-health.service loaded activating start\n")
        second, _ = self.release("second")
        out = self.update("--idle-secs", "0")
        self.assertIn("switches next run", out)
        self.assertEqual(self.live(), tag)
        self.assertTrue(os.path.isdir(self.path("opt/usnm-feed-monitor-releases", second)))
        os.remove(os.path.join(self.stubs, "busy"))
        self.update()
        self.assertEqual(self.live(), second)

    def test_runs_never_overlap(self):
        self.release()
        os.makedirs(self.path("run/lock"))
        with open(self.path("run/lock/usnm-feed-monitor-update.lock"), "w") as lock:
            import fcntl
            fcntl.flock(lock, fcntl.LOCK_EX)
            self.assertIn("another run holds", self.update())
        self.assertIsNone(self.live())

    def test_options_are_checked(self):
        for args in (["--version", "main"], ["--keep", "1"], ["--rollback", "--version", "feed-monitor-20261108-0123abc"],
                     ["positional"]):
            self.assertNotEqual(subprocess.run(["bash", os.path.join(HERE, "update.sh"), *args], env=self.env(),
                                               capture_output=True).returncode, 0, args)

    # The installer.

    def _gh_tarball(self):
        buf = io.BytesIO()
        with tarfile.open(fileobj=buf, mode="w:gz") as t:
            data = STUB_GH.encode()
            info = tarfile.TarInfo("gh_2.103.3_linux_amd64/bin/gh")
            info.size, info.mode = len(data), 0o755
            t.addfile(info, io.BytesIO(data))
        open(os.path.join(self.stubs, "gh.tar.gz"), "wb").write(buf.getvalue())
        return hashlib.sha256(buf.getvalue()).hexdigest()

    def _install(self, *args, gh_sha=None):
        script = os.path.join(self.tmp, "install.sh")
        text = open(os.path.join(HERE, "install.sh")).read().replace('HERE=$(cd "$(dirname "$0")" && pwd)', "HERE=" + HERE)
        if gh_sha:
            text = text.replace("GH_SHA256=4c806978ffce1cec70853302a56d70ac6da4f19bae7adca4e1a8ab00633dd91d", "GH_SHA256=" + gh_sha)
        open(script, "w").write(text)
        return subprocess.run(["bash", script, *args], env=self.env(), capture_output=True, text=True)

    def test_install_dry_run_changes_nothing(self):
        os.remove(self.path("usr/local/libexec/usnm-feed-monitor/gh"))
        before = sorted(os.listdir(self.root))
        p = self._install("--dry-run")
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertIn("would install gh 2.103.3", p.stdout)
        self.assertIn("would install %s" % self.path("usr/local/sbin/usnm-feed-monitor-update"), p.stdout)
        self.assertEqual(sorted(os.listdir(self.root)), before)

    def test_install_puts_gh_the_updater_and_units_in_place(self):
        os.remove(self.path("usr/local/libexec/usnm-feed-monitor/gh"))
        p = self._install(gh_sha=self._gh_tarball())
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertTrue(os.access(self.path("usr/local/libexec/usnm-feed-monitor/gh"), os.X_OK))
        self.assertEqual(open(self.path("usr/local/sbin/usnm-feed-monitor-update")).read(),
                         open(os.path.join(HERE, "update.sh")).read())
        for unit in ("usnm-feed-monitor-update.service", "usnm-feed-monitor-update.timer"):
            self.assertTrue(os.path.isfile(self.path("etc/systemd/system", unit)))
        self.assertIn("systemctl start usnm-feed-monitor-update.service", self.calls("systemctl"))
        self.assertIn("systemctl enable --now usnm-feed-monitor-update.timer", self.calls("systemctl"))
        # Run again: nothing to change.
        p = self._install(gh_sha=self._gh_tarball())
        self.assertNotIn("installed", p.stdout.replace("not installed", ""))

    def test_install_refuses_a_gh_tarball_that_doesn_t_match(self):
        os.remove(self.path("usr/local/libexec/usnm-feed-monitor/gh"))
        self._gh_tarball()
        p = self._install()
        self.assertNotEqual(p.returncode, 0)
        self.assertIn("doesn't match its pinned sha256", p.stderr)
        self.assertFalse(os.path.exists(self.path("usr/local/libexec/usnm-feed-monitor/gh")))


if __name__ == "__main__":
    unittest.main()
