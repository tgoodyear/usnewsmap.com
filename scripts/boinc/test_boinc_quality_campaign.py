"""Tests for boinc-quality-campaign.py launch-corpus: python3 -m unittest -v scripts/boinc/test_boinc_quality_campaign.py"""

import hashlib
import importlib.util
import io
import contextlib
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("campaign", ROOT / "scripts/boinc/boinc-quality-campaign.py")
campaign = importlib.util.module_from_spec(spec)
spec.loader.exec_module(campaign)

DIGEST = hashlib.sha256((ROOT / "scripts/boinc/quality_corpus_runner.py").read_bytes()).hexdigest()
URL = f"https://stusnmoffapzy.blob.core.windows.net/boinc-handoff/quality/v1-wave9/run_wave9-{DIGEST[:12]}.py"


class LaunchCorpus(unittest.TestCase):
    def run_launch(self, env, running=(), extra=()):
        calls = []

        def az(args, capture_json=True):
            calls.append(args)
            if args[:3] == ["containerapp", "job", "show"]:
                return {"properties": {"template": {"containers": [{"env": env}]}}}
            if args[:4] == ["containerapp", "job", "execution", "list"]:
                return list(running)
            if args[:3] == ["containerapp", "job", "start"]:
                return "exec-1"
            return None

        args = campaign.build_parser().parse_args(
            ["launch-corpus", "--handoff-account", "stusnmoffapzy", "--handoff-container", "boinc-handoff", *extra])
        err = io.StringIO()
        with mock.patch.object(campaign, "run_az", side_effect=az), \
             mock.patch.object(campaign, "get_storage_token", return_value="TOKEN-VALUE"), \
             mock.patch.object(campaign, "upload_blob_bytes") as upload, \
             mock.patch.object(campaign, "poke_local_boinc_client", return_value=False), \
             contextlib.redirect_stderr(err), contextlib.redirect_stdout(io.StringIO()):
            rc = args.func(args)
        return rc, calls, upload, err.getvalue()

    def pinned(self):
        return [{"name": "QUALITY_WAVE_SCRIPT", "value": URL}, {"name": "QUALITY_WAVE_SHA256", "value": DIGEST}]

    def test_uploads_under_the_digest_and_starts_without_editing_the_job(self):
        rc, calls, upload, _ = self.run_launch(self.pinned())
        self.assertEqual(rc, 0)
        self.assertEqual(upload.call_args.args[2], f"quality/v1-wave9/run_wave9-{DIGEST[:12]}.py")
        self.assertIn(["containerapp", "job", "start", "-g", "usnm-boinc-rg-prod", "-n",
                       "usnm-boinc-quality-caj-prod", "--query", "name", "-o", "tsv"], calls)
        flat = " ".join(" ".join(c) for c in calls)
        self.assertNotIn("rest", [c[0] for c in calls])
        self.assertNotIn("update", flat.split())
        self.assertNotIn("TOKEN-VALUE", flat)

    def test_another_pinned_runner_prints_the_setting_and_does_not_start(self):
        rc, calls, _, err = self.run_launch([{"name": "QUALITY_WAVE_SHA256", "value": "0" * 64}])
        self.assertEqual(rc, 1)
        self.assertIn(DIGEST, err)
        self.assertIn(URL, err)
        self.assertFalse(any(c[:3] == ["containerapp", "job", "start"] for c in calls))

    def test_a_running_execution_needs_restart(self):
        rc, calls, _, err = self.run_launch(self.pinned(), running=["exec-0"])
        self.assertEqual(rc, 1)
        self.assertIn("--restart", err)
        rc, calls, _, _ = self.run_launch(self.pinned(), running=["exec-0"], extra=["--restart"])
        self.assertEqual(rc, 0)
        self.assertTrue(any(c[:3] == ["containerapp", "job", "stop"] for c in calls))


if __name__ == "__main__":
    unittest.main()
