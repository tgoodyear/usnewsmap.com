"""Frozen, content-addressed subsets of a retained Qwen READY handoff."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil

SOURCE_KEYS = ("doc_id", "batch", "batch_version", "bytes", "sha256")
DOC = r"sn\d+_\d{4}-\d{2}-\d{2}_ed-\d+_seq-\d+"
NAME = r"[a-z0-9][a-z0-9-]{0,40}"


def encoded(value):
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False) + "\n").encode()


def digest(value):
    return hashlib.sha256(encoded(value)).hexdigest()


def file_hash(path):
    with path.open("rb") as stream:
        hasher = hashlib.sha256()
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            hasher.update(block)
        return hasher.hexdigest()


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def save(path, value):
    mkdir(path.parent)
    pending = path.with_name(path.name + ".pending")
    with pending.open("wb") as stream:
        stream.write(encoded(value))
        stream.flush()
        os.fsync(stream.fileno())
    pending.replace(path)
    sync_dir(path.parent)


def mkdir(path):
    if not path.exists():
        mkdir(path.parent)
        path.mkdir()
        sync_dir(path.parent)


def freeze(path, value):
    if path.exists():
        if json.loads(path.read_text()) != value:
            raise ValueError("changed frozen shipment evidence: " + str(path))
    else:
        save(path, value)


def freeze_file(path, incoming, expected):
    """Retain the supplied bytes, not a re-encoded provenance receipt."""
    check_file(incoming, expected)
    if path.exists():
        check_file(path, expected)
        sync_dir(path.parent)
        return
    mkdir(path.parent)
    pending = path.with_name(path.name + ".pending")
    shutil.copyfile(incoming, pending)
    check_file(pending, expected)
    with pending.open("rb") as stream:
        os.fsync(stream.fileno())
    pending.replace(path)
    sync_dir(path.parent)


def publication_evidence(*records):
    records = [row for row in records if row]
    native = any(row.get("native_acknowledged") for row in records)
    quality = any(row.get("quality_acknowledged") for row in records)
    partial = native or quality or any(row.get("status") == "partial" for row in records)
    attempted = partial or any(row.get("publication_attempted")
                               or row.get("status") in ("uncertain", "acknowledged") for row in records)
    return {"native_acknowledged": native, "quality_acknowledged": quality,
            "publication_attempted": attempted,
            "status": "partial" if partial else "uncertain" if attempted else "deferred"}


def source(page):
    if (not isinstance(page, dict)
            or not re.fullmatch(DOC, page.get("doc_id", ""))
            or not re.fullmatch(r"[a-z]+_[a-zA-Z0-9]+", page.get("batch", ""))
            or type(page.get("batch_version")) is not int or not 1 <= page["batch_version"] <= 99
            or type(page.get("bytes")) is not int or page["bytes"] < 1
            or not re.fullmatch(r"[a-f0-9]{64}", page.get("sha256", ""))):
        raise ValueError("invalid shipment source fingerprint")
    return {key: page[key] for key in SOURCE_KEYS}


def check_file(path, expected):
    if (path.is_symlink() or not path.is_file() or path.stat().st_size != expected["bytes"]
            or file_hash(path) != expected["sha256"]):
        raise ValueError("changed shipment input: " + str(path))


def quality_alto(raw):
    # Match quality stage's existing normalize_alto_ns, without changing source bytes.
    old = b"http://schema.ccs-gmbh.com/ALTO"
    offset = raw[:4096].find(old)
    if offset >= 0:
        raw = raw[:offset] + b"http://www.loc.gov/standards/alto/ns-v2#" + raw[offset + len(old):]
    return {"bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest()}


def chunk_snapshot(chunk, campaign):
    """Freeze original order, original-source fingerprints, and every transferred byte."""
    if not re.fullmatch(NAME, campaign) or not re.fullmatch(r"c\d{5}", chunk.name):
        raise ValueError("invalid parent shipment identity")
    sums = {}
    for line in (chunk / "SHA256SUMS").read_text().splitlines():
        sha, rel = line.split("  ", 1)
        if (rel in sums or not re.fullmatch(r"[a-f0-9]{64}", sha)
                or not re.fullmatch(
                    rf"(?:sample.jsonl|langs.txt|scans/{DOC}\.(?:jpg|xml)|"
                    rf"texts/{DOC}\.(?:json|ours.xml|image-provenance.json|loc-provenance.json))", rel)):
            raise ValueError("invalid parent checksum membership")
        sums[rel] = {"sha256": sha, "bytes": (chunk / rel).stat().st_size}
        check_file(chunk / rel, sums[rel])
    if not {"sample.jsonl", "langs.txt"} <= sums.keys():
        raise ValueError("unbound parent sample or languages")
    rows = [json.loads(line) for line in (chunk / "sample.jsonl").read_text().splitlines()]
    if not 1 <= len(rows) <= 490 or int((chunk / "READY").read_text()) != len(rows):
        raise ValueError("invalid READY membership count")
    members = []
    for row in rows:
        doc = row["doc_id"]
        if not re.fullmatch(DOC, doc) or not re.fullmatch(r"[a-z]{3}", row["lang"]):
            raise ValueError("invalid source sample")
        required = {f"scans/{doc}.jpg", f"scans/{doc}.xml", f"texts/{doc}.json",
                    f"texts/{doc}.image-provenance.json"}
        if not required <= sums.keys():
            raise ValueError("legacy/unbound READY source; retain parent")
        binding = json.loads((chunk / f"texts/{doc}.image-provenance.json").read_text())
        original = source(binding["source"])
        if (binding.get("schema") != 1 or original != binding["source"]
                or any(row[key] != original[key] for key in ("doc_id", "batch", "batch_version"))
                or binding["jpeg"] != sums[f"scans/{doc}.jpg"]):
            raise ValueError("source binding does not match frozen sample/JPEG")
        files = {rel: value for rel, value in sums.items()
                 if rel.startswith((f"scans/{doc}.", f"texts/{doc}."))}
        members.append({"source": original, "sample": row, "files": files,
                        "quality_alto": quality_alto((chunk / f"scans/{doc}.xml").read_bytes())})
    if len({member["source"]["doc_id"] for member in members}) != len(members):
        raise ValueError("duplicate parent source")
    covered = {rel for member in members for rel in member["files"]} | {"sample.jsonl", "langs.txt"}
    if covered != sums.keys():
        raise ValueError("unlisted source in parent checksum membership")
    return {"schema": 1, "campaign": campaign, "chunk": chunk.name,
            "checksums_sha256": file_hash(chunk / "SHA256SUMS"), "members": members}


def capacity(state, free_bytes, *, max_ready=1000, max_unresolved=2000,
             min_free_gb=50, reserve_mib=64):
    return max(0, min(500, max_ready // 2 - state["fresh_pairs"],
                      max_unresolved - state["unresolved_native"],
                      (min(state["free_bytes"], free_bytes) - min_free_gb * 1024**3)
                      // (reserve_mib * 1024**2)))


def manifest(parent, members, recipe):
    value = {"schema": 1, "parent": {
        "id": digest(parent), "campaign": parent["campaign"], "chunk": parent["chunk"],
        "sources": [member["source"] for member in parent["members"]]},
        "members": members, "recipe": recipe}
    value["id"] = digest(value)
    validate(value)
    return value


def validate(value):
    body = {key: item for key, item in value.items() if key != "id"}
    if (value.get("schema") != 1 or value.get("id") != digest(body)
            or set(body) != {"schema", "parent", "members", "recipe"}):
        raise ValueError("changed frozen transaction membership")
    parent, members, recipe = value["parent"], value["members"], value["recipe"]
    if (not re.fullmatch(NAME, parent["campaign"]) or not re.fullmatch(r"c\d{5}", parent["chunk"])
            or not re.fullmatch(r"[a-f0-9]{64}", parent["id"])
            or not 1 <= len(parent["sources"]) <= 490 or not 1 <= len(members) <= 490
            or not re.fullmatch(NAME, recipe["campaign"])
            or recipe["prompt_version"] not in (1, 2, 3, 4)
            or not re.fullmatch(r"sha256:[a-f0-9]{64}", recipe["engine_image"])
            or not re.fullmatch(r"[a-z0-9_,-]+", recipe["readers"])
            or type(recipe["bundle_size"]) is not int or not 1 <= recipe["bundle_size"] <= 500):
        raise ValueError("invalid transaction recipe or parent")
    originals = [source(page) for page in parent["sources"]]
    if len({page["doc_id"] for page in originals}) != len(originals):
        raise ValueError("duplicate parent membership")
    indexes = []
    for member in members:
        original = source(member["source"])
        if original != member["source"] or original not in originals:
            raise ValueError("subset is not bound to original parent")
        indexes.append(originals.index(original))
        doc = original["doc_id"]
        row = member["sample"]
        if (any(row[key] != original[key] for key in ("doc_id", "batch", "batch_version"))
                or not re.fullmatch(r"[a-z]{3}", row["lang"])):
            raise ValueError("subset sample changed")
        allowed = {f"scans/{doc}.{ext}" for ext in ("jpg", "xml")} | {
            f"texts/{doc}.{ext}" for ext in ("json", "ours.xml", "image-provenance.json", "loc-provenance.json")}
        required = allowed - {f"texts/{doc}.ours.xml", f"texts/{doc}.loc-provenance.json"}
        if not required <= member["files"].keys() <= allowed:
            raise ValueError("subset payload membership changed")
        for info in [*member["files"].values(), member["quality_alto"]]:
            if (set(info) != {"bytes", "sha256"} or type(info["bytes"]) is not int or info["bytes"] < 0
                    or not re.fullmatch(r"[a-f0-9]{64}", info["sha256"])):
                raise ValueError("invalid subset payload fingerprint")
    if indexes != sorted(set(indexes)):
        raise ValueError("subset order/identity changed")
    return value


def materialize(chunk, target, value):
    validate(value)
    mkdir(target)
    freeze(target / "manifest.json", value)
    for member in value["members"]:
        for rel, info in member["files"].items():
            check_file(chunk / rel, info)
            path = target / rel
            mkdir(path.parent)
            if path.exists():
                check_file(path, info)
            else:
                pending = path.with_name(path.name + ".pending")
                shutil.copyfile(chunk / rel, pending)
                with pending.open("rb") as stream:
                    os.fsync(stream.fileno())
                pending.replace(path)
                sync_dir(path.parent)
    (target / "sample.jsonl").write_bytes(b"".join(encoded(member["sample"]) for member in value["members"]))
    (target / "langs.txt").write_text(",".join(sorted({m["sample"]["lang"] for m in value["members"]})) + "\n")
    verify_payload(target, value)
    sync_payload(target)


def sync_payload(path):
    paths = list(path.rglob("*"))
    for file in paths:
        if file.is_file():
            with file.open("rb") as stream:
                os.fsync(stream.fileno())
    for directory in sorted((p for p in paths if p.is_dir()), key=lambda p: len(p.parts), reverse=True):
        sync_dir(directory)
    sync_dir(path)


def verify_payload(path, value):
    validate(value)
    expected = {"manifest.json", "sample.jsonl", "langs.txt"}
    for member in value["members"]:
        for rel, info in member["files"].items():
            expected.add(rel)
            check_file(path / rel, info)
        binding = json.loads((path / f"texts/{member['source']['doc_id']}.image-provenance.json").read_text())
        if (binding.get("schema") != 1 or binding.get("source") != member["source"]
                or binding.get("jpeg") != member["files"][f"scans/{member['source']['doc_id']}.jpg"]):
            raise ValueError("subset provenance changed")
        if quality_alto((path / f"scans/{member['source']['doc_id']}.xml").read_bytes()) != member["quality_alto"]:
            raise ValueError("prepared ALTO binding changed")
    actual = {str(p.relative_to(path)) for p in path.rglob("*") if not p.is_dir()}
    if actual != expected or any(p.is_symlink() for p in path.rglob("*")):
        raise ValueError("transfer contains files outside frozen subset")
    if (path / "sample.jsonl").read_bytes() != b"".join(encoded(m["sample"]) for m in value["members"]):
        raise ValueError("transferred sample changed")
    if (path / "langs.txt").read_text() != ",".join(sorted({m["sample"]["lang"] for m in value["members"]})) + "\n":
        raise ValueError("transferred languages changed")
