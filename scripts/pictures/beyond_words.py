#!/usr/bin/env python3
"""Score retained Newspaper Navigator boxes against the pinned, verified Beyond Words holdout.

Reads local metadata and curated parts only. It never fetches images or trains a model.
"""
import argparse
import collections
import datetime as dt
import hashlib
import json
import math
from pathlib import Path
import re
from urllib.parse import urlsplit

REVISION = "6c7f5fb3c60f02d9fe925cfc14aa7008f6c89099"
ANNOTATIONS = {
    "train_80_percent.json": "9d5a53a043b00b843da5fd012ed4bbf18e82c60d70a1732e4b32b3648ac4ebd9",
    "val_20_percent.json": "247c489f1880908bde8d8251e7a5ce99f40c5f24694cfdf21840da2663825c7f",
}
COCO_BLOBS = {
    "train_80_percent.json": "8bc0e0d439428f168c5643b91b0e25efb4cc6453",
    "val_20_percent.json": "57dc90cad99ed885732ed3dcb3283224e6ea5344",
}
KINDS = {"photo": 0, "illustration": 1, "map": 2, "comic": 3, "cartoon": 4}
CUTOFF = dt.date(2019, 12, 1)
NN_REVISION = "e2e585824cdd28d52755c481dca69ed0bfa992a1"
NN_PROVENANCE_SHA256 = "2caffb480c70a8b21d77cffafc3366620e25adc0be009bd9233611c21ebc625c"
NN_PROVENANCE = Path(__file__).with_name("newspaper-navigator-baseline.lock.json")
MAX_METADATA_BYTES = 8 * 1024 * 1024


def require(condition, message):
    if not condition:
        raise ValueError(message)


def scan_key(url):
    parsed = urlsplit(url)
    require(parsed.scheme in ("https", "http"), f"Not a scan URL: {url}")
    prefix, relative = parsed.path.rsplit("/data/", 1)
    parts = relative.split("/")
    require(len(parts) >= 4 and re.fullmatch(r"\d{4,8}\.jpg", parts[-1]),
            f"Unrecognized scan path: {url}")
    issue = parts[-2]
    require(re.fullmatch(r"\d{10}", issue), f"Unrecognized issue directory: {url}")
    date = dt.datetime.strptime(issue[:8], "%Y%m%d").date()
    edition = int(issue[8:])
    require(edition > 0, f"Invalid edition: {url}")
    return (prefix.rsplit("/", 1)[1], parts[0].lower(), str(date),
            edition, "/".join(parts[1:-2]), parts[-1][:-4])


def clipped(box):
    require(box is not None and len(box) == 4 and all(math.isfinite(v) for v in box),
            f"Invalid box: {box}")
    result = tuple(min(1.0, max(0.0, value)) for value in box)
    require(result[2] > result[0] and result[3] > result[1], f"Empty clipped box: {box}")
    return result


def coco_box(box, width, height):
    x, y, w, h = box
    require(width > 0 and height > 0 and w > 0 and h > 0, "Invalid COCO box or image size")
    return clipped((x / width, y / height, (x + w) / width, (y + h) / height))


def iou(a, b):
    overlap = max(0, min(a[2], b[2]) - max(a[0], b[0])) * max(0, min(a[3], b[3]) - max(a[1], b[1]))
    area_a = (a[2] - a[0]) * (a[3] - a[1])
    area_b = (b[2] - b[0]) * (b[3] - b[1])
    return overlap / (area_a + area_b - overlap)


def verified_holdout(train, validation):
    require(any(item["name"] == "CC0 1.0" for item in validation["licenses"]), "Missing CC0 licence")
    expected = ["Photograph", "Illustration", "Map", "Comics/Cartoon", "Editorial Cartoon"]
    categories = {item["id"]: item["name"] for item in validation["categories"]}
    require([categories.get(n) for n in range(5)] == expected, "Unexpected verified classes")
    train_ids = {image["id"] for image in train["images"]}
    require(not train_ids.intersection(image["id"] for image in validation["images"]),
            "Training and validation image ids overlap")
    train_scans = {scan_key(image["url"]) for image in train["images"]}
    images, gold = {}, {}
    excluded = collections.Counter()
    for image in validation["images"]:
        if dt.datetime.strptime(image["date_captured"], "%m/%d/%Y").date() > CUTOFF:
            excluded["post_verification_cutoff_images"] += 1
            continue
        key = scan_key(image["url"])
        if key in train_scans:
            excluded["training_overlap_images"] += 1
            continue
        require(image["id"] not in images and key not in gold, "Duplicate validation image or scan")
        images[image["id"]] = (image, key)
        gold[key] = []
    for annotation in validation["annotations"]:
        if annotation["category_id"] not in range(5):
            excluded["unverified_class_annotations"] += 1
            continue
        if annotation["image_id"] not in images:
            excluded["annotations_on_excluded_images"] += 1
            continue
        if annotation.get("iscrowd", 0):
            excluded["crowd_annotations"] += 1
            continue
        image, key = images[annotation["image_id"]]
        gold[key].append((annotation["category_id"],
                          coco_box(annotation["bbox"], image["width"], image["height"])))
    require(gold and any(gold.values()), "No verified holdout boxes")
    return gold, dict(excluded)


def agreement(gold, predictions, typed):
    matched = total = predicted = 0
    best_sum = 0.0
    for key, boxes in gold.items():
        candidates = predictions.get(key, [])
        total += len(boxes)
        predicted += len(candidates)
        for kind, box in boxes:
            best_sum += max((iou(box, pred) for pk, pred, _ in candidates if not typed or kind == pk),
                            default=0.0)
        used = set()
        for pk, pred, _ in sorted(candidates, key=lambda row: -row[2]):
            score, index = max(
                ((iou(box, pred), n) for n, (kind, box) in enumerate(boxes)
                 if n not in used and (not typed or kind == pk)), default=(0.0, None))
            if score >= 0.5:
                used.add(index)
                matched += 1
    require(total > 0, "No verified boxes to score")
    return {
        "verified_boxes": total, "predictions_at_fixed_threshold": predicted,
        "matched_iou_0_5": matched, "unmatched_verified_boxes": total - matched,
        "unmatched_predictions": predicted - matched, "mean_best_iou": best_sum / total,
        "matched_annotation_precision": matched / predicted if predicted else None,
        "verified_annotation_recall": matched / total,
    }


def load_annotations(root):
    documents = {}
    for name, expected in ANNOTATIONS.items():
        with (root / name).open("rb") as stream:
            body = stream.read(MAX_METADATA_BYTES + 1)
        require(hashlib.sha256(body).hexdigest() == expected, f"Annotation SHA256 mismatch: {name}")
        documents[name] = json.loads(body)
    return documents


def part_bytes(curated, part):
    path = (curated / part["path"]).resolve()
    require(path.is_relative_to(curated), f"Part escapes curated root: {part['path']}")
    require(isinstance(part["bytes"], int) and part["bytes"] > 0, f"Invalid part size: {path}")
    with path.open("rb") as stream:
        body = stream.read(part["bytes"] + 1)
    require(len(body) == part["bytes"] and hashlib.sha256(body).hexdigest() == part["sha256"],
            f"Part length/SHA256 mismatch: {path}")
    return body


def evaluate(args):
    documents = load_annotations(args.annotations)
    gold, excluded = verified_holdout(documents["train_80_percent.json"], documents["val_20_percent.json"])
    provenance_bytes = args.provenance.read_bytes()
    require(hashlib.sha256(provenance_bytes).hexdigest() == NN_PROVENANCE_SHA256,
            "Picture provenance SHA256 mismatch")
    provenance = json.loads(provenance_bytes)
    require(provenance["schema"] == 1 and provenance["source"] == "biglam/newspaper-navigator"
            and provenance["license"] == "cc0-1.0" and provenance["revision"] == NN_REVISION
            and set(provenance["kinds"]) == set(KINDS),
            "Not the supported finished Newspaper Navigator source")
    import pyarrow as pa
    import pyarrow.parquet as pq

    pa.set_cpu_count(1)
    curated = args.curated.resolve()
    predictions = collections.defaultdict(list)
    hashes, seen_pages = {}, set()
    scanned = 0
    for kind in sorted(provenance["kinds"]):
        kind_rows = 0
        for part in provenance["kinds"][kind]["parts"]:
            require(part["path"] not in hashes, f"Repeated part: {part['path']}")
            body = part_bytes(curated, part)
            hashes[part["path"]] = part["sha256"]
            table = pq.read_table(pa.BufferReader(body), columns=[
                "kind", "batch", "lccn", "date", "edition", "reel", "frame", "box", "score",
            ])
            require(table.num_rows == part["rows"], f"Part row count mismatch: {part['path']}")
            kind_rows += table.num_rows
            for batch in table.to_batches(max_chunksize=8192):
                for row in batch.to_pylist():
                    key = (row["batch"], row["lccn"], str(row["date"]),
                           row["edition"], row["reel"], row["frame"])
                    if key not in gold:
                        continue
                    seen_pages.add(key)
                    score = row["score"]
                    require(score is not None and math.isfinite(score) and 0 <= score <= 1,
                            f"Invalid score in {part['path']}")
                    require(row["kind"] == kind, f"Wrong kind in {part['path']}")
                    if score >= 0.5:
                        predictions[key].append((KINDS[row["kind"]], clipped(row["box"]), score))
        require(kind_rows == provenance["kinds"][kind]["rows"], f"Kind row count mismatch: {kind}")
        scanned += kind_rows
    require(scanned == provenance["rows"], "Total picture row count mismatch")
    require(args.provenance.read_bytes() == provenance_bytes, "Provenance changed during evaluation")
    return {
        "source_revision": REVISION, "canonical_coco_git_blobs": COCO_BLOBS,
        "annotation_sha256": ANNOTATIONS, "cutoff_inclusive": str(CUTOFF),
        "train_images": len(documents["train_80_percent.json"]["images"]),
        "validation_images": len(documents["val_20_percent.json"]["images"]),
        "verified_holdout_images": len(gold), "excluded": excluded,
        "holdout_scans_with_any_retained_prediction": len(seen_pages),
        "holdout_scans_without_retained_predictions_coverage_unknown": len(gold) - len(seen_pages),
        "newspaper_navigator_revision": NN_REVISION, "provenance_sha256": NN_PROVENANCE_SHA256,
        "verified_picture_part_sha256": hashes, "picture_rows_scanned": scanned,
        "threshold_fixed_before_evaluation": 0.5,
        "class_aware": agreement(gold, predictions, True),
        "geometry_only": agreement(gold, predictions, False),
        "matching": "descending confidence, one-to-one, IoU>=0.5; coordinates clipped to page",
        "limits": [
            "Annotation agreement, not AP or evidence that every unmatched prediction is false.",
            "No retained prediction does not establish absent dataset/page coverage.",
            "No engine layout/text-loss evaluation, curated page-sequence join or UI acceptance.",
        ],
        "image_or_loc_requests": 0, "model_training_or_threshold_tuning": False,
        "production_changes": False,
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--curated", type=Path, required=True, help="Local curated store root")
    parser.add_argument("--provenance", type=Path, default=NN_PROVENANCE,
                        help="Exact independently verified provenance (defaults to the bundled source lock)")
    parser.add_argument("--annotations", type=Path, required=True, help="Directory holding the two pinned COCO JSONs")
    parser.add_argument("--out", type=Path, required=True, help="Create-only result JSON")
    args = parser.parse_args(argv)
    require(not args.out.exists(), f"Output already exists: {args.out}")
    result = evaluate(args)
    with args.out.open("x") as stream:
        stream.write(json.dumps(result, indent=2, sort_keys=True) + "\n")
    print(json.dumps({key: value for key, value in result.items()
                      if key != "verified_picture_part_sha256"}))


if __name__ == "__main__":
    main()
