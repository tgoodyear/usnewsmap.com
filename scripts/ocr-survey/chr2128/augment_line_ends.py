#!/usr/bin/env python3
"""Line-end augmentation of real training lines for R3 (#2128): random margins and slivers, as
`r3_synth_lines.py` gives the synthetic lines, so the model sees line ends at varied distances from the
crop's edge and with a neighbouring column's sliver, the line-end errors round two still makes.

Each line of --src (<name>.png + <name>.gt.txt) is written to --out under the same name, its text unchanged.
With chance --margin-rate the left and right margins are recut: the ink's column extent is found (columns
where more than 3% of rows are darker than midway between the 5th and 95th percentile grey), and each
margin is drawn from --margin-min to --margin-max body heights (body = the crop's height / 1.4), trimmed
from the paper outside the ink or padded with the paper's grey (its 90th percentile). The ink is never cut.
Then, with chance --edge-rate on each side whose margin is at least --sliver-room bodies, part of a random
bank glyph (round two's `edge_sliver`) is pasted into that margin.

    augment_line_ends.py --src real --out real-aug --bank BANK --exclude-manifest exclude.json --seed 2128
"""
import argparse
import json
import os
import random
import shutil
import sys

import numpy as np
from PIL import Image

import common

sys.path.insert(0, str(common.REPO / "ocr" / "models" / "chr-phoenix" / "recipe"))
import chr_synth_lines as cs  # noqa: E402


def ink_cols(a):
    lo, hi = np.percentile(a, [5, 95])
    ink = a < (lo + hi) / 2
    cols = np.nonzero(ink.mean(axis=0) > 0.03)[0]
    return (cols[0], cols[-1] + 1) if len(cols) else (0, a.shape[1])


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--src", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--bank", required=True)
    ap.add_argument("--exclude-manifest", required=True)
    ap.add_argument("--seed", type=int, default=2128)
    ap.add_argument("--margin-rate", type=float, default=0.5)
    ap.add_argument("--margin-min", type=float, default=0.05)
    ap.add_argument("--margin-max", type=float, default=1.5)
    ap.add_argument("--edge-rate", type=float, default=0.3)
    ap.add_argument("--sliver-room", type=float, default=0.6)
    a = ap.parse_args()
    cs.refuse_used_out(a.out)
    os.makedirs(a.out, exist_ok=True)
    rng = random.Random(a.seed)
    pools = cs.load_bank(a.bank, cs.excluded_pages(a.exclude_manifest))
    cache = {}
    stats = {"lines": 0, "recut": 0, "slivers": 0}
    for name in sorted(n[:-4] for n in os.listdir(a.src) if n.endswith(".png")):
        arr = np.asarray(Image.open(os.path.join(a.src, name + ".png")).convert("L"), np.float32)
        body = max(8, int(arr.shape[0] / 1.4))
        x0, x1 = ink_cols(arr)
        left, right = x0, arr.shape[1] - x1
        if rng.random() < a.margin_rate:
            paper = float(np.percentile(arr, 90))
            left, right = int(rng.uniform(a.margin_min, a.margin_max) * body), int(rng.uniform(a.margin_min, a.margin_max) * body)
            out = np.full((arr.shape[0], left + (x1 - x0) + right), paper, np.float32)
            out[:, left:left + (x1 - x0)] = arr[:, x0:x1]
            arr = out
            stats["recut"] += 1
        for is_right, room in ((False, left), (True, right)):
            if rng.random() < a.edge_rate and room >= a.sliver_room * body:
                cs.edge_sliver(arr, pools, rng, min(body, int(arr.shape[0] / 1.9) or 1), cache, is_right)
                stats["slivers"] += 1
        Image.fromarray(np.clip(arr, 0, 255).astype(np.uint8)).save(os.path.join(a.out, name + ".png"))
        shutil.copyfile(os.path.join(a.src, name + ".gt.txt"), os.path.join(a.out, name + ".gt.txt"))
        stats["lines"] += 1
    if os.path.exists(os.path.join(a.src, "lines.jsonl")):
        shutil.copyfile(os.path.join(a.src, "lines.jsonl"), os.path.join(a.out, "lines.jsonl"))
    json.dump({**stats, "seed": a.seed}, open(os.path.join(a.out, "summary-augment.json"), "w"), indent=1)
    print(json.dumps(stats))


if __name__ == "__main__":
    main()
