#!/usr/bin/env python3
"""Immutable 100-crop, host-9-only Iris Xe evaluation; never production OCR publication."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET

CONTRACT = Path(__file__).resolve().parents[2] / "ocr/boinc/iris"
if CONTRACT.is_dir():
    sys.path.insert(0, str(CONTRACT))
from contract import MODEL_ID, MODEL_REVISION, PROMPT_VERSION, canonical_job_id, validate_job, validate_result

COUNT = 100
HOST_ID = 9
APP = "usnm_iris"
BATCH = 104
VERSION_NUM = 100
PLAN_CLASS = "docker_intel_opencl_4b"
IMAGE = re.compile(r"ghcr\.io/tgoodyear/usnm-boinc-worker@sha256:[0-9a-f]{64}\Z")
IDENTITY = re.compile(r"[0-9a-f]{64}\Z")


def encoded(value):
    return (json.dumps(value, sort_keys=True, ensure_ascii=True, indent=2) + "\n").encode()


def sha(data):
    return hashlib.sha256(data).hexdigest()


def durable(path, data):
    with path.open("xb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())


def prepare(dataset: Path, out: Path, image: str):
    if not IMAGE.fullmatch(image):
        raise ValueError("expected immutable public worker image digest")
    from PIL import Image
    source = json.loads((dataset / "dataset.json").read_text())
    if source.get("evaluation_only") is not True or len(source.get("crops", [])) != COUNT:
        raise ValueError("expected exactly 100 evaluation-only crops")
    out.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".iris-stage-", dir=out.parent) as temp:
        staged = Path(temp) / "campaign"
        (staged / "units").mkdir(parents=True)
        records = []
        seen = set()
        seen_images = set()
        for row in source["crops"]:
            crop_id = row["crop_id"]
            if not re.fullmatch(r"pub-(?:overproof-ca|reichsanzeiger-lines)-[0-9]{3}", crop_id) or crop_id in seen:
                raise ValueError("invalid or duplicate newspaper crop")
            seen.add(crop_id)
            path = (dataset / row["image"]).resolve(strict=True)
            if not path.is_relative_to((dataset / "images").resolve(strict=True)):
                raise ValueError("crop image outside dataset images")
            data = path.read_bytes()
            if sha(data) != row["image_sha256"]:
                raise ValueError(f"{crop_id}: image checksum mismatch")
            if row["image_sha256"] in seen_images:
                raise ValueError("pilot requires 100 distinct images, not duplicate crops")
            seen_images.add(row["image_sha256"])
            provenance = row["provenance"]
            if (provenance.get("for_scoring") is not True
                    or "public domain" not in provenance["source_license"].lower()):
                raise ValueError("expected public-domain newspaper evaluation image")
            if crop_id.startswith("pub-reichsanzeiger-lines-") and (
                provenance.get("source_dataset") != "reichsanzeiger"
                or not provenance.get("source_attribution")
                or provenance.get("ground_truth_status") != "GT"
                or not provenance.get("line_ids")
            ):
                raise ValueError("Reichsanzeiger crops require attributed PAGE ground truth and line IDs")
            with Image.open(path) as picture:
                if picture.format != "JPEG":
                    raise ValueError(f"{crop_id}: pilot input must be JPEG")
                picture.verify()
                width, height = picture.size
            job = {
                "v": 1, "crop_id": crop_id, "doc_id": provenance["page_id"],
                "host_id": HOST_ID, "worker_image": image,
                "model_id": MODEL_ID, "model_revision": MODEL_REVISION,
                "prompt_version": PROMPT_VERSION, "max_new_tokens": 512,
                "page_sha256": sha(data), "page_bytes": len(data),
                "box": [0, 0, width, height],
            }
            job["id"] = canonical_job_id(job)
            validate_job(job)
            unit = staged / "units" / job["id"]
            unit.mkdir()
            durable(unit / "iris.json", encoded(job))
            durable(unit / "page.jpg", data)
            records.append({
                "id": job["id"], "crop_id": crop_id,
                "reference": row["reference"], "provenance": provenance,
                "job_sha256": sha(encoded(job)), "page_sha256": sha(data),
            })
        manifest = {
            "v": 1, "app": APP, "host_id": HOST_ID, "batch": BATCH,
            "worker_image": image, "evaluation_only": True, "jobs": records,
        }
        durable(staged / "campaign.json", encoded(manifest))
        licenses = dataset / "LICENSES"
        if not licenses.is_dir():
            raise ValueError("dataset source licenses are required")
        shutil.copytree(licenses, staged / "LICENSES")
        if out.exists():
            raise FileExistsError("campaign already exists; inspect instead of overwriting")
        staged.rename(out)
    return {"jobs": COUNT, "host_id": HOST_ID, "campaign": str(out)}


def load_campaign(root):
    campaign = json.loads((root / "campaign.json").read_text())
    if (campaign["host_id"] != HOST_ID or campaign["batch"] != BATCH
            or campaign["app"] != APP or campaign["evaluation_only"] is not True
            or len(campaign["jobs"]) != COUNT or not IMAGE.fullmatch(campaign["worker_image"])):
        raise ValueError("not the authorized 100-job host-9 Iris campaign")
    ids = [row["id"] for row in campaign["jobs"]]
    if len(set(ids)) != COUNT or not all(IDENTITY.fullmatch(identity) for identity in ids):
        raise ValueError("invalid or duplicate campaign jobs")
    for row in campaign["jobs"]:
        unit = root / "units" / row["id"]
        raw = (unit / "iris.json").read_bytes()
        job = json.loads(raw)
        validate_job(job)
        if (job["id"] != row["id"] or job["worker_image"] != campaign["worker_image"]
                or sha(raw) != row["job_sha256"]
                or sha((unit / "page.jpg").read_bytes()) != job["page_sha256"]
                or job["page_sha256"] != row["page_sha256"]):
            raise ValueError("staged job changed after preparation")
    return campaign


def create_work_args(identity):
    if not IDENTITY.fullmatch(identity):
        raise ValueError("invalid workunit identity")
    return [
        "--appname", APP, "--wu_name", f"usnm_iris_{identity}",
        "--app_version_num", str(VERSION_NUM),
        "--wu_template", "templates/usnm_iris_in",
        "--result_template", "templates/usnm_iris_out",
        "--target_host", str(HOST_ID), "--batch", str(BATCH),
        "--min_quorum", "1", "--target_nresults", "1",
        "--max_error_results", "1", "--max_total_results", "1",
        "--max_success_results", "1",
        "--rsc_fpops_est", "1e12", "--rsc_fpops_bound", "1e16",
        "--rsc_memory_bound", "9663676416", "--rsc_disk_bound", "17179869184",
        "--delay_bound", "259200",
        f"iris_{identity}_page", f"iris_{identity}_job",
    ]


def verify_registered_image(project, image):
    import MySQLdb

    config = ET.parse(project / "config.xml").find("config")
    connection = MySQLdb.connect(
        host=config.findtext("db_host"), user=config.findtext("db_user"),
        passwd=config.findtext("db_passwd"), db=config.findtext("db_name"),
        ssl={"ca": "/usr/local/share/usnm/mysql-ca.pem"},
    )
    try:
        cursor = connection.cursor()
        cursor.execute(
            "SELECT av.xml_doc FROM app_version av JOIN app a ON a.id=av.appid "
            "JOIN platform p ON p.id=av.platformid "
            "WHERE a.name=%s AND av.version_num=%s AND av.plan_class=%s "
            "AND p.name=%s AND av.deprecated=0",
            (APP, VERSION_NUM, PLAN_CLASS, "x86_64-pc-linux-gnu"),
        )
        versions = cursor.fetchall()
        if len(versions) != 1:
            raise ValueError("expected exactly one registered active Iris version 1.0")
        verify_version_files(project, image, versions[0][0])
    finally:
        connection.close()


def verify_version_files(project, image, xml_doc):
    if isinstance(xml_doc, bytes):
        xml_doc = xml_doc.decode("utf-8")
    version = ET.fromstring("<version>" + xml_doc + "</version>")
    refs = [ref for ref in version.findall(".//file_ref") if ref.findtext("open_name") == "Dockerfile"]
    if len(refs) != 1:
        raise ValueError("registered app version must contain one Dockerfile")
    name = refs[0].findtext("file_name")
    if not name or not re.fullmatch(r"[A-Za-z0-9_.-]+", name):
        raise ValueError("invalid registered Dockerfile name")
    infos = [info for info in version.findall(".//file_info") if info.findtext("name") == name]
    if len(infos) != 1 or not infos[0].findtext("file_signature"):
        raise ValueError("registered Dockerfile lacks code signature")
    # Stock update_versions stages app files flat, unlike workunit input files.
    path = (project / "download" / name).resolve(strict=True)
    if not path.is_relative_to((project / "download").resolve()):
        raise ValueError("registered Dockerfile outside download directory")
    data = path.read_bytes()
    if data != f"FROM {image}\n".encode():
        raise ValueError("campaign image differs from registered signed app version")
    with tempfile.TemporaryDirectory(prefix="iris-signature-") as directory:
        signature = Path(directory) / "Dockerfile.sig"
        signature.write_text(infos[0].findtext("file_signature"))
        subprocess.run(
            ["/usr/local/boinc/lib/crypt_prog", "-verify", str(path),
             str(signature), str(project / "keys/code_sign_public")],
            check=True, capture_output=True, text=True,
        )


def submit(root, project, apply=False):
    campaign = load_campaign(root)
    project = project.resolve(strict=True)
    config = ET.parse(project / "config.xml")
    if config.findtext("./config/enable_assignment") != "1":
        raise ValueError("host-target dispatch requires enable_assignment=1")
    verify_registered_image(project, campaign["worker_image"])
    if not apply:
        return {"dry_run": True, "jobs": COUNT, "host_id": HOST_ID,
                "example": create_work_args(campaign["jobs"][0]["id"])}
    lock = root / ".submit-lock"
    lock.mkdir()
    submitted = skipped = 0
    try:
        registry = project / "iris/jobs"
        registry.mkdir(parents=True, exist_ok=True)
        for row in campaign["jobs"]:
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
                durable(registered, job_data)
                registered.chmod(0o664)
            for filename, physical in (
                ("page.jpg", f"iris_{identity}_page"),
                ("iris.json", f"iris_{identity}_job"),
            ):
                output = subprocess.run(
                    [str(project / "bin/dir_hier_path"), physical], cwd=project,
                    check=True, capture_output=True, text=True,
                ).stdout.strip()
                if not output:
                    raise ValueError("empty BOINC download path")
                target = Path(output)
                target = (target if target.is_absolute() else project / target).resolve()
                if not target.is_relative_to((project / "download").resolve()):
                    raise ValueError("download path outside BOINC download directory")
                target.parent.mkdir(parents=True, exist_ok=True)
                raw = (unit / filename).read_bytes()
                if target.exists():
                    if target.read_bytes() != raw:
                        raise ValueError("existing download file conflict")
                else:
                    durable(target, raw)
                    target.chmod(0o664)
            durable(unit / "submitting", encoded({"workunit": f"usnm_iris_{identity}", "host_id": HOST_ID}))
            subprocess.run(
                [str(project / "bin/create_work"), *create_work_args(identity)],
                cwd=project, check=True,
            )
            (unit / "submitting").rename(unit / "submitted")
            submitted += 1
    finally:
        lock.rmdir()
    return {"submitted": submitted, "previously_submitted": skipped, "host_id": HOST_ID}


def edit_distance(a, b):
    previous = list(range(len(b) + 1))
    for i, left in enumerate(a, 1):
        current = [i]
        for j, right in enumerate(b, 1):
            current.append(min(previous[j] + 1, current[-1] + 1, previous[j - 1] + (left != right)))
        previous = current
    return previous[-1]


def terminal_results(path, campaign):
    if path is None:
        return {}
    rows = json.loads(path.read_text())
    if not isinstance(rows, list) or len(rows) > COUNT:
        raise ValueError("expected at most 100 BOINC result records")
    identities = {record["id"] for record in campaign["jobs"]}
    results = {}
    for row in rows:
        name = row["workunit_name"]
        identity = name.removeprefix("usnm_iris_")
        if (name != f"usnm_iris_{identity}" or identity not in identities
                or row["host_id"] != HOST_ID or row["batch"] != BATCH):
            raise ValueError("BOINC result outside authorized campaign or host")
        if identity in results:
            raise ValueError("multiple BOINC results for single-replica job")
        for field in ("server_state", "outcome", "validate_state"):
            if type(row[field]) is not int:
                raise ValueError("invalid BOINC result state")
        if not isinstance(row["stderr_out"], str):
            raise ValueError("invalid BOINC result stderr")
        results[identity] = row
    return results


def collect(root, seen, terminal=None):
    campaign = load_campaign(root)
    server = terminal_results(terminal, campaign)
    returned = {}
    for path in sorted(seen.glob("*.json")):
        if path.stat().st_size > 65536:
            raise ValueError("result over size limit")
        result = json.loads(path.read_text())
        identity = result.get("id")
        if identity not in {row["id"] for row in campaign["jobs"]}:
            continue
        job = json.loads((root / "units" / identity / "iris.json").read_text())
        validate_result(result, job)
        if identity in returned:
            raise ValueError("duplicate result for single-replica pilot job")
        returned[identity] = result
    rows = []
    total_errors = total_chars = 0
    for record in campaign["jobs"]:
        result = returned.get(record["id"])
        state = server.get(record["id"])
        row = {"id": record["id"], "crop_id": record["crop_id"],
               "status": result["status"] if result else "pending"}
        if state:
            row["boinc"] = {
                key: state[key] for key in
                ("server_state", "outcome", "validate_state", "stderr_out")
            }
            if state["server_state"] == 5:
                if state["outcome"] != 1 or state["validate_state"] == 2:
                    row["status"] = "boinc_error"
                elif not result:
                    row["status"] = "terminal_missing_output"
        if result and result["status"] == "ok":
            reference = " ".join(record["reference"].split())
            text = " ".join(result["text"].split())
            errors = edit_distance(reference, text)
            total_errors += errors
            total_chars += len(reference)
            row.update({"cer": errors / max(1, len(reference)), "reference": record["reference"],
                        "text": result["text"], "elapsed_ms": result["elapsed_ms"]})
        rows.append(row)
    report = {
        "jobs": COUNT, "returned": len(returned),
        "pending": sum(row["status"] == "pending" for row in rows),
        "ok": sum(row["status"] == "ok" for row in rows),
        "errors": sum(row["status"] in ("error", "boinc_error", "terminal_missing_output") for row in rows),
        "boinc_records": len(server),
        "boinc_terminal": sum(state["server_state"] == 5 for state in server.values()),
        "boinc_valid": sum(
            state["server_state"] == 5 and state["outcome"] == 1 and state["validate_state"] == 1
            for state in server.values()
        ),
        "normalized_cer": total_errors / total_chars if total_chars else None,
        "validation": "experimental single-host structural validation, not multi-host scientific consensus",
        "rows": rows,
    }
    (root / "report.json").write_bytes(encoded(report))
    return {key: value for key, value in report.items() if key != "rows"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    stage = commands.add_parser("prepare")
    stage.add_argument("--dataset", type=Path, required=True)
    stage.add_argument("--out", type=Path, required=True)
    stage.add_argument("--worker-image", required=True)
    send = commands.add_parser("submit")
    send.add_argument("--root", type=Path, required=True)
    send.add_argument("--project", type=Path, required=True)
    send.add_argument("--apply", action="store_true")
    inspect = commands.add_parser("collect")
    inspect.add_argument("--root", type=Path, required=True)
    inspect.add_argument("--seen", type=Path, required=True)
    inspect.add_argument("--terminal-results", type=Path,
                         help="read-only BOINC DB snapshot including failed result stderr")
    args = parser.parse_args()
    if args.command == "prepare":
        result = prepare(args.dataset, args.out, args.worker_image)
    elif args.command == "submit":
        result = submit(args.root, args.project, args.apply)
    else:
        result = collect(args.root, args.seen, args.terminal_results)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
