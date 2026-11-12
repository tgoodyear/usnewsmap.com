#!/usr/bin/env python3
"""The held-out draws' exclusion list: every page of the publisher's validation archive.

    newseye_exclusions.py --dataset newseye-bnf --src DIR --out EXCLUDED.txt

DIR is a `gold.py public-fetch --subset all` directory. Its `ATR_ValidationSet_*` folder is read
by `gold.py public_regions`, and each page_id it yields is written once, sorted. That is a superset
of benchmark v1.1.0 `multilingual/all-52` (read from the validation archives) and of the Finnish
validation crops of the GPU studies.
"""
import argparse, hashlib, sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "scripts" / "ocr-survey"))
import gold  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--dataset", required=True)
ap.add_argument("--src", type=Path, required=True)
ap.add_argument("--out", type=Path, required=True)
a = ap.parse_args()
val = sorted(p for p in a.src.iterdir() if p.is_dir() and "Validation" in p.name)
if len(val) != 1:
    sys.exit(f"expected one validation folder under {a.src}, found {[p.name for p in val]}")
pages = sorted({r["page_id"] for r in gold.public_regions(a.dataset, val[0])})
a.out.write_text("".join(p + "\n" for p in pages), encoding="utf-8")
print(a.dataset, len(pages), "pages", hashlib.sha256(a.out.read_bytes()).hexdigest())
