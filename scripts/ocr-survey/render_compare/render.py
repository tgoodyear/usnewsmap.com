"""Step 2 of #2103: draw the OCR word in its ALTO box, matched to the scan crop's height and stroke width.

`crop_frame` cuts the scan around a word's ALTO box (a pad on every side, white outside the page). `render_word`
draws the word's text in a font, then
  * horizontally fits its ink to the ALTO box: the engine says this word fills this box, so a reading with the
    wrong number of letters draws squeezed or stretched letters;
  * vertically fits its ink to the rows that hold the scan's ink inside the box (`ink.ink_rows`), which is the
    crop's own height and does not depend on what the text says;
  * sets the ink threshold of the anti-aliased drawing so the mean stroke width is the scan's (`ink.stroke_width`).
The result is a boolean ink mask in the frame, ready for the distances of step 3.

Fonts come from fonts.py. A script with no font (Cherokee without Phoenix 1828) is skipped, not drawn in another.
"""

from __future__ import annotations

import functools
from pathlib import Path

import numpy as np

import ink

BITMAP_PX = 96  # the size the word is drawn at before it is fitted to the box


def crop_frame(page, box: list[float], pad_frac: float = 0.25) -> tuple[np.ndarray, int, tuple[int, int, int, int]]:
    """(grey frame, pad, (x0, y0, w, h) of the ALTO box in the frame). `page` is a PIL image; the frame holds the box
    plus `pad` pixels on every side (a quarter of the box's height, at least 2), white where it leaves the page."""
    from PIL import Image

    x, y, w, h = box
    bx0, by0 = int(round(x)), int(round(y))
    bw, bh = max(1, int(round(w))), max(1, int(round(h)))
    pad = max(2, int(round(pad_frac * bh)))
    frame = Image.new("L", (bw + 2 * pad, bh + 2 * pad), 255)
    left, top = bx0 - pad, by0 - pad
    cx0, cy0 = max(0, left), max(0, top)
    cx1, cy1 = min(page.width, left + frame.width), min(page.height, top + frame.height)
    if cx1 > cx0 and cy1 > cy0:
        frame.paste(page.convert("L").crop((cx0, cy0, cx1, cy1)), (cx0 - left, cy0 - top))
    return np.asarray(frame), pad, (pad, pad, bw, bh)


@functools.lru_cache(maxsize=16)
def _font(path: str, px: int):
    from PIL import ImageFont

    return ImageFont.truetype(path, px)


def glyph_bitmap(text: str, font_path: str | Path, px: int = BITMAP_PX) -> np.ndarray | None:
    """The text's ink on [0, 1], cropped to its bounding box, or None when nothing is drawn (blank text, or no glyphs)."""
    from PIL import Image, ImageDraw

    font = _font(str(font_path), px)
    width = int(font.getlength(text)) + 4 * px
    im = Image.new("L", (width, 4 * px), 0)
    ImageDraw.Draw(im).text((px, 2 * px), text, font=font, fill=255, anchor="ls")
    a = np.asarray(im, dtype=np.float32) / 255.0
    ys, xs = np.nonzero(a > 0.05)
    if len(xs) == 0:
        return None
    return a[ys.min(): ys.max() + 1, xs.min(): xs.max() + 1]


def has_glyphs(text: str, font_path: str | Path) -> bool:
    """False when the font draws nothing for some character of the text, or its missing-glyph box."""
    font = _font(str(font_path), BITMAP_PX)
    missing = bytes(font.getmask("\U0010fffd"))
    return all(ch.isspace() or bytes(font.getmask(ch)) != missing for ch in text)


def fit(bitmap: np.ndarray, w: int, h: int) -> np.ndarray:
    from PIL import Image

    im = Image.fromarray((bitmap * 255).astype(np.uint8)).resize((max(1, w), max(1, h)), Image.LANCZOS)
    return np.asarray(im, dtype=np.float32) / 255.0


THRESHOLDS = (0.12, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8)


def match_stroke(coverage: np.ndarray, target: float | None) -> tuple[np.ndarray, float | None]:
    """The ink mask of an anti-aliased rendering (ink coverage on [0, 1]) whose mean stroke width is nearest `target`:
    a lower threshold keeps more of each edge pixel and thickens the strokes by a fraction of a pixel. When even the
    lowest threshold is more than a pixel too thin, the mask is dilated once. Returns the mask and the width of the
    plain 0.5 threshold (before matching); with no target the mask is the 0.5 threshold."""
    from scipy.ndimage import binary_dilation
    from skimage.morphology import disk

    plain = coverage >= 0.5
    before = ink.stroke_width(plain)
    if target is None or before is None:
        return plain, before
    best, best_gap = plain, abs(before - target)
    for t in THRESHOLDS:
        m = coverage >= t
        w = ink.stroke_width(m)
        if w is not None and abs(w - target) < best_gap:
            best, best_gap = m, abs(w - target)
    w = ink.stroke_width(best)
    if w is not None and target - w > 1.0:
        best = binary_dilation(best, structure=disk(1))
    return best, before


def render_word(text: str, frame_shape: tuple[int, int], x_span: tuple[int, int], y_span: tuple[int, int],
                font_path: str | Path, stroke: float | None) -> tuple[np.ndarray, dict]:
    """The text's ink mask in a frame of `frame_shape` (rows, columns): its bounding box fills x_span (columns,
    end exclusive) and y_span (rows), and its strokes are about `stroke` pixels wide. Blank when nothing is drawn."""
    mask = np.zeros(frame_shape, bool)
    coverage = np.zeros(frame_shape, np.float32)
    bitmap = glyph_bitmap(text, font_path)
    meta = {"drawn": bitmap is not None, "stroke_before": None, "stroke_target": stroke}
    if bitmap is None:
        return mask, meta
    (x0, x1), (y0, y1) = x_span, y_span
    coverage[y0:y1, x0:x1] = fit(bitmap, x1 - x0, y1 - y0)
    mask, meta["stroke_before"] = match_stroke(coverage, stroke)
    meta["font_px_equivalent"] = round((y1 - y0) * BITMAP_PX / bitmap.shape[0], 1)
    return mask, meta


def render_for_crop(text: str, page, box: list[float], font_path: str | Path) -> dict:
    """Scan crop and rendering of one word: {"scan": grey frame, "scan_mask", "render_mask", "meta"}.

    The rendering's rows are the scan's ink rows inside the box (the box's rows when the crop has no ink)."""
    frame, pad, (bx, by, bw, bh) = crop_frame(page, box)
    scan_mask = ink.binarise(frame)
    inside = np.zeros_like(scan_mask)
    inside[by: by + bh, bx: bx + bw] = scan_mask[by: by + bh, bx: bx + bw]
    rows = ink.ink_rows(inside)
    y_span = (rows[0], rows[1]) if rows else (by, by + bh)
    render_mask, meta = render_word(text, frame.shape, (bx, bx + bw), y_span, font_path, ink.stroke_width(inside))
    meta.update({"pad": pad, "box_px": [bw, bh], "scan_ink": bool(inside.any()), "ink_rows": list(y_span)})
    return {"scan": frame, "scan_mask": scan_mask, "render_mask": render_mask, "meta": meta}
