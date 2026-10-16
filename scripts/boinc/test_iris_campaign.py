import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import xml.etree.ElementTree as ET
from unittest.mock import patch


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


campaign = load("iris_campaign", "iris-campaign.py")
checker = load("iris_checker", "iris-check.py")


class IrisCampaignTest(unittest.TestCase):
    def test_work_is_assigned_only_to_authorized_host_with_no_retry_replicas(self):
        args = campaign.create_work_args("a" * 64)
        for key, expected in (
            ("--target_host", "9"), ("--batch", "104"),
            ("--appname", "usnm_iris"), ("--min_quorum", "1"),
            ("--target_nresults", "1"), ("--max_total_results", "1"),
            ("--rsc_memory_bound", "9663676416"),
        ):
            self.assertEqual(args[args.index(key) + 1], expected)
        self.assertNotIn("--target_user", args)
        self.assertNotIn("--target_team", args)
        self.assertNotIn("usnm_qwen", args)

    def test_invalid_identity_cannot_enter_create_work(self):
        for identity in ("../escape", "x" * 64, "", "a" * 63, "a" * 64 + "\n"):
            with self.assertRaises(ValueError):
                campaign.create_work_args(identity)

    def test_exact_character_error_distance(self):
        self.assertEqual(campaign.edit_distance("kitten", "sitting"), 3)
        self.assertEqual(campaign.edit_distance("", "newspaper"), 9)
        self.assertEqual(campaign.edit_distance("newspaper", "newspaper"), 0)

    def test_preparation_requires_immutable_worker_before_writing(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for image in ("image:latest", "ghcr.io/tgoodyear/usnm-boinc-worker:iris"):
                with self.assertRaises(ValueError):
                    campaign.prepare(root, root / "out", image)
            self.assertFalse((root / "out").exists())

    def test_input_output_templates_match_worker_filenames(self):
        root = Path(__file__).resolve().parents[2] / "ocr/boinc"
        inputs = ET.parse(root / "usnm_iris_in")
        outputs = ET.parse(root / "usnm_iris_out")
        self.assertEqual(
            [ref.findtext("open_name") for ref in inputs.findall("./workunit/file_ref")],
            ["page.jpg", "iris.json"],
        )
        self.assertEqual(outputs.findtext("./result/file_ref/open_name"), "iris-result.json")
        self.assertEqual(outputs.findtext("./file_info/max_nbytes"), "65536")

    def test_validator_binds_to_registered_job_and_retains_failure_diagnostics(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            jobs = root / "jobs"
            jobs.mkdir()
            identity = "a" * 64
            result = {"id": identity, "status": "error", "text": ""}
            (jobs / f"{identity}.json").write_text("{}")
            uploaded = root / "usnm_iris_result_0"
            uploaded.write_text(json.dumps(result))
            with patch.object(checker, "validate_job"), patch.object(checker, "validate_result") as validate:
                with self.assertRaisesRegex(ValueError, "inference failed"):
                    checker.check(uploaded, jobs, root / "seen")
                validate.assert_called_once_with(result, {})
            self.assertEqual(json.loads((root / "seen/usnm_iris_result_0.json").read_text()), result)

    def test_unregistered_result_and_conflicting_replay_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            jobs = root / "jobs"
            jobs.mkdir()
            identity = "a" * 64
            uploaded = root / "usnm_iris_result_0"
            uploaded.write_text(json.dumps({"id": identity, "status": "ok"}))
            with self.assertRaises(FileNotFoundError):
                checker.check(uploaded, jobs, root / "seen")
            (jobs / f"{identity}.json").write_text("{}")
            with patch.object(checker, "validate_job"), patch.object(checker, "validate_result"):
                checker.check(uploaded, jobs, root / "seen")
                uploaded.write_text(json.dumps({"id": identity, "status": "ok", "text": "different"}))
                with self.assertRaisesRegex(ValueError, "conflicting"):
                    checker.check(uploaded, jobs, root / "seen")

    def test_failed_boinc_job_without_uploaded_output_is_terminal_error(self):
        identity = "a" * 64
        manifest = {"jobs": [{"id": identity, "crop_id": "pub-overproof-ca-001"}]}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            seen = root / "seen"
            seen.mkdir()
            snapshot = root / "terminal.json"
            snapshot.write_text(json.dumps([{
                "workunit_name": f"usnm_iris_{identity}", "host_id": 9,
                "batch": 104, "server_state": 5, "outcome": 3,
                "validate_state": 0, "stderr_out": "worker_crashed: SIGKILL",
            }]))
            with patch.object(campaign, "load_campaign", return_value=manifest):
                report = campaign.collect(root, seen, snapshot)
            self.assertEqual(report["pending"], 0)
            self.assertEqual(report["errors"], 1)
            self.assertEqual(report["boinc_terminal"], 1)
            row = json.loads((root / "report.json").read_text())["rows"][0]
            self.assertEqual(row["status"], "boinc_error")
            self.assertIn("SIGKILL", row["boinc"]["stderr_out"])

    def test_terminal_snapshot_rejects_other_hosts_and_duplicate_results(self):
        identity = "a" * 64
        manifest = {"jobs": [{"id": identity}]}
        row = {
            "workunit_name": f"usnm_iris_{identity}", "host_id": 8,
            "batch": 104, "server_state": 5, "outcome": 1,
            "validate_state": 1, "stderr_out": "",
        }
        with tempfile.TemporaryDirectory() as directory:
            snapshot = Path(directory) / "terminal.json"
            snapshot.write_text(json.dumps([row]))
            with self.assertRaisesRegex(ValueError, "authorized"):
                campaign.terminal_results(snapshot, manifest)
            row["host_id"] = 9
            snapshot.write_text(json.dumps([row, row]))
            with self.assertRaisesRegex(ValueError, "multiple"):
                campaign.terminal_results(snapshot, manifest)


if __name__ == "__main__":
    unittest.main()
