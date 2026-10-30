"""Reconstructed provenance never substitutes identity assertions for byte verification."""
import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import subprocess
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).parent))
import qwen_provenance as recovery
import qwen_submit as submit


class ProvenanceRecoveryTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.share = Path(self.directory.name)
        self.doc = "sn1_1900-01-02_ed-1_seq-3"
        self.original = b"original scan"
        self.jpeg = b"native jpeg"
        self.fingerprint = {"doc_id": self.doc, "batch": "xx_test", "batch_version": 1,
                            "bytes": len(self.original), "sha256": hashlib.sha256(self.original).hexdigest()}
        self.input = {"bytes": len(self.jpeg), "sha256": hashlib.sha256(self.jpeg).hexdigest()}
        self.job = {"id": "a" * 64, "doc_id": self.doc, "input": self.input}
        self.campaign = self.share / "qwen/test"
        self.unit = self.campaign / "units" / self.doc
        self.unit.mkdir(parents=True)
        self.job_raw = json.dumps(self.job).encode()
        (self.unit / "qwen.json").write_bytes(self.job_raw)
        (self.unit / "page.jpg").write_bytes(self.jpeg)
        (self.unit / "submitted").write_text("usnm_qwen_" + self.job["id"])
        self.audit = {"workunit_id": 42, "job_sha256": hashlib.sha256(self.job_raw).hexdigest(),
                      "batch": 8, "min_quorum": 2, "target_nresults": 2}
        self.audit_raw = json.dumps({self.job["id"]: self.audit}).encode()
        (self.campaign / "audit.json").write_bytes(self.audit_raw)
        self.target = {"campaign": "test", "job_id": self.job["id"], "job_sha256": self.audit["job_sha256"],
                       "workunit_id": 42, "source": self.fingerprint, "scan": "keep/xx_test/sn1/1900-01-02/1/3.jp2",
                       "jpeg": self.input, "transform": "reduced-gray"}

    def proof(self):
        with mock.patch.object(recovery, "derive_jpeg", return_value=self.jpeg), \
                mock.patch.object(recovery, "decoder_info", return_value={"verifier": "test"}):
            return recovery.verify(self.target, self.original)

    def test_exact_reconstruction_resolves_only_provenance_and_preserves_audits_and_quorum(self):
        self.assertIsNone(recovery.recovered_binding(
            self.campaign, self.job, self.audit, self.job, self.unit / "submitted"))
        proof = self.proof()
        recovery.install(self.share, [proof])
        recovery.install(self.share, [proof])
        self.assertEqual((self.campaign / "audit.json").read_bytes(), self.audit_raw)
        self.assertEqual((self.unit / "qwen.json").read_bytes(), self.job_raw)
        response = json.dumps([{"id": 42, "canonical_resultid": 0, "error_mask": 0,
                               "unsent": 2, "running": 0, "returned": 0, "invalid": 0}])
        with mock.patch.object(submit.subprocess, "check_output", return_value=response):
            status = submit.source_status(self.share, self.share, json.dumps(self.fingerprint))
        self.assertEqual(status["published_workunits"], 1)
        self.assertEqual(status["agreed_workunits"], 0)
        self.assertEqual(status["source_conflict_pages"], 0)
        self.assertEqual(status["pages"][self.doc]["provenance"], "verified_reconstruction")

    def test_original_or_derived_byte_mismatch_never_produces_a_receipt(self):
        with mock.patch.object(recovery, "derive_jpeg") as derive:
            with self.assertRaisesRegex(ValueError, "original scan"):
                recovery.verify(self.target, self.original + b"changed")
            derive.assert_not_called()
        with mock.patch.object(recovery, "derive_jpeg", return_value=b"different jpeg"):
            with self.assertRaisesRegex(ValueError, "reproduced JPEG"):
                recovery.verify(self.target, self.original)

    def test_invalid_frozen_identity_path_or_transform_is_rejected(self):
        for change in ({"scan": "../../etc/passwd"}, {"scan": "keep/xx_other/sn1/1900-01-02/1/3.jp2"},
                       {"scan": "buffer/xx_test/sn1/1900-01-02/1/../3.jp2"},
                       {"scan": "buffer/xx_other/sn1/1900-01-02/1/3.jp2"},
                       {"campaign": "../other"}, {"transform": "guess"},
                       {"source": {**self.fingerprint, "batch_version": 0}}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                recovery.validate_target({**self.target, **change})

    def test_retained_buffer_scan_can_be_verified_without_relaxing_source_identity(self):
        self.target["scan"] = self.target["scan"].replace("keep/", "buffer/", 1)
        self.assertEqual(self.proof()["target"]["scan"], self.target["scan"])

    @unittest.skipUnless(importlib.util.find_spec("PIL"), "Pillow required for real-image verification")
    def test_real_jpeg2000_transforms_preserve_full_geometry_or_reduce_only_above_threshold(self):
        from PIL import Image

        for width in (4000, 4100):
            image = Image.frombytes("RGB", (width, 12),
                                    bytes((i * 37) % 256 for i in range(width * 12 * 3)))
            encoded = io.BytesIO()
            image.save(encoded, "JPEG2000")
            raw = encoded.getvalue()
            for transform in ("reduced-gray", "full-gray", "full-rgb"):
                with self.subTest(width=width, transform=transform):
                    with Image.open(io.BytesIO(raw)) as original:
                        if transform == "reduced-gray" and width > 4000:
                            original.reduce = 1
                        expected = io.BytesIO()
                        original.convert("RGB" if transform == "full-rgb" else "L").save(
                            expected, "JPEG", quality=75)
                    target = {**self.target, "transform": transform,
                              "source": {**self.fingerprint, "bytes": len(raw),
                                         "sha256": hashlib.sha256(raw).hexdigest()},
                              "jpeg": {"bytes": len(expected.getvalue()),
                                       "sha256": hashlib.sha256(expected.getvalue()).hexdigest()}}
                    self.assertEqual(recovery.verify(target, raw)["target"], target)
                    with Image.open(io.BytesIO(recovery.derive_jpeg(raw, transform))) as result:
                        reduced = transform == "reduced-gray" and width > 4000
                        self.assertEqual(result.size, (width // 2, 6) if reduced else (width, 12))
                        self.assertEqual(result.mode, "RGB" if transform == "full-rgb" else "L")

    @unittest.skipUnless(importlib.util.find_spec("PIL"), "Pillow required for real-image verification")
    def test_passthrough_verifies_unchanged_gray_jpeg_but_rejects_color_and_non_jpeg(self):
        from PIL import Image

        for mode, fmt in (("L", "JPEG"), ("RGB", "JPEG"), ("L", "JPEG2000")):
            encoded = io.BytesIO()
            Image.new(mode, (64, 48)).save(encoded, fmt, quality=91)
            raw = encoded.getvalue()
            target = {**self.target, "transform": "passthrough",
                      "source": {**self.fingerprint, "bytes": len(raw),
                                 "sha256": hashlib.sha256(raw).hexdigest()},
                      "jpeg": {"bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest()}}
            with self.subTest(mode=mode, fmt=fmt):
                if mode == "L" and fmt == "JPEG":
                    self.assertEqual(recovery.verify(target, raw)["target"], target)
                    self.assertEqual(recovery.derive_jpeg(raw, "passthrough"), raw)
                    with self.assertRaisesRegex(ValueError, "reproduced JPEG"):
                        recovery.verify({**target, "jpeg": self.input}, raw)
                else:
                    with self.assertRaisesRegex(ValueError, "bounded grayscale JPEG"):
                        recovery.verify(target, raw)

    def test_changed_native_image_audit_or_submitted_identity_cannot_be_installed(self):
        proof = self.proof()
        for filename, changed in (("page.jpg", b"changed pixels"), ("qwen.json", self.job_raw + b" ")):
            path = self.unit / filename
            old = path.read_bytes()
            path.write_bytes(changed)
            with self.subTest(filename=filename), self.assertRaisesRegex(ValueError, "retained native"):
                recovery.install(self.share, [proof])
            path.write_bytes(old)
        wrong = copy.deepcopy(proof)
        wrong["target"]["workunit_id"] = 43
        with self.assertRaisesRegex(ValueError, "retained native"):
            recovery.install(self.share, [wrong])
        self.assertFalse((self.campaign / "recovered-provenance").exists())

    def test_conflicting_existing_proof_is_never_overwritten_and_stale_proof_never_counts(self):
        proof = self.proof()
        recovery.install(self.share, [proof])
        wrong = copy.deepcopy(proof)
        wrong["verified_at"] = "different"
        with self.assertRaisesRegex(ValueError, "overwrite"):
            recovery.install(self.share, [wrong])
        with self.assertRaisesRegex(ValueError, "bind to native audit"):
            recovery.recovered_binding(self.campaign, self.job, {**self.audit, "workunit_id": 43},
                                       self.job, self.unit / "submitted")

    def test_batch_is_validated_before_publication_and_duplicate_targets_are_rejected(self):
        proof = self.proof()
        wrong = copy.deepcopy(proof)
        wrong["target"]["workunit_id"] = 43
        for batch in ([proof, wrong], [proof, proof]):
            with mock.patch.object(submit, "jobs", wraps=submit.jobs) as scan, self.assertRaises(ValueError):
                recovery.install(self.share, batch)
            self.assertEqual(scan.call_count, 1)
            self.assertFalse((self.campaign / "recovered-provenance").exists())

    def test_recovered_evidence_cannot_override_a_conflicting_original_binding(self):
        recovery.install(self.share, [self.proof()])
        audit = {**self.audit, "source_bindings": {self.doc: {
            "schema": 1, "source": {**self.fingerprint, "batch_version": 2}, "jpeg": self.input}}}
        (self.campaign / "audit.json").write_text(json.dumps({self.job["id"]: audit}))
        with mock.patch.object(submit.subprocess, "check_output") as query:
            status = submit.source_status(self.share, self.share, json.dumps(self.fingerprint))
        query.assert_not_called()
        self.assertEqual(status["pages"][self.doc]["status"], "source_conflict")
        self.assertEqual(status["published_workunits"], 0)

    def test_present_null_original_binding_never_falls_back_to_recovered_evidence(self):
        recovery.install(self.share, [self.proof()])
        audit = {**self.audit, "source_bindings": {self.doc: None}}
        (self.campaign / "audit.json").write_text(json.dumps({self.job["id"]: audit}))
        with mock.patch.object(submit.subprocess, "check_output") as query:
            status = submit.source_status(self.share, self.share, json.dumps(self.fingerprint))
        query.assert_not_called()
        self.assertEqual(status["pages"][self.doc]["status"], "source_provenance_unavailable")
        self.assertNotIn("provenance", status["pages"][self.doc])
        self.assertEqual(status["published_workunits"], 0)

    def test_empty_or_mismatched_submission_marker_blocks_install_and_receipt_use(self):
        proof = self.proof()
        marker = self.unit / "submitted"
        for value in ("", "usnm_qwen_" + "b" * 64):
            marker.write_text(value)
            with self.assertRaisesRegex(ValueError, "submitted marker"):
                recovery.install(self.share, [proof])
        marker.write_text("usnm_qwen_" + self.job["id"])
        recovery.install(self.share, [proof])
        marker.write_text("")
        with self.assertRaisesRegex(ValueError, "submitted marker"):
            submit.source_status(self.share, self.share, json.dumps(self.fingerprint))

    def test_conflicting_other_binding_keeps_precedence_when_recovered_job_history_is_missing(self):
        recovery.install(self.share, [self.proof()])
        other = self.unit.parent / "second-job"
        other.mkdir()
        job = {**self.job, "id": "b" * 64}
        raw = json.dumps(job).encode()
        (other / "qwen.json").write_bytes(raw)
        (other / "submitted").write_text("usnm_qwen_" + job["id"])
        audit = {**self.audit, "workunit_id": 43, "job_sha256": hashlib.sha256(raw).hexdigest(),
                 "source_bindings": {self.doc: {
                     "schema": 1, "source": {**self.fingerprint, "batch_version": 2}, "jpeg": self.input}}}
        (self.campaign / "audit.json").write_text(json.dumps({self.job["id"]: self.audit, job["id"]: audit}))
        with mock.patch.object(submit.subprocess, "check_output", return_value="[]"):
            status = submit.source_status(self.share, self.share, json.dumps(self.fingerprint))
        self.assertEqual(status["pages"][self.doc]["status"], "source_conflict")
        self.assertEqual(status["source_conflict_pages"], 1)
        self.assertEqual(status["history_unavailable_workunits"], 1)

    def test_standalone_existing_inventory_deployment_does_not_require_recovery_module(self):
        isolated = self.share / "isolated"
        isolated.mkdir()
        (isolated / "qwen_submit.py").write_bytes(Path(submit.__file__).read_bytes())
        program = (
            "import json,sys; from pathlib import Path; from types import SimpleNamespace; "
            "from unittest.mock import patch; sys.path.insert(0,sys.argv[1]); import qwen_submit as q; "
            "root=Path(sys.argv[1]); "
            "counts=patch.object(q.subprocess,'check_output',return_value='{}'); "
            "disk=patch.object(q.shutil,'disk_usage',return_value=SimpleNamespace(free=100)); "
            "counts.start(); disk.start(); print(json.dumps(q.inventory(root,root))); "
            "assert 'qwen_provenance' not in sys.modules"
        )
        result = subprocess.run([sys.executable, "-B", "-c", program, str(isolated)],
                                capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), {"documents": [], "free_bytes": 100})


if __name__ == "__main__":
    unittest.main()
