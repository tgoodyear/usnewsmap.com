import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
import xml.etree.ElementTree as ET
from unittest.mock import MagicMock, patch


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


campaign = load("iris_campaign", "iris-campaign.py")
checker = load("iris_checker", "iris-check.py")


class IrisCampaignTest(unittest.TestCase):
    def stage(self, root):
        image = "ghcr.io/tgoodyear/usnm-boinc-worker@sha256:" + "a" * 64
        records = []
        for number in range(100):
            data = f"fixture-image-{number}".encode()
            job = {
                "v": 1, "crop_id": f"pub-overproof-ca-{number:03}",
                "doc_id": f"fixture/page-{number}", "host_id": 9,
                "worker_image": image, "model_id": campaign.MODEL_ID,
                "model_revision": campaign.MODEL_REVISION,
                "prompt_version": campaign.PROMPT_VERSION, "max_new_tokens": 512,
                "page_sha256": campaign.sha(data), "page_bytes": len(data),
                "box": [0, 0, 16, 16],
            }
            job["id"] = campaign.canonical_job_id(job)
            raw = campaign.encoded(job)
            unit = root / "units" / job["id"]
            unit.mkdir(parents=True)
            (unit / "iris.json").write_bytes(raw)
            (unit / "page.jpg").write_bytes(data)
            records.append({
                "id": job["id"], "crop_id": job["crop_id"], "reference": "fixture",
                "job_sha256": campaign.sha(raw), "page_sha256": job["page_sha256"],
            })
        manifest = {
            "v": 1, "app": "usnm_iris", "host_id": 9, "batch": 104,
            "worker_image": image, "evaluation_only": True, "jobs": records,
        }
        (root / "campaign.json").write_bytes(campaign.encoded(manifest))
        return manifest

    def test_dispatch_is_exactly_100_and_repeated_apply_does_not_duplicate(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "campaign"
            self.stage(root)
            project = Path(directory) / "project"
            (project / "download").mkdir(parents=True)
            (project / "config.xml").write_text(
                "<boinc><config><enable_assignment>1</enable_assignment></config></boinc>"
            )
            work = []

            def run(args, **kwargs):
                if Path(args[0]).name == "dir_hier_path":
                    return SimpleNamespace(stdout=str(project / "download" / args[1]))
                work.append(args)
                return SimpleNamespace(returncode=0)

            with patch.object(campaign, "verify_registered_image"), patch.object(campaign.subprocess, "run", side_effect=run):
                self.assertEqual(campaign.submit(root, project)["jobs"], 100)
                self.assertEqual(work, [])
                self.assertEqual(campaign.submit(root, project, True)["submitted"], 100)
                self.assertEqual(campaign.submit(root, project, True)["previously_submitted"], 100)
            self.assertEqual(len(work), 100)
            self.assertEqual(len(list((project / "iris/jobs").glob("*.json"))), 100)
            self.assertEqual(len(list((project / "download").iterdir())), 200)

    def test_uncertain_dispatch_is_not_retried_and_changed_input_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "campaign"
            manifest = self.stage(root)
            project = Path(directory) / "project"
            (project / "download").mkdir(parents=True)
            (project / "config.xml").write_text(
                "<boinc><config><enable_assignment>1</enable_assignment></config></boinc>"
            )

            def run(args, **kwargs):
                if Path(args[0]).name == "dir_hier_path":
                    return SimpleNamespace(stdout=str(project / "download" / args[1]))
                raise subprocess.CalledProcessError(1, args)

            with patch.object(campaign, "verify_registered_image"), patch.object(campaign.subprocess, "run", side_effect=run):
                with self.assertRaises(subprocess.CalledProcessError):
                    campaign.submit(root, project, True)
            with patch.object(campaign, "verify_registered_image"), patch.object(campaign.subprocess, "run") as retry:
                with self.assertRaisesRegex(ValueError, "uncertain"):
                    campaign.submit(root, project, True)
                retry.assert_not_called()
            first = root / "units" / manifest["jobs"][0]["id"]
            (first / "page.jpg").write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "changed"):
                campaign.load_campaign(root)

    def test_work_is_assigned_only_to_authorized_host_with_no_retry_replicas(self):
        args = campaign.create_work_args("a" * 64)
        for key, expected in (
            ("--target_host", "9"), ("--batch", "104"),
            ("--appname", "usnm_iris"), ("--min_quorum", "1"),
            ("--app_version_num", "100"),
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

    def test_dispatch_checks_registered_image_even_in_dry_run(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "campaign"
            self.stage(root)
            project = Path(directory) / "project"
            project.mkdir()
            (project / "config.xml").write_text(
                "<boinc><config><enable_assignment>1</enable_assignment></config></boinc>"
            )
            with patch.object(campaign, "verify_registered_image", side_effect=ValueError("image mismatch")):
                with self.assertRaisesRegex(ValueError, "image mismatch"):
                    campaign.submit(root, project)

    def test_unrecognized_assignment_option_cannot_authorize_dispatch(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "campaign"
            self.stage(root)
            project = Path(directory) / "project"
            project.mkdir()
            (project / "config.xml").write_text(
                "<boinc><config><enable_assigned_jobs>1</enable_assigned_jobs></config></boinc>"
            )
            with patch.object(campaign, "verify_registered_image") as verify:
                with self.assertRaisesRegex(ValueError, "enable_assignment=1"):
                    campaign.submit(root, project)
                verify.assert_not_called()

    def test_registered_signed_dockerfile_must_match_campaign(self):
        with tempfile.TemporaryDirectory() as directory:
            project = Path(directory)
            (project / "download").mkdir()
            image = "ghcr.io/tgoodyear/usnm-boinc-worker@sha256:" + "a" * 64
            raw = f"FROM {image}\n".encode()
            dockerfile = project / "download/Dockerfile_fixture"
            dockerfile.write_bytes(raw)
            xml = (
                "<file_info><name>Dockerfile_fixture</name>"
                "<file_signature>published-signature</file_signature></file_info>"
                "<file_ref><file_name>Dockerfile_fixture</file_name>"
                "<open_name>Dockerfile</open_name></file_ref>"
            )
            with patch.object(campaign.subprocess, "run", return_value=SimpleNamespace(stdout=str(dockerfile))) as run:
                campaign.verify_version_files(project, image, xml.encode())
                self.assertEqual(run.call_args.args[0][1], "-verify")
                with self.assertRaisesRegex(ValueError, "differs"):
                    campaign.verify_version_files(project, image.replace("a" * 64, "b" * 64), xml)
                dockerfile.write_text("changed")
                with self.assertRaisesRegex(ValueError, "differs"):
                    campaign.verify_version_files(project, image, xml)
                dockerfile.write_bytes(raw)
                run.side_effect = [
                    SimpleNamespace(stdout=str(dockerfile)),
                    subprocess.CalledProcessError(1, ["crypt_prog", "-verify"]),
                ]
                with self.assertRaises(subprocess.CalledProcessError):
                    campaign.verify_version_files(project, image, xml)
            with self.assertRaisesRegex(ValueError, "signature"):
                campaign.verify_version_files(project, image, xml.replace("published-signature", ""))

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

    def test_preparation_rejects_non_jpeg_before_staging_any_job(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "images").mkdir()
            data = b"png image bytes"
            (root / "images/crop.png").write_bytes(data)
            row = {
                "crop_id": "pub-overproof-ca-001", "image": "images/crop.png",
                "image_sha256": campaign.sha(data),
                "provenance": {"for_scoring": True, "source_license": "public domain"},
            }
            (root / "dataset.json").write_text(json.dumps({
                "evaluation_only": True, "crops": [row] * 100,
            }))
            image = MagicMock()
            image.open.return_value.__enter__.return_value.format = "PNG"
            with patch.dict(sys.modules, {"PIL": SimpleNamespace(Image=image)}):
                with self.assertRaisesRegex(ValueError, "must be JPEG"):
                    campaign.prepare(
                        root, root / "out",
                        "ghcr.io/tgoodyear/usnm-boinc-worker@sha256:" + "a" * 64,
                    )
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
            uploaded = root / f"usnm_iris_{identity}_0_0"
            uploaded.write_text(json.dumps(result))
            with patch.object(checker, "validate_job"), patch.object(checker, "validate_result") as validate:
                with self.assertRaisesRegex(ValueError, "inference failed"):
                    checker.check(uploaded, jobs, root / "seen")
                validate.assert_called_once_with(result, {})
            self.assertEqual(json.loads((root / "seen" / (uploaded.name + ".json")).read_text()), result)

    def test_unregistered_result_and_conflicting_replay_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            jobs = root / "jobs"
            jobs.mkdir()
            identity = "a" * 64
            uploaded = root / f"usnm_iris_{identity}_0_0"
            uploaded.write_text(json.dumps({"id": identity, "status": "ok"}))
            with self.assertRaises(FileNotFoundError):
                checker.check(uploaded, jobs, root / "seen")
            (jobs / f"{identity}.json").write_text("{}")
            with patch.object(checker, "validate_job"), patch.object(checker, "validate_result"):
                checker.check(uploaded, jobs, root / "seen")
                uploaded.write_text(json.dumps({"id": identity, "status": "ok", "text": "different"}))
                with self.assertRaisesRegex(ValueError, "conflicting"):
                    checker.check(uploaded, jobs, root / "seen")

    def test_registered_result_cannot_validate_another_jobs_upload(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            jobs = root / "jobs"
            jobs.mkdir()
            first, second = "a" * 64, "b" * 64
            for identity in (first, second):
                (jobs / f"{identity}.json").write_text("{}")
            uploaded = root / f"usnm_iris_{second}_0_r12345_0"
            uploaded.write_text(json.dumps({"id": first, "status": "ok"}))
            with patch.object(checker, "validate_job") as validate:
                with self.assertRaisesRegex(ValueError, "does not match"):
                    checker.check(uploaded, jobs, root / "seen")
                validate.assert_not_called()
            self.assertFalse((root / "seen").exists())

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
