"""Tests for the column finder, reference gutters and scorer, on synthetic pages.

Method follows jscancella/NYTribuneOCRExperiments (findText_usingSums.py, GPL-3.0), reimplemented;
no code copied. Datasets named in columns.py (Reichsanzeiger-GT, American Stories gold, Chronicling Germany).
"""
import json
import os
import sys

import numpy as np
import pytest

sys.path.insert(0, os.path.dirname(__file__))
import columns  # noqa: E402
import refgutters  # noqa: E402
import score  # noqa: E402


def text_block(img, x0, x1, y0, y1, rng, line=8, gap=5):
    """Fill a box with dark 'text lines' made of random word-sized dashes."""
    y = y0
    while y + line <= y1:
        x = x0
        while x < x1:
            w = int(rng.integers(8, 30))
            img[y:y + line, x:min(x + w, x1)] = 20
            x += w + int(rng.integers(3, 6))
        y += line + gap


def page(w=1000, h=1400, cols=((60, 320), (360, 640), (680, 940)), seed=0, headline=None, skew=0.0):
    rng = np.random.default_rng(seed)
    img = np.full((h, w), 235, np.uint8)
    top = 100
    if headline:
        text_block(img, 60, 940, 60, 160, rng, line=30, gap=6)
        top = 200
    for x0, x1 in cols:
        text_block(img, x0, x1, top, h - 80, rng)
    if skew:
        from scipy.ndimage import rotate
        img = rotate(img, skew, reshape=False, order=1, cval=235).astype(np.uint8)
    return img


def small_params(**kw):
    p = columns.Params(work_width=1000, min_gutter=0.02, fh_min_gutter=0.02)
    for k, v in kw.items():
        setattr(p, k, v)
    return p


def centers(gs):
    return sorted((g[0] + g[1]) / 2 for g in gs)


def test_banded_finds_two_gutters():
    p = small_params()
    g = columns.find_banded(page(), p)
    assert len(g) == 2
    c = centers(g)
    assert abs(c[0] - 340) < 15 and abs(c[1] - 660) < 15
    assert all(x[3] - x[2] > 0.6 * 1400 for x in g)


def test_fullheight_finds_two_gutters():
    g = columns.find_fullheight(page(), small_params())
    assert len(g) == 2


def test_banded_beats_fullheight_with_headline_spanning_columns():
    img = page(headline=True)
    banded = columns.find_banded(img, small_params())
    full = columns.find_fullheight(img, small_params())
    # the full-height gutters run through the headline; the banded ones start below it
    assert len(banded) == 2 and len(full) == 2
    assert all(g[2] < 100 for g in full)
    assert all(g[2] >= 130 for g in banded)


def test_deskew_keeps_gutters_on_a_skewed_page():
    img = page(skew=0.5)
    g = columns.find_banded(img, small_params())
    assert len(g) == 2


def test_blank_page_has_no_gutters():
    img = np.full((1400, 1000), 235, np.uint8)
    assert columns.find_banded(img, small_params()) == []
    assert columns.find_fullheight(img, small_params()) == []


def test_single_column_has_no_gutters():
    img = page(cols=((60, 940),))
    assert columns.find_banded(img, small_params()) == []


def test_shear_profile_recovers_tilted_line():
    ink = np.zeros((100, 200))
    for y in range(100):
        ink[y, 100 + int(round((y - 49.5) * np.tan(np.radians(1.0))))] = 1
    flat = columns.shear_profile(ink, 0).max()
    sheared = max(columns.shear_profile(ink, a).max() for a in (1.0, -1.0))
    assert sheared > flat


# ---- reference gutters

def reg(t, box):
    return {"type": t, "box": box}


def test_reference_gutter_from_side_by_side_regions():
    regions = [reg("paragraph", [0, 0, 100, 1000]), reg("paragraph", [120, 0, 220, 1000])]
    g, _ = refgutters.page_reference(regions, 1000)
    assert g == [[100, 120, 0, 1000]]


def test_reference_needs_half_overlap_of_shorter():
    # shared height 40 of the shorter 100: no gutter
    regions = [reg("paragraph", [0, 0, 100, 100]), reg("paragraph", [120, 60, 220, 160])]
    assert refgutters.page_reference(regions, 1000)[0] == []
    # shared height 60 of the shorter 100: gutter
    regions = [reg("paragraph", [0, 0, 100, 100]), reg("paragraph", [120, 40, 220, 140])]
    assert refgutters.page_reference(regions, 1000)[0] == [[100, 120, 40, 100]]


def test_reference_skips_non_adjacent_pairs_and_non_text():
    regions = [reg("article", [0, 0, 100, 500]), reg("article", [120, 0, 220, 500]),
               reg("article", [240, 0, 340, 500]), reg("cartoon_or_advertisement", [0, 600, 100, 900]),
               reg("table", [120, 600, 220, 900]), reg("T0C0", [240, 600, 340, 900])]
    g, exc = refgutters.page_reference(regions, 1000)
    assert [x[:2] for x in g] == [[100, 120], [220, 240]]
    assert len(exc) == 3


def test_reference_merges_pairs_of_one_gap():
    regions = [reg("paragraph", [0, 0, 100, 300]), reg("paragraph", [0, 305, 100, 600]),
               reg("paragraph", [120, 0, 220, 600])]
    g, _ = refgutters.page_reference(regions, 1000)
    assert len(g) == 1 and g[0][2] == 0 and g[0][3] == 600


# ---- scoring

def test_perfect_match():
    ref = [[100, 120, 0, 1000]]
    s = score.match_page([[102, 118, 0, 1000]], ref, 1000, [])
    assert s["matched_det_height"] == 1000 and s["matched_ref_height"] == 1000


def test_margin_is_one_percent_of_width():
    ref = [[100, 120, 0, 1000]]
    assert score.match_page([[95, 125, 0, 1000]], ref, 1000, [])["matched_n"] == 1   # 5 px < 10 px
    assert score.match_page([[85, 125, 0, 1000]], ref, 1000, [])["matched_n"] == 0   # 15 px > 10 px
    assert score.match_page([[85, 125, 0, 1000]], ref, 2000, [])["matched_n"] == 1   # width 2000: 20 px


def test_half_height_rule():
    ref = [[100, 120, 0, 1000]]
    assert score.match_page([[100, 120, 0, 499]], ref, 1000, [])["matched_n"] == 0
    assert score.match_page([[100, 120, 0, 500]], ref, 1000, [])["matched_n"] == 1


def test_one_to_one_extra_detections_unmatched():
    ref = [[100, 120, 0, 1000]]
    s = score.match_page([[100, 110, 0, 1000], [110, 120, 0, 600]], ref, 1000, [])
    assert s["matched_n"] == 1 and s["matched_det_height"] == 1000 and s["det_height"] == 1600


def test_greedy_takes_largest_overlap_first():
    ref = [[100, 120, 0, 500], [100, 120, 400, 1000]]
    det = [[100, 120, 0, 1000]]
    s = score.match_page(det, ref, 1000, [])
    assert s["matched_ref_height"] == 600  # the second reference (overlap 600) wins


def test_detection_in_excluded_region_is_ignored():
    s = score.match_page([[400, 420, 0, 1000]], [], 1000, [[300, 0, 600, 1000]])
    assert s["det_n"] == 0 and s["det_ignored"] == 1 and s["det_height"] == 0
    s = score.match_page([[400, 420, 0, 1000]], [], 1000, [[300, 0, 600, 400]])
    assert s["det_n"] == 1  # only 40% inside


def test_excluded_fraction_union_not_double_counted():
    f = score.excluded_fraction([0, 10, 0, 10], [[0, 0, 10, 6], [0, 4, 10, 10]])
    assert abs(f - 1.0) < 1e-9


def test_pooled_f1_and_thresholds_unrounded():
    p, r, f = score.prf(8, 10, 6, 10)
    assert (p, r) == (0.8, 0.6)
    assert abs(f - 2 * 0.8 * 0.6 / 1.4) < 1e-12


def test_bootstrap_is_deterministic():
    rows = [{"det_height": 10 * (i + 1), "matched_det_height": 8 * (i + 1),
             "ref_height": 10 * (i + 1), "matched_ref_height": 7 * (i + 1)} for i in range(7)]
    assert score.bootstrap(rows) == score.bootstrap(rows)


def test_score_end_to_end(tmp_path):
    refs = {"p1": {"gutters": [[100, 120, 0, 1000]], "excluded": [], "width": 1000, "height": 1000}}
    det = {"p1": {"gutters": [[100, 120, 0, 1000], [500, 520, 0, 1000]]}}
    r = score.score(det, refs)
    assert r["recall"] == 1.0 and r["precision"] == 0.5
    json.dumps(r, sort_keys=True)


def test_columns_cli_writes_sorted_json(tmp_path):
    from PIL import Image
    d = tmp_path / "im"
    d.mkdir()
    Image.fromarray(page()).save(d / "a.png")
    Image.fromarray(page(seed=1)).save(d / "b.png")
    out = tmp_path / "o.json"
    columns.main(["--images", str(d), "--out", str(out), "--params-json", '{"work_width": 1000}'])
    data = json.loads(out.read_text())
    assert set(data) == {"_meta", "a", "b"}
    assert len(data["a"]["gutters"]) == 2
    assert list(data) == sorted(data)
