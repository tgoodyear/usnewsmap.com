"""Synthetic shadow observations; no BOINC/database/network/image access."""
import contextlib
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location("qwen_shadow", Path(__file__).with_name("qwen_shadow.py"))
shadow = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(shadow)


class ShadowTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        (self.root / "units").mkdir()
        self.audit = {}
        self.save_audit()

    def save_audit(self):
        (self.root / "audit.json").write_text(json.dumps(self.audit))

    def job(self, name, submitted=True, workunit_id=1, doc_id="sn1_1900-01-02_ed-1_seq-3"):
        unit = self.root / "units" / name
        unit.mkdir()
        job_id = hashlib.sha256(name.encode()).hexdigest()
        job = {"v": 1, "id": job_id, "doc_id": doc_id, "input": {"sha256": "a" * 64}}
        raw = json.dumps(job).encode()
        (unit / "qwen.json").write_bytes(raw)
        if submitted:
            (unit / "submitted").write_text(f"usnm_qwen_{job_id}")
            self.audit[job_id] = {"job_sha256": hashlib.sha256(raw).hexdigest(),
                                  "workunit_id": workunit_id, "batch": 8,
                                  "min_quorum": 2, "target_nresults": 2}
            self.save_audit()
        return unit, job_id

    def test_idempotent_observation_distinguishes_jobs_documents_and_replicas(self):
        self.job("first")
        self.job("second", workunit_id=2)
        self.job("pending", submitted=False, doc_id="sn2_1900-01-02_ed-1_seq-3")
        no_crops = self.root / "units" / "empty"
        no_crops.mkdir()
        (no_crops / "no_crops").write_text("explicit source disposition")
        before = {p: p.read_bytes() for p in self.root.rglob("*") if p.is_file()}
        one = shadow.observe(self.root)
        self.assertEqual(one, shadow.observe(self.root))
        self.assertEqual(one["counts"], {"manifest_jobs": 3, "unique_document_ids": 2,
                                         "audited_workunits": 2, "expected_original_replicas": 4,
                                         "no_crops_dispositions": 1, "too_large_dispositions": 0})
        self.assertTrue(all(not row["source_bytes_verified"] for row in one["jobs"]))
        self.assertEqual(sum(row["state"] == "staged_unverified" for row in one["jobs"]), 1)
        self.assertEqual(before, {p: p.read_bytes() for p in self.root.rglob("*") if p.is_file()})
        sha = one.pop("observation_sha256")
        self.assertEqual(sha, hashlib.sha256(json.dumps(
            one, sort_keys=True, separators=(",", ":")).encode()).hexdigest())

    def test_changed_or_missing_audited_job_is_never_an_empty_success(self):
        unit, _ = self.job("first")
        job_path = unit / "qwen.json"
        original = job_path.read_bytes()
        job_path.write_bytes(original + b" ")
        with self.assertRaisesRegex(shadow.ObservationError, "audited manifest changed"):
            shadow.observe(self.root)
        job_path.unlink()
        (unit / "submitted").unlink()
        unit.rmdir()
        with self.assertRaisesRegex(shadow.ObservationError, "audited job missing"):
            shadow.observe(self.root)

    def test_uncertain_and_marker_receipt_mismatch_fail_without_stdout(self):
        unit, _ = self.job("first")
        for marker in ("submitting", "submitted"):
            with self.subTest(marker=marker):
                if marker == "submitting":
                    (unit / marker).write_text("uncertain")
                else:
                    (unit / "submitting").unlink()
                    (unit / marker).unlink()
                out, err = io.StringIO(), io.StringIO()
                with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                    code = shadow.main(["--root", str(self.root)])
                self.assertEqual(code, 1)
                self.assertEqual(out.getvalue(), "")
                self.assertIn("failed:", err.getvalue())

    def test_duplicate_job_and_workunit_identity_are_rejected(self):
        first, _ = self.job("first")
        second, _ = self.job("second", workunit_id=1)
        with self.assertRaisesRegex(shadow.ObservationError, "duplicate audited workunit"):
            shadow.observe(self.root)
        (second / "qwen.json").write_bytes((first / "qwen.json").read_bytes())
        with self.assertRaisesRegex(shadow.ObservationError, "duplicate staged job"):
            shadow.observe(self.root)

    def test_bundle_and_unexpected_quorum_are_rejected(self):
        unit, job_id = self.job("first")
        self.audit[job_id]["min_quorum"] = 1
        self.save_audit()
        with self.assertRaisesRegex(shadow.ObservationError, "routing/quorum"):
            shadow.observe(self.root)
        job = json.loads((unit / "qwen.json").read_bytes())
        job["pages"] = [{"doc_id": "not-supported"}]
        (unit / "qwen.json").write_text(json.dumps(job))
        with self.assertRaisesRegex(shadow.ObservationError, "bundle membership"):
            shadow.observe(self.root)

    def test_metadata_limits_duplicate_json_keys_and_symlinks_fail(self):
        unit, _ = self.job("first")
        with self.assertRaisesRegex(shadow.ObservationError, "max-units"):
            shadow.observe(self.root, 0)
        self.job("second", submitted=False)
        with self.assertRaisesRegex(shadow.ObservationError, "unit limit"):
            shadow.observe(self.root, 1)
        with mock.patch.object(shadow, "MAX_JOB_BYTES", 8):
            with self.assertRaisesRegex(shadow.ObservationError, "size limit"):
                shadow.observe(self.root)
        original = (unit / "qwen.json").read_bytes()
        (unit / "qwen.json").write_bytes(b'{"id":"one","id":"two"}')
        with self.assertRaisesRegex(shadow.ObservationError, "duplicate JSON"):
            shadow.observe(self.root)
        target = self.root / "outside.json"
        target.write_bytes(original)
        (unit / "qwen.json").unlink()
        (unit / "qwen.json").symlink_to(target)
        with self.assertRaisesRegex(shadow.ObservationError, "regular file"):
            shadow.observe(self.root)

    def test_concurrent_audit_update_discards_whole_observation(self):
        self.job("first")
        original = shadow.read_file
        reads = 0

        def changed(path, limit):
            nonlocal reads
            raw = original(path, limit)
            if path.name == "audit.json":
                reads += 1
                if reads == 2:
                    return raw + b" "
            return raw

        with mock.patch.object(shadow, "read_file", side_effect=changed):
            with self.assertRaisesRegex(shadow.ObservationError, "audit changed"):
                shadow.observe(self.root)

    def test_oversized_disposition_is_retained_not_counted_as_a_job(self):
        unit = self.root / "units" / "oversized"
        unit.mkdir()
        (unit / "too_large").write_text("source crop exceeds text bound; evidence retained")
        observed = shadow.observe(self.root)
        self.assertEqual(observed["counts"]["manifest_jobs"], 0)
        self.assertEqual(observed["counts"]["too_large_dispositions"], 1)
        self.assertEqual(observed["dispositions"][0]["state"], "too_large")
        self.assertFalse(observed["dispositions"][0]["evidence_verified"])
        (unit / "submitted").write_text("conflicting receipt")
        with self.assertRaisesRegex(shadow.ObservationError, "conflicting terminal"):
            shadow.observe(self.root)

    def test_partial_bundled_layout_is_not_silently_omitted(self):
        self.job("first")
        pages = self.root / "pages"
        pages.mkdir()
        (pages / "no_crops").write_text("new disposition")
        with self.assertRaisesRegex(shadow.ObservationError, "bundled campaign layout"):
            shadow.observe(self.root)

    def test_null_audit_is_not_an_absent_receipt(self):
        _, job_id = self.job("pending", submitted=False)
        self.audit[job_id] = None
        self.save_audit()
        with self.assertRaisesRegex(shadow.ObservationError, "receipt must be an object"):
            shadow.observe(self.root)

    def test_crossed_submission_marker_is_rejected(self):
        unit, _ = self.job("first")
        (unit / "submitted").write_text("usnm_qwen_" + "f" * 64)
        with self.assertRaisesRegex(shadow.ObservationError, "marker names another"):
            shadow.observe(self.root)


if __name__ == "__main__":
    unittest.main()
