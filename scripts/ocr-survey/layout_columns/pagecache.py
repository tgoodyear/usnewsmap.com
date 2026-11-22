#!/usr/bin/env python3
"""Decode page images once into grayscale arrays the finder reads (#2107 step 1, second attempt).

Method follows jscancella/NYTribuneOCRExperiments (findText_usingSums.py, GPL-3.0), reimplemented;
no code copied. Image sources: Teklia/Newspapers-finlam and -La-Liberte (MIT), biglam/europeana-
newspapers-ground-truth (CC0), NealCaren/co-newspaper-layout-yolo (no licence stated; private use),
Reichsanzeiger-GT (CC BY 4.0, Mannheim University Library), American Stories gold (Apache-2.0 text,
Dell Research Harvard, arXiv:2308.12477).

    pagecache.py --paths paths.json --out-dir DIR [--work-width 2000] [--workers 4] [--only-ids FILE]

paths.json maps page id -> image_path: a plain file, or "<parquet>#row=N" (embedded `page_image`).
Writes DIR/<page id>.<work width>.npz with `gray` (uint8) and `size` (original width, height),
which columns.load_gray reads. Images are only read, never altered or deleted.
"""
import argparse
import io
import json
import multiprocessing as mp
import os
import sys
import time

import numpy as np
from PIL import Image

Image.MAX_IMAGE_PIXELS = None


def to_gray(data_or_path, work_width):
    im = Image.open(io.BytesIO(data_or_path) if isinstance(data_or_path, bytes) else data_or_path)
    w0, h0 = im.size
    f = max(1, w0 // work_width)
    if im.format == "JPEG" and f > 1:
        im.draft("L", (w0 // f, h0 // f))
    im = im.convert("L")
    if im.size[0] != work_width and w0 > work_width:
        im = im.resize((work_width, max(1, round(h0 * work_width / w0))), Image.BOX)
    return np.asarray(im, dtype=np.uint8), (w0, h0)


def save(out_dir, pid, work_width, gray, size):
    np.savez_compressed(os.path.join(out_dir, f"{pid}.{work_width}.npz"), gray=gray, size=np.array(size))


def _job(args):
    src, wanted, out_dir, work_width = args  # wanted: {row index: page id} for a parquet, or {None: pid}
    done = 0
    if None in wanted:
        pid = wanted[None]
        g, size = to_gray(src, work_width)
        save(out_dir, pid, work_width, g, size)
        return 1
    import pyarrow.parquet as pq
    pf = pq.ParquetFile(src)
    start = 0
    for rg in range(pf.metadata.num_row_groups):
        n = pf.metadata.row_group(rg).num_rows
        need = [i for i in wanted if start <= i < start + n]
        if need:
            col = pf.read_row_group(rg, columns=["page_image"]).column("page_image").to_pylist()
            for i in need:
                g, size = to_gray(col[i - start]["bytes"], work_width)
                save(out_dir, wanted[i], work_width, g, size)
                done += 1
        start += n
    return done


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--paths", required=True)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--work-width", type=int, default=2000)
    ap.add_argument("--workers", type=int, default=2)
    ap.add_argument("--only-ids", help="file of page ids, one per line (default: all in --paths)")
    a = ap.parse_args(argv)
    paths = json.load(open(a.paths))
    if a.only_ids:
        keep = {ln.strip() for ln in open(a.only_ids) if ln.strip()}
        paths = {k: v for k, v in paths.items() if k in keep}
    os.makedirs(a.out_dir, exist_ok=True)
    groups = {}
    for pid, p in sorted(paths.items()):
        if "#row=" in p:
            f, r = p.rsplit("#row=", 1)
            groups.setdefault(f, {})[int(r)] = pid
        else:
            groups[p] = {None: pid}
    todo = []
    for src, wanted in groups.items():
        wanted = {k: v for k, v in wanted.items()
                  if not os.path.exists(os.path.join(a.out_dir, f"{v}.{a.work_width}.npz"))}
        if wanted:
            todo.append((src, wanted, a.out_dir, a.work_width))
    total = len(paths)
    done = total - sum(len(t[1]) for t in todo)
    print(f"pages {done}/{total}", flush=True)
    t0 = time.time()
    with mp.Pool(a.workers) as pool:
        for n in pool.imap_unordered(_job, todo):
            done += n
            print(f"pages {done}/{total}", flush=True)
    print(f"done in {time.time() - t0:.0f}s")
    return 0


if __name__ == "__main__":
    sys.exit(main())
