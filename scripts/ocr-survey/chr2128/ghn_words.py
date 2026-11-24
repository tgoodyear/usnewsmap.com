#!/usr/bin/env python3
"""Readings of the GHN training pages' Cherokee crops for R3 and R4c (#2128).

For each crop that phase 1 read with `chr_phoenix` (`read_lines.py`'s output for the 489 training pages),
this reads the same PNG again the same way (Tesseract 5.5.0 --oem 1 --psm 13 --dpi 300, one run per page)
with `chr_phoenix`, keeping each word's box in the crop, and with stock `chr`. The `chr_phoenix` text must
equal phase 1's reading; a line where it does not is counted (and keeps the new reading).

    ghn_words.py --reads train-chr_phoenix.jsonl --crops-dir DIR --tessdata DIR --out words.jsonl [--jobs N]

One JSON line per crop: {"line_id", "page", "crop", "pixel_box", "chr_phoenix": text, "words":
[{"text", "box": [x0, y0, x1, y1] in the crop}], "chr": text, "same_as_phase1": bool}.
"""
import argparse
import collections
import concurrent.futures as cf
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

import common


def tess_words(paths, lang, tessdata):
    with tempfile.TemporaryDirectory() as tmp:
        Path(tmp, "lines.txt").write_text("\n".join(str(p) for p in paths) + "\n")
        subprocess.run(["tesseract", "lines.txt", "lines", "-l", lang, "--tessdata-dir", os.path.abspath(tessdata),
                        "--oem", "1", "--psm", "13", "--dpi", "300", "-c", "tessedit_create_tsv=1"], cwd=tmp,
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                       env=dict(os.environ, OMP_THREAD_LIMIT="1"))
        words = collections.defaultdict(list)
        for row in Path(tmp, "lines.tsv").read_text(encoding="utf-8").splitlines()[1:]:
            f = row.split("\t")
            if len(f) >= 12 and f[0] == "5" and f[11].strip():
                x, y, w, h = (int(v) for v in f[6:10])
                words[int(f[1])].append({"text": f[11], "box": [x, y, x + w, y + h]})
    return [words.get(i + 1, []) for i in range(len(paths))]


def do_page(args):
    rows, crops_dir, tessdata = args
    paths = [(Path(crops_dir) / r["crop"]).resolve() for r in rows]
    ph = tess_words(paths, "chr_phoenix", tessdata)
    st = tess_words(paths, "chr", tessdata)
    out = []
    for r, w, s in zip(rows, ph, st):
        text = " ".join(x["text"] for x in w)
        out.append({"line_id": r["line_id"], "page": r["page"], "crop": r["crop"], "pixel_box": r["pixel_box"],
                    "chr_phoenix": text, "words": w, "chr": " ".join(x["text"] for x in s),
                    "same_as_phase1": text == r["text"]})
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--reads", required=True)
    ap.add_argument("--crops-dir", required=True)
    ap.add_argument("--tessdata", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--jobs", type=int, default=1)
    a = ap.parse_args()
    by_page = collections.defaultdict(list)
    for r in common.read_jsonl(a.reads):
        by_page[r["page"]].append(r)
    jobs = [(rows, a.crops_dir, a.tessdata) for _, rows in sorted(by_page.items())]
    n = differ = 0
    with open(a.out, "w", encoding="utf-8") as out, cf.ProcessPoolExecutor(a.jobs) as pool:
        for k, rows in enumerate(pool.map(do_page, jobs), 1):
            for r in rows:
                out.write(json.dumps(r, ensure_ascii=False, sort_keys=True) + "\n")
                differ += not r["same_as_phase1"]
            n += len(rows)
            if k % 20 == 0 or k == len(jobs):
                print(f"{k}/{len(jobs)} pages, {n} lines, {differ} differ from phase 1", file=sys.stderr, flush=True)


if __name__ == "__main__":
    main()
