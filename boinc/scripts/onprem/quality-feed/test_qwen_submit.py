"""The staging and publishing phases of qwen_submit.py, without BOINC or a server."""
import contextlib
import io
import json
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import qwen_submit  # noqa: E402


class PhaseTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.share = Path(tmp.name)
        (self.share / "qwen/camp/units").mkdir(parents=True)
        (self.share / "quality/camp/pages").mkdir(parents=True)
        self.plan = self.share / "plan.json"
        self.plan.write_text(json.dumps({"engine": {"image": "i", "llama": "l", "weights": "w"}}))
        self.calls = []

    def run_main(self, *extra, bundle=None, unsent=0, binaries=()):
        argv = ["qwen_submit.py", "--share", str(self.share), "--project", str(self.share / "usnmocr"),
                "--campaign", "camp", "--engine-plan", str(self.plan), "--models", "/models",
                "--max-pages", "7", "--stage-binary", "/ops/usnm-boinc-stage", *extra]
        if bundle:
            argv += ["--bundle-size", str(bundle)]
        admitted = []
        self.batches = []
        patches = (
            mock.patch.object(sys, "argv", argv),
            mock.patch.object(qwen_submit, "inventory",
                              lambda share, project, batch=8: self.batches.append(("inventory", batch)) or {}),
            mock.patch.object(qwen_submit, "admission", lambda a, state, root: admitted.append(a.max_pages)),
            mock.patch.object(qwen_submit, "assert_bundle_layout"),
            mock.patch.object(qwen_submit, "unsubmitted_units", return_value=unsent),
            mock.patch.object(qwen_submit, "held_pages", return_value=0),
            mock.patch.object(qwen_submit, "assert_submitted"),
            mock.patch.object(qwen_submit, "audit",
                              lambda root, project, batch=8: self.batches.append(("audit", batch))
                              or {"verified_new": 0}),
            mock.patch.object(qwen_submit.subprocess, "run", lambda cmd, **kw: self.calls.append(cmd)),
        )
        with contextlib.ExitStack() as stack:
            for patch in patches:
                stack.enter_context(patch)
            with contextlib.redirect_stdout(io.StringIO()) as out:
                qwen_submit.main()
        return admitted, out.getvalue()

    def verbs(self):
        return [(c[0], c[2]) for c in self.calls]

    def test_all_stays_one_run_that_stages_then_submits(self):
        self.run_main("--prompt-version", "3")
        self.assertEqual(self.verbs(), [("/ops/usnm-boinc-stage", "stage"), ("/ops/usnm-boinc-stage", "submit")])
        self.assertNotIn("--models-prechecked", self.calls[1])
        self.assertNotIn("--models-only", self.calls[1])

    def test_prepare_stages_and_verifies_the_models_but_submits_nothing(self):
        _, out = self.run_main("--phase", "prepare", "--prompt-version", "3")
        stage, models = self.calls
        self.assertEqual(stage[2], "stage")
        self.assertEqual(stage[stage.index("--max-pages") + 1], "7")
        self.assertEqual(models[:3], ["/ops/usnm-boinc-stage", "qwen", "submit"])
        self.assertIn("--models-only", models)
        self.assertNotIn("--max-units", models)
        self.assertEqual(json.loads(out)["phase"], "prepared")

    def test_publish_submits_only_and_skips_the_model_hashing_with_the_stage_binary(self):
        admitted, _ = self.run_main("--phase", "publish", "--prompt-version", "3")
        (submit,) = self.calls
        self.assertEqual(submit[:3], ["/ops/usnm-boinc-stage", "qwen", "submit"])
        self.assertIn("--models-prechecked", submit)
        self.assertEqual(submit[submit.index("--max-units") + 1], "7")
        self.assertEqual(admitted, [7])

    def test_publish_counts_the_staged_units_when_bundling(self):
        admitted, _ = self.run_main("--phase", "publish", "--prompt-version", "3", bundle=4, unsent=3)
        (submit,) = self.calls
        self.assertEqual(submit[submit.index("--max-units") + 1], "3")
        self.assertEqual(admitted, [3])

    def test_publish_with_the_images_own_binary_hashes_the_models_as_before(self):
        self.run_main("--phase", "publish", "--prompt-version", "3", "--submit-binary", "usnm-boinc")
        (submit,) = self.calls
        self.assertEqual(submit[0], "usnm-boinc")
        self.assertNotIn("--models-prechecked", submit)

    def test_staging_holds_one_lock_across_roots(self):
        import fcntl
        seen = []

        def staged(cmd, **kw):
            with (self.share / "qwen" / ".stage-roots.lock").open("a") as other:
                try:
                    fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    seen.append((cmd[2], "free"))
                except BlockingIOError:
                    seen.append((cmd[2], "held"))

        with mock.patch.object(qwen_submit.subprocess, "run", staged):
            qwen_submit.stage_root(types.SimpleNamespace(
                share=self.share, engine_plan=self.plan, stage_binary="/ops/s", bundle_size=1, prompt_version=3,
                page_records=None), self.share / "qwen/camp", self.share / "quality/camp", 1)
        self.assertEqual(seen, [("stage", "held")])

    def test_a_transaction_defers_rather_than_wait_for_a_long_staging(self):
        import fcntl
        (self.share / "qwen").mkdir(exist_ok=True)
        with (self.share / "qwen" / ".stage-roots.lock").open("a") as held:
            fcntl.flock(held, fcntl.LOCK_EX)
            with contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(SystemExit) as stop, qwen_submit.stage_roots_lock(self.share, wait_secs=0):
                    pass
        self.assertEqual(stop.exception.code, 75)

    def test_versions_1_to_4_stage_unpinned_and_send_native_8b_as_batch_8(self):
        for version in ("1", "2", "3", "4"):
            self.calls = []
            self.run_main("--prompt-version", version)
            stage, submit = self.calls
            self.assertNotIn("--model", stage)
            self.assertEqual(stage[stage.index("--prompt-version") + 1], version)
            self.assertEqual(submit[submit.index("--target") + 1], "native-8b")
            self.assertEqual(submit[0], "/ops/usnm-boinc-stage" if version in ("3", "4") else "usnm-boinc")
            self.assertEqual(self.batches, [("inventory", 8), ("audit", 8)])

    def test_version_5_pins_xiaomi_and_sends_native_xiaomi_as_batch_108(self):
        self.run_main("--prompt-version", "5", bundle=32)
        stage, submit = self.calls
        self.assertEqual(stage[stage.index("--model") + 1], "xiaomi-ocr-0-q8")
        self.assertEqual(stage[stage.index("--prompt-version") + 1], "5")
        self.assertEqual(stage[stage.index("--bundle-size") + 1], "32")
        self.assertEqual(submit[:3], ["/ops/usnm-boinc-stage", "qwen", "submit"])
        self.assertEqual(submit[submit.index("--target") + 1], "native-xiaomi")
        self.assertEqual(self.batches, [("inventory", 108), ("audit", 108)])
        self.calls = []
        self.run_main("--phase", "prepare", "--prompt-version", "5", bundle=32)
        self.assertEqual(self.calls[1][self.calls[1].index("--target") + 1], "native-xiaomi")
        self.assertEqual(qwen_submit.REQUEST_MIN_APP[5], 120)

    def test_english_needs_a_xiaomi_root_that_allows_it(self):
        page = self.share / "quality/camp/pages/sn1_1900-01-01_ed-1_seq-1"
        page.mkdir(parents=True)
        (page / "quality.json").write_text(json.dumps(
            {"doc_id": page.name, "language": "eng", "pages": []}))
        for extra in ((), ("--prompt-version", "4"), ("--prompt-version", "5")):
            with self.assertRaisesRegex(RuntimeError, "Refusing English"):
                self.run_main(*extra)
        self.calls = []
        self.run_main("--prompt-version", "5", "--allow-english")
        self.assertEqual(self.verbs(), [("/ops/usnm-boinc-stage", "stage"), ("/ops/usnm-boinc-stage", "submit")])
        argv = ["qwen_submit.py", "--share", str(self.share), "--campaign", "camp", "--engine-plan", str(self.plan),
                "--models", "/models", "--prompt-version", "3", "--allow-english"]
        with mock.patch.object(sys, "argv", argv):
            with contextlib.redirect_stderr(io.StringIO()) as err, self.assertRaises(SystemExit):
                qwen_submit.main()
        self.assertIn("--allow-english is for a Xiaomi-OCR-0 root", err.getvalue())

    def test_a_phase_is_not_a_preflight_or_a_transaction(self):
        argv = ["qwen_submit.py", "--share", str(self.share), "--campaign", "camp", "--engine-plan", str(self.plan),
                "--models", "/models", "--phase", "publish", "--preflight"]
        with mock.patch.object(sys, "argv", argv), mock.patch.object(qwen_submit, "inventory", return_value={}):
            with contextlib.redirect_stderr(io.StringIO()) as err, self.assertRaises(SystemExit):
                qwen_submit.main()
        self.assertIn("--phase is for a native submission", err.getvalue())


if __name__ == "__main__":
    unittest.main()
