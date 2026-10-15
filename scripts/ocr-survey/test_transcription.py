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
                         "source_url": "https://example.org/synthetic", "source_attribution": "Synthetic fixture",
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

    def selection(self, root):
        src = self.source(root)
        cache = root / "cache"
        cache.mkdir()
        src = src.rename(cache / "public")
        rows = t.read_jsonl(src / "crops.jsonl")
        meta = t.read_jsonl(src / "manifest.jsonl")
        spec = {"v": 1, "evaluation_only": True,
                "engine": {**{k: "sha256:" + "a" * 64 for k in ("image", "llama", "weights")},
                           "models": [{"name": "synthetic-model"}]},
                "included": [
                    {"crop_id": f"audited-{i}", "source_dataset": "public",
                     "image_sha256": meta[i]["crop_sha256"],
                     "reference_sha256": t.digest(rows[i]["transcription"].encode()),
                     "provenance": {k: meta[i][k] for k in
                                    ("page_id", "source_url", "source_license", "source_attribution")}}
                    for i in range(2)]}
        path = root / "selection.json"
        path.write_text(json.dumps(spec))
        return src, cache, path, spec

    def test_audited_cache_preparation_is_idempotent_and_private(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _, cache, selection, _ = self.selection(root)
            out = root / "campaign"
            a = t.prepare_benchmark(selection, cache, out)
            (out / "units").mkdir()
            (out / "units" / "existing-work").write_text("must remain")
            b = t.prepare_benchmark(selection, cache, out)
            self.assertEqual(a, b)
            self.assertEqual(a["counts"], {"test": 2})
            self.assertEqual((out / "audit.json").read_bytes(), selection.read_bytes())
            self.assertTrue((out / "prepared.json").is_file())
            self.assertFalse(list(out.rglob("sft.json")))
            self.assertEqual((out / "units" / "existing-work").read_text(), "must remain")
            self.assertTrue((out / "test/LICENSES/public/SOURCE.json").exists())

    def test_audited_cache_rejects_missing_duplicate_or_changed_inputs(self):
        for mode, message in (
            ("missing", "missing from cache"), ("duplicate", "duplicate cached image"),
            ("reference", "reference checksum"), ("pixels", "image checksum"),
            ("provenance", "provenance differs"), ("escape", "escapes dataset"),
            ("nd", "NoDerivatives"),
        ):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                src, cache, selection, spec = self.selection(root)
                rows = t.read_jsonl(src / "crops.jsonl")
                metadata = t.read_jsonl(src / "manifest.jsonl")
                if mode == "missing":
                    metadata = metadata[1:]
                elif mode == "duplicate":
                    metadata.append(metadata[0])
                elif mode == "reference":
                    rows[0]["transcription"] = "different human reference"
                elif mode == "pixels":
                    (src / "0.jpg").write_bytes(b"changed")
                elif mode == "provenance":
                    metadata[0]["source_url"] = "https://example.org/other"
                elif mode == "escape":
                    (src / "0.jpg").unlink()
                    (root / "outside.jpg").write_bytes(b"outside")
                    (src / "0.jpg").symlink_to(root / "outside.jpg")
                else:
                    metadata[0]["source_license"] = "CC-BY-ND-4.0"
                    spec["included"][0]["provenance"]["source_license"] = "CC-BY-ND-4.0"
                    selection.write_text(json.dumps(spec))
                write_jsonl(src / "crops.jsonl", rows)
                write_jsonl(src / "manifest.jsonl", metadata)
                with self.assertRaisesRegex(ValueError, message):
                    t.prepare_benchmark(selection, cache, root / "out")
                self.assertFalse((root / "out").exists())
                self.assertFalse((root / ".out.prepare.lock").exists())

    def test_audited_preparation_rejects_selection_and_destination_drift(self):
        for mode, message in (
            ("selection", "differs from reviewed"), ("image", "checksum differs"),
            ("partial", "differs from reviewed"), ("lock", "another preparation"),
        ):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                _, cache, selection, spec = self.selection(root)
                out = root / "out"
                t.prepare_benchmark(selection, cache, out)
                if mode == "selection":
                    spec["review_note"] = "changed"
                    selection.write_text(json.dumps(spec))
                elif mode == "image":
                    next((out / "test/images").iterdir()).write_bytes(b"changed")
                elif mode == "partial":
                    (out / "prepared.json").unlink()
                else:
                    (root / ".out.prepare.lock").mkdir()
                with self.assertRaisesRegex(ValueError, message):
                    t.prepare_benchmark(selection, cache, out)

    def test_audited_preparation_rejects_invalid_selection(self):
        for mode in ("training", "duplicate-id", "duplicate-image", "mutable-engine", "source-path",
                     "empty", "missing-provenance", "two-models"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                _, cache, selection, spec = self.selection(root)
                if mode == "training":
                    spec["evaluation_only"] = False
                elif mode == "duplicate-id":
                    spec["included"][1]["crop_id"] = spec["included"][0]["crop_id"]
                elif mode == "duplicate-image":
                    spec["included"][1]["image_sha256"] = spec["included"][0]["image_sha256"]
                elif mode == "mutable-engine":
                    spec["engine"]["image"] = "latest"
                elif mode == "source-path":
                    spec["included"][0]["source_dataset"] = "../source"
                elif mode == "empty":
                    spec["included"] = []
                elif mode == "missing-provenance":
                    spec["included"][0]["provenance"].pop("page_id")
                else:
                    spec["engine"]["models"] *= 2
                selection.write_text(json.dumps(spec))
                with self.assertRaises(ValueError):
                    t.prepare_benchmark(selection, cache, root / "out")
                self.assertFalse((root / "out").exists())


if __name__ == "__main__":
    unittest.main()
