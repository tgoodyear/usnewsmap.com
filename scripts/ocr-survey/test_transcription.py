"""Synthetic-only tests for the transcription experiment exporter."""

import json
import tempfile
import unittest
from pathlib import Path

import transcription as t
from gold import write_jsonl


class TranscriptionTest(unittest.TestCase):
    def source(self, root):
        src = root / "source"
        src.mkdir()
        rows, meta = [], []
        for i in range(8):
            (src / f"{i}.jpg").write_bytes(f"synthetic pixels {i}".encode())
            rows.append({"crop_id": f"crop-{i}", "image": f"{i}.jpg", "lang": "eng", "group": "public",
                         "for_scoring": True, "transcription": f"SYNTHETIC NEWSPAPER {i}",
                         "transcriber": "fixture", "reviewer": "fixture-review"})
            meta.append({"crop_id": f"crop-{i}", "source_dataset": "public", "source_license": "CC-BY-4.0",
                         "title": f"title-{i // 2}", "page_id": f"page-{i}",
                         "crop_sha256": t.digest(f"synthetic pixels {i}".encode())})
        write_jsonl(src / "crops.jsonl", rows)
        write_jsonl(src / "manifest.jsonl", meta)
        (src / "LICENSES").mkdir()
        (src / "LICENSES" / "SOURCE.json").write_text('{"synthetic":true}')
        return src

    def test_title_disjoint_deterministic_sft_and_test_separation(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            src = self.source(root)
            a = t.prepare([f"public={src}"], root / "a", 542, False, ["public"])
            b = t.prepare([f"public={src}"], root / "b", 542, False, ["public"])
            self.assertEqual(a, b)
            self.assertEqual(a["counts"], {"train": 4, "validation": 2, "test": 2})
            self.assertFalse((root / "a/test/sft.json").exists())
            for split in a["counts"]:
                self.assertTrue((root / f"a/{split}/LICENSES/public/SOURCE.json").exists())
                self.assertEqual((root / f"a/{split}/crops.jsonl").read_bytes(),
                                 (root / f"b/{split}/crops.jsonl").read_bytes())
            with self.assertRaisesRegex(ValueError, "immutable"):
                t.prepare([f"public={src}"], root / "a", 542, False, ["public"])

    def test_permission_nd_and_evaluation_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            src = self.source(root)
            with self.assertRaisesRegex(ValueError, "training needs"):
                t.prepare([f"public={src}"], root / "denied", 542, False, [])
            result = t.prepare([f"public={src}"], root / "eval", 542, True, [])
            self.assertEqual(result["counts"], {"test": 8})
            rows = t.read_jsonl(src / "manifest.jsonl")
            rows[0]["source_license"] = "CC-BY-NC-ND-4.0"
            write_jsonl(src / "manifest.jsonl", rows)
            with self.assertRaisesRegex(ValueError, "NoDerivatives"):
                t.prepare([f"public={src}"], root / "nd", 542, True, [])

    def test_license_prose_does_not_match_nd_inside_words(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            src = self.source(root)
            rows = t.read_jsonl(src / "manifest.jsonl")
            for row in rows:
                row["source_license"] = "CC BY-NC-SA 4.0; text and images are public domain"
            write_jsonl(src / "manifest.jsonl", rows)
            result = t.prepare([f"public={src}"], root / "out", 542, True, [])
            self.assertEqual(result["counts"], {"test": 8})

    def test_duplicate_text_across_titles_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            src = self.source(root)
            rows = t.read_jsonl(src / "crops.jsonl")
            rows[4]["transcription"] = rows[0]["transcription"]
            write_jsonl(src / "crops.jsonl", rows)
            with self.assertRaisesRegex(ValueError, "duplicate pixels or transcription"):
                t.prepare([f"public={src}"], root / "out", 542, False, ["public"])

    def test_prompt_matches_worker(self):
        contract = Path(__file__).resolve().parents[2] / "crates/usnm-ocr-contract/src/qwen.rs"
        self.assertIn(f'pub const PROMPT: &str = "{t.PROMPT}";', contract.read_text())

    def test_changed_pixels_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            src = self.source(root)
            (src / "0.jpg").write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "checksum differs"):
                t.prepare([f"public={src}"], root / "out", 542, True, [])

    def test_partial_page_identity_metadata_rejected(self):
        for field in ("title", "lccn"):
            with self.subTest(field=field), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                src = self.source(root)
                rows = t.read_jsonl(src / "manifest.jsonl")
                rows[1]["page_id"] = rows[0]["page_id"]
                if field == "title":
                    rows[1].pop("title")
                else:
                    rows[0]["lccn"] = "sn12345678"
                write_jsonl(src / "manifest.jsonl", rows)
                with self.assertRaisesRegex(ValueError, "inconsistent page identity"):
                    t.prepare([f"public={src}"], root / "out", 542, False, ["public"])


if __name__ == "__main__":
    unittest.main()
