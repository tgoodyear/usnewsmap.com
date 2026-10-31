#!/usr/bin/env python3
"""scripts/ci/mirror-image.sh against stand-in az and skopeo, and ci.yml's use of it."""
import hashlib
import os
import re
import stat
import subprocess
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
SCRIPT = os.path.join(HERE, "mirror-image.sh")
SOURCE = "crexampletest.azurecr.io/usnewsmap-api"
DEST = "ghcr.io/example/usnewsmap-api"
COMMIT = "c" * 40
MANIFEST = '{"schemaVersion":2}'
DIGEST = "sha256:" + hashlib.sha256(MANIFEST.encode()).hexdigest()

STUB = r'''#!/usr/bin/env bash
echo "$(basename "$0") $*" >> "$STUB_LOG"
case "$(basename "$0") $1" in
  "az acr") echo acr-token ;;
  "skopeo login")
    case "$*" in *"${STUB_LOGIN_DOWN:-none}"*) exit 1 ;; esac
    cat > /dev/null ;;
  "skopeo copy")
    for a in "$@"; do dest=$a; done
    case "$dest" in *":${STUB_COPY_DOWN:-none}") exit 1 ;; esac
    printf '%s' "${STUB_MANIFEST:?}" > "$STUB_DIR/${dest##*:}" ;;
  "skopeo inspect")
    for a in "$@"; do ref=$a; done
    cat "$STUB_DIR/${ref##*:}" ;;
esac
'''


class Mirror(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.dir = tmp.name
        self.log = os.path.join(self.dir, "log")
        for tool in ("az", "skopeo"):
            path = os.path.join(self.dir, tool)
            with open(path, "w") as f:
                f.write(STUB)
            os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR)

    def run_script(self, *extra, **env):
        args = {"--source": SOURCE, "--digest": DIGEST, "--dest": DEST, "--commit": COMMIT, "--attempts": "1"}
        argv = [x for pair in args.items() for x in pair] + list(extra)
        full = dict(os.environ, PATH=self.dir + os.pathsep + os.environ["PATH"], STUB_LOG=self.log,
                    STUB_DIR=self.dir, STUB_MANIFEST=MANIFEST, GH_TOKEN="gh-token", RUNNER_TEMP=self.dir,
                    GITHUB_ACTOR="ci")
        full.update(env)
        return subprocess.run([SCRIPT] + argv, env=full, capture_output=True, text=True)

    def calls(self):
        with open(self.log) as f:
            return f.read().splitlines()

    def copies(self):
        return [c for c in self.calls() if c.startswith("skopeo copy")]

    def test_copies_by_digest_keeping_it_and_moves_the_tag_last(self):
        r = self.run_script()
        self.assertEqual(r.returncode, 0, r.stderr)
        copies = self.copies()
        self.assertEqual(len(copies), 2)
        for c, tag in zip(copies, (COMMIT, "main")):
            self.assertIn("--all --preserve-digests", c)
            self.assertIn("docker://%s@%s docker://%s:%s" % (SOURCE, DIGEST, DEST, tag), c)

    def test_a_different_digest_at_the_destination_fails(self):
        r = self.run_script(STUB_MANIFEST='{"schemaVersion":3}')
        self.assertEqual(r.returncode, 1)
        self.assertIn("::warning::", r.stdout)
        self.assertEqual(len(self.copies()), 1)

    def test_main_never_moves_when_the_commit_tag_fails(self):
        r = self.run_script(STUB_COPY_DOWN=COMMIT)
        self.assertEqual(r.returncode, 1)
        self.assertEqual(len(self.copies()), 1)

    def test_a_failed_sign_in_copies_nothing(self):
        r = self.run_script(STUB_LOGIN_DOWN="ghcr.io")
        self.assertEqual(r.returncode, 1)
        self.assertEqual(self.copies(), [])

    def test_flags_are_checked(self):
        for extra in (("--digest", "main"), ("--source", "docker.io/x"), ("--dest", "evil.example/x"),
                      ("--commit", "main"), ("--nope",)):
            self.assertEqual(self.run_script(*extra).returncode, 2, extra)

    def test_no_token_reaches_a_command_line(self):
        self.run_script()
        self.assertNotIn("gh-token", "\n".join(self.calls()))
        self.assertNotIn("acr-token", "\n".join(self.calls()))


class Workflow(unittest.TestCase):
    def setUp(self):
        with open(os.path.join(ROOT, ".github", "workflows", "ci.yml")) as f:
            text = f.read()
        self.publish = re.search(r"^  publish:\n(.*?)^  indexnow:", text, re.S | re.M)[1]

    def steps(self):
        return re.split(r"^      - ", self.publish, flags=re.M)

    def test_every_ghcr_step_leaves_the_azure_rollout_alone(self):
        steps = [s for s in self.steps() if "env.GHCR" in s or "mirror-image.sh" in s]
        self.assertEqual(len(steps), 6)
        for s in steps:
            self.assertIn("continue-on-error: true", s, s)
            self.assertIn("matrix.environment == 'prod'", s, s)

    def test_the_acr_builds_push_only_to_acr(self):
        for s in self.steps():
            if "docker/build-push-action" in s:
                self.assertNotIn("ghcr.io", s)

    def test_the_api_is_mirrored_before_prod_rolls_and_attested_for_both(self):
        names = [s.splitlines()[0] for s in self.steps()]
        roll = names.index("name: Roll the API onto this commit's image")
        self.assertLess(names.index("name: Mirror the API image to GHCR"), roll)
        self.assertGreater(names.index("name: Attest the API image"), roll)
        self.assertLess(names.index("name: Attest the API image"), names.index("name: Attest the API image for GHCR"))


if __name__ == "__main__":
    unittest.main()
