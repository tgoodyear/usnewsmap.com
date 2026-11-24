#!/usr/bin/env python3
"""Synthetic newspaper-like pages with exact column labels, for tests and the CPU smoke run (#2107).

    synth.py --out-dir DIR --pages 40 [--seed 2107] [--width 1400]

Each page: a masthead band, then one to three sections stacked down the page, each with 3 to 7
columns of text-like bars separated by gutters (10 to 30 px, with a vertical rule on some), and a
spanning headline bar between sections. Labels are the column rectangles; no real data is used.
Writes DIR/pages/<id>.jpg and DIR/labels.jsonl (labels shape of alto_weak.py, source "synth").
"""
import argparse
import json
import os
import random
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import colgeom  # noqa: E402


def page(rng, width):
    from PIL import Image, ImageDraw
    height = int(width * rng.uniform(1.3, 1.5))
    img = Image.new("L", (width, height), rng.randint(200, 240))
    d = ImageDraw.Draw(img)
    m = int(width * 0.03)
    d.rectangle([m, m, width - m, m + int(height * 0.06)], fill=50)
    y = m + int(height * 0.08)
    cols = []
    nsec = rng.randint(1, 3)
    for k in range(nsec):
        y_end = height - m if k == nsec - 1 else y + int((height - m - y) * rng.uniform(0.35, 0.6))
        n = rng.randint(3, 7)
        gut = rng.randint(10, 30)
        cw = (width - 2 * m - (n - 1) * gut) / n
        pitch = rng.randint(9, 15)
        for c in range(n):
            x0 = m + c * (cw + gut)
            x1 = x0 + cw
            yy = y
            while yy + pitch <= y_end:
                d.rectangle([x0, yy, x1 - rng.randint(0, int(cw * 0.3)) if rng.random() < 0.1 else x1, yy + pitch * 0.55],
                            fill=rng.randint(20, 70))
                yy += pitch
            cols.append([round(x0, 2), float(y), round(x1, 2), float(yy - pitch + pitch * 0.55)])
            if c and rng.random() < 0.4:
                gx = x0 - gut / 2
                d.line([gx, y, gx, y_end], fill=60, width=1)
        y = y_end
        if k < nsec - 1:
            d.rectangle([m, y + 6, width - m, y + 6 + int(height * 0.025)], fill=40)
            y += 12 + int(height * 0.025)
    return img, cols


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--pages", type=int, default=40)
    ap.add_argument("--seed", type=int, default=2107)
    ap.add_argument("--width", type=int, default=1400)
    a = ap.parse_args(argv)
    rng = random.Random(a.seed)
    os.makedirs(os.path.join(a.out_dir, "pages"), exist_ok=True)
    with open(os.path.join(a.out_dir, "labels.jsonl"), "w") as f:
        for i in range(a.pages):
            img, cols = page(rng, a.width)
            pid = "synth%04d" % i
            p = os.path.join(a.out_dir, "pages", pid + ".jpg")
            img.save(p, quality=90)
            f.write(json.dumps({"columns": cols, "gutters": colgeom.gutters_from_regions(cols, *img.size),
                                "height": img.size[1], "image_path": p, "licence": "synthetic", "page_id": pid,
                                "source": "synth", "width": img.size[0]}, sort_keys=True) + "\n")
    print(json.dumps({"pages": a.pages, "out": a.out_dir}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
