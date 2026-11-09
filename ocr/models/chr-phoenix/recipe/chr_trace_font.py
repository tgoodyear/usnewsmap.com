#!/usr/bin/env python3
"""A font traced from the Cherokee Phoenix's own glyphs (#1620): "Phoenix 1828".

For each letter with enough kept samples in the glyph bank (`chr_glyph_bank.py`), the best
samples (highest neighbour agreement, from as many pages as possible) are cut to their ink,
scaled to one body height, centred and averaged; the average is thresholded, traced with potrace
and placed in the font at the letter's code point, on one baseline with small-capital height
and a side bearing. Word-initial Ꮎ, which this paper prints as a block G, goes in as the unencoded
glyph `uni13BE.init` with an OpenType `init` substitution, so a shaper that applies `init` sets
it at the start of a word. Letters without enough samples are left out and listed.

The outlines are averages of public-domain print (Library of Congress scans of the paper, 1828 to
1834), so the font is CC0. It is a research tool for synthetic OCR data, not a revival.

    chr_trace_font.py --bank bank --exclude-manifest testset/v2/manifest.json --out phoenix1828 --min-samples 8
"""
from __future__ import annotations

import argparse
import collections
import json
import os
import subprocess
import sys
import tempfile

import numpy as np
from PIL import Image

NA = "Ꮎ"
EM, BODY, SIDE = 1000, 640, 60


def best_samples(bank, exclude, per_letter):
    by = collections.defaultdict(list)
    for line in open(os.path.join(bank, "index.jsonl"), encoding="utf-8"):
        r = json.loads(line)
        if not r.get("kept") or r["page"] in exclude:
            continue
        key = "init" if r["letter"] == NA and r["initial"] else r["letter"]
        by[key].append(r)
    out = {}
    for key, rows in by.items():
        rows.sort(key=lambda r: -r["agree"])
        seen, pick = collections.Counter(), []
        for r in rows:  # spread over pages: at most 3 from one page
            if seen[r["page"]] < 3:
                pick.append(r)
                seen[r["page"]] += 1
            if len(pick) >= per_letter:
                break
        out[key] = pick
    return out


def average(bank, rows, size=200):
    acc, n = np.zeros((size, size), np.float32), 0
    widths = []
    for r in rows:
        a = np.asarray(Image.open(os.path.join(bank, "glyphs", f"{ord(r['letter']):04x}", r["id"] + ".png")).convert("L"), np.float32)
        thr = a.mean() - 0.5 * a.std()
        ys, xs = np.nonzero(a < thr)
        if len(ys) < 10:
            continue
        g = a[ys.min():ys.max() + 1, xs.min():xs.max() + 1]
        s = (size * 0.8) / g.shape[0]
        w = max(1, int(g.shape[1] * s))
        if w > size:
            continue
        g = np.asarray(Image.fromarray(g).resize((w, int(size * 0.8)), Image.BILINEAR), np.float32)
        ink = 255 - g
        y0, x0 = int(size * 0.1), (size - w) // 2
        acc[y0:y0 + ink.shape[0], x0:x0 + w] += ink
        widths.append(w / (size * 0.8))
        n += 1
    if n == 0:
        return None, None
    avg = acc / n
    return avg, float(np.median(widths))


def main(argv=None) -> int:
    import fontforge  # noqa: PLC0415

    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bank", required=True)
    ap.add_argument("--exclude-manifest")
    ap.add_argument("--out", required=True)
    ap.add_argument("--min-samples", type=int, default=8)
    ap.add_argument("--per-letter", type=int, default=40)
    a = ap.parse_args(argv)
    exclude = set()
    if a.exclude_manifest:
        m = json.load(open(a.exclude_manifest, encoding="utf-8"))
        for s in m.get("sets", {}).values():
            exclude.update(p for p in ((s.get("pages") if isinstance(s, dict) else None) if isinstance((s.get("pages") if isinstance(s, dict) else None), list) else []) if isinstance(p, str))
    samples = best_samples(a.bank, exclude, a.per_letter)
    os.makedirs(a.out, exist_ok=True)
    font = fontforge.font()
    font.encoding = "UnicodeFull"
    font.familyname = font.fontname = "Phoenix1828"
    font.fullname = "Phoenix 1828 (traced)"
    font.copyright = "CC0 1.0: traced from public-domain scans of the Cherokee Phoenix (1828-1834), usnewsmap.com #1620"
    font.em = EM
    font.ascent, font.descent = 800, 200
    space = font.createChar(0x20)
    space.width = 280
    made, skipped = {}, {}
    previews = []
    with tempfile.TemporaryDirectory() as tmp:
        for key, rows in sorted(samples.items()):
            if len(rows) < a.min_samples:
                skipped[key] = len(rows)
                continue
            avg, wratio = average(a.bank, rows)
            if avg is None:
                skipped[key] = 0
                continue
            mask = avg > 0.5 * avg.max()
            pbm = os.path.join(tmp, "g.pbm")
            Image.fromarray(((~mask) * 255).astype(np.uint8)).convert("1").save(pbm)
            svg = os.path.join(tmp, "g.svg")
            subprocess.run(["potrace", "-s", "-o", svg, pbm], check=True)
            glyph = font.createChar(-1, "uni13BE.init") if key == "init" else font.createChar(ord(key))
            glyph.importOutlines(svg)
            # Every sample was scaled to one height, so each letter fills the body: baseline 0,
            # top BODY (the type was cast at small-capital height), with SIDE on the left.
            bb = glyph.boundingBox()
            s = BODY / max(1.0, bb[3] - bb[1])
            glyph.transform((s, 0, 0, s, 0, 0))
            bb = glyph.boundingBox()
            glyph.transform((1, 0, 0, 1, SIDE - bb[0], -bb[1]))
            glyph.width = int(glyph.boundingBox()[2] + SIDE)
            made[key] = len(rows)
            previews.append((key, avg))
        if "init" in made:
            font.addLookup("init", "gsub_single", (), (("init", (("cher", ("dflt",)), ("DFLT", ("dflt",)))),))
            font.addLookupSubtable("init", "init-1")
            font[0x13BE].addPosSub("init-1", "uni13BE.init")
        ttf = os.path.join(a.out, "Phoenix1828.ttf")
        font.generate(ttf)
    # A preview sheet of the averages.
    cols = 12
    tile = 100
    rows_n = (len(previews) + cols - 1) // cols
    sheet = Image.new("L", (cols * tile, rows_n * tile), 255)
    for i, (key, avg) in enumerate(previews):
        im = Image.fromarray((255 - np.clip(avg / avg.max() * 255, 0, 255)).astype(np.uint8)).resize((tile, tile))
        sheet.paste(im, ((i % cols) * tile, (i // cols) * tile))
    sheet.save(os.path.join(a.out, "averages.png"))
    missing = "".join(chr(c) for c in range(0x13A0, 0x13F5) if chr(c) not in made)
    summary = {"letters": len([k for k in made if k != "init"]), "init_form": "init" in made, "samples": made,
               "too_few_samples": skipped, "missing": missing, "license": "CC0-1.0"}
    json.dump(summary, open(os.path.join(a.out, "summary.json"), "w"), ensure_ascii=False, indent=1)
    print(json.dumps({k: summary[k] for k in ("letters", "init_form", "missing")}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())
