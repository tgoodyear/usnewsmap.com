#!/usr/bin/env python3
"""The 20 Reichsanzeiger pages inside benchmark v1.0.0 `all-700`, as gold.py page_ids.

    rz_exclusions.py --src DIR --out EXCLUDED.txt [--pages 20]

`gold.py public-fetch --dataset reichsanzeiger` shuffles the archive's imageurls.list with
Random("public-fetch") and takes the first --pages (20 by default). Those are the pages the
GPU studies' all-700 came from, so `de-rz81` is the rest: this file is what to exclude.
"""
import argparse, random
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("--src", type=Path, required=True)
ap.add_argument("--out", type=Path, required=True)
ap.add_argument("--pages", type=int, default=20)
a = ap.parse_args()
lists = sorted((a.src / "gt").rglob("imageurls.list"))
lines = [x.split() for x in lists[0].read_text().splitlines() if x.strip()]
random.Random("public-fetch").shuffle(lines)
stems = sorted(f"reichsanzeiger-{Path(name).stem}" for _, name in lines[: a.pages])
a.out.write_text("".join(s + "\n" for s in stems), encoding="utf-8")
print(len(stems), "pages")
