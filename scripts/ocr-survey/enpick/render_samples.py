#!/usr/bin/env python3
"""Render a few tess_pick.py tasks as the check page would show them, for eyeballing before anything is loaded (#2122).

  render_samples.py --out-dir OUT --samples DIR [--n 12] [--seed 2122]

Takes tasks from OUT/tasks.jsonl, one page at a time (round robin over pages, each page's tasks in sha256(seed:id)
order) and writes DIR/<nn>.png: the line crop with the word's box drawn, the word crop enlarged, and "Reading 1" and
"Reading 2" (a and b, in the page's order). Which reading is which goes only to DIR/key.tsv, so the pictures can be
judged blind first. Needs Pillow.
"""
import argparse
import hashlib
import json
import os


def pick(tasks, n, seed):
    by_page = {}
    for t in sorted(tasks, key=lambda t: hashlib.sha256(f"{seed}:{t['id']}".encode()).hexdigest()):
        by_page.setdefault(t["page"], []).append(t)
    pages = sorted(by_page, key=lambda p: hashlib.sha256(f"{seed}:{p}".encode()).hexdigest())
    out, k = [], 0
    while len(out) < n and any(by_page.values()):
        p = pages[k % len(pages)]
        if by_page[p]:
            out.append(by_page[p].pop(0))
        k += 1
    return out


def render(t, clips, path):
    from PIL import Image, ImageDraw, ImageFont
    line = Image.open(os.path.join(clips, t["line"])).convert("RGB")
    word = Image.open(os.path.join(clips, t["word"])).convert("RGB")
    x, y, w, h = t["box"]
    W, H = line.size
    ImageDraw.Draw(line).rectangle([x * W, y * H, (x + w) * W, (y + h) * H], outline=(220, 30, 30), width=2)
    scale = max(1, min(4, 120 // max(1, word.size[1])))
    word = word.resize((word.size[0] * scale, word.size[1] * scale))
    try:
        font = ImageFont.truetype("DejaVuSans.ttf", 28)
    except OSError:
        font = ImageFont.load_default()
    width = max(line.size[0], word.size[0], 500) + 20
    height = line.size[1] + word.size[1] + 150
    img = Image.new("RGB", (width, height), "white")
    img.paste(line, (10, 10))
    img.paste(word, (10, line.size[1] + 20))
    d = ImageDraw.Draw(img)
    y0 = line.size[1] + word.size[1] + 35
    d.text((10, y0), "Reading 1:  " + t["a"], fill="black", font=font)
    d.text((10, y0 + 50), "Reading 2:  " + t["b"], fill="black", font=font)
    img.save(path)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--samples", required=True)
    ap.add_argument("--n", type=int, default=12)
    ap.add_argument("--seed", default="2122")
    a = ap.parse_args(argv)
    tasks = [json.loads(l) for l in open(os.path.join(a.out_dir, "tasks.jsonl")) if l.strip()]
    os.makedirs(a.samples, exist_ok=True)
    with open(os.path.join(a.samples, "key.tsv"), "w") as key:
        key.write("n\ttask\tpage\treading_1\treading_2\treading_1_is\treading_2_is\tiou\n")
        for i, t in enumerate(pick(tasks, a.n, a.seed), 1):
            render(t, os.path.join(a.out_dir, "clips"), os.path.join(a.samples, "%02d.png" % i))
            p = t["private"]
            key.write("\t".join(str(v) for v in (i, t["id"], t["page"], t["a"], t["b"], p["a_is"], p["b_is"], p["iou"])) + "\n")


if __name__ == "__main__":
    main()
