#!/usr/bin/env python3
"""Cache selected pages' original ALTO from retained LoC archives; no network requests.

Run on the archive host with --pages FILE (or --pages - for JSONL on stdin),
--archives DIR and --out DIR. Input is quality_feed.py select's JSONL.
Only named regular XML members are read; nothing is extracted as a filesystem path.
Missing pages are reported, not replaced with empty ALTO.
"""
import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import tarfile

DOC = re.compile(r"^[a-z0-9]+_\d{4}-\d{2}-\d{2}_ed-\d+_seq-\d+$")
BATCH = re.compile(r"^[a-z]+_[a-zA-Z0-9]+$")
MAX_ALTO = 32 * 1024 * 1024


def member_doc(name):
    while name.startswith("./"):
        name = name[2:]
    fields = name.split("/")
    if name.startswith("/") or any(p in ("", ".", "..") for p in fields) or len(fields) < 5:
        return None
    ed, seq, filename = fields[-3:]
    if filename != "ocr.xml" or not ed.startswith("ed-") or not seq.startswith("seq-"):
        return None
    if re.fullmatch(r"\d{4}-\d{2}-\d{2}", fields[-4]):
        title, date = fields[-5:-3]
    elif len(fields) >= 7:
        title = fields[-7]
        date = "-".join(fields[-6:-3])
    else:
        return None
    try:
        dt.date.fromisoformat(date)
    except ValueError:
        return None
    doc = f"{title}_{date}_{ed}_{seq}"
    return doc if DOC.fullmatch(doc) else None


def atomic(path, raw):
    tmp = path.with_suffix(path.suffix + ".tmp")
    with tmp.open("wb") as f:
        f.write(raw)
        f.flush()
        os.fsync(f.fileno())
    tmp.replace(path)


def extract(pages, archives, out):
    out.mkdir(parents=True, exist_ok=True)
    groups = {}
    for row in pages:
        doc, batch, version = row["doc_id"], row["batch"], int(row["batch_version"])
        if not DOC.fullmatch(doc) or not BATCH.fullmatch(batch) or not 1 <= version <= 99:
            raise ValueError("invalid archive page identity")
        groups.setdefault((batch, version), set()).add(doc)
    report = {"cached": [], "missing": []}
    for (batch, version), docs in sorted(groups.items()):
        archive = archives / f"{batch}_ver{version:02d}.tar.bz2"
        if not archive.is_file():
            report["missing"].extend(sorted(docs))
            continue
        signature = {"archive": archive.name, "archive_bytes": archive.stat().st_size,
                     "archive_mtime_ns": archive.stat().st_mtime_ns}
        wanted = set(docs)
        for doc in sorted(docs):
            receipt = out / (doc + ".provenance.json")
            xml = out / (doc + ".xml")
            if receipt.exists() and xml.exists():
                previous = json.loads(receipt.read_text())
                if (all(previous.get(k) == v for k, v in signature.items())
                        and previous.get("doc_id") == doc
                        and hashlib.sha256(xml.read_bytes()).hexdigest() == previous.get("sha256")):
                    wanted.remove(doc)
                    report["cached"].append(doc)
        if wanted:
            with tarfile.open(archive, "r|bz2") as packed:
                for member in packed:
                    doc = member_doc(member.name)
                    if doc not in wanted:
                        continue
                    if not member.isfile() or not 0 < member.size <= MAX_ALTO:
                        raise ValueError(f"{archive.name}: invalid ALTO member {member.name}")
                    with packed.extractfile(member) as f:
                        raw = f.read(MAX_ALTO + 1)
                    if len(raw) != member.size:
                        raise ValueError(f"{doc}: truncated archive ALTO")
                    provenance = {**signature, "member": member.name, "doc_id": doc,
                                  "sha256": hashlib.sha256(raw).hexdigest()}
                    atomic(out / (doc + ".xml"), raw)
                    atomic(out / (doc + ".provenance.json"), json.dumps(provenance, sort_keys=True).encode())
                    wanted.remove(doc)
                    report["cached"].append(doc)
                    if not wanted:
                        break
        report["missing"].extend(sorted(wanted))
    return report


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--pages", required=True)
    ap.add_argument("--archives", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    args = ap.parse_args()
    with (sys.stdin if args.pages == "-" else open(args.pages)) as f:
        pages = [json.loads(line) for line in f if line.strip()]
    print(json.dumps(extract(pages, args.archives, args.out), sort_keys=True))


if __name__ == "__main__":
    main()
