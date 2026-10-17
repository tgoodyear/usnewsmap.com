#!/usr/bin/env python3
"""Host-9-only multi-crop (v2 bundle) re-run of an already-prepared Iris evaluation campaign (#736).

Groups the crops of a prepared 100-crop campaign (iris-campaign.py prepare) into bundles for app
version 1.1 so the bundle worker can be compared with the accepted 1.0 per-crop results on the same
images. Evaluation only: never production OCR publication, never the gated 20,000-page set.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("iris_campaign", HERE / "iris-campaign.py")
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)
import bundle as b  # noqa: E402  (path set up by iris-campaign)
import contract as c  # noqa: E402

BATCH = 105
VERSION_NUM = 110
DEFAULT_SIZE = 16


def prepare(source: Path, out: Path, image: str, size: int):
    if not base.IMAGE.fullmatch(image):
        raise ValueError("expected immutable public worker image digest")
    if not 1 <= size <= b.MAX_CROPS:
        raise ValueError(f"bundle size must be 1..{b.MAX_CROPS}")
    origin = base.load_campaign(source)
    rows = origin["jobs"]
    out.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".iris-bundle-", dir=out.parent) as temp:
        staged = Path(temp) / "campaign"
        (staged / "units").mkdir(parents=True)
        bundles = []
        for start in range(0, len(rows), size):
            group = rows[start:start + size]
            crops = []
            for row in group:
                v1 = json.loads((source / "units" / row["id"] / "iris.json").read_text())
                crops.append({"crop_id": v1["crop_id"], "doc_id": v1["doc_id"], "width": v1["box"][2],
                              "height": v1["box"][3],
                              "page": (source / "units" / row["id"] / "page.jpg").read_bytes()})
            job, pack = b.make_bundle(crops=crops, worker_image=image, host_id=base.HOST_ID)
            unit = staged / "units" / job["id"]
            unit.mkdir()
            data = b.encode_bundle_job(job)
            base.durable(unit / "iris.json", data)
            base.durable(unit / "pages.pack", pack)
            bundles.append({"id": job["id"], "job_sha256": base.sha(data), "pack_sha256": job["pack_sha256"],
                            "crops": [{"crop_id": r["crop_id"], "v1_id": r["id"], "reference": r["reference"]}
                                      for r in group]})
        manifest = {"v": 2, "app": base.APP, "host_id": base.HOST_ID, "batch": BATCH, "worker_image": image,
                    "evaluation_only": True, "bundle_size": size, "bundles": bundles}
        base.durable(staged / "campaign.json", base.encoded(manifest))
        if out.exists():
            raise FileExistsError("campaign already exists; inspect instead of overwriting")
        staged.rename(out)
    return {"bundles": len(bundles), "crops": len(rows), "bundle_size": size, "campaign": str(out)}


def load(root: Path):
    campaign = json.loads((root / "campaign.json").read_text())
    if (campaign["v"] != 2 or campaign["host_id"] != base.HOST_ID or campaign["batch"] != BATCH
            or campaign["evaluation_only"] is not True or not base.IMAGE.fullmatch(campaign["worker_image"])):
        raise ValueError("not the authorized host-9 Iris bundle campaign")
    for row in campaign["bundles"]:
        unit = root / "units" / row["id"]
        raw = (unit / "iris.json").read_bytes()
        job = b.validate_bundle_job(json.loads(raw))
        pack = (unit / "pages.pack").read_bytes()
        b.check_pack(job, pack)
        if (job["id"] != row["id"] or job["worker_image"] != campaign["worker_image"]
                or base.sha(raw) != row["job_sha256"] or job["pack_sha256"] != row["pack_sha256"]):
            raise ValueError("staged bundle changed after preparation")
    return campaign


def create_work_args(identity: str, crops: int):
    if not base.IDENTITY.fullmatch(identity):
        raise ValueError("invalid workunit identity")
    return [
        "--appname", base.APP, "--wu_name", f"usnm_iris_{identity}", "--app_version_num", str(VERSION_NUM),
        "--wu_template", "templates/usnm_iris_bundle_in", "--result_template", "templates/usnm_iris_bundle_out",
        "--target_host", str(base.HOST_ID), "--batch", str(BATCH),
        "--min_quorum", "1", "--target_nresults", "1", "--max_error_results", "1", "--max_total_results", "1",
        "--max_success_results", "1", "--rsc_fpops_est", str(crops * 10 ** 12), "--rsc_fpops_bound", "1e17",
        "--rsc_memory_bound", "9663676416", "--rsc_disk_bound", "17179869184", "--delay_bound", "259200",
        f"iris_{identity}_pack", f"iris_{identity}_job",
    ]


def submit(root: Path, project: Path, apply: bool = False):
    campaign = load(root)
    project = project.resolve(strict=True)
    if ET.parse(project / "config.xml").findtext("./config/enable_assignment") != "1":
        raise ValueError("host-target dispatch requires enable_assignment=1")
    base.VERSION_NUM = VERSION_NUM
    base.verify_registered_image(project, campaign["worker_image"])
    first = campaign["bundles"][0]
    if not apply:
        return {"dry_run": True, "bundles": len(campaign["bundles"]), "host_id": base.HOST_ID,
                "example": create_work_args(first["id"], len(first["crops"]))}
    lock = root / ".submit-lock"
    lock.mkdir()
    submitted = skipped = 0
    try:
        registry = project / "iris/jobs"
        registry.mkdir(parents=True, exist_ok=True)
        for row in campaign["bundles"]:
            identity = row["id"]
            unit = root / "units" / identity
            if (unit / "submitted").exists():
                skipped += 1
                continue
            if (unit / "submitting").exists():
                raise ValueError(f"{identity}: uncertain dispatch; inspect database before recovery")
            job_data = (unit / "iris.json").read_bytes()
            registered = registry / f"{identity}.json"
            if registered.exists():
                if registered.read_bytes() != job_data:
                    raise ValueError("validator registry job conflict")
            else:
                base.durable(registered, job_data)
                registered.chmod(0o664)
            for filename, physical in (("pages.pack", f"iris_{identity}_pack"), ("iris.json", f"iris_{identity}_job")):
                output = subprocess.run([str(project / "bin/dir_hier_path"), physical], cwd=project, check=True,
                                        capture_output=True, text=True).stdout.strip()
                target = Path(output)
                target = (target if target.is_absolute() else project / target).resolve()
                if not output or not target.is_relative_to((project / "download").resolve()):
                    raise ValueError("bad BOINC download path")
                target.parent.mkdir(parents=True, exist_ok=True)
                raw = (unit / filename).read_bytes()
                if target.exists():
                    if target.read_bytes() != raw:
                        raise ValueError("existing download file conflict")
                else:
                    base.durable(target, raw)
                    target.chmod(0o664)
            base.durable(unit / "submitting", base.encoded({"workunit": f"usnm_iris_{identity}", "host_id": base.HOST_ID}))
            subprocess.run([str(project / "bin/create_work"), *create_work_args(identity, len(row["crops"]))],
                           cwd=project, check=True)
            (unit / "submitting").rename(unit / "submitted")
            submitted += 1
    finally:
        lock.rmdir()
    return {"submitted": submitted, "previously_submitted": skipped, "host_id": base.HOST_ID}


def collect(root: Path, seen: Path, v1_seen: Path | None = None):
    """Per-crop CER against the references; with --v1-seen also the text difference from the 1.0 result."""
    campaign = load(root)
    v1_text = {}
    for path in sorted(v1_seen.glob("*.json")) if v1_seen else []:
        result = json.loads(path.read_text())
        if result.get("status") == "ok":
            v1_text[result["id"]] = result["text"]
    ids = {row["id"]: row for row in campaign["bundles"]}
    returned = {}
    for path in sorted(seen.glob("*.json")):
        if path.stat().st_size > b.MAX_BUNDLE_RESULT_BYTES:
            raise ValueError("result over size limit")
        result = json.loads(path.read_text())
        if result.get("id") in ids:
            job = json.loads((root / "units" / result["id"] / "iris.json").read_text())
            b.validate_bundle_result(result, job)
            returned[result["id"]] = result
    rows, errs, chars, drift, compared = [], 0, 0, 0, 0
    for record in campaign["bundles"]:
        result = returned.get(record["id"])
        for index, crop in enumerate(record["crops"]):
            row = {"bundle": record["id"], "crop_id": crop["crop_id"], "status": "pending"}
            if result:
                one = result["results"][index]
                row["status"] = one["status"]
                if one["status"] == "ok":
                    ref, text = " ".join(crop["reference"].split()), " ".join(one["text"].split())
                    n = base.edit_distance(ref, text)
                    errs, chars = errs + n, chars + len(ref)
                    row.update({"cer": n / max(1, len(ref)), "generate_ms": one["generate_ms"]})
                    old = v1_text.get(crop["v1_id"])
                    if old is not None:
                        d = base.edit_distance(" ".join(old.split()), text)
                        drift, compared = drift + d, compared + len(ref)
                        row["v1_edit_distance"] = d
            rows.append(row)
    report = {
        "bundles": len(ids), "returned": len(returned), "crops": len(rows),
        "ok": sum(r["status"] == "ok" for r in rows), "pending": sum(r["status"] == "pending" for r in rows),
        "normalized_cer": errs / chars if chars else None,
        "drift_vs_v1": drift / compared if compared else None,
        "bundle_elapsed_ms": [r["elapsed_ms"] for r in returned.values()],
        "load_ms": [r["load_ms"] for r in returned.values()],
        "validation": "experimental single-host structural validation, not multi-host scientific consensus",
        "rows": rows,
    }
    (root / "report.json").write_bytes(base.encoded(report))
    return {k: v for k, v in report.items() if k != "rows"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    stage = commands.add_parser("prepare")
    stage.add_argument("--source", type=Path, required=True, help="a prepared iris-campaign.py campaign")
    stage.add_argument("--out", type=Path, required=True)
    stage.add_argument("--worker-image", required=True)
    stage.add_argument("--bundle-size", type=int, default=DEFAULT_SIZE)
    send = commands.add_parser("submit")
    send.add_argument("--root", type=Path, required=True)
    send.add_argument("--project", type=Path, required=True)
    send.add_argument("--apply", action="store_true")
    inspect = commands.add_parser("collect")
    inspect.add_argument("--root", type=Path, required=True)
    inspect.add_argument("--seen", type=Path, required=True)
    inspect.add_argument("--v1-seen", type=Path)
    args = parser.parse_args()
    if args.command == "prepare":
        result = prepare(args.source, args.out, args.worker_image, args.bundle_size)
    elif args.command == "submit":
        result = submit(args.root, args.project, args.apply)
    else:
        result = collect(args.root, args.seen, args.v1_seen)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    sys.exit(main())
