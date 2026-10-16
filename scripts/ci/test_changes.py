"""scripts/ci/changes.sh: the Quickwit, image and web end-to-end jobs run only for what they build or load.

Each test commits a change in a scratch clone of this checkout and runs
changes.sh on it as a push. The lock-file cases resolve packages with cargo
(the registry index, over the network), so they skip without it.
"""

import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CARGO = shutil.which("cargo")

class Dispatch(unittest.TestCase):
    def flags(self, scope=None, event="workflow_dispatch", ref="refs/heads/main"):
        env = dict(os.environ, GITHUB_EVENT_NAME=event, GITHUB_REF=ref,
                   GITHUB_OUTPUT="/dev/stdout")
        env.pop("CI_SCOPE", None)
        if scope is not None:
            env["CI_SCOPE"] = scope
        result = subprocess.run(
            ["bash", str(ROOT / "scripts/ci/changes.sh")], cwd=ROOT,
            env=env, capture_output=True, text=True,
        )
        return result, dict(re.findall(r"^(\w+)=(true|false)$", result.stdout, re.M))

    def test_worker_dispatch_runs_checks_without_application_or_boinc_rollout(self):
        result, flags = self.flags("ocr-worker")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual({key for key, value in flags.items() if value == "true"},
                         {"rust", "ocr_worker"})
        self.assertEqual(len(flags), 15)
        self.assertEqual(flags["image"], "false")
        self.assertEqual(flags["contrib"], "false")
        self.assertEqual(flags["provision"], "false")

    def test_default_and_explicit_full_dispatch_preserve_bootstrap(self):
        for scope in [None, "", "full"]:
            with self.subTest(scope=scope):
                result, flags = self.flags(scope)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(flags["image"], "true")
                self.assertEqual(flags["ocr_worker"], "true")
                self.assertEqual(flags["rust"], "true")
                self.assertEqual(flags["contrib"], "true")

    def test_unknown_dispatch_scope_fails_closed(self):
        result, flags = self.flags("ocr-wroker")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(flags)

    def test_worker_dispatch_requires_main(self):
        result, flags = self.flags("ocr-worker", ref="refs/heads/other")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(flags)

    def test_worker_scope_cannot_narrow_other_events(self):
        result, flags = self.flags("ocr-worker", event="schedule")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(flags["image"], "true")
        self.assertEqual(flags["contrib"], "true")

    def test_scope_is_wired_without_weakening_release_guards(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn("CI_SCOPE: ${{ inputs.scope }}", workflow)
        release = workflow.split("\n  ocr-worker-release:\n", 1)[1].split("\n  private-addresses:", 1)[0]
        self.assertIn("needs: [changes, rust, ocr-worker-el9, ocr-worker-check]", release)
        self.assertIn("github.ref == 'refs/heads/main'", release)
        self.assertIn("scripts/ci/head-of-main.sh", release)
        self.assertIn("actions/attest-build-provenance@", release)
        self.assertIn("if: steps.head.outputs.current == 'true'", release)
        rollout = workflow.split("name: publish and roll out", 1)[1]
        self.assertIn("needs.changes.outputs.image == 'true'", rollout)


def git(repo: Path, *args: str) -> str:
    return subprocess.run(["git", "-C", str(repo), *args], check=True,
                          capture_output=True, text=True).stdout.strip()


class Quickwit(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.repo = self.tmp / "repo"
        subprocess.run(["git", "clone", "-q", "--no-hardlinks", str(ROOT), str(self.repo)], check=True)
        # The working tree's changes.sh and Cargo manifests, committed or not.
        for rel in ["scripts/ci/changes.sh", "Cargo.toml", "Cargo.lock"]:
            shutil.copy(ROOT / rel, self.repo / rel)
        for manifest in (ROOT / "crates").glob("*/Cargo.toml"):
            rel_dir = manifest.parent.relative_to(ROOT)
            if (self.repo / rel_dir).exists():
                shutil.copy(manifest, self.repo / manifest.relative_to(ROOT))
            else:
                shutil.copytree(manifest.parent, self.repo / rel_dir)
        self.commit("this changes.sh")

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def commit(self, message: str) -> str:
        git(self.repo, "add", "-A")
        git(self.repo, "-c", "user.name=t", "-c", "user.email=t@example.com",
            "commit", "-q", "--allow-empty", "-m", message)
        return git(self.repo, "rev-parse", "HEAD")

    def outputs(self) -> dict:
        before = git(self.repo, "rev-parse", "HEAD")
        self.change()
        after = self.commit("change")
        out = self.tmp / "out"
        out.write_text("")
        env = dict(os.environ, GITHUB_EVENT_NAME="push", BEFORE_SHA=before,
                   GITHUB_SHA=after, GITHUB_OUTPUT=str(out))
        subprocess.run(["scripts/ci/changes.sh"], cwd=self.repo, env=env, check=True,
                       capture_output=True, text=True)
        return {k: v == "true" for k, v in re.findall(r"^(\w+)=(\w+)$", out.read_text(), re.M)}

    def quickwit(self) -> bool:
        return self.outputs()["quickwit"]

    def touch(self, path: str):
        f = self.repo / path
        with f.open("a") as h:
            h.write("\n// touched\n")

    def test_catalog_changes_validate_web_credits(self):
        self.change = lambda: self.touch("ocr/engines/engines.toml")
        flags = self.outputs()
        self.assertTrue(flags["web"])
        self.assertTrue(flags["e2e"])
        self.assertTrue(flags["image"])

    def test_a_crate_outside_the_closure_doesnt_run_it(self):
        self.change = lambda: self.touch("crates/usnm-boinc/src/lib.rs")
        self.assertFalse(self.quickwit())

    def test_the_ocr_survey_crate_doesnt_run_it(self):
        self.change = lambda: self.touch("crates/usnm-ocr-survey/src/lib.rs")
        self.assertFalse(self.quickwit())

    def test_a_crate_in_the_closure_runs_it(self):
        self.change = lambda: self.touch("crates/usnm-core/src/lib.rs")
        self.assertTrue(self.quickwit())

    def test_a_boinc_only_change_runs_no_image_or_e2e(self):
        self.change = lambda: self.touch("crates/usnm-boinc/src/lib.rs")
        o = self.outputs()
        self.assertFalse(o["image"])
        self.assertFalse(o["e2e"])

    def test_a_crate_in_the_api_closure_runs_image_and_e2e(self):
        self.change = lambda: self.touch("crates/usnm-api/src/main.rs")
        o = self.outputs()
        self.assertTrue(o["image"])
        self.assertTrue(o["e2e"])

    def test_an_ingest_only_crate_runs_image_not_e2e(self):
        self.change = lambda: self.touch("crates/usnm-ingest/src/lib.rs")
        o = self.outputs()
        self.assertTrue(o["image"])
        self.assertFalse(o["e2e"])

    def test_a_fixture_runs_it(self):
        self.change = lambda: (self.repo / "fixtures/new.txt").write_text("x\n")
        self.assertTrue(self.quickwit())

    @unittest.skipUnless(CARGO, "no cargo")
    def test_a_new_crate_outside_the_closure_doesnt_run_it(self):
        def change():
            crate = self.repo / "crates/zz-new"
            (crate / "src").mkdir(parents=True)
            (crate / "src/lib.rs").write_text("")
            (crate / "Cargo.toml").write_text(
                '[package]\nname = "zz-new"\nversion = "0.1.0"\nedition = "2021"\n\n'
                '[dependencies]\nserde_json.workspace = true\n')
            root = self.repo / "Cargo.toml"
            root.write_text(root.read_text().replace("members = [\n", 'members = [\n    "crates/zz-new",\n', 1))
            subprocess.run([CARGO, "metadata", "--format-version", "1", "-q"], cwd=self.repo,
                           check=True, capture_output=True)
            self.assertIn('name = "zz-new"', (self.repo / "Cargo.lock").read_text())
        self.change = change
        self.assertFalse(self.quickwit())

    def test_a_profile_setting_runs_it(self):
        def change():
            with (self.repo / "Cargo.toml").open("a") as h:
                h.write("\n[profile.test]\noverflow-checks = false\n")
        self.change = change
        self.assertTrue(self.quickwit())

    @unittest.skipUnless(CARGO, "no cargo")
    def test_a_dependency_bump_under_usnm_search_runs_it(self):
        def change():
            lock = (self.repo / "Cargo.lock").read_text()
            version = re.search(r'name = "thiserror"\nversion = "(2\.0\.(\d+))"', lock)
            older = f"2.0.{int(version.group(2)) - 1}"
            subprocess.run([CARGO, "update", "-q", "-p", f"thiserror@{version.group(1)}",
                            "--precise", older], cwd=self.repo, check=True, capture_output=True)
        self.change = change
        self.assertTrue(self.quickwit())


if __name__ == "__main__":
    unittest.main()


class BoincPages(unittest.TestCase):
    """A change to the BOINC project's web pages alone runs only their PHP check."""

    setUp, tearDown, commit, touch = Quickwit.setUp, Quickwit.tearDown, Quickwit.commit, Quickwit.touch

    def outputs(self) -> dict:
        before = git(self.repo, "rev-parse", "HEAD")
        self.change()
        after = self.commit("change")
        out = self.tmp / "out"
        out.write_text("")
        env = dict(os.environ, GITHUB_EVENT_NAME="push", BEFORE_SHA=before,
                   GITHUB_SHA=after, GITHUB_OUTPUT=str(out))
        subprocess.run(["scripts/ci/changes.sh"], cwd=self.repo, env=env, check=True,
                       capture_output=True, text=True)
        return dict(re.findall(r"^(\w+)=(\w+)$", out.read_text(), re.M))

    def test_a_page_alone_runs_only_the_page_check(self):
        self.change = lambda: self.touch("contrib-compute/boinc/html/user/about.php")
        on = {k for k, v in self.outputs().items() if v == "true"}
        self.assertEqual(on, {"boinc_pages"})

    def test_a_page_with_the_entrypoint_runs_the_contrib_jobs(self):
        def change():
            self.touch("contrib-compute/boinc/html/user/about.php")
            self.touch("contrib-compute/images/server/entrypoint.sh")
        self.change = change
        o = self.outputs()
        self.assertEqual((o["boinc_pages"], o["contrib"], o["code"]), ("true", "true", "true"))

    def test_a_reviewed_selection_builds_the_bridge(self):
        def change():
            path = self.repo / "fixtures/ocr-gold/selections/test.json"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("{}\n")
        self.change = change
        o = self.outputs()
        self.assertEqual((o["contrib"], o["ocr_gold"]), ("true", "true"))
        self.assertEqual((o["rust"], o["image"], o["fixtures"]), ("false", "false", "false"))

    def test_the_transcription_preparer_builds_the_bridge(self):
        self.change = lambda: self.touch("scripts/ocr-survey/transcription.py")
        o = self.outputs()
        self.assertEqual((o["contrib"], o["ocr_gold"]), ("true", "true"))

    def test_embedded_gold_lock_runs_rust_and_bridge_checks(self):
        self.change = lambda: self.touch("fixtures/ocr-gold/bundle-v1.lock.json")
        o = self.outputs()
        self.assertEqual((o["rust"], o["contrib"], o["ocr_gold"]), ("true", "true", "true"))

class OcrCatalog(unittest.TestCase):
    """Embedded catalog and routing edits must rebuild and test OCR releases."""

    setUp, tearDown, commit, touch, outputs = (Quickwit.setUp, Quickwit.tearDown,
                                               Quickwit.commit, Quickwit.touch, Quickwit.outputs)

    def test_catalog_and_routes_run_rust_and_worker_release(self):
        for path in ["ocr/engines/engines.toml", "ocr/routing/routes.toml",
                     "crates/usnm-ocr-route/src/lib.rs"]:
            with self.subTest(path=path):
                self.change = lambda path=path: self.touch(path)
                o = self.outputs()
                self.assertTrue(o["rust"])
                self.assertTrue(o["ocr_worker"])

    def test_boinc_image_filter_includes_embedded_catalog(self):
        workflow = (ROOT / ".github/workflows/boinc-worker.yml").read_text()
        self.assertIn("      - ocr/engines/engines.toml\n", workflow)
