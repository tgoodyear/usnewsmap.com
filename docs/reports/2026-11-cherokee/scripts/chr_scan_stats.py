"""Brightness and contrast of page scans (#1622): are the GHN scans darker than LoC's? One JSON object per set.

  --set NAME=DIR_OR_MANIFEST ...  (--set loc=<dir of jpgs> --set ghn=<ghn-pages.jsonl with --ghn-images DIR>)
  [--limit N] [--seed S]

For each page: the median grey level of the page (paper, since most of the page is paper), the 1st and 99th
percentile grey levels, and the paper-to-ink spread: the median of the brightest pixels in each 64-pixel
block minus the median of the darkest, taken on the page reduced 8 times. Reported as quartiles over pages.
"""
import argparse
import json
import random
from pathlib import Path

import numpy as np
from PIL import Image

Image.MAX_IMAGE_PIXELS = None


def stats(path):
    g = np.asarray(Image.open(path).convert("L").reduce(8), dtype=np.float32)
    h, w = (g.shape[0] // 8) * 8, (g.shape[1] // 8) * 8
    blocks = g[:h, :w].reshape(h // 8, 8, w // 8, 8).transpose(0, 2, 1, 3).reshape(-1, 64)
    hi, lo = blocks.max(1), blocks.min(1)
    p1, p99 = np.percentile(g, [1, 99])
    return {"median": float(np.median(g)), "p1": float(p1), "p99": float(p99),
            "spread": float(np.median(hi) - np.median(lo))}


def quart(v):
    q = np.percentile(v, [25, 50, 75])
    return [round(float(x), 1) for x in q]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--set", action="append", required=True)
    ap.add_argument("--ghn-images")
    ap.add_argument("--limit", type=int, default=60)
    ap.add_argument("--seed", type=int, default=1)
    a = ap.parse_args()
    out = {}
    for spec in a.set:
        name, src = spec.split("=", 1)
        if src.endswith(".jsonl"):
            paths = [Path(a.ghn_images) / json.loads(l)["file"] for l in open(src)]
        else:
            paths = sorted(Path(src).glob("*.jpg"))
        random.Random(a.seed).shuffle(paths)
        rows = [stats(p) for p in paths[: a.limit]]
        out[name] = {"pages": len(rows), **{k: quart([r[k] for r in rows]) for k in rows[0]}}
    print(json.dumps(out, indent=1))


if __name__ == "__main__":
    main()
