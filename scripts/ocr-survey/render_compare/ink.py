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


def _extent(counts: np.ndarray, frac: float) -> tuple[int, int] | None:
    if counts.max(initial=0) == 0:
        return None
    idx = np.nonzero(counts >= max(1, frac * counts.max()))[0]
    return int(idx[0]), int(idx[-1]) + 1


def ink_rows(mask: np.ndarray, frac: float = 0.03) -> tuple[int, int] | None:
    """(top, bottom) rows, bottom exclusive, holding the word's ink: rows whose ink count is at least `frac` of the
    busiest row's, so a speck above a word does not stretch it. None for an empty mask."""
    return _extent(mask.sum(axis=1), frac)


def ink_cols(mask: np.ndarray, frac: float = 0.03) -> tuple[int, int] | None:
    """(left, right) columns, right exclusive, holding the word's ink, as ink_rows does for rows."""
    return _extent(mask.sum(axis=0), frac)


def drop_specks(mask: np.ndarray, size: int = 4) -> np.ndarray:
    """The mask without connected pieces smaller than `size` pixels (paper noise, a neighbour's stray serif)."""
    from skimage.morphology import remove_small_objects

    return remove_small_objects(mask, max_size=size - 1) if mask.any() else mask
