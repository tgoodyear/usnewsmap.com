#!/usr/bin/env python3
"""Prepare immutable permissive trials and submit them with write-ahead recovery."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import zipfile

from worker import BASE_REVISION, load_job, sha

SOURCES = ("newseye-bnf", "newseye-nlf-fi", "newseye-nlf-sv")


def encoded(value) -> bytes:
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode()


def checksums(root: Path) -> dict:
    return {str(p.relative_to(root)): sha(p.read_bytes())
            for p in sorted(root.rglob("*")) if p.is_file()
            and not {"units", "out"}.intersection(p.relative_to(root).parts) and p.name != "prepared.json"}


def prepare(cache: Path, out: Path, worker_image: str) -> dict:
    import re
    if not re.fullmatch(r"ghcr.io/tgoodyear/usnm-boinc-worker@sha256:[0-9a-f]{64}", worker_image):
        raise ValueError("training worker must be a pinned public image")
    source_dir = Path("/usr/local/lib/usnm-boinc/gold/scripts/ocr-survey")
    if not source_dir.is_dir():
        source_dir = Path(__file__).resolve().parents[3] / "scripts/ocr-survey"
    sys.path.insert(0, str(source_dir))
    import transcription
    out.parent.mkdir(parents=True, exist_ok=True)
    lock = out.with_name(out.name + ".prepare.lock")
    lock.mkdir()
    try:
        with tempfile.TemporaryDirectory(prefix=f".{out.name}.", dir=out.parent) as tmp:
            candidate = Path(tmp) / "campaign"
            transcription.prepare([f"{s}={cache / s}" for s in SOURCES],
                                  candidate, 542, False, list(SOURCES))
            train = candidate / "train"
            rows = [json.loads(line) for line in (train / "crops.jsonl").read_text().splitlines()]
            meta = {row["crop_id"]: row for row in
                    map(json.loads, (train / "manifest.jsonl").read_text().splitlines())}
            examples, images = [], {}
            for row in rows:
                if len(examples) == 16:
                    break
                if not 1 <= len(row["transcription"]) <= 800:
                    continue
                data = (train / row["image"]).read_bytes()
                if len(data) > 2 * 1024**2:
                    continue
                m = meta[row["crop_id"]]
                examples.append({"crop_id": row["crop_id"], "image": row["image"],
                                 "image_sha256": sha(data), "text": row["transcription"],
                                 "source_license": m["source_license"], "source_url": m["source_url"]})
                images[row["image"]] = data
            if not examples:
                raise ValueError("no train-only crops satisfy the bounded pilot")
            notices = []
            for path in sorted((train / "LICENSES").rglob("*")):
                if path.is_file():
                    notices.append(f"{path.relative_to(train)}:\n{path.read_text()}")
            for row in examples:
                attribution = meta[row["crop_id"]].get("source_attribution")
                if not isinstance(attribution, str) or not attribution.strip():
                    raise ValueError("training crop lacks source attribution")
                notices.append(f'{row["crop_id"]}: {attribution}\n'
                               f'{row["source_url"]}\n{row["source_license"]}\n'
                               "Unchanged converted crop; source conversion details retained in campaign manifest.")
            manifest = {"v": 1, "split": "train",
                        "split_sha256": sha((candidate / "split.json").read_bytes()),
                        "examples": examples, "notices": notices}
            public = candidate / "training.zip"
            with zipfile.ZipFile(public, "w", compression=zipfile.ZIP_STORED) as archive:
                files = {"training.json": encoded(manifest),
                         "NOTICE.txt": ("\n\n".join(notices) + "\n\nOriginal crop bytes; no re-encoding.\n").encode(),
                         **images}
                for name, data in sorted(files.items()):
                    archive.writestr(zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0)), data)
            units = candidate / "units"
            units.mkdir()
            jobs = []
            for seed, rank in ((542, 4), (543, 8)):
                job = {"v": 1, "base_revision": BASE_REVISION, "bundle_sha256": sha(public.read_bytes()),
                       "seed": seed, "rank": rank, "steps": 16, "learning_rate": 1e-4}
                raw = encoded(job)
                identity = sha(raw)
                unit = units / identity
                unit.mkdir()
                (unit / "train.json").write_bytes(raw)
                shutil.copyfile(public, unit / "training.zip")
                load_job(unit)
                jobs.append(identity)
            (candidate / "campaign.json").write_bytes(encoded({
                "v": 1, "worker_image": worker_image, "jobs": jobs,
                "training_examples": len(examples),
                "candidate_status": "untrusted-requires-held-out-evaluation",
            }))
            record = {"v": 1, "files": checksums(candidate)}
            (candidate / "prepared.json").write_bytes(encoded(record))
            if out.exists():
                if json.loads((out / "prepared.json").read_text()) != record or checksums(out) != record["files"]:
                    raise ValueError("existing training campaign differs from immutable preparation")
                for identity in jobs:
                    for name in ("train.json", "training.zip"):
                        if (out / "units" / identity / name).read_bytes() != (units / identity / name).read_bytes():
                            raise ValueError("existing staged training job changed")
                return {"replayed": True, "jobs": jobs}
            candidate.rename(out)
            return {"replayed": False, "jobs": jobs}
    finally:
        lock.rmdir()


def create_work_args(identity: str) -> list[str]:
    return [
        "--appname", "usnm_qwen_train", "--wu_name", f"usnm_train_{identity}",
        "--wu_template", "templates/usnm_qwen_train_in",
        "--result_template", "templates/usnm_qwen_train_out",
        "--min_quorum", "1", "--target_nresults", "1",
        "--max_error_results", "2", "--max_total_results", "3", "--max_success_results", "1",
        "--rsc_fpops_est", "1e15", "--rsc_fpops_bound", "1e18",
        "--rsc_memory_bound", "17179869184",
        "--rsc_disk_bound", "42949672960", "--delay_bound", "604800",
        f"train_{identity}_bundle", f"train_{identity}_config",
    ]


def submit(root: Path, project: Path) -> dict:
    project = project.resolve(strict=True)
    campaign = json.loads((root / "campaign.json").read_text())
    record = json.loads((root / "prepared.json").read_text())
    if checksums(root) != record["files"]:
        raise ValueError("training campaign changed after preparation")
    submitted = 0
    for identity in campaign["jobs"]:
        unit = root / "units" / identity
        load_job(unit)
        if sha((unit / "train.json").read_bytes()) != identity:
            raise ValueError("training job identity mismatch")
        if (unit / "submitted").exists():
            continue
        if (unit / "submitting").exists():
            raise ValueError(f"{identity}: uncertain submission; check database before explicit recovery")
        with (unit / "submitting").open("x") as marker:
            marker.write(f"usnm_train_{identity}\n")
        for name, physical in (("training.zip", f"train_{identity}_bundle"),
                               ("train.json", f"train_{identity}_config")):
            output = subprocess.check_output([str(project / "bin/dir_hier_path"), physical],
                                             cwd=project, text=True).strip()
            if not output:
                raise ValueError("dir_hier_path returned an empty path")
            dest = Path(output)
            if not dest.is_absolute():
                dest = project / dest
            dest = dest.resolve()
            if not dest.is_relative_to(project / "download"):
                raise ValueError("dir_hier_path escaped the download tree")
            dest.parent.mkdir(parents=True, exist_ok=True)
            data = (unit / name).read_bytes()
            if dest.exists() and dest.read_bytes() != data:
                raise ValueError("existing download input differs")
            fd, temp = tempfile.mkstemp(prefix=f".{physical}.", dir=dest.parent)
            try:
                with os.fdopen(fd, "wb") as stream:
                    stream.write(data)
                os.chmod(temp, 0o664)
                os.replace(temp, dest)
            finally:
                Path(temp).unlink(missing_ok=True)
        subprocess.run([str(project / "bin/create_work"), *create_work_args(identity)],
                       cwd=project, check=True)
        (unit / "submitting").rename(unit / "submitted")
        submitted += 1
    return {"submitted": submitted}


def collect(root: Path, seen: Path) -> dict:
    from check import bounded, validate
    from worker import HEX, MAX_ADAPTER
    campaign = json.loads((root / "campaign.json").read_text())
    if checksums(root) != json.loads((root / "prepared.json").read_text())["files"]:
        raise ValueError("training campaign changed after preparation")
    expected = set(campaign["jobs"])
    returned, candidates = set(), []
    if seen.exists():
        for path in sorted(seen.iterdir()):
            if not HEX.fullmatch(path.name):
                raise ValueError("unexpected entry in candidate quarantine")
            raw = bounded(path / "result.json", 16384)
            report = json.loads(raw)
            identity = report["job_sha256"]
            if identity not in expected:
                continue
            adapter = bounded(path / "adapter.safetensors", MAX_ADAPTER)
            report = validate(adapter, raw, root / "units" / identity)
            if path.name != sha(adapter + raw):
                raise ValueError("quarantined candidate changed identity")
            returned.add(identity)
            candidates.append({"candidate": path.name, **report})
    result = {"expected_trials": len(expected), "returned_trials": len(returned),
              "pending_trials": sorted(expected - returned), "candidates": candidates,
              "scientific_acceptance": "pending-independent-held-out-evaluation",
              "promoted": False}
    dest = root / "out"
    dest.mkdir(exist_ok=True)
    (dest / "candidates.json").write_bytes(encoded(result))
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("prepare")
    p.add_argument("--cache", required=True, type=Path)
    p.add_argument("--root", required=True, type=Path)
    p.add_argument("--worker-image", required=True)
    p = sub.add_parser("submit")
    p.add_argument("--root", required=True, type=Path)
    p.add_argument("--project", required=True, type=Path)
    p = sub.add_parser("collect")
    p.add_argument("--root", required=True, type=Path)
    p.add_argument("--seen", required=True, type=Path)
    args = parser.parse_args()
    if args.command == "prepare":
        result = prepare(args.cache, args.root, args.worker_image)
    elif args.command == "submit":
        result = submit(args.root, args.project)
    else:
        result = collect(args.root, args.seen)
    print(json.dumps(result))
