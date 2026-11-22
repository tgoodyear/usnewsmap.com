#!/usr/bin/env python3
"""Step 3 of #2103: how far is each word's rendering from its scan crop.

    distances.py --words words.jsonl --alto-dir LOC --out distances.parquet [--limit N] [--font SCRIPT=PATH ...]

`--words` is loader.py's output; `--alto-dir` holds the page images (`<stem>.jpg`). For each word the crop is
binarised (Sauvola, ink.py), the OCR word is rendered into its box (render.py), the two ink masks are aligned by
the integer shift that minimises their chamfer distance, and three numbers are written next to the word's row:

  chamfer    symmetric mean distance from each mask's ink to the other's nearest ink, in pixels, over the box's
             height (so a word of any size is on one scale); 0 for the same shapes
  pixel      1 - intersection over union of the aligned masks
  hog_cos    1 - cosine similarity of the HOG features (9 orientations, 8x8 cells, 2x2 blocks) of the two blurred
             masks, resized to 32 pixels high; the shift is not re-searched

plus `status` (ok, no_font, no_glyph, no_ink), the shift, the stroke widths and the font record
(font_family, font_version, font_sha256, font_licence, font_package) of the font that drew the word. A word
whose script has no font (Cherokee without --font chr=PATH) or whose characters the font lacks gets null
distances and its status; it is kept so counts stay honest. `--out` ending in .parquet needs pyarrow, else
JSON lines.
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
import ink
import render

HOG_HEIGHT = 32
BLUR_SIGMA = 1.0
MAX_SHIFT_FRAC = 0.2  # of the box height, at least 2 px


def _shifted(a: np.ndarray, dx: int, dy: int, fill) -> np.ndarray:
    out = np.full_like(a, fill)
    h, w = a.shape
    ys, yd = (slice(max(0, dy), h), slice(0, h - max(0, dy))) if dy >= 0 else (slice(0, h + dy), slice(-dy, h))
    xs, xd = (slice(max(0, dx), w), slice(0, w - max(0, dx))) if dx >= 0 else (slice(0, w + dx), slice(-dx, w))
    out[ys, xs] = a[yd, xd]
    return out


def chamfer(a_mask: np.ndarray, b_mask: np.ndarray, dt_a: np.ndarray, dt_b: np.ndarray) -> float:
    """Symmetric mean nearest-ink distance in pixels. dt_x is the distance transform of x's background (0 on ink)."""
    return 0.5 * (float(dt_b[a_mask].mean()) + float(dt_a[b_mask].mean()))


def align(scan: np.ndarray, rendered: np.ndarray, max_shift: int) -> tuple[np.ndarray, int, int, float]:
    """The rendering shifted by the integer (dx, dy), within max_shift, that minimises the chamfer distance to the scan.
    Returns (shifted rendering, dx, dy, the chamfer in pixels)."""
    from scipy.ndimage import distance_transform_edt

    dt_scan, dt_rend = distance_transform_edt(~scan), distance_transform_edt(~rendered)
    big = float(max(scan.shape) * 2)
    best = None
    for dy in range(-max_shift, max_shift + 1):
        for dx in range(-max_shift, max_shift + 1):
            moved = _shifted(rendered, dx, dy, False)
            if not moved.any():
                continue
            c = chamfer(scan, moved, dt_scan, _shifted(dt_rend, dx, dy, big))
            if best is None or c < best[0]:
                best = (c, dx, dy, moved)
    if best is None:
        return rendered, 0, 0, float("nan")
    return best[3], best[1], best[2], best[0]


def hog_cosine(a_mask: np.ndarray, b_mask: np.ndarray) -> float | None:
    """1 - the cosine similarity of the two masks' HOG vectors, after a Gaussian blur and a resize to HOG_HEIGHT rows."""
    from scipy.ndimage import gaussian_filter
    from skimage.feature import hog
    from PIL import Image

    h, w = a_mask.shape
    new_w = max(16, int(round(w * HOG_HEIGHT / h)))
    vecs = []
    for m in (a_mask, b_mask):
        g = gaussian_filter(m.astype(np.float32), BLUR_SIGMA)
        g = np.asarray(Image.fromarray((g * 255).astype(np.uint8)).resize((new_w, HOG_HEIGHT), Image.BILINEAR), dtype=np.float32)
        vecs.append(hog(g / 255.0, orientations=9, pixels_per_cell=(8, 8), cells_per_block=(2, 2), feature_vector=True))
    na, nb = (float(np.linalg.norm(v)) for v in vecs)
    if na == 0 and nb == 0:
        return 0.0
    if na == 0 or nb == 0:
        return 1.0
    return max(0.0, float(1.0 - np.dot(vecs[0], vecs[1]) / (na * nb)))


def word_distances(text: str, page, box: list[float], font_path: str | Path, word_bitmap=None) -> dict:
    """The distance fields for one word drawn in one font (or by `word_bitmap`, see render.render_for_crop): status,
    chamfer, pixel, hog_cos, dx, dy and the strokes, plus any renderer fields (`tpl_*`)."""
    out = render.render_for_crop(text, page, box, font_path, word_bitmap)
    scan, rend, meta = out["scan_mask"], out["render_mask"], out["meta"]
    res = {"status": "ok", "chamfer": None, "pixel": None, "hog_cos": None, "dx": None, "dy": None,
           "scan_stroke": meta["stroke_target"], "render_stroke_before": meta["stroke_before"],
           "ink_height": meta["ink_rows"][1] - meta["ink_rows"][0], "box_w": meta["box_px"][0], "box_h": meta["box_px"][1]}
    res.update({k: v for k, v in meta.items() if k.startswith("tpl_")})
    if not scan.any() or not rend.any():
        res["status"] = "no_ink" if not scan.any() else "no_glyph"
        return res
    bh = meta["box_px"][1]
    moved, dx, dy, c = align(scan, rend, max(2, int(round(MAX_SHIFT_FRAC * bh))))
    union = int((scan | moved).sum())
    res.update({"chamfer": c / bh, "pixel": 1.0 - int((scan & moved).sum()) / union if union else 0.0,
                "hog_cos": hog_cosine(scan, moved), "dx": dx, "dy": dy})
    return res


def main(argv: list[str] | None = None) -> int:
    from PIL import Image

    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--words", type=Path, required=True, help="loader.py output (JSON lines)")
    ap.add_argument("--alto-dir", type=Path, required=True, help="<stem>.jpg per page")
    ap.add_argument("--out", type=Path, required=True, help=".parquet or .jsonl")
    ap.add_argument("--limit", type=int, help="the first N words only")
    ap.add_argument("--font", action="append", metavar="SCRIPT=PATH", help="font file for a script (chr, latin)")
    ap.add_argument("--font-cache", help="font cache dir (default $RENDER_COMPARE_FONTS)")
    ap.add_argument("--renderer", choices=["font", "templates"], default="font",
                    help="font: the pinned font; templates: letters cut from the page's own ink (templates.py build)")
    ap.add_argument("--templates", type=Path, help="templates.py build output (.npz), for --renderer templates")
    a = ap.parse_args(argv)
    Image.MAX_IMAGE_PIXELS = None
    rows = gold.read_jsonl(a.words)[: a.limit]
    rows.sort(key=lambda r: r.get("source_image") or "")  # one page image in memory at a time (the sort is stable)
    overrides = fonts.parse_overrides(a.font)
    available: dict[str, list] = {}
    page_stem, page = None, None
    out_rows, counts = [], {}
    store = None
    if a.renderer == "templates":
        import templates

        if not a.templates:
            ap.error("--renderer templates needs --templates")
        store = templates.TemplateStore.load(a.templates)
    for r in rows:
        script = fonts.script_of_lang(r["lang"])
        fs = available.setdefault(script, fonts.resolve(script, overrides, a.font_cache))
        rec = dict(r)
        if not fs:
            rec.update({"status": "no_font", "script": script})
            out_rows.append(rec)
            counts["no_font"] = counts.get("no_font", 0) + 1
            continue
        font = next((f for f in fs if render.has_glyphs(r["ocr_word"], f.path)), None)
        if font is None:
            rec.update({"status": "no_glyph", "script": script})
            out_rows.append(rec)
            counts["no_glyph"] = counts.get("no_glyph", 0) + 1
            continue
        stem = r["source_image"].rsplit(".", 1)[0]
        if stem != page_stem:
            page = Image.open(a.alto_dir / f"{stem}.jpg").convert("L")
            page_stem = stem
            wb = store.renderer(stem, font.path) if store else None
        rec.update(word_distances(r["ocr_word"], page, r["box"], font.path, wb))
        rec["renderer"] = a.renderer
        rec.update(font.record())
        rec["script"] = script
        out_rows.append(rec)
        counts[rec["status"]] = counts.get(rec["status"], 0) + 1
    a.out.parent.mkdir(parents=True, exist_ok=True)
    if a.out.suffix == ".parquet":
        import pyarrow as pa
        import pyarrow.parquet as pq

        keys = list(dict.fromkeys(k for r in out_rows for k in r))
        pq.write_table(pa.table({k: [r.get(k) for r in out_rows] for k in keys}), a.out)
    else:
        gold.write_jsonl(a.out, out_rows)
    for script, fs in available.items():
        print(f"{script}: " + (json.dumps(fs[0].record()) if fs else f"skipped, {fonts.unavailable_reason(script)}"), file=sys.stderr)
    print(f"{len(out_rows)} words: {json.dumps(counts, sort_keys=True)}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
