#!/usr/bin/env python3
"""LoC's reading of every Chronicling America crop in a gold.py manifest, as scorer hypotheses (#968).

    loc_column.py --manifest DIR/manifest.jsonl --alto-dir DIR --out NEW_DIR
                  [--image-dir DIR] [--alto-crop-bin usnm-ocr-survey]

For each crop on a Chronicling America page, runs `usnm-ocr-survey alto-crop` on LoC's ALTO for the
page, the crop's `box` and the page image's size, and keeps the clipped ALTO's text (`alto_text`):
the words whose box centre is in the crop, in the ALTO's reading order, a kept line hyphen on its
line's end. That is the rule docs/ocr/gold-set.md describes under "A crop against its text"; this
script only loops over the manifest.

Chronicling America rows are those of `gold.py public-convert --dataset overproof-ca` (page_id
`overproof-ca-<key>`, files `<key>.xml` and `<key>.jpg` as `public-fetch` writes them under `loc/`)
and those of `gold.py cut` (page_id `<lccn>_<date>_ed-<n>_seq-<n>`, files `<page_id>.xml` and
`<page_id>.jpg` in the sample's `alto/` and `images/`). Other rows have no LoC text and are skipped.

The page image is checked against the manifest's `page_image_sha256` and only its JPEG header is
read, for the size the box is in. Writes, into --out (which must not exist):

  outputs.jsonl   one row {"crop_id", "id", "text"} per aligned crop: `crop_id` for gold.py and
                  `usnm-ocr-survey score`, `id` for `bench-score` and the GPU studies' scorer
  loc-column.json per-crop status (aligned, missing, error, not-chronicling-america), the ALTO's
                  SHA-256 and processing software, and the SHA-256 of the manifest, the outputs and
                  the alto-crop binary

A crop with no row in outputs.jsonl is scored as empty by every scorer. Exit status 1 when any
Chronicling America crop is missing or failed, after writing both files.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import re
import shutil
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import gold  # noqa: E402

_spec = importlib.util.spec_from_file_location("loc_align", HERE.parent / "ocr-bench" / "loc_align.py")
loc_align = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(loc_align)

FORMAT = "usnm-loc-column/1"
POLICY = "alto-crop: word centres in box, ALTO reading order, kept line hyphens"
OVERPROOF_PREFIX = "overproof-ca-"
MAX_PAGE_IMAGE = 128 << 20
MAX_ALTO = 64 << 20


def page_stem(row: dict) -> str | None:
    """The file stem of the row's page under --alto-dir and --image-dir, or None off Chronicling America."""
    pid = row.get("page_id") or ""
    dataset = row.get("source_dataset")
    if dataset == "overproof-ca" and pid.startswith(OVERPROOF_PREFIX):
        return pid[len(OVERPROOF_PREFIX):]
    if dataset is None and gold.PAGE_ID.match(pid):
        return pid
    return None


def software(xml: bytes) -> list[dict]:
    """The ALTO's processingSoftware entries, as loc_archive.py records them."""
    root = ET.fromstring(xml)
    return [{child.tag.rsplit("}", 1)[-1]: child.text for child in s}
            for s in root.findall(".//{*}processingSoftware")]


def alto_crop(binary: str, alto: Path, box: list, size: tuple[int, int], empty: Path) -> dict:
    x0, y0, x1, y1 = box
    out = subprocess.run(
        [binary, "alto-crop", "--alto", str(alto), "--box", f"{x0},{y0},{x1},{y1}", "--text", str(empty),
         "--image-width", str(size[0]), "--image-height", str(size[1])],
        capture_output=True, text=True, check=False)
    if out.returncode != 0:
        raise ValueError(f"alto-crop failed: {out.stderr.strip()[-500:]}")
    result = json.loads(out.stdout)
    if not isinstance(result.get("alto_text"), str):
        raise ValueError("alto-crop printed no alto_text (an older usnm-ocr-survey?)")
    return result


def column(rows: list[dict], alto_dir: Path, image_dir: Path, binary: str) -> tuple[list[dict], list[dict]]:
    """(outputs, statuses) for the manifest's rows, in manifest order."""
    outputs, statuses = [], []
    with tempfile.TemporaryDirectory() as tmp:
        empty = Path(tmp) / "empty.txt"
        empty.write_text("", encoding="utf-8")
        for row in rows:
            cid = row["crop_id"]
            stem = page_stem(row)
            if stem is None:
                statuses.append({"crop_id": cid, "status": "not-chronicling-america"})
                continue
            alto, image = alto_dir / f"{stem}.xml", image_dir / f"{stem}.jpg"
            if not alto.is_file() or not image.is_file():
                statuses.append({"crop_id": cid, "status": "missing", "page_id": row["page_id"],
                                 "reason": "no ALTO" if not alto.is_file() else "no page image"})
                continue
            try:
                box = row.get("box")
                if not (isinstance(box, list) and len(box) == 4):
                    raise ValueError("row has no box")
                want = row.get("page_image_sha256")
                if not want:
                    raise ValueError("row has no page_image_sha256")
                size = loc_align.jpeg_dimensions(loc_align.checked_file(image, want, MAX_PAGE_IMAGE))
                xml = alto.read_bytes()
                if len(xml) > MAX_ALTO:
                    raise ValueError("ALTO exceeds size bound")
                result = alto_crop(binary, alto, box, size, empty)
                entry = {"crop_id": cid, "status": "aligned", "page_id": row["page_id"],
                         "alto": alto.name, "alto_sha256": hashlib.sha256(xml).hexdigest(),
                         "alto_software": software(xml), "page_image_sha256": want,
                         "image_size": list(size), "box": box, "frame": result.get("frame"),
                         "clipped_words": result.get("clipped_words")}
            except (ValueError, KeyError, OSError, ET.ParseError, json.JSONDecodeError) as e:
                # The checksum message names no file; say which.
                reason = re.sub(r"^input ", "page image ", str(e))
                statuses.append({"crop_id": cid, "status": "error", "page_id": row["page_id"], "reason": reason})
                continue
            outputs.append({"crop_id": cid, "id": cid, "text": result["alto_text"]})
            statuses.append(entry)
    return outputs, statuses


def binary_sha256(binary: str) -> str | None:
    """The SHA-256 of the alto-crop binary that ran, for the reader's identity."""
    path = shutil.which(binary)
    return gold.sha256(Path(path).read_bytes()) if path else None


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--manifest", type=Path, required=True, help="gold.py manifest.jsonl")
    ap.add_argument("--alto-dir", type=Path, required=True, help="LoC ALTO per page, <stem>.xml")
    ap.add_argument("--image-dir", type=Path, help="page images, <stem>.jpg (default: --alto-dir)")
    ap.add_argument("--out", type=Path, required=True, help="new directory for outputs.jsonl and loc-column.json")
    ap.add_argument("--alto-crop-bin", default="usnm-ocr-survey", help="the usnm-ocr-survey binary")
    a = ap.parse_args(argv)
    if a.out.exists():
        print(f"--out must not exist: {a.out}", file=sys.stderr)
        return 2
    manifest_bytes = a.manifest.read_bytes()
    rows = gold.read_jsonl(a.manifest)
    ids = [r["crop_id"] for r in rows]
    if len(ids) != len(set(ids)):
        print("duplicate crop_id in the manifest", file=sys.stderr)
        return 2
    outputs, statuses = column(rows, a.alto_dir, a.image_dir or a.alto_dir, a.alto_crop_bin)
    counts = dict(sorted(Counter(s["status"] for s in statuses).items()))
    a.out.mkdir(parents=True)
    out_bytes = "".join(json.dumps(r, ensure_ascii=False) + "\n" for r in outputs).encode("utf-8")
    (a.out / "outputs.jsonl").write_bytes(out_bytes)
    report = {"format": FORMAT, "reader": "loc-alto", "policy": POLICY,
              "alto_crop_sha256": binary_sha256(a.alto_crop_bin),
              "manifest_sha256": hashlib.sha256(manifest_bytes).hexdigest(),
              "outputs_sha256": hashlib.sha256(out_bytes).hexdigest(),
              "counts": counts, "crops": statuses}
    (a.out / "loc-column.json").write_text(json.dumps(report, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps(counts))
    return 1 if counts.get("missing") or counts.get("error") else 0


if __name__ == "__main__":
    sys.exit(main())
