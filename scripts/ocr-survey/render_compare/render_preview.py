#!/usr/bin/env python3
"""A contact sheet to look at step 2's renderings (#2103): for each word, the scan crop above its rendering.

    render_preview.py --words words.jsonl --alto-dir LOC --out sheet.png [--limit 40] [--wrong-first] [--font SCRIPT=PATH ...]

The sheet is for eyes only; the full run (distances.py) draws and measures in one pass and keeps no images.
Wrong words are drawn with a red rule under them. The font record is printed on stderr.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import numpy as np

import paths  # noqa: F401
import fonts
import gold
import render


def main(argv: list[str] | None = None) -> int:
    from PIL import Image, ImageDraw

    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--words", type=Path, required=True)
    ap.add_argument("--alto-dir", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--limit", type=int, default=40)
    ap.add_argument("--wrong-first", action="store_true", help="show wrong words before right ones")
    ap.add_argument("--font", action="append", metavar="SCRIPT=PATH")
    ap.add_argument("--font-cache")
    a = ap.parse_args(argv)
    Image.MAX_IMAGE_PIXELS = None
    rows = gold.read_jsonl(a.words)
    if a.wrong_first:
        rows.sort(key=lambda r: r["label"])
    overrides = fonts.parse_overrides(a.font)
    tiles, pages, used = [], {}, {}
    for r in rows:
        script = fonts.script_of_lang(r["lang"])
        fs = used.setdefault(script, fonts.resolve(script, overrides, a.font_cache))
        if not fs:
            continue
        stem = r["source_image"].rsplit(".", 1)[0]
        if stem not in pages:
            pages[stem] = Image.open(a.alto_dir / f"{stem}.jpg").convert("L")
        font = next((f for f in fs if render.has_glyphs(r["ocr_word"], f.path)), None)
        if font is None:
            continue
        out = render.render_for_crop(r["ocr_word"], pages[stem], r["box"], font.path)
        top = out["scan"]
        bottom = np.where(out["render_mask"], 0, 255).astype(np.uint8)
        tile = Image.new("L", (top.shape[1] + 6, top.shape[0] * 2 + 10), 200)
        tile.paste(Image.fromarray(top), (3, 3))
        tile.paste(Image.fromarray(bottom), (3, top.shape[0] + 7))
        rgb = tile.convert("RGB")
        if r["label"] == 0:
            ImageDraw.Draw(rgb).rectangle((0, tile.height - 3, tile.width, tile.height), fill=(200, 0, 0))
        tiles.append(rgb)
        if len(tiles) >= a.limit:
            break
    if not tiles:
        for script, fs in used.items():
            if not fs:
                print(f"{script}: skipped, {fonts.unavailable_reason(script)}", file=sys.stderr)
        return 1
    width, x, y, row_h, placed = 1200, 0, 0, 0, []
    for t in tiles:
        if x + t.width > width:
            x, y, row_h = 0, y + row_h + 4, 0
        placed.append((t, x, y))
        x, row_h = x + t.width + 4, max(row_h, t.height)
    sheet = Image.new("RGB", (width, y + row_h), (255, 255, 255))
    for t, px, py in placed:
        sheet.paste(t, (px, py))
    a.out.parent.mkdir(parents=True, exist_ok=True)
    sheet.save(a.out)
    for script, fs in used.items():
        for f in fs:
            print(f"{script}: {json.dumps(f.record())}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
