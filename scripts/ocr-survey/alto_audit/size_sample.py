#!/usr/bin/env python3
"""Sizing sample for #2167 (the BOINC port): ALTO pages from held LoC bulk batches, never fetched.

    size_sample.py --locbulk /data/locbulk/ocr --batches 10 --per-batch 50 --out DIR

Batches are taken in sha256("2167:size:" + tar name) order; from each, the first --per-batch ALTO members
(`.../seq-N/ocr.xml`) are streamed out of the tar and
written to DIR. Prints per-page ALTO bytes: uncompressed, gzip -6 and bz2 -9, with mean and p90.
"""
import argparse
import bz2
import gzip
import hashlib
import os
import sys
import tarfile

import numpy as np


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--locbulk", required=True)
    ap.add_argument("--batches", type=int, default=10)
    ap.add_argument("--per-batch", type=int, default=50)
    ap.add_argument("--out", required=True)
    a = ap.parse_args(argv)
    tars = sorted((t for t in os.listdir(a.locbulk) if t.endswith(".tar.bz2")),
                  key=lambda t: hashlib.sha256(("2167:size:" + t).encode()).hexdigest())[: a.batches]
    os.makedirs(a.out, exist_ok=True)
    raw, gz, bz = [], [], []
    for t in tars:
        n = 0
        with tarfile.open(os.path.join(a.locbulk, t), "r|bz2") as tf:
            for m in tf:
                base = os.path.basename(m.name)
                if not (m.isfile() and base == "ocr.xml"):
                    continue
                data = tf.extractfile(m).read()
                if b"alto" not in data[:2000].lower():
                    continue
                name = f"{t[:-8]}__{n:03d}.xml"
                with open(os.path.join(a.out, name), "wb") as f:
                    f.write(data)
                raw.append(len(data))
                gz.append(len(gzip.compress(data, 6)))
                bz.append(len(bz2.compress(data, 9)))
                n += 1
                if n >= a.per_batch:
                    break
        print(f"{t}: {n} pages", flush=True)
    for k, v in (("uncompressed", raw), ("gzip6", gz), ("bz2_9", bz)):
        v = np.array(v, dtype=float)
        print(f"{k}: n {len(v)} mean {v.mean() / 1024:.1f} KiB p90 {np.quantile(v, 0.9) / 1024:.1f} KiB")
    return 0


if __name__ == "__main__":
    sys.exit(main())
