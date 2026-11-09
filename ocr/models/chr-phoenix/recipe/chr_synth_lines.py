#!/usr/bin/env python3
"""Synthetic Cherokee Phoenix lines made from the paper's own glyphs (#1620, #1621).

Each line is a run of words from a CC0 text (the 1860 New Testament as typeset by Joyner and
Legg), set glyph by glyph from the glyph bank (`chr_glyph_bank.py`): a random kept sample of each
letter, word-initial Ꮎ as the block G this paper prints there. Glyphs are scaled to a common body
height and set on a shared baseline with the paper's spacing, then degraded like the microfilm
(blur, ink spread or loss, noise, contrast, slight skew, JPEG). Each line is written as
`<name>.png` and `<name>.gt.txt` (tesstrain's layout), and `lines.jsonl` lists them with the
glyphs used.

Glyphs from the frozen test set's pages (`--exclude-manifest`, its `exclude_from_training`) are
never used, so a model trained on these lines is still measured on unseen pages. A line whose text
needs a letter with no sample is skipped and counted.

    chr_synth_lines.py --bank bank --text cnts.raw.txt --exclude-manifest testset/v2/manifest.json \
        --out synth --lines 20000 --seed 1620
"""
from __future__ import annotations

import argparse
import collections
import json
import os
import random
import re
import sys

import numpy as np
from PIL import Image, ImageFilter

SYL = re.compile("[Ꭰ-Ᏽ]+")
NA = "Ꮎ"


def excluded_pages(path: str | None) -> set[str]:
    if not path:
        return set()
    m = json.load(open(path, encoding="utf-8"))
    out = set()
    ex = m.get("exclude_from_training")
    if isinstance(ex, dict):
        for v in ex.values():
            if isinstance(v, list):
                out.update(x for x in v if isinstance(x, str))
    elif isinstance(ex, list):
        out.update(x for x in ex if isinstance(x, str))
    for s in m.get("sets", {}).values():
        out.update(p for p in ((s.get("pages") if isinstance(s, dict) else None) if isinstance((s.get("pages") if isinstance(s, dict) else None), list) else []) if isinstance(p, str))
    return out


def load_bank(bank: str, exclude: set[str]):
    pools = collections.defaultdict(list)
    for line in open(os.path.join(bank, "index.jsonl"), encoding="utf-8"):
        r = json.loads(line)
        if not r.get("kept") or r["page"] in exclude:
            continue
        key = NA + "^" if r["letter"] == NA and r["initial"] else r["letter"]
        pools[key].append(os.path.join(bank, "glyphs", f"{ord(r['letter']):04x}", r["id"] + ".png"))
    return pools


def ink_box(a: np.ndarray):
    """Rows and columns holding ink (dark) in a grayscale glyph crop."""
    thr = 128 if a.max() > 200 else a.mean() - 0.5 * a.std()
    ys, xs = np.nonzero(a < thr)
    if len(ys) == 0:
        return 0, a.shape[0], 0, a.shape[1]
    return ys.min(), ys.max() + 1, xs.min(), xs.max() + 1


def sentences(path: str, rng: random.Random, n_words=(3, 9)):
    words = []
    for line in open(path, encoding="utf-8"):
        words.extend(w for w in SYL.findall(line))
    i = 0
    while True:
        k = rng.randint(*n_words)
        if i + k >= len(words):
            i = 0
        yield words[i:i + k]
        i += k + rng.randint(0, 40)


def render(words, pools, rng, body: int, cache):
    """A grayscale line image of `words`, or None when a letter has no sample."""
    glyphs = []
    for wi, w in enumerate(words):
        for i, ch in enumerate(w):
            key = NA + "^" if ch == NA and i == 0 and (NA + "^") in pools else ch
            if not pools.get(key):
                return None, ch
            p = rng.choice(pools[key])
            if p not in cache:
                raw = np.asarray(Image.open(p).convert("L"), np.float32)
                # Paper to white and ink to black, so pasted glyphs carry no box.
                ink, paper = np.percentile(raw, 5), np.percentile(raw, 80)
                cache[p] = np.clip((raw - ink) / max(1.0, paper - ink), 0, 1) * 255
            a = cache[p]
            y0, y1, x0, x1 = ink_box(a)
            g = a[y0:y1, x0:x1]
            s = body / max(1, g.shape[0])
            g = np.asarray(Image.fromarray(g).resize((max(1, int(g.shape[1] * s)), body), Image.BILINEAR), np.float32)
            glyphs.append((g, i == 0 and wi > 0))
    gap, space = max(1, int(0.08 * body)), int(0.55 * body)
    width = sum(g.shape[1] for g, _ in glyphs) + gap * len(glyphs) + space * sum(1 for _, s in glyphs if s) + 2 * body
    h = int(body * 1.9)
    canvas = np.full((h, width), 255.0, np.float32)
    x = body
    base = int(body * 0.45)
    for g, new_word in glyphs:
        if new_word:
            x += space
        jitter = rng.randint(-1, 1)
        y = base + jitter
        region = canvas[y:y + g.shape[0], x:x + g.shape[1]]
        canvas[y:y + g.shape[0], x:x + g.shape[1]] = np.minimum(region, g[:region.shape[0], :region.shape[1]])
        x += g.shape[1] + gap + rng.randint(-1, 1)
    return canvas[:, :x + body], None


def degrade(a: np.ndarray, rng: random.Random) -> Image.Image:
    im = Image.fromarray(np.clip(a, 0, 255).astype(np.uint8))
    if rng.random() < 0.7:
        im = im.filter(ImageFilter.GaussianBlur(rng.uniform(0.3, 1.1)))
    r = rng.random()
    if r < 0.25:
        im = im.filter(ImageFilter.MinFilter(3))  # ink spread
    elif r < 0.4:
        im = im.filter(ImageFilter.MaxFilter(3))  # ink loss
    arr = np.asarray(im, np.float32)
    lo, hi = rng.uniform(10, 70), rng.uniform(190, 245)
    arr = lo + (arr / 255.0) * (hi - lo)
    arr += np.random.default_rng(rng.randint(0, 2**31)).normal(0, rng.uniform(2, 12), arr.shape)
    im = Image.fromarray(np.clip(arr, 0, 255).astype(np.uint8))
    if rng.random() < 0.5:
        im = im.rotate(rng.uniform(-0.6, 0.6), resample=Image.BILINEAR, expand=False, fillcolor=int(hi))
    return im


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bank", required=True)
    ap.add_argument("--text", required=True)
    ap.add_argument("--exclude-manifest")
    ap.add_argument("--out", required=True)
    ap.add_argument("--lines", type=int, default=1000)
    ap.add_argument("--seed", type=int, default=1620)
    ap.add_argument("--body", type=int, default=36, help="glyph body height in pixels")
    a = ap.parse_args(argv)
    rng = random.Random(a.seed)
    exclude = excluded_pages(a.exclude_manifest)
    pools = load_bank(a.bank, exclude)
    os.makedirs(a.out, exist_ok=True)
    missing = collections.Counter()
    cache = {}
    made = 0
    with open(os.path.join(a.out, "lines.jsonl"), "w", encoding="utf-8") as idx:
        for words in sentences(a.text, rng):
            if made >= a.lines:
                break
            arr, miss = render(words, pools, rng, a.body + rng.randint(-4, 4), cache)
            if arr is None:
                missing[miss] += 1
                continue
            name = f"chr-synth-{a.seed}-{made:06d}"
            degrade(arr, rng).save(os.path.join(a.out, name + ".png"))
            text = " ".join(words)
            open(os.path.join(a.out, name + ".gt.txt"), "w", encoding="utf-8").write(text + "\n")
            idx.write(json.dumps({"name": name, "text": text}, ensure_ascii=False) + "\n")
            made += 1
    summary = {"lines": made, "excluded_pages": len(exclude), "letters_with_samples": len(pools),
               "skipped_for_missing_letter": dict(missing.most_common())}
    json.dump(summary, open(os.path.join(a.out, "summary.json"), "w"), ensure_ascii=False, indent=1)
    print(json.dumps(summary, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())
