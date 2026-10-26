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
    def test_boinc_build_paths_exclude_markdown_after_positive_patterns(self):
        for workflow in (ROOT / ".github/workflows").glob("boinc-*.yml"):
            paths = re.search(r"(?m)^    paths:\n((?:      .*\n)+)", workflow.read_text())
            self.assertIsNotNone(paths, workflow.name)
            patterns = [
                line.strip()[2:].strip("'\"")
                for line in paths[1].splitlines() if line.strip().startswith("- ")
            ]
            if any("**" in pattern for pattern in patterns):
                with self.subTest(workflow=workflow.name):
                    self.assertEqual(patterns[-1], "!**/*.md")
                    self.assertIn(f".github/workflows/{workflow.name}", patterns)
                    self.assertTrue(any(not p.startswith("!") and "**" in p
                                        for p in patterns[:-1]))

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
        self.assertEqual(len(flags), 24)
        self.assertEqual(flags["image"], "false")
        self.assertEqual(flags["contrib"], "false")
        self.assertEqual(flags["edge"], "false")
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
                self.assertEqual(flags["edge"], "true")

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
        # An older commit publishes when main's newer commits change neither build (#1074), which
        # needs the history from it to main's head.
        self.assertIn("scripts/ci/head-of-main.sh --unless-changed ocr_worker,ocr_dash", release)
        self.assertIn("fetch-depth: 0", release)
        # The flags it waits on are the ones that start the job.
        started_by = set(re.findall(r"needs\.changes\.outputs\.([a-z0-9_]+)", release.split("steps:", 1)[0]))
        self.assertEqual(started_by, {"ocr_worker", "ocr_dash"})
        self.assertEqual(set(re.search(r"--unless-changed (\S+)", release).group(1).split(",")), started_by)
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
        self.assertEqual({key for key, value in o.items() if value == "true"},
                         {"rust", "contrib", "ocr_gold", "code", "infra"} | BOINC_IMAGES)

    def test_v2_gold_lock_runs_bridge_and_gold_checks(self):
        self.change = lambda: self.touch("fixtures/ocr-gold/bundle-v2.lock.json")
        o = self.outputs()
        self.assertEqual((o["contrib"], o["ocr_gold"]), ("true", "true"))
        self.assertEqual((o["rust"], o["image"], o["fixtures"]), ("false", "false", "false"))

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


BOINC_IMAGES = {"boinc_worker", "boinc_qwen", "boinc_surya", "boinc_ndlocr"}
DASHBOARD = {"ocr_dash", "ocr_dashboard"}
NATIVE = {"boinc_native"}
# Workflow file -> the changes.sh output that gates its pull request job.
GATED_WORKFLOWS = {
    "boinc-worker.yml": "boinc_worker",
    "boinc-qwen.yml": "boinc_qwen",
    "boinc-surya.yml": "boinc_surya",
    "boinc-ndlocr-lite.yml": "boinc_ndlocr",
    "boinc-quality-native.yml": "boinc_native",
    "ocr-dashboard.yml": "ocr_dashboard",
}
# What the workflows leave to the crate closure and the lock-file check.
CLOSURE_GOVERNED = {"crates/**", "Cargo.toml", "Cargo.lock"}


def workflow_paths(workflow: Path) -> list:
    block = re.search(r"(?m)^    paths:\n((?:      .*\n)+)", workflow.read_text())
    return [line.strip()[2:].strip("'\"") for line in block[1].splitlines()
            if line.strip().startswith("- ")]


def job_text(workflow: str, job: str) -> str:
    """One job's lines of a workflow file (a two-space-indented key to the next)."""
    text = workflow.split(f"\n  {job}:\n", 1)[1]
    return re.split(r"\n  [A-Za-z0-9_-]+:\n", text, maxsplit=1)[0]


def glob_regex(glob: str) -> re.Pattern:
    """A workflow `paths:` glob as GitHub reads it: ** crosses directories, * does not."""
    out, i = "", 0
    while i < len(glob):
        if glob.startswith("**", i):
            out, i = out + ".*", i + 2
        elif glob[i] == "*":
            out, i = out + "[^/]*", i + 1
        elif glob[i] == "?":
            out, i = out + "[^/]", i + 1
        else:
            out, i = out + re.escape(glob[i]), i + 1
    return re.compile(f"^{out}$")


def embedded_files(dev: str, *roots: str) -> set:
    """The repository files outside crates/ that the crates built for ROOTS embed, as changes.sh
    finds them (its own functions, run here)."""
    script = (ROOT / "scripts/ci/changes.sh").read_text()
    functions = "\n".join(re.search(rf"(?ms)^{name}\(\) \{{.*?^\}}$", script)[0]
                          for name in ("closure_build_crates", "closure_embeds"))
    program = functions + f'\nclosure_embeds $(closure_build_crates {dev} "$@")'
    result = subprocess.run(["bash", "-c", program, "bash", *roots], cwd=ROOT, capture_output=True,
                            text=True, check=True)
    files = {re.sub(r"\\(.)", r"\1", line)[1:-1] for line in result.stdout.splitlines()}
    return {f for f in files if not f.startswith("crates/")}


# Each gated workflow's roots, and whether its image builds the roots' examples (changes.sh).
EMBED_ROOTS = {
    "boinc-worker.yml": ("true", "usnm-boinc"),
    "boinc-qwen.yml": ("false", "usnm-boinc"),
    "boinc-surya.yml": ("false", "usnm-boinc"),
    "boinc-ndlocr-lite.yml": ("true", "usnm-boinc", "usnm-ocr-adapter"),
    "boinc-quality-native.yml": ("true", "usnm-quality-app", "usnm-qwen-app", "usnm-ocr-quality",
                                 "usnm-ocr-contract"),
    "ocr-dashboard.yml": ("false", "usnm-ocr-dash"),
}


class BuildInputs(unittest.TestCase):
    """The BOINC image workflows, the dashboard and the OCR worker's release run for what they
    build (#739): a crate in the closure of the image or binary, a file those crates embed, the
    image's own recipe, the toolchain, or a change to what Cargo resolves for them."""

    setUp, tearDown, commit = Quickwit.setUp, Quickwit.tearDown, Quickwit.commit

    def select(self, *changes, env=None) -> set:
        """The outputs that are true after committing each change (a path to append to, or a
        (path, text) pair to write), as changes.sh sees a push."""
        before = git(self.repo, "rev-parse", "HEAD")
        for change in changes:
            path, text = change if isinstance(change, tuple) else (change, None)
            file = self.repo / path
            file.parent.mkdir(parents=True, exist_ok=True)
            if text is not None:
                file.write_text(text)
            else:
                with file.open("a") as h:
                    h.write("\n")
        after = self.commit("change")
        out = self.tmp / "out"
        out.write_text("")
        run_env = dict(os.environ, GITHUB_EVENT_NAME="push", BEFORE_SHA=before,
                       GITHUB_SHA=after, GITHUB_OUTPUT=str(out), **(env or {}))
        result = subprocess.run(["scripts/ci/changes.sh"], cwd=self.repo, env=run_env,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.log = result.stdout + result.stderr
        return {k for k, v in re.findall(r"^(\w+)=(\w+)$", out.read_text(), re.M) if v == "true"}

    def stub_tool(self, name: str) -> dict:
        """An environment whose `name` fails, as when the tool is broken or missing."""
        bin_dir = self.tmp / f"stub-{name}-bin"
        bin_dir.mkdir(exist_ok=True)
        stub = bin_dir / name
        stub.write_text("#!/bin/sh\necho 'stub: broken' >&2\nexit 1\n")
        stub.chmod(0o755)
        return {"PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}"}

    def test_dashboard_renderer_and_markdown_change_runs_only_the_dashboard_checks(self):
        # The change #734 makes: html.rs and docs/operations/onprem-ocr.md.
        on = self.select("crates/usnm-ocr-dash/src/html.rs", "docs/operations/onprem-ocr.md")
        self.assertEqual(on, {"rust", "ocr_dash", "ocr_dashboard", "markdown", "code"})

    def test_docs_only_change_runs_only_the_markdown_check(self):
        self.assertEqual(self.select("docs/operations/onprem-ocr.md", "README.md"), {"markdown"})

    def test_boinc_only_change_runs_the_boinc_images_but_no_dashboard_or_worker_release(self):
        on = self.select("crates/usnm-boinc/src/lib.rs")
        self.assertEqual(on, {"rust", "contrib", "code", "infra"} | BOINC_IMAGES)

    def test_shared_dependency_runs_every_affected_job(self):
        on = self.select("crates/usnm-core/src/lib.rs")
        self.assertGreaterEqual(on, {"rust", "quickwit", "image", "e2e", "ocr_worker", "infra",
                                     "code"} | BOINC_IMAGES | DASHBOARD | NATIVE)

    def test_worker_crate_runs_the_release_the_dashboard_and_the_images(self):
        on = self.select("crates/usnm-ocr-worker/src/lib.rs")
        self.assertGreaterEqual(on, {"rust", "ocr_worker"} | BOINC_IMAGES | DASHBOARD)

    def test_a_dev_dependency_of_a_dependency_is_not_a_build_input(self):
        # The worker's tests use usnm-ingest, the BOINC client links it, the dashboard doesn't.
        on = self.select("crates/usnm-ingest/src/lib.rs")
        self.assertGreaterEqual(on, BOINC_IMAGES)
        self.assertFalse(on & (DASHBOARD | {"ocr_worker"}), on)

    def test_the_crates_split_out_of_ingest_run_what_ingest_runs(self):
        # The Congress fetches and the local OCR pipeline are crates of their own that usnm-ingest
        # links: the ingest image, the Quickwit job and the BOINC client rebuild for them; the
        # web end-to-end job, the worker and the dashboard don't.
        for path in ["crates/usnm-congress/src/lib.rs", "crates/usnm-ocr-local/src/lib.rs"]:
            with self.subTest(path=path):
                on = self.select(path)
                self.assertGreaterEqual(on, {"rust", "quickwit", "image"} | BOINC_IMAGES)
                self.assertFalse(on & (DASHBOARD | NATIVE | {"ocr_worker", "e2e"}), on)

    def test_a_file_a_crate_embeds_counts_for_what_links_the_crate(self):
        # usnm-ingest embeds the catalog; the BOINC client links usnm-ingest, the worker doesn't.
        on = self.select("catalog/overrides/places.json")
        self.assertGreaterEqual(on, BOINC_IMAGES)
        self.assertFalse(on & (DASHBOARD | {"ocr_worker"}), on)
        # usnm-ocr-worker embeds the Tesseract models lock; everything that links it rebuilds.
        on = self.select("ocr/engines/tesseract/models.lock")
        self.assertGreaterEqual(on, {"ocr_worker"} | BOINC_IMAGES | DASHBOARD)
        # usnm-boinc embeds the gold set's v1 lock, which the build rules otherwise leave out.
        on = self.select("fixtures/ocr-gold/bundle-v1.lock.json")
        self.assertGreaterEqual(on, BOINC_IMAGES)
        # usnm-boinc embeds the Qwen benchmark input, which no workflow's paths listed before #739.
        on = self.select("ocr/boinc/usnm_qwen_benchmark_in")
        self.assertGreaterEqual(on, BOINC_IMAGES)
        self.assertFalse(on & (DASHBOARD | {"ocr_worker"}), on)
        # usnm-boinc embeds the Surya image's pins (the default of `surya stage --engine-surya`),
        # so it rebuilds every image that links it; the rest of ocr/boinc/surya/ is the Surya image's alone.
        on = self.select("ocr/boinc/surya/engine.lock")
        self.assertGreaterEqual(on, BOINC_IMAGES)
        self.assertFalse(on & (DASHBOARD | {"ocr_worker"}), on)

    def test_an_images_own_recipe_runs_that_image_only(self):
        for path, image in [
            ("ocr/boinc/Containerfile", "boinc_worker"),
            ("scripts/boinc/smoke-worker.py", "boinc_worker"),
            ("ocr/boinc/qwen/Containerfile", "boinc_qwen"),
            ("scripts/boinc/qwen_disk.py", "boinc_qwen"),
            ("ocr/boinc/surya/run.py", "boinc_surya"),
            ("ocr/engines/ndlocr-lite/Containerfile", "boinc_ndlocr"),
            ("ocr/ja-ocr/jaocr.py", "boinc_ndlocr"),
        ]:
            with self.subTest(path=path):
                on = self.select(path)
                self.assertEqual(on & (BOINC_IMAGES | DASHBOARD | {"ocr_worker"}), {image}, on)

    def test_scripts_grouped_by_purpose_still_select_their_jobs(self):
        # The loose scripts moved under scripts/azure, boinc, data, dev and ops; the paths in
        # changes.sh follow them.
        for path, jobs in [
            ("scripts/ops/check-version-snapshots.py", {"ops"}),
            ("scripts/ops/test_check_index_history.py", {"ops"}),
            ("scripts/azure/test_date_spans.py", {"ops"}),
            ("scripts/dev/quickwit-fixtures.sh", {"quickwit", "rust", "e2e"}),
            ("scripts/data/build-page-language-words.py", {"image", "page_language"}),
            ("scripts/data/page-language-samples.py", {"page_language"}),
            ("scripts/boinc/boinc-app-release.sh", {"contrib"}),
        ]:
            with self.subTest(path=path):
                self.assertGreaterEqual(self.select(path), jobs)

    def test_the_tesseract_image_runs_the_worker_image_and_release_only(self):
        on = self.select("ocr/engines/tesseract/Containerfile")
        self.assertEqual(on & (BOINC_IMAGES | DASHBOARD | {"ocr_worker"}),
                         {"boinc_worker", "ocr_worker"}, on)

    def test_a_toolchain_change_runs_every_build(self):
        on = self.select("rust-toolchain.toml")
        self.assertGreaterEqual(on, {"ocr_worker"} | BOINC_IMAGES | DASHBOARD | NATIVE)

    def test_the_docker_ignore_file_runs_every_image_but_not_the_dashboard(self):
        on = self.select(".dockerignore")
        self.assertGreaterEqual(on, BOINC_IMAGES)
        self.assertFalse(on & (DASHBOARD | {"ocr_worker"}), on)

    def test_a_dockerfile_under_docker_runs_the_image_job(self):
        for path in ["docker/Dockerfile.api", "docker/Dockerfile.ingest", "docker/Dockerfile.ja-ocr"]:
            with self.subTest(path=path):
                self.assertIn("image", self.select(path))

    def test_a_workflow_or_its_action_runs_only_its_own_job(self):
        for path, job in [
            (".github/workflows/boinc-worker.yml", "boinc_worker"),
            (".github/actions/boinc-worker-build/action.yml", "boinc_worker"),
            (".github/workflows/boinc-qwen.yml", "boinc_qwen"),
            (".github/actions/boinc-qwen-build/action.yml", "boinc_qwen"),
            (".github/workflows/boinc-surya.yml", "boinc_surya"),
            (".github/actions/boinc-surya-build/action.yml", "boinc_surya"),
            (".github/workflows/boinc-ndlocr-lite.yml", "boinc_ndlocr"),
            (".github/workflows/boinc-quality-native.yml", "boinc_native"),
            (".github/workflows/ocr-dashboard.yml", "ocr_dashboard"),
        ]:
            with self.subTest(path=path):
                on = self.select(path)
                self.assertEqual(on & (BOINC_IMAGES | DASHBOARD | NATIVE | {"ocr_worker"}), {job}, on)

    def test_a_shared_ci_action_runs_the_ci_jobs_that_call_it(self):
        # An action ci.yml calls holds steps of its jobs, so an edit to it must run them, as an
        # edit to the same steps in ci.yml would (the flags each job's `if:` reads).
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        called = {}
        for job in re.findall(r"(?m)^  ([a-z0-9_-]+):$", ci.split("\njobs:\n", 1)[1]):
            text = job_text(ci, job)
            flags = set(re.findall(r"needs\.changes\.outputs\.(\w+)", text))
            for action in re.findall(r"uses: \./\.github/actions/([\w-]+)", text):
                called.setdefault(action, set()).update(flags)
        self.assertGreaterEqual(len(called), 3)
        for action, flags in sorted(called.items()):
            # The markdown flag comes from the changed files alone, and the web job, which
            # runs the same check, is forced with it.
            expected = flags - {"markdown"}
            with self.subTest(action=action):
                self.assertTrue((ROOT / ".github/actions" / action / "action.yml").is_file())
                self.assertTrue(expected)
                self.assertGreaterEqual(self.select(f".github/actions/{action}/action.yml"), expected)

    def test_the_native_apps_run_for_the_crates_they_build_and_no_others(self):
        # #744: usnm-core and usnm-overlay are built by both apps and were on no path list.
        for path in ["crates/usnm-core/src/lib.rs", "crates/usnm-overlay/src/lib.rs",
                     "crates/usnm-alto/src/lib.rs", "crates/usnm-ocr-contract/src/lib.rs",
                     "crates/usnm-ocr-quality/src/lib.rs", "crates/usnm-quality-app/src/main.rs",
                     "crates/usnm-qwen-app/src/main.rs"]:
            with self.subTest(path=path):
                self.assertIn("boinc_native", self.select(path))
        for path in ["crates/usnm-ocr-dash/src/html.rs", "crates/usnm-boinc/src/lib.rs",
                     "crates/usnm-ocr-worker/src/lib.rs", "crates/usnm-ingest/src/lib.rs"]:
            with self.subTest(path=path):
                self.assertNotIn("boinc_native", self.select(path))

    def test_the_native_apps_run_for_the_files_they_embed_or_their_tests_read(self):
        for path in ["ocr/boinc/qwen/llama.lock", "ocr/boinc/qwen/weights.lock", "ocr/contract/v1/schema.json",
                     "ocr/boinc/qwen/amd-device-map.cpp", "scripts/boinc/test_amd_device_map.py"]:
            with self.subTest(path=path):
                self.assertIn("boinc_native", self.select(path))
        # What only the images embed is not theirs.
        self.assertNotIn("boinc_native", self.select("catalog/overrides/places.json"))

    def test_dashboard_changes_never_skip_the_policy_scans(self):
        # code runs the infra job, whose shared-key and named-argument scans read the crate.
        self.assertIn("code", self.select("crates/usnm-ocr-dash/src/html.rs"))
        self.assertNotIn("infra", self.select("crates/usnm-ocr-dash/tests/render.rs"))

    def test_the_dashboard_manifest_still_runs_the_infra_stack(self):
        # Only src/ and tests/ are exempt: the manifest is not.
        self.assertIn("infra", self.select("crates/usnm-ocr-dash/Cargo.toml"))

    def test_other_changes_run_the_infra_stack(self):
        for path in ["infra/main.bicep", "scripts/ci/changes.sh", "crates/usnm-api/src/status/sanitize.rs",
                     "crates/usnm-ocr-contract/src/runs.rs", "Cargo.lock"]:
            with self.subTest(path=path):
                self.assertIn("infra", self.select(path))

    def test_the_fallback_worker_runs_its_own_job_and_the_policy_scans_only(self):
        # #1098: JavaScript, a Wrangler config and a shell script under edge/, which no crate,
        # image or web build reads. Its README is documentation.
        for path in ["edge/fallback-worker/worker.mjs", "edge/fallback-worker/worker.test.mjs",
                     "edge/fallback-worker/wrangler.jsonc", "edge/fallback-worker/deploy.sh"]:
            with self.subTest(path=path):
                self.assertEqual(self.select(path), {"edge", "code", "infra"})
        self.assertEqual(self.select("edge/fallback-worker/README.md"), {"markdown"})

    def test_other_changes_do_not_run_the_fallback_worker_job(self):
        for path in ["crates/usnm-api/src/main.rs", "web/src/main.tsx", "scripts/onprem/serving/install.sh",
                     "scripts/ci/changes.sh", "docs/design/09-operations-security-cost.md"]:
            with self.subTest(path=path):
                self.assertNotIn("edge", self.select(path))

    def test_an_edit_inside_the_edge_job_runs_that_job_only(self):
        path = ".github/workflows/ci.yml"
        ci = (ROOT / path).read_text()
        self.select((path, ci))  # this checkout's ci.yml as the base, committed or not
        marker = "      - run: node --test edge/fallback-worker/*.test.mjs\n"
        self.assertIn(marker, ci)
        self.assertEqual(self.select((path, ci.replace(marker, marker + "      # touched\n"))), {"edge"})

    def test_a_broken_cargo_runs_everything_that_builds_a_crate(self):
        on = self.select("crates/usnm-ocr-dash/src/html.rs", env=self.stub_tool("cargo"))
        self.assertGreaterEqual(on, {"ocr_worker"} | BOINC_IMAGES | DASHBOARD)
        self.assertIn("running boinc_qwen", self.log)

    def test_a_broken_tool_still_runs_the_images_for_a_gold_set_only_change(self):
        # The gold set is left out of the built files, so only the images that embed its lock can
        # run for this change, and only if the failure is handled as such.
        for tool in ("cargo", "perl"):
            with self.subTest(tool=tool):
                on = self.select("fixtures/ocr-gold/bundle-v1.lock.json", env=self.stub_tool(tool))
                self.assertGreaterEqual(on, BOINC_IMAGES | NATIVE | DASHBOARD | {"ocr_worker"})
                if tool == "perl":
                    self.assertIn("can't list what usnm-boinc embed", self.log)
                    self.assertNotIn("cargo metadata failed", self.log)

    def test_a_broken_embed_scan_runs_everything_that_links_a_crate(self):
        on = self.select("crates/usnm-ocr-dash/src/html.rs", env=self.stub_tool("perl"))
        self.assertGreaterEqual(on, {"ocr_worker"} | BOINC_IMAGES | DASHBOARD)

    def test_a_full_workflow_dispatch_runs_every_image(self):
        env = dict(os.environ, GITHUB_EVENT_NAME="workflow_dispatch", GITHUB_REF="refs/heads/main",
                   GITHUB_OUTPUT="/dev/stdout")
        env.pop("CI_SCOPE", None)
        result = subprocess.run(["bash", str(ROOT / "scripts/ci/changes.sh")], cwd=ROOT, env=env,
                                capture_output=True, text=True)
        on = {k for k, v in re.findall(r"^(\w+)=(\w+)$", result.stdout, re.M) if v == "true"}
        self.assertGreaterEqual(on, {"ocr_worker", "infra"} | BOINC_IMAGES | DASHBOARD)

    @unittest.skipUnless(CARGO, "no cargo")
    def test_a_workspace_setting_runs_every_build(self):
        def append():
            return ("Cargo.toml", (self.repo / "Cargo.toml").read_text()
                    + "\n[profile.test]\noverflow-checks = false\n")
        on = self.select(append())
        self.assertGreaterEqual(on, {"ocr_worker"} | BOINC_IMAGES | DASHBOARD)

    @unittest.skipUnless(CARGO, "no cargo")
    def test_a_new_crate_outside_every_closure_runs_none_of_them(self):
        crate = self.repo / "crates/zz-new"
        (crate / "src").mkdir(parents=True)
        root = self.repo / "Cargo.toml"
        manifest = ('[package]\nname = "zz-new"\nversion = "0.1.0"\nedition = "2021"\n\n'
                    '[dependencies]\nserde_json.workspace = true\n')
        members = root.read_text().replace("members = [\n", 'members = [\n    "crates/zz-new",\n', 1)
        (crate / "Cargo.toml").write_text(manifest)
        (crate / "src/lib.rs").write_text("")
        root.write_text(members)
        subprocess.run([CARGO, "metadata", "--format-version", "1", "-q"], cwd=self.repo,
                       check=True, capture_output=True)
        # Cargo.lock is written now; the change is committed by select().
        on = self.select("crates/zz-new/src/lib.rs")
        self.assertFalse(on & (BOINC_IMAGES | DASHBOARD | {"ocr_worker"}), on)

    @unittest.skipUnless(CARGO, "no cargo")
    def test_a_dependency_only_the_dashboard_adds_doesnt_run_the_boinc_images(self):
        manifest = self.repo / "crates/usnm-ocr-dash/Cargo.toml"
        manifest.write_text(manifest.read_text().replace(
            "[dev-dependencies]", "sha2.workspace = true\n\n[dev-dependencies]", 1))
        subprocess.run([CARGO, "metadata", "--format-version", "1", "-q"], cwd=self.repo,
                       check=True, capture_output=True)
        on = self.select(("crates/usnm-ocr-dash/src/lib.rs", ""))
        self.assertGreaterEqual(on, DASHBOARD)
        self.assertFalse(on & (BOINC_IMAGES | {"ocr_worker"}), on)

    @unittest.skipUnless(CARGO, "no cargo")
    def test_a_release_feature_hidden_by_dev_dependencies_runs_worker_checks(self):
        def tree(edges):
            return subprocess.run(
                [CARGO, "tree", "--locked", "--target", "all", "-p", "usnm-ocr-worker",
                 "-e", edges, "--prefix", "none", "-f", "{p} {f}"],
                cwd=self.repo, check=True, capture_output=True, text=True,
            ).stdout

        tests_before = tree("normal,build,dev")
        release_before = tree("normal,build")
        manifest = self.repo / "Cargo.toml"
        original = manifest.read_text()
        updated = original.replace(
            'tokio = { version = "1", features = ["macros", "rt-multi-thread", "signal"] }',
            'tokio = { version = "1", features = ["macros", "rt-multi-thread", "signal", "test-util"] }',
        )
        self.assertNotEqual(original, updated)
        manifest.write_text(updated)
        self.assertEqual(tree("normal,build,dev"), tests_before)
        self.assertNotEqual(tree("normal,build"), release_before)
        self.assertEqual(git(self.repo, "diff", "--name-only"), "Cargo.toml")
        self.assertIn("ocr_worker", self.select())

    @unittest.skipUnless(CARGO, "no cargo")
    def test_a_windows_only_lockfile_bump_runs_the_native_matrix(self):
        def host_tree():
            return subprocess.run(
                [CARGO, "tree", "--locked", "-p", "usnm-quality-app", "-p", "usnm-qwen-app",
                 "-e", "normal,build,dev", "--prefix", "none", "-f", "{p} {f}"],
                cwd=self.repo, check=True, capture_output=True, text=True,
            ).stdout

        before = host_tree()
        self.assertNotIn("windows-sys", before)
        subprocess.run([CARGO, "update", "-q", "-p", "windows-sys@0.61.2", "--precise", "0.61.1"],
                       cwd=self.repo, check=True, capture_output=True)
        self.assertEqual(git(self.repo, "diff", "--name-only"), "Cargo.lock")
        self.assertEqual(host_tree(), before)
        self.assertIn("boinc_native", self.select())

    @unittest.skipUnless(CARGO, "no cargo")
    def test_a_dependency_bump_under_usnm_boinc_runs_the_boinc_images(self):
        lock = (self.repo / "Cargo.lock").read_text()
        version = re.search(r'name = "thiserror"\nversion = "(2\.0\.(\d+))"', lock)
        older = f"2.0.{int(version.group(2)) - 1}"
        subprocess.run([CARGO, "update", "-q", "-p", f"thiserror@{version.group(1)}",
                        "--precise", older], cwd=self.repo, check=True, capture_output=True)
        self.assertGreaterEqual(self.select(), BOINC_IMAGES)


class Wiring(unittest.TestCase):
    """What the selector can't check: the workflows read it, a skipped job is not a missing
    check, and the lists it keeps by hand name every file the workflows list."""

    def workflow(self, name: str) -> str:
        return (ROOT / ".github/workflows" / name).read_text()

    def pattern(self, flag: str) -> re.Pattern:
        script = (ROOT / "scripts/ci/changes.sh").read_text()
        value = re.search(rf"(?m)^{flag}=(?:'(.*)'|\$(\w+))$", script)
        self.assertIsNotNone(value, flag)
        return re.compile(value[1]) if value[1] else self.pattern(value[2])

    def test_every_non_crate_path_of_a_workflow_is_a_selector_input(self):
        for name, flag in GATED_WORKFLOWS.items():
            pattern = self.pattern(flag)
            for glob in workflow_paths(ROOT / ".github/workflows" / name):
                if glob.startswith(("!", ".github/")) or glob in CLOSURE_GOVERNED:
                    continue
                sample = glob.replace("**", "x/y").replace("*", "x")
                if CARGO and any(glob_regex(glob).match(f) for f in embedded_files(*EMBED_ROOTS[name])):
                    continue  # an embedded file: the selector finds it from the crates
                with self.subTest(workflow=name, path=glob):
                    self.assertRegex(sample, pattern)

    @unittest.skipUnless(CARGO, "no cargo")
    def test_every_workflow_starts_for_the_files_its_crates_embed(self):
        # A pull request that changes only such a file must start the workflow, whose selector
        # then runs the job. A file a crate starts to embed fails this until it is listed.
        for name, roots in EMBED_ROOTS.items():
            globs = [glob_regex(g) for g in workflow_paths(ROOT / ".github/workflows" / name)
                     if not g.startswith("!")]
            embedded = embedded_files(*roots)
            self.assertTrue(embedded, name)
            for file in sorted(embedded):
                with self.subTest(workflow=name, file=file):
                    self.assertTrue(any(g.match(file) for g in globs))

    def test_the_gated_workflows_still_list_the_crates_so_a_crate_change_reaches_the_selector(self):
        for name in GATED_WORKFLOWS:
            with self.subTest(workflow=name):
                self.assertTrue(CLOSURE_GOVERNED <= set(workflow_paths(ROOT / ".github/workflows" / name)))

    def test_pull_request_jobs_wait_for_the_selector_and_publishing_does_not(self):
        for name, flag in GATED_WORKFLOWS.items():
            text = self.workflow(name)
            with self.subTest(workflow=name):
                inputs = job_text(text, "inputs")
                self.assertIn("scripts/ci/changes.sh", inputs)
                self.assertIn(f"steps.f.outputs.{flag}", inputs)
                self.assertIn("fetch-depth: 0", inputs)
                self.assertIn("needs.inputs.outputs.build == 'true'", text)
                self.assertNotRegex(text, r"(?m)^  changes:")
                for job in re.findall(r"(?m)^  (publish):", text):
                    self.assertNotIn("inputs", job_text(text, job))

    def test_ci_reports_every_required_check_on_every_pull_request(self):
        # The names the "Protect main" ruleset requires. A job skipped by its `if:` reports as
        # skipped, which satisfies a required check; a workflow its `paths:` filter skips
        # doesn't report at all, so none of these may sit behind one.
        required = ["changes", "rust (fmt, clippy, test)", "quickwit (parity, ingest release)",
                    "fixtures are reproducible", "container image builds", "infra (bicep lint, build)",
                    "web (lint, typecheck, test, build)", "web end-to-end (Playwright + axe)"]
        ci = self.workflow("ci.yml")
        triggers = ci.split("\npermissions:", 1)[0]
        self.assertNotIn("paths", triggers)
        names = re.findall(r"(?m)^    name: (.*)$", ci)
        for check in required:
            with self.subTest(check=check):
                self.assertIn(check, names)
        for name in GATED_WORKFLOWS:
            text = self.workflow(name)
            declared = set(re.findall(r"(?m)^    name: (.*)$", text)) | {job_name for job_name in
                re.findall(r"(?m)^  ([a-z-]+):$", text)}
            self.assertFalse(declared & set(required), name)

    def test_the_edge_job_follows_its_output_and_runs_every_test_file(self):
        ci = self.workflow("ci.yml")
        self.assertIn("edge: ${{ steps.f.outputs.edge }}", job_text(ci, "changes"))
        job = job_text(ci, "edge")
        self.assertIn("    needs: changes\n", job)
        self.assertIn("    if: needs.changes.outputs.edge == 'true'\n", job)
        self.assertIn("shellcheck -x -S warning edge/fallback-worker/deploy.sh", job)
        # `node --test edge/fallback-worker/` is an error on Node 24, so the job names the test
        # files with a glob, and only that directory's files match it.
        self.assertIn("node-version: 24", job)
        self.assertIn("      - run: node --test edge/fallback-worker/*.test.mjs\n", job)
        found = sorted(p.relative_to(ROOT).as_posix() for p in (ROOT / "edge").rglob("*")
                       if p.is_file() and re.search(r"\.test\.(mjs|js|cjs)$", p.name))
        self.assertTrue(found)
        for file in found:
            self.assertRegex(file, r"^edge/fallback-worker/[^/]+\.test\.mjs$")

    def test_the_infra_jobs_stack_steps_follow_the_infra_output(self):
        job = job_text(self.workflow("ci.yml"), "infra")
        always = ("actions/checkout", "No shared-key authentication", "Scripts and CLIs take named arguments")
        steps = re.split(r"(?m)^      - ", job.split("    steps:\n", 1)[1])[1:]
        self.assertGreaterEqual(len(steps), 12)  # the split found the steps; script unit tests are one step
        for step in steps:
            if any(marker in step for marker in always):
                self.assertNotIn("outputs.infra", step)
            elif "outputs.ocr_gold" not in step:
                with self.subTest(step=step.splitlines()[0]):
                    self.assertIn("if: needs.changes.outputs.infra == 'true'", step)

    def test_the_dashboard_sources_are_read_by_nothing_the_infra_stack_runs(self):
        # changes.sh skips the stack's steps for a change in the dashboard crate's src/ and
        # tests/. The two scans in the infra job read every crate; no other script, schema or
        # step may name this one. test_head_of_main.py only names a path of it, in a commit of a
        # scratch clone, to see which flags changes.sh sets for it; it reads none of its sources.
        scans = {"scripts/ci/changes.sh", "scripts/ci/test_changes.py", "scripts/ci/no-shared-keys.sh",
                 "scripts/ci/no-positional-args.py", "scripts/ci/test_no_positional_args.py",
                 "scripts/ci/test_head_of_main.py"}
        tokens = ("usnm-ocr-dash", "usnm_ocr_dash")
        for base in ("scripts", "infra"):
            for path in (ROOT / base).rglob("*"):
                rel = str(path.relative_to(ROOT))
                if not path.is_file() or rel in scans or "__pycache__" in rel:
                    continue
                try:
                    text = path.read_text()
                except UnicodeDecodeError:
                    continue
                with self.subTest(path=rel):
                    self.assertFalse(any(token in text for token in tokens))
        self.assertFalse(any(token in job_text(self.workflow("ci.yml"), "infra") for token in tokens))

    def test_the_dashboard_only_build_keeps_the_release_whole(self):
        ci = self.workflow("ci.yml")
        el9, check, release = (job_text(ci, j) for j in ("ocr-worker-el9", "ocr-worker-check", "ocr-worker-release"))
        # A run that isn't a pull request, or one that changes the worker, builds both binaries.
        self.assertIn("ocr_worker == 'true' || github.event_name != 'pull_request'", el9)
        self.assertIn("-p usnm-ocr-dash", el9)
        self.assertIn("-p usnm-ocr-worker", el9)
        # The release is made from the check job's artifact, so main runs it for either change.
        self.assertIn("ocr_dash == 'true' && github.event_name != 'pull_request'", check)
        self.assertIn("ocr_dash == 'true'", release)
        for file in ["dist/usnm-ocr-worker", "dist/usnm-ocrdash", "dist/usnm-ocr-tesseract.oci.tar.gz",
                     "dist/usnm-ocr-tesseract.image-id"]:
            self.assertIn(file, check)
            self.assertIn(file, release)
