#!/usr/bin/env python3
"""Line fit for the combined segmenter's atoms (#2107): the banded v2 refit, used as it is.

Every atom's line is fitted with layout_columns/refit.py's fit() from branch banded-v2-2107 (54eb0e99), the
en2107-l2 refit ported unchanged: angle within +-2 degrees in 0.05 degree steps, pivot at the atom's vertical
centre, sideways shift within +-max(1% of page width, half the gutter width), each side stopping halfway to any
other candidate overlapping vertically; rule snap (a near-vertical dark run over >= 80% of the rows, at most
max(6 px, 0.15% of page width) wide) else the 3-px line with the fewest dark pixels. Dark = gray <= the page's
Otsu threshold.

The fit is meant to run on the full-resolution page, as banded v2 does (build_features.py --paths). On the
2,000 px page cache instead (no --paths), the 6 px rule minimum applies in cache pixels, which is wider than v2's
at full resolution: a disclosed difference, used only where full-resolution pages are not available.
"""
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "layout_columns"))
import refit  # noqa: E402

otsu = refit.otsu


def fit(ink, x, y0, y1, half_gutter=None, others=()):
    """refit.fit, with the keys a white fit leaves out filled in (rule_width 0, rule_dark_frac 0)."""
    r = refit.fit(ink, x, y0, y1, half_gutter, list(others))
    if r is None:
        return None
    r.setdefault("rule_width", 0)
    r.setdefault("rule_dark_frac", 0.0)
    r["offset"] = float(r["offset"])
    return r


def x_at(line, y):
    """x of a fitted line (x_top at y0, x_bottom at y1) at height y."""
    return refit.line_x(line, y)
