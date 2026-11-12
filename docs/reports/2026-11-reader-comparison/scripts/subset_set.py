#!/usr/bin/env python3
"""Copy a gold.py-format set to a new directory, keeping or dropping named crops.

    subset_set.py --src DIR --out NEW_DIR (--drop ID... | --keep-file IDS.txt)
"""
import argparse, json, shutil, sys
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("--src", type=Path, required=True)
ap.add_argument("--out", type=Path, required=True)
ap.add_argument("--drop", nargs="*", default=[])
ap.add_argument("--keep-file", type=Path)
a = ap.parse_args()
if a.out.exists():
    sys.exit(f"--out must not exist: {a.out}")
keep = set(a.keep_file.read_text().split()) if a.keep_file else None
drop = set(a.drop)
(a.out / "images").mkdir(parents=True)
for name in ("manifest.jsonl", "crops.jsonl"):
    rows = [json.loads(x) for x in (a.src / name).read_text(encoding="utf-8").splitlines() if x.strip()]
    kept = [r for r in rows if r["crop_id"] not in drop and (keep is None or r["crop_id"] in keep)]
    (a.out / name).write_text("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in kept), encoding="utf-8")
    if name == "manifest.jsonl":
        for r in kept:
            if r.get("image"):
                shutil.copy2(a.src / r["image"], a.out / r["image"])
    print(name, len(rows), "->", len(kept))
shutil.copytree(a.src / "LICENSES", a.out / "LICENSES")
for extra in ("match.json",):
    if (a.src / extra).exists():
        shutil.copy2(a.src / extra, a.out / extra)
