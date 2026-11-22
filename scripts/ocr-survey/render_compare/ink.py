"""Binarising a word crop, and measuring its ink (#2103): Sauvola ink mask, stroke width, ink rows."""

from __future__ import annotations

import numpy as np

SAUVOLA_K = 0.2
MIN_WINDOW = 15


def odd(n: int) -> int:
    n = int(round(n))
    return n if n % 2 else n + 1


def binarise(gray: np.ndarray) -> np.ndarray:
    """Ink mask (True = ink) of a grey crop (uint8, 0 black), by Sauvola's local threshold.

    The window is about the crop's height, so a word's strokes set the local mean and spread. A flat crop
    (blank paper, or a uniform dark patch) has no ink."""
    from skimage.filters import threshold_sauvola

    g = gray.astype(np.float64) / 255.0
    if g.size == 0 or float(g.max()) - float(g.min()) < 8 / 255:
        return np.zeros(g.shape, bool)
    window = odd(max(MIN_WINDOW, g.shape[0]))
    return g < threshold_sauvola(g, window_size=window, k=SAUVOLA_K, r=0.5)


def stroke_width(mask: np.ndarray) -> float | None:
    """The mean stroke width in pixels: ink area over the length of the skeleton (a bar w pixels wide and L long has
    area w * L and a skeleton of about L). A mean, so a serif face's hairlines and stems both count and a half-pixel
    change in the ink moves it smoothly. None for an empty mask."""
    from skimage.morphology import skeletonize

    if not mask.any():
        return None
    length = int(skeletonize(mask).sum())
    if length == 0:
        return None
    return max(1.0, float(mask.sum()) / length)


def ink_rows(mask: np.ndarray, frac: float = 0.03) -> tuple[int, int] | None:
    """(top, bottom) rows, bottom exclusive, holding the word's ink: rows whose ink count is at least `frac` of the
    busiest row's, so a speck above a word does not stretch it. None for an empty mask."""
    counts = mask.sum(axis=1)
    if counts.max(initial=0) == 0:
        return None
    rows = np.nonzero(counts >= max(1, frac * counts.max()))[0]
    return int(rows[0]), int(rows[-1]) + 1
