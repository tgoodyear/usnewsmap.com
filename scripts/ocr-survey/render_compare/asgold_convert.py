#!/usr/bin/env python3
"""Converts the frozen en-asgold set (s9-heldout, American Stories gold) into a `loader.py --set-dir` (#2103).

    asgold_convert.py --frozen-dir en-asgold --alto-dir ALTO --exclude-file exclude.txt --out SET [--report r.json]

`gold.py public-convert` marks americanstories as not for scoring (its card is empty and the pages' sources were
undocumented when PUBLIC was written), so it can't feed the loader. The frozen set's README documents what was
missing: seven of the ten pages are matched to a Chronicling America page (asgold-ca-map.json), whose image rights
are public domain; three are not matched and their rights are UNVERIFIED. This converter keeps a page only when
  - its ALTO and page image (`<id>.xml`, `<id>.jpg` in --alto-dir) are held, and
  - the set matched it to a Chronicling America page, and
  - it isn't one of the Overproof pages in --exclude-file (LCCN, date and page number the same).
Every other page is dropped with its reason, in --report. Nothing is fetched. Text licence: Apache-2.0 (Hugging
Face repository metadata); images: Chronicling America, public domain. Region boxes are the frozen set's, in the
pixels of its page image; the loader maps the ALTO onto the same image by proportion.
"""

from __future__ import annotations

import argparse
import csv
import json
import re
import sys
from pathlib import Path

import paths  # noqa: F401  (sets sys.path)
import gold

LICENCE = ("Text: Apache-2.0 (Hugging Face repository metadata, American Stories gold). "
           "Images: Chronicling America, public domain (page matched to a LoC page in asgold-ca-map.json)")
CA_ID = re.compile(r"^(?P<lccn>[a-z]{2}\d+)_(?P<date>\d{4}-\d{2}-\d{2})_ed-(?P<ed>\d+)_seq-(?P<seq>\d+)$")
EXCLUDE_KEY = re.compile(r"^(?P<lccn>[a-z]{2}\d+)-[^-]+-(?P<date>\d{8})(?P<ed>\d{2})-(?P<seq>\d{4})$")


def read_exclude(path: Path) -> list[dict]:
    keys = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.split("#")[0].strip()
        if not line:
            continue
        m = EXCLUDE_KEY.match(line)
        if not m:
            raise SystemExit(f"{path}: unexpected exclude key {line!r}")
        keys.append({"key": line, "lccn": m["lccn"], "date": m["date"], "num": int(m["seq"])})
    return keys


def overlap(ca_page: str, file_number: int, exclude: list[dict]) -> str | None:
    """The exclude key this page matches (same LCCN and date, and the same page number as the file or the sequence)."""
    m = CA_ID.match(ca_page)
    if not m:
        return None
    date = m["date"].replace("-", "")
    for k in exclude:
        if k["lccn"] == m["lccn"] and k["date"] == date and k["num"] in (file_number, int(m["seq"])):
            return k["key"]
    return None


def convert(frozen: Path, alto_dir: Path, exclude: list[dict]) -> tuple[list[dict], list[dict], dict]:
    with (frozen / "selection.tsv").open(encoding="utf-8") as f:
        selection = list(csv.DictReader(f, delimiter="\t"))
    regions = gold.read_jsonl(frozen / "regions.jsonl")
    report: dict = {"pages": [], "licence": LICENCE}
    keep: dict[str, dict] = {}
    for s in selection:
        pid, ca = s["id"], s["ca_page"]
        file_number = int(s["page"].rsplit("-", 1)[-1])
        row = {"id": pid, "americanstories": s["page"], "ca_page": ca or None}
        if not ca:
            row |= {"used": False, "reason": "no Chronicling America page matched; image rights unverified; no ALTO held"}
        elif not ((alto_dir / f"{pid}.xml").is_file() and (alto_dir / f"{pid}.jpg").is_file()):
            row |= {"used": False, "reason": "ALTO or image not held in --alto-dir"}
        elif (hit := overlap(ca, file_number, exclude)):
            row |= {"used": False, "reason": f"overlaps Overproof page {hit}"}
        else:
            row |= {"used": True}
            keep[pid] = s
        report["pages"].append(row)
    manifest, crops = [], []
    for r in regions:
        if r["id"] not in keep or not r.get("transcribed") or not r["text"].strip():
            continue
        n = len(manifest) + 1
        crop_id = f"asgold-{n:04d}"
        manifest.append({
            "crop_id": crop_id, "page_id": r["id"], "group": "asgold", "lang": "eng", "era": None, "stratum": "asgold",
            "box": r["box"], "image": None, "source_image": f"{r['id']}.jpg", "region": r["region"],
            "region_type": r["type"], "ca_page": keep[r["id"]]["ca_page"],
            "source_dataset": "americanstories", "source_license": LICENCE, "for_scoring": True})
        crops.append({"crop_id": crop_id, "group": "asgold", "lang": "eng", "image": None, "transcription": r["text"],
                      "transcriber": "american stories gold: hand transcription", "truth_source": "americanstories",
                      "for_scoring": True})
    report["crops"] = len(manifest)
    report["pages_used"] = sum(p["used"] for p in report["pages"])
    return manifest, crops, report


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--frozen-dir", type=Path, required=True, help="the frozen en-asgold directory")
    ap.add_argument("--alto-dir", type=Path, required=True, help="<id>.xml and <id>.jpg per page")
    ap.add_argument("--exclude-file", type=Path, required=True, help="Overproof page keys to drop")
    ap.add_argument("--out", type=Path, required=True, help="the set directory for loader.py")
    ap.add_argument("--report", type=Path, help="pages used and dropped (JSON)")
    a = ap.parse_args(argv)
    manifest, crops, report = convert(a.frozen_dir, a.alto_dir, read_exclude(a.exclude_file))
    a.out.mkdir(parents=True, exist_ok=True)
    gold.write_jsonl(a.out / "manifest.jsonl", manifest)
    gold.write_jsonl(a.out / "crops.jsonl", crops)
    lic = a.out / "LICENSES"
    lic.mkdir(exist_ok=True)
    for p in sorted((a.frozen_dir / "LICENSES").iterdir()):
        (lic / p.name).write_bytes(p.read_bytes())
    (lic / "SOURCE.json").write_text(json.dumps({"dataset": "americanstories (en-asgold)", "license": LICENCE,
                                                 "url": "https://huggingface.co/datasets/dell-research-harvard/AmericanStoriesTraining"},
                                                indent=1) + "\n", encoding="utf-8")
    if a.report:
        a.report.write_text(json.dumps(report, indent=1) + "\n", encoding="utf-8")
    for p in report["pages"]:
        print(p["id"], "used" if p["used"] else f"dropped: {p['reason']}", file=sys.stderr)
    print(f"{len(manifest)} crops from {report['pages_used']} pages", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
