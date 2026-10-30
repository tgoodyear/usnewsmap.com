"""Verified holdout, geometry, matching and immutable-input gates for #1102."""
import copy
import contextlib
import datetime as dt
import hashlib
import io
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import beyond_words as baseline  # noqa: E402


def image(number, frame="0001", captured="12/1/2019"):
    return {
        "id": number, "width": 100, "height": 200, "date_captured": captured,
        "url": f"https://example.test/xx_test_ver01/data/sn12345678/001/1918010101/{frame}.jpg",
    }


def annotation(image_id, category=0):
    return {"image_id": image_id, "category_id": category, "bbox": [10, 20, 30, 40]}


def validation(images, annotations):
    names = ["Photograph", "Illustration", "Map", "Comics/Cartoon", "Editorial Cartoon"]
    return {
        "images": images, "annotations": annotations, "licenses": [{"name": "CC0 1.0"}],
        "categories": [{"id": number, "name": name} for number, name in enumerate(names)],
    }


class VerifiedBaseline(unittest.TestCase):
    def test_bundled_provenance_retains_the_independently_verified_bytes(self):
        self.assertEqual(hashlib.sha256(baseline.NN_PROVENANCE.read_bytes()).hexdigest(),
                         baseline.NN_PROVENANCE_SHA256)

    def test_coco_xywh_and_prediction_xyxy_are_not_confused(self):
        self.assertEqual(baseline.coco_box([10, 20, 30, 40], 100, 200), (0.1, 0.1, 0.4, 0.3))
        self.assertAlmostEqual(baseline.iou((0, 0, 0.5, 0.5), (0.25, 0.25, 0.75, 0.75)), 1 / 7)
        self.assertEqual(baseline.clipped((-0.1, 0, 1.1, 1)), (0, 0, 1, 1))
        for box in ((0, 0, float("nan"), 1), (2, 0, 3, 1), (0, 0, 0, 1), None):
            with self.subTest(box=box), self.assertRaises(ValueError):
                baseline.clipped(box)

    def test_confidence_matching_is_one_to_one_and_class_aware(self):
        left, right = (0, 0, 0.4, 1), (0.6, 0, 1, 1)
        gold = {"page": [(0, left), (1, right)], "unknown": [(0, left)]}
        predictions = {"page": [(0, left, 0.8), (0, left, 0.9), (0, right, 0.7)]}
        typed = baseline.agreement(gold, predictions, True)
        geometry = baseline.agreement(gold, predictions, False)
        self.assertEqual((typed["matched_iou_0_5"], geometry["matched_iou_0_5"]), (1, 2))
        self.assertEqual(typed["unmatched_predictions"], 2)
        self.assertEqual(typed["unmatched_verified_boxes"], 2)
        self.assertAlmostEqual(geometry["verified_annotation_recall"], 2 / 3)
        empty = baseline.agreement(gold, {}, True)
        self.assertIsNone(empty["matched_annotation_precision"])
        self.assertEqual(empty["verified_annotation_recall"], 0)

    def test_cutoff_training_scan_overlap_and_unverified_classes_are_excluded(self):
        train = {"images": [image(1)]}
        holdout = validation(
            [image(2, "0002"), image(3), image(4, "0004", "12/2/2019")],
            [annotation(2), annotation(2, 5), annotation(3), annotation(4),
             {**annotation(2), "iscrowd": 1}],
        )
        gold, excluded = baseline.verified_holdout(train, holdout)
        self.assertEqual(list(gold), [baseline.scan_key(image(2, "0002")["url"])])
        self.assertEqual(len(next(iter(gold.values()))), 1)
        self.assertEqual(excluded, {
            "training_overlap_images": 1, "post_verification_cutoff_images": 1,
            "unverified_class_annotations": 1, "annotations_on_excluded_images": 2,
            "crowd_annotations": 1,
        })
        overlap = copy.deepcopy(holdout)
        overlap["images"][0]["id"] = 1
        with self.assertRaisesRegex(ValueError, "image ids overlap"):
            baseline.verified_holdout(train, overlap)

    def test_duplicate_validation_scans_and_wrong_verified_classes_are_refused(self):
        holdout = validation([image(2), image(3)], [annotation(2)])
        with self.assertRaisesRegex(ValueError, "Duplicate validation"):
            baseline.verified_holdout({"images": []}, holdout)
        holdout["categories"][0]["name"] = "Advertisement"
        with self.assertRaisesRegex(ValueError, "Unexpected verified classes"):
            baseline.verified_holdout({"images": []}, holdout)

    def test_annotation_changes_are_refused_before_scoring(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "train_80_percent.json").write_text('{"images":[]}')
            with self.assertRaisesRegex(ValueError, "Annotation SHA256 mismatch"):
                baseline.load_annotations(root)

    def test_provenance_changes_are_refused_before_reading_any_picture_parts(self):
        documents = {
            "train_80_percent.json": {"images": []},
            "val_20_percent.json": validation([image(1)], [annotation(1)]),
        }
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "provenance.json"
            path.write_text("{}")
            args = SimpleNamespace(annotations=Path(directory), provenance=path)
            with patch.object(baseline, "load_annotations", return_value=documents):
                with self.assertRaisesRegex(ValueError, "Picture provenance SHA256 mismatch"):
                    baseline.evaluate(args)

    def test_part_hash_and_root_are_checked_on_the_bytes_that_will_be_read(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            part = {"path": "part", "bytes": 6, "sha256": hashlib.sha256(b"before").hexdigest()}
            (root / "part").write_bytes(b"before")
            self.assertEqual(baseline.part_bytes(root, part), b"before")
            (root / "part").write_bytes(b"after!")
            with self.assertRaisesRegex(ValueError, "length/SHA256 mismatch"):
                baseline.part_bytes(root, part)
            with self.assertRaisesRegex(ValueError, "escapes curated"):
                baseline.part_bytes(root, {**part, "path": "../elsewhere"})

    def fixture(self, root, mismatch=None):
        import pyarrow as pa
        import pyarrow.parquet as pq

        documents = {
            "train_80_percent.json": {"images": []},
            "val_20_percent.json": validation(
                [image(1), image(2, "0010")],
                [
                    {**annotation(1), "bbox": [0, 0, 40, 200]},
                    {**annotation(1, 2), "bbox": [60, 0, 40, 200]},
                    {**annotation(2), "bbox": [0, 0, 40, 200]},
                ],
            ),
        }
        annotation_hashes = {}
        for name, document in documents.items():
            body = json.dumps(document).encode()
            (root / name).write_bytes(body)
            annotation_hashes[name] = hashlib.sha256(body).hexdigest()
        schema = pa.schema([
            ("kind", pa.string()), ("batch", pa.string()), ("lccn", pa.string()),
            ("date", pa.date32()), ("edition", pa.uint32()), ("reel", pa.string()),
            ("frame", pa.string()), ("box", pa.list_(pa.float64(), 4)), ("score", pa.float64()),
        ])
        base = {
            "batch": "xx_test_ver01", "lccn": "sn12345678", "date": dt.date(1918, 1, 1),
            "edition": 1, "reel": "001", "frame": "0001", "box": [0, 0, 0.4, 1],
            "score": 0.9,
        }
        rows = {
            "photo": [base, {**base, "frame": "0002"}, {**base, "reel": "002"}],
            "illustration": [{**base, "score": 0.8}],
            "map": [{**base, "box": [0.6, 0, 1, 1], "score": 0.5}],
            "comic": [{**base, "score": 0.49}],
            "cartoon": [{**base, "batch": "xx_test_ver02"}],
        }
        provenance = {
            "schema": 1, "source": "biglam/newspaper-navigator", "license": "cc0-1.0",
            "revision": baseline.NN_REVISION, "rows": 7, "kinds": {},
        }
        for kind, values in rows.items():
            path = root / f"{kind}.parquet"
            pq.write_table(pa.Table.from_pylist([{**row, "kind": kind} for row in values],
                                              schema=schema), path)
            body = path.read_bytes()
            part = {"path": path.name, "bytes": len(body), "sha256": hashlib.sha256(body).hexdigest(),
                    "rows": len(values)}
            provenance["kinds"][kind] = {"rows": len(values), "parts": [part]}
        if mismatch == "part":
            provenance["kinds"]["photo"]["parts"][0]["rows"] += 1
        elif mismatch == "kind":
            provenance["kinds"]["photo"]["rows"] += 1
        elif mismatch == "total":
            provenance["rows"] += 1
        body = json.dumps(provenance).encode()
        path = root / "provenance.json"
        path.write_bytes(body)
        return annotation_hashes, hashlib.sha256(body).hexdigest(), path

    def test_real_parquet_cli_preserves_scan_identity_threshold_and_unknown_coverage(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            hashes, provenance_hash, provenance = self.fixture(root)
            output = root / "result.json"
            with patch.object(baseline, "ANNOTATIONS", hashes), \
                    patch.object(baseline, "NN_PROVENANCE_SHA256", provenance_hash), \
                    contextlib.redirect_stdout(io.StringIO()):
                baseline.main(["--curated", str(root), "--annotations", str(root),
                               "--provenance", str(provenance), "--out", str(output)])
            result = json.loads(output.read_text())
            self.assertEqual(result["picture_rows_scanned"], 7)
            self.assertEqual(result["holdout_scans_with_any_retained_prediction"], 1)
            self.assertEqual(result["holdout_scans_without_retained_predictions_coverage_unknown"], 1)
            for mode in ("class_aware", "geometry_only"):
                self.assertEqual(result[mode]["verified_boxes"], 3)
                self.assertEqual(result[mode]["predictions_at_fixed_threshold"], 3)
                self.assertEqual(result[mode]["matched_iou_0_5"], 2)
                self.assertAlmostEqual(result[mode]["matched_annotation_precision"], 2 / 3)
                self.assertAlmostEqual(result[mode]["verified_annotation_recall"], 2 / 3)
                self.assertAlmostEqual(result[mode]["mean_best_iou"], 2 / 3)

    def test_real_parquet_count_mismatches_refuse_result_creation(self):
        for mismatch, message in (("part", "Part row count"), ("kind", "Kind row count"),
                                  ("total", "Total picture row count")):
            with self.subTest(mismatch=mismatch), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                hashes, provenance_hash, provenance = self.fixture(root, mismatch)
                output = root / "result.json"
                with patch.object(baseline, "ANNOTATIONS", hashes), \
                        patch.object(baseline, "NN_PROVENANCE_SHA256", provenance_hash):
                    with self.assertRaisesRegex(ValueError, message):
                        baseline.main(["--curated", str(root), "--annotations", str(root),
                                       "--provenance", str(provenance), "--out", str(output)])
                self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
