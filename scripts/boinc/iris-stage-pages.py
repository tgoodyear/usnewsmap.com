#!/usr/bin/env python3
"""Stage 20,000 distinct cached source pages after inspected Iris pilot acceptance; never dispatch."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil

COUNT = 20000
IMAGE = re.compile(r"ghcr\.io/tgoodyear/usnm-boinc-worker@sha256:[0-9a-f]{64}\Z")
DOC_ID = re.compile(r"([A-Za-z0-9]+)_(\d{4}-\d{2}-\d{2})_ed-(\d+)_seq-(\d+)\Z")


def stage(cache, index, acceptance_path, out, image):
    acceptance = json.loads(acceptance_path.read_text())
    if (not IMAGE.fullmatch(image) or acceptance.get("worker_image") != image
            or acceptance.get("host_id") != 9 or acceptance.get("inspected") is not True
            or any(acceptance.get(key) != 100 for key in
                   ("jobs", "ok", "boinc_records", "boinc_terminal", "boinc_valid"))
            or any(acceptance.get(key) != 0 for key in ("errors", "pending"))):
        raise ValueError("inspected 100-job terminal-valid pilot acceptance for this image is required")
    cache = cache.resolve(strict=True)
    out = out.resolve()
    partial = out.with_name(out.name + ".staging")
    if out.exists() or partial.exists():
        raise FileExistsError("campaign or incomplete staging exists; inspect rather than overwrite")
    if out.is_relative_to(cache):
        raise ValueError("staging must not be inside the fetcher's keep tree")
    selected, seen = [], set()
    with index.open() as source:
        for line in source:
            row = json.loads(line)
            match = DOC_ID.fullmatch(row["doc_id"])
            if not match or match[2][:4] > "1922" or row["doc_id"] in seen:
                continue
            if not re.fullmatch(r"[A-Za-z0-9_-]+", row["batch"]):
                raise ValueError("invalid source batch")
            path = cache / row["batch"] / match[1] / match[2] / match[3] / (match[4] + ".jpg")
            if not path.is_file():
                continue
            if not path.resolve().is_relative_to(cache):
                raise ValueError("source image outside cache")
            sidecar_path = path.with_suffix(".jpg.json")
            if not sidecar_path.resolve().is_relative_to(cache):
                raise ValueError("source sidecar outside cache")
            sidecar = json.loads(sidecar_path.read_text())
            if (path.stat().st_size != row["bytes"] or row["resolution"] != "full"
                    or not re.fullmatch(r"[0-9a-f]{64}", row["sha256"])
                    or not row["loc_url"].startswith("https://tile.loc.gov/")
                    or any(sidecar[key] != row[key] for key in
                           ("doc_id", "batch", "bytes", "sha256", "loc_url", "resolution"))):
                raise ValueError("source metadata differs from the full-resolution cache index")
            selected.append((path, row))
            seen.add(row["doc_id"])
            if len(selected) == COUNT:
                break
    if len(selected) != COUNT:
        raise ValueError(f"need {COUNT} distinct cached pre-1923 pages; found {len(selected)}")
    total = sum(row["bytes"] for _, row in selected)
    out.parent.mkdir(parents=True, exist_ok=True)
    if shutil.disk_usage(out.parent).free < total + 1024**3:
        raise ValueError("insufficient staging disk space")
    partial.mkdir()
    (partial / "pages").mkdir()
    shutil.copyfile(acceptance_path, partial / "acceptance.json")
    with (partial / "pages.jsonl").open("x") as manifest:
        for number, (path, row) in enumerate(selected, 1):
            relative = "pages/" + row["doc_id"] + ".jpg"
            digest, copied = hashlib.sha256(), 0
            with path.open("rb") as source, (partial / relative).open("xb") as target:
                while block := source.read(4 * 1024 * 1024):
                    digest.update(block)
                    target.write(block)
                    copied += len(block)
                target.flush()
                os.fsync(target.fileno())
            if copied != row["bytes"] or digest.hexdigest() != row["sha256"]:
                raise ValueError("source changed or checksum differs; incomplete staging retained")
            manifest.write(json.dumps({
                "doc_id": row["doc_id"], "batch": row["batch"], "image": relative,
                "bytes": copied, "sha256": digest.hexdigest(), "source_url": row["loc_url"],
                "source_resolution": "full", "worker_image": image,
                "source_license": "Public-domain US newspaper published before 1923",
                "source_attribution": "Library of Congress, Chronicling America; contributing institution identified by source batch",
                "page_policy": "layout-aware bounded line blocks; preserve source pixels; no whole-page resize",
                "max_new_tokens_per_block": 512,
            }, sort_keys=True) + "\n")
            if number % 2000 == 0:
                print(json.dumps({"staged_pages": number}), flush=True)
        manifest.flush()
        os.fsync(manifest.fileno())
    summary = {
        "v": 1, "pages": COUNT, "distinct_page_ids": len(seen), "source_bytes": total,
        "worker_image": image, "stage_only": True, "dispatch_enabled": False,
        "executable_workunits": 0,
        "host_policy": "Intel OpenVINO only; NVIDIA and AMD excluded",
        "manifest_sha256": hashlib.sha256((partial / "pages.jsonl").read_bytes()).hexdigest(),
        "readiness": "source-pages-staged; full-page tiling acceptance required before dispatch",
        "required_before_dispatch": [
            "Validate layout-aware line-block segmentation and page reconstruction on full-page canaries",
            "Measure full-page memory, output truncation and end-to-end runtime",
            "Bind each tile checksum and page/box identity to an immutable job manifest",
            "Obtain explicit bulk-dispatch authorization",
        ],
    }
    (partial / "campaign.json").write_text(json.dumps(summary, indent=2) + "\n")
    (partial / "DISPATCH_DISABLED").write_text("Source-page staging only. No BOINC workunits created.\n")
    partial.rename(out)
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("cache", "index", "acceptance", "out"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--worker-image", required=True)
    args = parser.parse_args()
    print(json.dumps(stage(args.cache, args.index, args.acceptance, args.out, args.worker_image)))


if __name__ == "__main__":
    main()
