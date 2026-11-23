#!/usr/bin/env python3
"""Read the Cherokee lines of S0 (the boxes `classify_lines.py` called Cherokee) with one Tesseract model,
the way a line job reads them (`chr_layout_read.py`): the box padded 12 px sideways and 8 px up and down
(crates/usnm-boinc/src/lines.rs), grey, Tesseract 5.5.0 --oem 1 --psm 13 --dpi 300, one run over the list
of a page's crops, words joined by single spaces.

Each crop is saved once as <crops-dir>/<doc_id>/<tid>.png (an existing crop is checked, not rewritten), so
every model reads the same pixels and later arms can read the same files.

    read_lines.py --classified classified.jsonl --pages pages.txt --scans DIR --layout ghn|loc
                  --tessdata DIR --lang chr|chr_phoenix --crops-dir DIR --out reads.jsonl [--jobs N]

One JSON line per Cherokee line: {"line_id", "page", "tid", "box", "pixel_box", "crop", "crop_sha256",
"lang", "text"}. Pages already in --out are skipped.
"""
import argparse
import concurrent.futures as cf
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np

import common

PAD_X, PAD_Y = 12, 8


def pixel_box(box, w, h):
    return [max(box[0] - PAD_X, 0), max(box[1] - PAD_Y, 0), min(box[2] + PAD_X, w), min(box[3] + PAD_Y, h)]


def tess_read(paths, lang, tessdata):
    """One Tesseract run over the image files; one text per file."""
    with tempfile.TemporaryDirectory() as tmp:
        Path(tmp, "lines.txt").write_text("\n".join(str(p) for p in paths) + "\n")
        subprocess.run(["tesseract", "lines.txt", "lines", "-l", lang, "--tessdata-dir", os.path.abspath(tessdata), "--oem", "1",
                        "--psm", "13", "--dpi", "300", "-c", "tessedit_create_tsv=1"], cwd=tmp, check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=dict(os.environ, OMP_THREAD_LIMIT="1"))
        words = {}
        for row in Path(tmp, "lines.tsv").read_text(encoding="utf-8").splitlines()[1:]:
            f = row.split("\t")
            if len(f) >= 12 and f[0] == "5" and f[11].strip():
                words.setdefault(int(f[1]), []).append(f[11])
    return [" ".join(words.get(i + 1, [])) for i in range(len(paths))]


def do_page(args):
    rec, scan, crops_dir, lang, tessdata = args
    from PIL import Image
    Image.MAX_IMAGE_PIXELS = None
    lines = [l for l in rec["lines"] if l.get("chr")]
    if not lines:
        return []
    d = Path(crops_dir) / common.doc_id(rec["page"])
    d.mkdir(parents=True, exist_ok=True)
    img = None
    out, paths = [], []
    for l in lines:
        path = d / f"{l['tid']}.png"
        pb = pixel_box(l["box"], rec["width"], rec["height"])
        if not path.exists():
            if img is None:
                img = np.asarray(Image.open(scan).convert("L"))
            Image.fromarray(img[pb[1]:pb[3], pb[0]:pb[2]]).save(str(path) + ".part.png")
            os.replace(str(path) + ".part.png", path)
        paths.append(path.resolve())
        out.append({"line_id": common.line_id(rec["page"], l["tid"]), "page": rec["page"], "tid": l["tid"],
                    "box": l["box"], "pixel_box": pb, "crop": f"{common.doc_id(rec['page'])}/{l['tid']}.png",
                    "crop_sha256": common.sha256_file(path), "lang": lang})
    for r, t in zip(out, tess_read(paths, lang, tessdata)):
        r["text"] = t
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--classified", required=True)
    ap.add_argument("--pages", required=True)
    ap.add_argument("--scans", required=True)
    ap.add_argument("--layout", choices=["ghn", "loc"], required=True)
    ap.add_argument("--tessdata", required=True)
    ap.add_argument("--lang", required=True)
    ap.add_argument("--crops-dir", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--jobs", type=int, default=1)
    a = ap.parse_args()
    want = set(common.read_pages(a.pages))
    done = {json.loads(l)["page"] for l in open(a.out)} if os.path.exists(a.out) else set()
    recs = [r for r in common.read_jsonl(a.classified) if r["page"] in want and r["page"] not in done]
    print(f"{len(recs)} pages to read with {a.lang}", file=sys.stderr, flush=True)
    jobs = [(r, common.scan_path(a.scans, r["page"], a.layout), a.crops_dir, a.lang, a.tessdata) for r in recs]
    n = 0
    with open(a.out, "a", encoding="utf-8") as out, cf.ProcessPoolExecutor(a.jobs) as pool:
        for k, rows in enumerate(pool.map(do_page, jobs), 1):
            for r in rows:
                out.write(json.dumps(r, ensure_ascii=False, sort_keys=True) + "\n")
            out.flush()
            n += len(rows)
            if k % 10 == 0 or k == len(jobs):
                print(f"{k}/{len(jobs)} pages, {n} lines", file=sys.stderr, flush=True)


if __name__ == "__main__":
    main()
