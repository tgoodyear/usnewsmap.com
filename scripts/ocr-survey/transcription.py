#!/usr/bin/env python3
"""Prepare title-disjoint public gold data for central SFT and BOINC evaluation (#542)."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import sys
from pathlib import Path

from gold import read_jsonl, validate_rows, write_jsonl

PROMPT = (
    "Transcribe exactly the printed text in the image. Keep the original spelling, punctuation, "
    "line breaks and hyphenation. Do not correct or modernise anything, and do not add anything "
    "that is not printed. If a word is unreadable, write [?] for it. Output only the transcription."
)
TRAINING_LICENSES = {
    "CC0-1.0", "CC-BY-4.0", "CC-BY-4.0 (Zenodo record)",
    "CC0-1.0 (GitHub LICENSE file); CC-BY-4.0 (Zenodo record); images Public Domain Mark 1.0",
}


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def prepare(sources: list[str], out: Path, seed: int, evaluation_only: bool,
            allow_training: list[str]) -> dict:
    if out.exists():
        raise ValueError("--out must not exist; dataset splits are immutable")
    rows = []
    groups = set()
    ids = set()
    images = {}
    text_groups = {}
    source_records = []
    for spec in sources:
        name, sep, directory = spec.partition("=")
        if not sep or not re.fullmatch(r"[a-z0-9][a-z0-9-]*", name):
            raise ValueError("--source wants DATASET=DIRECTORY")
        base = Path(directory).resolve(strict=True)
        if not (base / "LICENSES").is_dir():
            raise ValueError(f"{name}: missing LICENSES provenance directory")
        crop_bytes = (base / "crops.jsonl").read_bytes()
        crops = read_jsonl(base / "crops.jsonl")
        manifest_rows = read_jsonl(base / "manifest.jsonl")
        metadata = {r["crop_id"]: r for r in manifest_rows}
        if len(metadata) != len(manifest_rows):
            raise ValueError(f"{name}: duplicate manifest crop")
        errors, _ = validate_rows(crops, require_review=True)
        if errors:
            raise ValueError(f"{name}: invalid references: {errors[:3]}")
        source_records.append({"dataset": name, "crops_sha256": digest(crop_bytes),
                               "manifest_sha256": digest((base / "manifest.jsonl").read_bytes())})
        for row in crops:
            cid = row["crop_id"]
            if cid in ids:
                raise ValueError(f"duplicate crop_id: {cid}")
            ids.add(cid)
            if row.get("for_scoring") is not True:
                raise ValueError(f"{cid}: not human ground truth eligible for scoring")
            meta = metadata[cid]
            if meta["source_dataset"] != name:
                raise ValueError(f"{cid}: source dataset mismatch")
            # ND images cannot be repackaged into this volunteer campaign.
            license_name = meta["source_license"]
            if re.search(r"(?:^|[-\s])ND(?:[-\s]|$)|NODERIVATIVES", license_name.upper()):
                raise ValueError(f"{cid}: NoDerivatives source needs original-file transport, not this exporter")
            if not evaluation_only and (name not in allow_training or license_name not in TRAINING_LICENSES):
                raise ValueError(f"{name}: training needs explicit permission and a supported permissive licence")
            relative = Path(row["image"])
            if relative.is_absolute():
                raise ValueError(f"{cid}: image path must be relative")
            image = (base / relative).resolve(strict=True)
            if not image.is_relative_to(base):
                raise ValueError(f"{cid}: image escapes source directory")
            image_bytes = image.read_bytes()
            # Missing title metadata falls back to the entire source, never to random crops.
            lccn = meta.get("lccn")
            if not lccn:
                match = re.search(r"(sn\d+)", meta.get("page_id", ""))
                lccn = match.group(1) if match else None
            title = meta.get("title")
            group = f"lccn:{lccn}" if lccn else f"title:{title}" if title else f"source:{name}"
            groups.add(group)
            text_key = digest(" ".join(row["transcription"].split()).encode())
            for key in (digest(image_bytes), text_key):
                previous = text_groups.setdefault(key, group)
                if previous != group:
                    raise ValueError(f"{cid}: duplicate pixels or transcription across title groups")
            images[cid] = (image, image_bytes)
            rows.append((row, meta, group, name))
    if not rows:
        raise ValueError("no eligible crops")
    ordered = sorted(groups, key=lambda g: digest(f"{seed}:{g}".encode()))
    if evaluation_only:
        splits = {g: "test" for g in ordered}
    else:
        if len(ordered) < 3:
            raise ValueError("need at least three independent title/source groups, or --evaluation-only")
        n = max(1, len(ordered) // 10)
        splits = {g: ("test" if i < n else "validation" if i < 2 * n else "train")
                  for i, g in enumerate(ordered)}
    counts = {}
    out.mkdir(parents=True)
    for split in ("train", "validation", "test"):
        selected = [(r, m, g, ds) for r, m, g, ds in rows if splits[g] == split]
        if not selected:
            continue
        dest = out / split
        (dest / "images").mkdir(parents=True)
        output_rows, output_meta, conversations = [], [], []
        for row, meta, group, ds in sorted(selected, key=lambda item: item[0]["crop_id"]):
            cid = row["crop_id"]
            image, data = images[cid]
            relative = f"images/{digest(cid.encode())}{image.suffix.lower()}"
            (dest / relative).write_bytes(data)
            output_rows.append({**row, "image": relative})
            output_meta.append({**meta, "image": relative, "split_group": group,
                                "image_sha256": digest(data)})
            if split != "test":
                conversations.append({
                    "image": relative,
                    "conversations": [
                        {"from": "human", "value": "<image>\n" + PROMPT},
                        {"from": "gpt", "value": row["transcription"]},
                    ],
                })
        write_jsonl(dest / "crops.jsonl", output_rows)
        write_jsonl(dest / "manifest.jsonl", output_meta)
        if conversations:
            (dest / "sft.json").write_text(json.dumps(conversations, ensure_ascii=False) + "\n", encoding="utf-8")
        for ds in sorted({item[3] for item in selected}):
            spec = next(s for s in sources if s.partition("=")[0] == ds)
            licenses = Path(spec.partition("=")[2]) / "LICENSES"
            shutil.copytree(licenses, dest / "LICENSES" / ds)
        counts[split] = len(selected)
    record = {"v": 1, "seed": seed, "evaluation_only": evaluation_only, "sources": source_records,
              "groups": splits, "counts": counts, "prompt_sha256": digest(PROMPT.encode())}
    (out / "split.json").write_text(json.dumps(record, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return record


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", action="append", required=True, help="DATASET=DIRECTORY; repeatable")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--seed", type=int, default=542)
    parser.add_argument("--evaluation-only", action="store_true")
    parser.add_argument("--allow-training-source", action="append", default=[],
                        help="Affirm training/distribution rights were checked; repeatable")
    args = parser.parse_args()
    try:
        record = prepare(args.source, args.out, args.seed, args.evaluation_only, args.allow_training_source)
    except (ValueError, KeyError, OSError) as error:
        print(f"transcription: {error}", file=sys.stderr)
        return 1
    print(json.dumps(record, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
