#!/usr/bin/env python3
"""Read a checksum-pinned LoC OCR archive without extracting its filesystem paths."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import re
import tarfile
import xml.etree.ElementTree as ET

PAGE = re.compile(r"(sn\d+)/(\d{4})/(\d{2})/(\d{2})/ed-(\d+)/seq-(\d+)/ocr\.(xml|txt)\Z")


def software_summary(pages):
    counts = Counter((entry.get("softwareName"), entry.get("softwareVersion"))
                     for page in pages for entry in page["software"])
    return [{"name": name, "version": version, "pages": count}
            for (name, version), count in sorted(
                counts.items(), key=lambda item: tuple(value or "" for value in item[0]))]


def inspect(path, expected_sha256):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1 << 20), b""):
            digest.update(chunk)
    if digest.hexdigest() != expected_sha256:
        raise ValueError("archive checksum mismatch")
    pages = {}
    total = 0
    with tarfile.open(path, "r|bz2") as archive:
        for member in archive:
            match = PAGE.fullmatch(member.name)
            if not member.isfile() or not match:
                raise ValueError("unexpected archive member: " + member.name)
            if member.size > 16 << 20:
                raise ValueError("oversized OCR member")
            total += member.size
            if total > 512 << 20:
                raise ValueError("archive exceeds uncompressed size bound")
            lccn, year, month, day, edition, seq, kind = match.groups()
            doc_id = f"{lccn}_{year}-{month}-{day}_ed-{edition}_seq-{seq}"
            row = pages.setdefault(doc_id, {"doc_id": doc_id, "artifacts": {},
                                            "source_image_identity": "unverified"})
            if kind in row["artifacts"]:
                raise ValueError("duplicate page artifact")
            data = archive.extractfile(member).read()
            row["artifacts"][kind] = {"path": member.name, "sha256": hashlib.sha256(data).hexdigest(),
                                      "bytes": len(data)}
            if kind == "xml":
                root = ET.fromstring(data)
                if root.tag.rsplit("}", 1)[-1] != "alto":
                    raise ValueError("XML member is not ALTO")
                row["software"] = [{child.tag.rsplit("}", 1)[-1]: child.text
                                    for child in software}
                                   for software in root.findall(".//{*}processingSoftware")]
                row["measurement_units"] = [unit.text for unit in root.findall(".//{*}MeasurementUnit")]
                row["page_geometry"] = [page.attrib for page in root.findall(".//{*}Page")]
    if not pages or any(set(page["artifacts"]) != {"xml", "txt"} for page in pages.values()):
        raise ValueError("missing ALTO/text page pairs")
    return {"format": "usnm-loc-ocr-artifacts/1", "archive_sha256": expected_sha256,
            "pages": [pages[key] for key in sorted(pages)], "page_count": len(pages),
            "source_image_identity": "not established by OCR archive",
            "uncompressed_bytes": total}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--metadata", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--repair-pages", type=Path)
    parser.add_argument("--summary-out", type=Path)
    parser.add_argument("--retrieved-at", required=True)
    args = parser.parse_args()
    data = args.metadata.read_bytes()
    metadata = json.loads(data)
    result = inspect(args.archive, metadata["sha256"])
    if args.archive.stat().st_size != metadata["size"] or result["page_count"] != metadata["page_count"]:
        raise ValueError("archive size/page count differs from metadata")
    result["metadata_sha256"] = hashlib.sha256(data).hexdigest()
    result["publication_metadata"] = metadata
    result["retrieved_at"] = args.retrieved_at
    if args.repair_pages:
        published = {page["doc_id"] for page in result["pages"]}
        matches = []
        digest = hashlib.sha256()
        with args.repair_pages.open("rb") as source:
            for line in source:
                digest.update(line)
                row = json.loads(line)
                if row["doc_id"] in published:
                    matches.append(row)
        result["repair_overlap"] = {
            "repair_input_sha256": digest.hexdigest(), "pages": matches, "page_count": len(matches),
            "status": "exact canonical page ID, not source-image registration or OCR equality proof"}
    with args.out.open("x") as output:
        json.dump(result, output, indent=2, sort_keys=True)
        output.write("\n")
    software = software_summary(result["pages"])
    if args.summary_out:
        result["page_inventory_sha256"] = hashlib.sha256(
            json.dumps(result.pop("pages"), sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        result["software_counts"] = software
        with args.summary_out.open("x") as output:
            json.dump(result, output, indent=2, sort_keys=True)
            output.write("\n")
    print(json.dumps({"pages": result["page_count"], "software": software}))


if __name__ == "__main__":
    main()
