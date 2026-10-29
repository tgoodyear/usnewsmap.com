"""Verified holdout, geometry, matching and immutable-input gates for #1102."""
import copy
import hashlib
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
            args = SimpleNamespace(annotations=Path(directory), provenance=path,
                                   provenance_sha256="0" * 64)
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


if __name__ == "__main__":
    unittest.main()
