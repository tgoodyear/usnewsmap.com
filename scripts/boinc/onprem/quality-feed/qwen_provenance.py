#!/usr/bin/env python3
"""Reconstruct missing source bindings by exact original-to-native image reproduction."""
import argparse
from datetime import datetime, timezone
import hashlib
import io
import json
import os
from pathlib import Path
import re
import tempfile
import urllib.request


TRANSFORMS = ("reduced-gray", "full-gray", "full-rgb")
MAX_SCAN = 32 * 1024**2


def validate_target(target):
    from qwen_submit import source_fingerprint

    source = source_fingerprint(target["source"])
    if source != target["source"]:
        raise ValueError("recovery requires an exact source fingerprint")
    title, date, edition, sequence = source["doc_id"].split("_")
    prefix = f"keep/{source['batch']}/{title}/{date}/{edition[3:]}/{sequence[4:]}"
    if (not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,40}", target["campaign"])
            or any(not re.fullmatch(r"[a-f0-9]{64}", target[key])
                   for key in ("job_id", "job_sha256"))
            or type(target["workunit_id"]) is not int or target["workunit_id"] < 1
            or target["scan"] not in (prefix + ".jp2", prefix + ".jpg")
            or target["transform"] not in TRANSFORMS
            or type(target["jpeg"]["bytes"]) is not int or target["jpeg"]["bytes"] < 1
            or not re.fullmatch(r"[a-f0-9]{64}", target["jpeg"]["sha256"])):
        raise ValueError("invalid bounded provenance recovery target")


def derive_jpeg(raw, transform):
    from PIL import Image

    with Image.open(io.BytesIO(raw)) as image:
        if image.width * image.height > 100_000_000:
            raise ValueError("recovery image exceeds 100 megapixels")
        if transform == "reduced-gray" and image.format == "JPEG2000" and max(image.size) > 4000:
            image.reduce = 1
        with image.convert("RGB" if transform == "full-rgb" else "L") as converted:
            output = io.BytesIO()
            converted.save(output, "JPEG", quality=75)
            return output.getvalue()


def verify(target, raw):
    validate_target(target)
    if (len(raw) > MAX_SCAN or len(raw) != target["source"]["bytes"]
            or hashlib.sha256(raw).hexdigest() != target["source"]["sha256"]):
        raise ValueError("original scan does not match frozen source")
    jpeg = derive_jpeg(raw, target["transform"])
    if {"bytes": len(jpeg), "sha256": hashlib.sha256(jpeg).hexdigest()} != target["jpeg"]:
        raise ValueError("reproduced JPEG does not match native input")
    return {
        "schema": 1, "kind": "verified-reconstruction", "target": target,
        "verified_at": datetime.now(timezone.utc).isoformat(),
        "verification": decoder_info(),
    }


def decoder_info():
    from PIL import __version__, features

    return {"pillow": __version__, "openjpeg": features.version("jpg_2000"),
            "libjpeg": features.version("jpg"),
            "verifier_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}



def check_submitted(path, job_id):
    if not path.is_file() or path.read_bytes() != ("usnm_qwen_" + job_id).encode():
        raise ValueError("submitted marker does not bind to native job")


def recovered_binding(campaign, job, audit, member, submitted):
    path = campaign / "recovered-provenance" / job["id"] / (member["doc_id"] + ".json")
    if not path.exists():
        return None
    check_submitted(submitted, job["id"])
    proof = json.loads(path.read_bytes())
    target = proof["target"]
    validate_target(target)
    if (proof["schema"] != 1 or proof["kind"] != "verified-reconstruction"
            or not proof["verified_at"] or not proof["verification"]
            or target["campaign"] != campaign.name or target["job_id"] != job["id"]
            or target["job_sha256"] != audit["job_sha256"]
            or target["workunit_id"] != audit["workunit_id"]
            or target["source"]["doc_id"] != member["doc_id"]
            or target["jpeg"] != {k: member["input"][k] for k in ("bytes", "sha256")}):
        raise ValueError("reconstructed provenance does not bind to native audit")
    return {"schema": 1, "source": target["source"], "jpeg": target["jpeg"],
            "reconstructed_at": proof["verified_at"]}


def install(share, proofs):
    from qwen_submit import jobs

    pending = []
    seen = set()
    campaigns = {}
    for proof in proofs:
        target = proof["target"]
        validate_target(target)
        campaign = share / "qwen" / target["campaign"]
        if campaign not in campaigns:
            indexed = {}
            for path, job in jobs(campaign):
                indexed.setdefault(job["id"], []).append((path, job))
            campaigns[campaign] = (indexed, json.loads((campaign / "audit.json").read_bytes()))
        indexed, audits = campaigns[campaign]
        matches = indexed.get(target["job_id"], [])
        if len(matches) != 1:
            raise ValueError("recovery needs exactly one retained native job")
        path, job = matches[0]
        if job.get("pages"):
            raise ValueError("recovery currently supports only single-page native jobs")
        audit = audits[job["id"]]
        check_submitted(path.parent / "submitted", job["id"])
        member = next(p for p in job.get("pages") or [job] if p["doc_id"] == target["source"]["doc_id"])
        image = path.parent / "page.jpg"
        raw = image.read_bytes()
        if (hashlib.sha256(path.read_bytes()).hexdigest() != target["job_sha256"]
                or audit["job_sha256"] != target["job_sha256"]
                or audit["workunit_id"] != target["workunit_id"]
                or audit["batch"] != 8 or audit["min_quorum"] != 2 or audit["target_nresults"] != 2
                or not (path.parent / "submitted").is_file()
                or target["source"]["doc_id"] in audit.get("source_bindings", {})
                or target["jpeg"] != {k: member["input"][k] for k in ("bytes", "sha256")}
                or {"bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest()} != target["jpeg"]
                or proof["schema"] != 1 or proof["kind"] != "verified-reconstruction"
                or not proof["verified_at"] or not proof["verification"]):
            raise ValueError("recovery target differs from retained native evidence")
        output = campaign / "recovered-provenance" / job["id"] / (member["doc_id"] + ".json")
        if output in seen:
            raise ValueError("duplicate recovery target")
        seen.add(output)
        if output.exists() and json.loads(output.read_bytes()) != proof:
            raise ValueError("refusing to overwrite an existing recovery receipt")
        pending.append((output, proof))
    for path, proof in pending:
        path.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, prefix=".recovery-") as stream:
            try:
                json.dump(proof, stream, sort_keys=True)
                stream.flush()
                os.fsync(stream.fileno())
                os.link(stream.name, path)
            except FileExistsError:
                if json.loads(path.read_bytes()) != proof:
                    raise ValueError("concurrent recovery receipt differs")
        descriptor = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    return {"verified_receipts": len(pending), "original_audits_changed": False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("verify", "install"))
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--scan-server")
    parser.add_argument("--share", type=Path, default=Path("/project"))
    args = parser.parse_args()
    rows = json.loads(args.input.read_bytes())
    if not isinstance(rows, list) or not 1 <= len(rows) <= 500:
        parser.error("input must contain 1..500 explicit recovery records")
    if args.mode == "install":
        print(json.dumps(install(args.share, rows)))
        return
    if not args.output or not args.scan_server:
        parser.error("verify needs --output and --scan-server")
    proofs = []
    for target in rows:
        validate_target(target)
        with urllib.request.urlopen(args.scan_server.rstrip("/") + "/scans/" + target["scan"],
                                    timeout=60) as response:
            raw = response.read(MAX_SCAN + 1)
        proofs.append(verify(target, raw))
    with args.output.open("x") as stream:
        json.dump(proofs, stream, sort_keys=True)
        stream.flush()
        os.fsync(stream.fileno())
    print(json.dumps({"exact_original_to_native_matches": len(proofs), "output": str(args.output)}))


if __name__ == "__main__":
    main()
