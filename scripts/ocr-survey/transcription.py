#!/usr/bin/env python3
"""Prepare title-disjoint public gold data for SFT and BOINC evaluation (#542)."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import sys
import tempfile
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
    page_identities = {}
    title_identities = {}
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
            if meta.get("crop_sha256") != digest(image_bytes):
                raise ValueError(f"{cid}: crop checksum differs from source manifest")
            # Missing title metadata falls back to the entire source, never to random crops.
            lccn = meta.get("lccn")
            if not lccn:
                match = re.search(r"(sn\d+)", meta.get("page_id", ""))
                lccn = match.group(1) if match else None
            title = meta.get("title")
            group = f"lccn:{lccn}" if lccn else f"title:{title}" if title else f"source:{name}"
            page_id = meta.get("page_id")
            if not page_id:
                raise ValueError(f"{cid}: missing page_id")
            page_key = (name, page_id)
            if page_identities.setdefault(page_key, group) != group:
                raise ValueError(f"{cid}: inconsistent page identity; reconcile title/LCCN metadata")
            if title and title_identities.setdefault(title, group) != group:
                raise ValueError(f"{cid}: inconsistent title identity; reconcile LCCN metadata")
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


def prepare_benchmark(selection: Path, cache: Path, out: Path) -> dict:
    """Resolve a reviewed evaluation selection against private, already converted gold."""
    raw = selection.read_bytes()
    spec = json.loads(raw)
    if not isinstance(spec, dict) or spec.get("v") != 1 or spec.get("evaluation_only") is not True:
        raise ValueError("selection must be a v1 evaluation-only audit")
    items = spec["included"]
    if not isinstance(items, list) or not items or len(items) > 1000:
        raise ValueError("selection must include 1-1000 audited crops")
    engine = spec["engine"]
    if (not isinstance(engine, dict)
            or any(not re.fullmatch(r"sha256:[0-9a-f]{64}", str(engine.get(k, "")))
                   for k in ("image", "llama", "weights"))
            or not isinstance(engine.get("models"), list) or len(engine["models"]) != 1):
        raise ValueError("selection needs immutable engine digests and exactly one model")
    wanted = {}
    ids = set()
    for item in items:
        if not isinstance(item, dict):
            raise ValueError("selection entries must be crop objects")
        cid, source, sha = item["crop_id"], item["source_dataset"], item["image_sha256"]
        if (not isinstance(cid, str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,128}", cid)
                or not isinstance(source, str) or not re.fullmatch(r"[a-z0-9][a-z0-9-]*", source)
                or not re.fullmatch(r"[0-9a-f]{64}", str(sha))
                or not re.fullmatch(r"[0-9a-f]{64}", str(item["reference_sha256"]))):
            raise ValueError("invalid selection crop identity or checksum")
        key = (source, sha)
        if cid in ids or key in wanted:
            raise ValueError(f"{cid}: duplicate selection crop or image")
        if not isinstance(item.get("provenance"), dict) or not {
            "page_id", "source_url", "source_license", "source_attribution",
        }.issubset(item["provenance"]):
            raise ValueError(f"{cid}: missing reviewed provenance")
        ids.add(cid)
        wanted[key] = item
    cache = cache.resolve(strict=True)
    out = out.absolute()
    out.parent.mkdir(parents=True, exist_ok=True)
    lock = out.parent / f".{out.name}.prepare.lock"
    try:
        lock.mkdir()
    except FileExistsError as error:
        raise ValueError(f"another preparation holds {lock}; verify it stopped before recovery") from error
    try:
        with tempfile.TemporaryDirectory(prefix=f".{out.name}.prepare-", dir=out.parent) as temp:
            work = Path(temp)
            sources = []
            found = set()
            for source in sorted({key[0] for key in wanted}):
                base = (cache / source).resolve(strict=True)
                if not base.is_relative_to(cache):
                    raise ValueError(f"{source}: dataset escapes cache")
                rows = read_jsonl(base / "crops.jsonl")
                crops = {row["crop_id"]: row for row in rows}
                if len(crops) != len(rows):
                    raise ValueError(f"{source}: duplicate cached crop")
                dest = work / "sources" / source
                (dest / "images").mkdir(parents=True)
                selected, metadata = [], []
                for meta in read_jsonl(base / "manifest.jsonl"):
                    key = (source, meta["crop_sha256"])
                    if key not in wanted:
                        continue
                    item = wanted[key]
                    cid = item["crop_id"]
                    if key in found:
                        raise ValueError(f"{cid}: duplicate cached image match")
                    found.add(key)
                    if (meta.get("source_dataset") != source
                            or any(meta.get(k) != v for k, v in item["provenance"].items())):
                        raise ValueError(f"{cid}: reviewed provenance differs from cache")
                    row = crops[meta["crop_id"]]
                    if digest(row["transcription"].encode()) != item["reference_sha256"]:
                        raise ValueError(f"{cid}: reference checksum differs from audit")
                    relative = Path(row["image"])
                    image = (base / relative).resolve(strict=True)
                    if relative.is_absolute() or not image.is_relative_to(base):
                        raise ValueError(f"{cid}: source image escapes dataset")
                    data = image.read_bytes()
                    if digest(data) != item["image_sha256"]:
                        raise ValueError(f"{cid}: image checksum differs from audit")
                    path = f"images/{key[1]}{image.suffix.lower()}"
                    (dest / path).write_bytes(data)
                    selected.append({**row, "crop_id": cid, "image": path})
                    metadata.append({**meta, "crop_id": cid, "image": path})
                if not selected:
                    raise ValueError(f"{source}: audited selection is missing from cache")
                write_jsonl(dest / "crops.jsonl", selected)
                write_jsonl(dest / "manifest.jsonl", metadata)
                shutil.copytree(base / "LICENSES", dest / "LICENSES")
                sources.append(f"{source}={dest}")
            if found != set(wanted):
                missing = sorted(wanted[key]["crop_id"] for key in wanted.keys() - found)
                raise ValueError(f"audited selection missing from cache: {missing}")
            candidate = work / "campaign"
            record = prepare(sources, candidate, 542, True, [])
            (candidate / "engine.json").write_text(json.dumps(engine, sort_keys=True) + "\n")
            (candidate / "audit.json").write_bytes(raw)
            files = {str(p.relative_to(candidate)): digest(p.read_bytes())
                     for p in sorted(candidate.rglob("*")) if p.is_file()}
            marker = {"v": 1, "selection_sha256": digest(raw), "files": files}
            if out.exists():
                if (out.is_symlink() or not out.is_dir()
                        or not (out / "prepared.json").is_file()
                        or json.loads((out / "prepared.json").read_bytes()) != marker):
                    raise ValueError(f"{out}: existing campaign differs from reviewed preparation")
                for path, sha in files.items():
                    target = (out / path).resolve(strict=True)
                    if not target.is_relative_to(out.resolve()) or digest(target.read_bytes()) != sha:
                        raise ValueError(f"{path}: prepared campaign checksum differs")
            else:
                (candidate / "prepared.json").write_text(json.dumps(marker, sort_keys=True) + "\n")
                candidate.rename(out)
            return record
    finally:
        lock.rmdir()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--source", action="append", help="DATASET=DIRECTORY; repeatable")
    mode.add_argument("--selection", type=Path, help="Reviewed evaluation-only audit with engine pins")
    parser.add_argument("--cache", type=Path, help="Converted gold cache containing dataset directories")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--seed", type=int, default=542)
    parser.add_argument("--evaluation-only", action="store_true")
    parser.add_argument("--allow-training-source", action="append", default=[],
                        help="Affirm training/distribution rights were checked; repeatable")
    args = parser.parse_args()
    try:
        if args.selection:
            if not args.cache or args.allow_training_source:
                raise ValueError("--selection requires --cache and cannot authorize training")
            record = prepare_benchmark(args.selection, args.cache, args.out)
        else:
            if args.cache:
                raise ValueError("--cache requires --selection")
            record = prepare(args.source, args.out, args.seed, args.evaluation_only, args.allow_training_source)
    except (ValueError, KeyError, OSError, TypeError) as error:
        print(f"transcription: {error}", file=sys.stderr)
        return 1
    print(json.dumps(record, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
