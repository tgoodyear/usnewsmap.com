#!/usr/bin/env python3
"""Prepare pinned smoke inputs or export validator-kept Surya text for bench-score."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

SUBSETS = ("overproof-en/smoke-10", "reichsanzeiger-de/smoke-10")
MODES = ("tiled", "whole", "capped")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def encode(value):
    return (json.dumps(value, ensure_ascii=False, indent=2) + "\n").encode()


def checked(root, name, expected):
    path = (root / name).resolve(strict=True)
    if not path.is_relative_to(root.resolve()) or not path.is_file():
        raise ValueError("input outside benchmark release")
    data = path.read_bytes()
    if digest(data) != expected:
        raise ValueError("benchmark file checksum mismatch: " + name)
    return data


def prepare(release, out):
    index = Path(__file__).resolve().parents[2] / "ocr/bench/releases.json"
    lock = next(row for row in json.loads(index.read_text())["releases"] if row["release"] == "v1.0.0")
    raw = (release / "manifest.json").read_bytes()
    if digest(raw) != lock["manifest_sha256"]:
        raise ValueError("release manifest differs from committed v1.0.0 lock")
    inventory = (release / "SHA256SUMS").read_bytes()
    if digest(inventory) != lock["content_digest"]:
        raise ValueError("release checksum inventory differs from committed lock")
    licenses = {}
    for line in inventory.decode().splitlines():
        expected, name = line.split("  ", 1)
        if name.startswith("LICENSES/"):
            licenses[name] = checked(release, name, expected)
    if not licenses:
        raise ValueError("benchmark source licenses are missing")
    manifest = json.loads(raw)
    samples = {row["id"]: row for row in manifest["samples"]}
    rows, files = [], {}
    for subset in SUBSETS:
        members = manifest["subsets"][subset]["members"]
        lines = "".join(f"{identity} {samples[identity]['image']['sha256']} {samples[identity]['reference']['sha256']}\n"
                        for identity in sorted(members))
        if len(members) != 10 or digest(lines.encode()) != lock["subsets"][subset]["digest"]:
            raise ValueError("immutable smoke subset changed")
        english = subset.startswith("overproof")
        for identity in members:
            row = samples[identity]
            for kind in ("image", "reference"):
                member = row[kind]
                files[member["path"]] = checked(release, member["path"], member["sha256"])
            rows.append({
                "sample_id": identity, "group": "english" if english else "german", "language": "eng" if english else "deu",
                "image": row["image"]["path"], "image_sha256": row["image"]["sha256"],
                "reference": row["reference"]["path"], "reference_sha256": row["reference"]["sha256"],
                "source_url": f"https://github.com/tgoodyear/usnewsmap.com/releases/tag/{lock['tag']}",
                "license": "Public domain images; references CC BY-NC-SA 4.0" if english else "Public Domain Mark 1.0 images; references CC BY 4.0",
            })
    # Validate every byte before creating an output. Existing bundles are never overwritten.
    out.mkdir(parents=True, exist_ok=False)
    for name, data in files.items():
        path = out / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    for name, data in licenses.items():
        path = out / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    (out / "upstream-manifest.json").write_bytes(raw)
    data = encode(rows)
    (out / "inputs.json").write_bytes(data)
    receipt = {"release": lock["release"], "content_digest": lock["content_digest"],
               "subsets": {name: lock["subsets"][name] for name in SUBSETS},
               "inputs_sha256": digest(data), "units": len(rows)}
    (out / "receipt.json").write_bytes(encode(receipt))
    return receipt


def collect(root, seen, out, binary, intended_device):
    manifest = (root / "benchmark-inputs.json").read_bytes()
    expected = (root / "benchmark-inputs.sha256").read_text()
    if digest(manifest) != expected or (root / "benchmark-ready").read_text() != expected:
        raise ValueError("benchmark campaign changed or incomplete")
    inputs = json.loads(manifest)
    jobs = {}
    for row in inputs:
        unit = root / "units" / row["sample_id"]
        job = json.loads((unit / "surya.json").read_text())
        if job["doc_id"] != row["sample_id"] or job["input"]["sha256"] != row["image_sha256"]:
            raise ValueError("job differs from immutable benchmark lock")
        checked(unit, "page.jpg", row["image_sha256"])
        checked(unit, "reference.txt", row["reference_sha256"])
        jobs[job["id"]] = (row, job)
    if intended_device not in ("cpu", "cuda"):
        raise ValueError("intended device must be cpu or cuda")
    outputs = {(intended_device, mode, row["group"]): {}
               for row, job in jobs.values()
               for mode in job["gpu_modes" if intended_device == "cuda" else "cpu_modes"]}
    records, returned = [], set()
    for directory in sorted(seen.iterdir()) if seen.exists() else []:
        path = directory / "result.json"
        if not path.is_file() or path.stat().st_size > 16384:
            continue
        result = json.loads(path.read_text())
        if result.get("job_id") not in jobs:
            continue
        row, job = jobs[result["job_id"]]
        files = [directory / f"text-{mode}.txt" for mode in MODES] + [path, directory / "diag.txt"]
        subprocess.run([binary, "surya", "check", "--files", *map(str, files)], check=True)
        if result["engine"] != job["engine"]:
            raise ValueError("validator-kept result engine differs from staged job")
        returned.add(row["sample_id"])
        if result.get("unit_error"):
            records.append({"sample_id": row["sample_id"], "result": directory.name,
                            "unit_error": result["unit_error"]})
        for run in result["runs"]:
            mode, device = run["mode"], run["device"]
            if mode not in MODES or device not in ("cpu", "cuda", "none"):
                raise ValueError("unexpected mode/device")
            records.append({"sample_id": row["sample_id"], "result": directory.name, **run})
            key = (device, mode, row["group"])
            group = outputs.setdefault(key, {})
            if run["outcome"] == "ok":
                if row["sample_id"] in group:
                    raise ValueError("duplicate successful output; select one campaign/host rather than silently averaging")
                group[row["sample_id"]] = (directory / f"text-{mode}.txt").read_bytes().decode("utf-8")
    out.mkdir(parents=True, exist_ok=False)
    for (device, mode, group), texts in outputs.items():
        # One JSON object per line, compatible with the existing bench-score command.
        data = "".join(json.dumps({"id": identity, "text": text}, ensure_ascii=False) + "\n"
                       for identity, text in sorted(texts.items())).encode()
        (out / f"{device}-{mode}-{group}.jsonl").write_bytes(data)
    receipt = {"inputs_sha256": expected, "expected": len(inputs), "returned": len(returned),
               "missing": sorted(set(row["sample_id"] for row in inputs) - returned), "records": records,
               "validation": "Per-result Surya script check; quorum-one probe is not cross-host consensus"}
    (out / "receipt.json").write_bytes(encode(receipt))
    return {key: value for key, value in receipt.items() if key != "records"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    stage = commands.add_parser("prepare")
    stage.add_argument("--release", type=Path, required=True)
    stage.add_argument("--out", type=Path, required=True)
    export = commands.add_parser("collect")
    export.add_argument("--root", type=Path, required=True)
    export.add_argument("--seen", type=Path, required=True)
    export.add_argument("--out", type=Path, required=True)
    export.add_argument("--binary", default="usnm-boinc")
    export.add_argument("--intended-device", choices=("cpu", "cuda"), required=True)
    args = parser.parse_args()
    result = prepare(args.release, args.out) if args.command == "prepare" else collect(args.root, args.seen, args.out, args.binary, args.intended_device)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
