"""The staging and publishing phases of qwen_submit.py, without BOINC or a server."""
import contextlib
import io
import json
from pathlib import Path
import sys
import tempfile
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
        patches = (
            mock.patch.object(sys, "argv", argv),
            mock.patch.object(qwen_submit, "inventory", return_value={}),
            mock.patch.object(qwen_submit, "admission", lambda a, state, root: admitted.append(a.max_pages)),
            mock.patch.object(qwen_submit, "assert_bundle_layout"),
            mock.patch.object(qwen_submit, "unsubmitted_units", return_value=unsent),
            mock.patch.object(qwen_submit, "held_pages", return_value=0),
            mock.patch.object(qwen_submit, "assert_submitted"),
            mock.patch.object(qwen_submit, "audit", return_value={"verified_new": 0}),
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

    def test_a_phase_is_not_a_preflight_or_a_transaction(self):
        with mock.patch.object(sys, "argv", ["qwen_submit.py", "--phase", "publish", "--preflight"]):
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                qwen_submit.main()


if __name__ == "__main__":
    unittest.main()
