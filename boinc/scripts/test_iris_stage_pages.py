import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("stage_pages", Path(__file__).with_name("iris-stage-pages.py"))
pages = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pages)


class StagePagesTest(unittest.TestCase):
    def fixture(self, root):
        image = "ghcr.io/tgoodyear/usnm-boinc-worker@sha256:" + "a" * 64
        acceptance = {"worker_image": image, "host_id": 9, "inspected": True,
                      "jobs": 100, "ok": 100, "boinc_records": 100, "boinc_terminal": 100,
                      "boinc_valid": 100, "errors": 0, "pending": 0}
        proof = root / "acceptance.json"
        proof.write_text(json.dumps(acceptance))
        cache = root / "keep"
        rows = []
        for number in (1, 2):
            data = f"source-{number}".encode()
            path = cache / f"batch/sn1/1900-01-01/1/{number}.jpg"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            row = {"doc_id": f"sn1_1900-01-01_ed-1_seq-{number}", "batch": "batch",
                   "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest(),
                   "resolution": "full", "loc_url": f"https://tile.loc.gov/{number}"}
            path.with_suffix(".jpg.json").write_text(json.dumps(row))
            rows.append(row)
        index = root / "index.jsonl"
        index.write_text("\n".join(map(json.dumps, [rows[0], rows[0], rows[1]])) + "\n")
        return cache, index, proof, root / "campaign", image

    def test_exact_full_page_snapshot_preserves_sources_and_disables_dispatch(self):
        self.assertEqual(pages.COUNT, 20000)
        with tempfile.TemporaryDirectory() as directory, patch.object(pages, "COUNT", 2):
            args = self.fixture(Path(directory))
            result = pages.stage(*args)
            self.assertEqual(result["pages"], 2)
            self.assertEqual(result["distinct_page_ids"], 2)
            self.assertEqual(result["distinct_page_sha256"], 2)
            self.assertFalse(result["dispatch_enabled"])
            self.assertEqual(result["executable_workunits"], 0)
            self.assertTrue((args[3] / "DISPATCH_DISABLED").is_file())
            records = [json.loads(line) for line in (args[3] / "pages.jsonl").read_text().splitlines()]
            self.assertEqual(len(records), 2)
            source = args[0] / "batch/sn1/1900-01-01/1/1.jpg"
            destination = args[3] / records[0]["image"]
            self.assertEqual(destination.read_bytes(), source.read_bytes())
            self.assertNotEqual(destination.stat().st_ino, source.stat().st_ino)
            with self.assertRaises(FileExistsError):
                pages.stage(*args)

    def test_gate_rejects_incomplete_uninspected_or_different_image_acceptance(self):
        for field, value in (("ok", 99), ("boinc_valid", 99), ("errors", 1),
                             ("pending", 1), ("inspected", False), ("host_id", 8),
                             ("worker_image", "other")):
            with self.subTest(field=field), tempfile.TemporaryDirectory() as directory:
                args = self.fixture(Path(directory))
                proof = json.loads(args[2].read_text())
                proof[field] = value
                args[2].write_text(json.dumps(proof))
                with self.assertRaisesRegex(ValueError, "pilot acceptance"):
                    pages.stage(*args)
                self.assertFalse(args[3].exists())

    def test_missing_pages_and_source_checksum_failure_cannot_publish_campaign(self):
        with tempfile.TemporaryDirectory() as directory:
            args = self.fixture(Path(directory))
            with self.assertRaisesRegex(ValueError, "20000 distinct"):
                pages.stage(*args)
            self.assertFalse(args[3].exists())
        with tempfile.TemporaryDirectory() as directory, patch.object(pages, "COUNT", 2):
            args = self.fixture(Path(directory))
            (args[0] / "batch/sn1/1900-01-01/1/1.jpg").write_bytes(b"corrupt!")
            with self.assertRaisesRegex(ValueError, "checksum"):
                pages.stage(*args)
            self.assertFalse(args[3].exists())
            self.assertTrue(args[3].with_name("campaign.staging").is_dir())

    def test_staging_cannot_write_through_a_symlink_into_fetcher_keep_tree(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            args = self.fixture(root)
            (root / "alias").symlink_to(args[0], target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "fetcher's keep tree"):
                pages.stage(*args[:3], root / "alias/new-campaign", args[4])
            self.assertFalse((args[0] / "new-campaign").exists())

    def test_conflicting_sidecar_resolution_cannot_publish_campaign(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(pages, "COUNT", 2):
            args = self.fixture(Path(directory))
            sidecar = args[0] / "batch/sn1/1900-01-01/1/1.jpg.json"
            metadata = json.loads(sidecar.read_text())
            metadata["resolution"] = "thumbnail"
            sidecar.write_text(json.dumps(metadata))
            with self.assertRaisesRegex(ValueError, "metadata differs"):
                pages.stage(*args)
            self.assertFalse(args[3].exists())
            self.assertFalse(args[3].with_name("campaign.staging").exists())

    def test_latest_jpeg_index_entry_is_used_after_source_refresh(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(pages, "COUNT", 2):
            args = self.fixture(Path(directory))
            records = [json.loads(line) for line in args[1].read_text().splitlines()]
            records[0] = dict(records[0], sha256="0" * 64, bytes=999)
            records.append(dict(records[1], format="jp2", sha256="b" * 64))
            args[1].write_text("".join(json.dumps(row) + "\n" for row in records))
            self.assertEqual(pages.stage(*args)["pages"], 2)

    def test_identical_pixels_under_different_page_ids_do_not_count_twice(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(pages, "COUNT", 2):
            args = self.fixture(Path(directory))
            records = [json.loads(line) for line in args[1].read_text().splitlines()]
            records[-1]["sha256"] = records[0]["sha256"]
            source = args[0] / "batch/sn1/1900-01-01/1"
            (source / "2.jpg").write_bytes((source / "1.jpg").read_bytes())
            (source / "2.jpg.json").write_text(json.dumps(records[-1]))
            args[1].write_text("".join(json.dumps(row) + "\n" for row in records))
            with self.assertRaisesRegex(ValueError, "found 1"):
                pages.stage(*args)
            self.assertFalse(args[3].exists())


if __name__ == "__main__":
    unittest.main()
