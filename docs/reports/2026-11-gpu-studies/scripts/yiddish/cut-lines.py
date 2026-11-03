#!/usr/bin/env python3
"""Cut the Jochre Yiddish corpus's training-split text lines into tesstrain ground truth (#1479).

Lines and their text are read the way Study 8's `alto_lines` (tools/src/yiddish.rs) reads them:
each ALTO `TextLine` in file order, its text the `String` CONTENTs, `SP` as one space and `HYP`
CONTENTs (the corpus's `_alto4.txt`), its box clamped to the image. The text is NFC, in logical
order. Each line is written as `<page>_l<NNN>.png` (grayscale, cut from the page at the box plus a
margin) and `<page>_l<NNN>.gt.txt`. Only `--split` is read; the test split is never opened.

--corpus DIR (checkout at bd69b412), --split training, --out DIR, --pad-y F, --pad-x F (fractions
of the line's height), --stats (geometry only, nothing written).
"""
import argparse
import hashlib
import html
import json
import math
import os
import statistics
import sys
import unicodedata

from PIL import Image

Image.MAX_IMAGE_PIXELS = None


def attr(tag, name):
    key = f' {name}="'
    i = tag.find(key)
    if i < 0:
        return None
    i += len(key)
    j = tag.find('"', i)
    return html.unescape(tag[i:j]) if j >= 0 else None


def alto_lines(xml, width, height):
    out = []
    for part in xml.split("<TextLine")[1:]:
        k = part.find(">")
        head = part[: k if k >= 0 else 0]

        def num(n):
            try:
                return float(attr(head, n))
            except (TypeError, ValueError):
                return 0.0

        x, y, w, h = num("HPOS"), num("VPOS"), num("WIDTH"), num("HEIGHT")
        end = part.find("</TextLine")
        body = part[: end if end >= 0 else len(part)]
        text = []
        for el in body.split("<")[1:]:
            k = el.find(">")
            tag = el[: k if k >= 0 else len(el)]
            if tag.startswith("String ") or tag.startswith("String\t"):
                text.append(attr(tag, "CONTENT") or "")
            elif tag.startswith("SP ") or tag.startswith("SP/") or tag == "SP":
                text.append(" ")
            elif tag.startswith("HYP "):
                text.append(attr(tag, "CONTENT") or "")

        def clamp(v, m):
            return min(int(math.floor(max(v, 0.0) + 0.5)), m)

        out.append(([clamp(x, width), clamp(y, height), clamp(x + w, width), clamp(y + h, height)], "".join(text)))
    return out


def sha256(path):
    with open(path, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--split", default="training")
    ap.add_argument("--out")
    ap.add_argument("--pad-y", type=float, default=0.15)
    ap.add_argument("--pad-x", type=float, default=0.5)
    ap.add_argument("--stats", action="store_true")
    a = ap.parse_args()
    root = os.path.join(a.corpus, a.split)
    pages = sorted(
        os.path.join(d, f[:-4])
        for d in sorted(os.listdir(root))
        for f in os.listdir(os.path.join(root, d))
        if f.endswith(".png")
    )
    if not a.stats:
        os.makedirs(a.out, exist_ok=True)
        hashes = open(os.path.join(a.out, "..", f"{a.split}-split.sha256.txt"), "w", encoding="utf-8")
    heights, gaps, n_lines, n_written, skipped = [], [], 0, 0, {"empty box": 0, "empty text": 0}
    for rel in pages:
        base = os.path.join(root, rel)
        page = os.path.basename(rel)
        img = Image.open(base + ".png")
        img.load()
        if img.mode != "L":
            img = img.convert("L")
        with open(base + "_alto4.xml", encoding="utf-8") as f:
            lines = alto_lines(f.read(), img.width, img.height)
        if not a.stats:
            hashes.write(f"{sha256(base + '.png')} {sha256(base + '_alto4.xml')} {rel}.png\n")
        prev = None
        for n, (r, text) in enumerate(lines, 1):
            n_lines += 1
            if r[0] >= r[2] or r[1] >= r[3]:
                skipped["empty box"] += 1
                continue
            text = " ".join(unicodedata.normalize("NFC", text).split())
            if not text:
                skipped["empty text"] += 1
                continue
            h = r[3] - r[1]
            heights.append(h)
            if prev is not None and r[1] >= prev[1] and min(r[2], prev[2]) > max(r[0], prev[0]):
                gaps.append(r[1] - prev[3])
            prev = r
            if a.stats:
                continue
            py, px = round(a.pad_y * h), round(a.pad_x * h)
            box = (max(r[0] - px, 0), max(r[1] - py, 0), min(r[2] + px, img.width), min(r[3] + py, img.height))
            stem = os.path.join(a.out, f"{page}_l{n:03d}")
            img.crop(box).save(stem + ".png")
            with open(stem + ".gt.txt", "w", encoding="utf-8") as f:
                f.write(text + "\n")
            n_written += 1
    q = statistics.quantiles
    print(json.dumps({
        "split": a.split, "pages": len(pages), "lines": n_lines, "written": n_written, "skipped": skipped,
        "height_px_q10_50_90": [round(v) for v in q(heights, n=10)[::4]] if len(heights) > 10 else heights,
        "gap_px_q10_50_90": [round(v) for v in q(gaps, n=10)[::4]] if len(gaps) > 10 else gaps,
    }))


if __name__ == "__main__":
    sys.exit(main())
