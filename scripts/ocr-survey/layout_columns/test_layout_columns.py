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
import pagecache  # noqa: E402
import refgutters  # noqa: E402
import refs  # noqa: E402
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
    return sorted((g[0][0] + g[0][1]) / 2 for g in gs)


def gh(g):
    return sum(s[3] - s[2] for s in g)


def test_banded_finds_two_gutters():
    p = small_params()
    g = columns.find_banded(page(), p)
    assert len(g) == 2
    c = centers(g)
    assert abs(c[0] - 340) < 15 and abs(c[1] - 660) < 15
    assert all(gh(x) > 0.6 * 1400 for x in g)


def test_fullheight_finds_two_gutters():
    g = columns.find_fullheight(page(), small_params())
    assert len(g) == 2


def test_banded_beats_fullheight_with_headline_spanning_columns():
    img = page(headline=True)
    banded = columns.find_banded(img, small_params())
    full = columns.find_fullheight(img, small_params())
    # the full-height gutters run through the headline; the banded ones start below it
    assert len(banded) == 2 and len(full) == 2
    assert all(g[0][2] < 100 for g in full)
    assert all(g[0][2] >= 130 for g in banded)


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


def G(x0, x1, y0, y1):
    """A one-segment gutter."""
    return [[x0, x1, y0, y1]]


def test_reference_gutter_from_side_by_side_regions():
    regions = [reg("paragraph", [0, 0, 100, 1000]), reg("paragraph", [120, 0, 220, 1000])]
    g, _ = refgutters.page_reference(regions, 1000, 1000)
    assert g == [G(100, 120, 0, 1000)]


def test_reference_needs_half_overlap_of_shorter():
    # shared height 40 of the shorter 100: no gutter
    regions = [reg("paragraph", [0, 0, 100, 100]), reg("paragraph", [120, 60, 220, 160])]
    assert refgutters.page_reference(regions, 1000, 1000)[0] == []
    # shared height 60 of the shorter 100: gutter
    regions = [reg("paragraph", [0, 0, 100, 100]), reg("paragraph", [120, 40, 220, 140])]
    assert refgutters.page_reference(regions, 1000, 1000)[0] == [G(100, 120, 40, 100)]


def test_reference_skips_non_adjacent_pairs_and_non_text():
    regions = [reg("article", [0, 0, 100, 500]), reg("article", [120, 0, 220, 500]),
               reg("article", [240, 0, 340, 500]), reg("cartoon_or_advertisement", [0, 600, 100, 900]),
               reg("table", [120, 600, 220, 900]), reg("T0C0", [240, 600, 340, 900])]
    g, exc = refgutters.page_reference(regions, 1000, 1000)
    assert [x[0][:2] for x in g] == [[100, 120], [220, 240]]
    assert len(exc) == 3


def test_reference_chains_segments_of_one_gap_and_follows_drift():
    regions = [reg("paragraph", [0, 0, 100, 300]), reg("paragraph", [0, 305, 104, 600]),
               reg("paragraph", [120, 0, 220, 300]), reg("paragraph", [124, 305, 220, 600])]
    g, _ = refgutters.page_reference(regions, 1000, 1000)
    assert len(g) == 1 and len(g[0]) == 2
    assert g[0][0][:2] == [100, 120] and g[0][1][:2] == [104, 124]


# ---- scoring

def test_perfect_match():
    ref = [G(100, 120, 0, 1000)]
    s = score.match_page([G(102, 118, 0, 1000)], ref, 1000, [])
    assert s["matched_det_height"] == 1000 and s["matched_ref_height"] == 1000


def test_margin_is_one_percent_of_width():
    ref = [G(100, 120, 0, 1000)]
    assert score.match_page([G(95, 125, 0, 1000)], ref, 1000, [])["matched_n"] == 1   # 5 px < 10 px
    assert score.match_page([G(85, 125, 0, 1000)], ref, 1000, [])["matched_n"] == 0   # 15 px > 10 px
    assert score.match_page([G(85, 125, 0, 1000)], ref, 2000, [])["matched_n"] == 1   # width 2000: 20 px


def test_half_height_rule():
    ref = [G(100, 120, 0, 1000)]
    assert score.match_page([G(100, 120, 0, 499)], ref, 1000, [])["matched_n"] == 0
    assert score.match_page([G(100, 120, 0, 500)], ref, 1000, [])["matched_n"] == 1


def test_inside_gap_only_over_part_of_the_height():
    ref = [G(100, 120, 0, 1000)]
    det = [[[100, 120, 0, 400], [300, 320, 400, 1000]]]   # leaves the gap after y = 400
    assert score.match_page(det, ref, 1000, [])["matched_n"] == 0
    det = [[[100, 120, 0, 600], [300, 320, 600, 1000]]]
    assert score.match_page(det, ref, 1000, [])["matched_n"] == 1


def test_drifting_detection_matches_drifting_reference():
    ref = [[[100, 120, 0, 500], [150, 170, 500, 1000]]]
    det = [[[102, 118, 0, 500], [152, 168, 500, 1000]]]
    s = score.match_page(det, ref, 1000, [])
    assert s["matched_n"] == 1
    # a straight detection covers only one half of the drifting reference
    assert score.match_page([G(100, 120, 0, 1000)], ref, 1000, [])["matched_n"] == 1  # exactly half
    assert score.match_page([G(100, 118, 0, 1000)], [[[100, 120, 0, 400], [200, 220, 400, 1000]]], 1000, [])["matched_n"] == 0


def test_one_to_one_extra_detections_unmatched():
    ref = [G(100, 120, 0, 1000)]
    s = score.match_page([G(100, 110, 0, 1000), G(110, 120, 0, 600)], ref, 1000, [])
    assert s["matched_n"] == 1 and s["matched_det_height"] == 1000 and s["det_height"] == 1600


def test_greedy_takes_largest_overlap_first():
    ref = [G(100, 120, 0, 500), G(100, 120, 400, 1000)]
    det = [G(100, 120, 0, 1000)]
    s = score.match_page(det, ref, 1000, [])
    assert s["matched_ref_height"] == 600  # the second reference (overlap 600) wins


def test_detection_in_excluded_region_is_ignored():
    s = score.match_page([G(400, 420, 0, 1000)], [], 1000, [[300, 0, 600, 1000]])
    assert s["det_n"] == 0 and s["det_ignored"] == 1 and s["det_height"] == 0
    s = score.match_page([G(400, 420, 0, 1000)], [], 1000, [[300, 0, 600, 400]])
    assert s["det_n"] == 1  # only 40% inside


def test_excluded_fraction_union_not_double_counted():
    f = score.excluded_fraction(G(0, 10, 0, 10), [[0, 0, 10, 6], [0, 4, 10, 10]])
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
    refs = {"p1": {"gutters": [G(100, 120, 0, 1000)], "excluded": [], "width": 1000, "height": 1000}}
    det = {"p1": {"gutters": [G(100, 120, 0, 1000), G(500, 520, 0, 1000)]}}
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


# ---- second attempt: references, equal-weight pooling

def test_gap_cap_drops_wide_gaps():
    g = [[[100, 120, 0, 500]], [[200, 400, 0, 500]], [[10, 20, 0, 100], [30, 400, 100, 200]]]
    out = refs.cap_gutters(g, 1000)  # cap 30 px
    assert out == [[[100, 120, 0, 500]], [[10, 20, 0, 100]]]


def row(ds, **kw):
    base = {"dataset": ds, "width": 1000, "height": 1000, "text_regions": [], "nontext_regions": [],
            "separators": [], "column_gutters": []}
    base.update(kw)
    return base


def test_europeana_reference_is_the_vertical_separators():
    r = row("biglam/europeana-newspapers-ground-truth",
            separators=[{"box": [500, 100, 504, 900], "orientation": "vertical"},
                        {"box": [0, 50, 900, 54], "orientation": "horizontal"}],
            nontext_regions=[{"box": [0, 0, 10, 10], "type": "x"}])
    ref = refs.row_reference(r)
    assert ref["gutters"] == [[[500, 504, 100, 900]]]
    assert ref["excluded"] == [[0, 0, 10, 10]]


def test_finlam_percent_coordinates_are_scaled_and_gutters_rebuilt():
    tr = [{"class": "ARTICLE-TEXT", "box": [10, 10, 45, 90]}, {"class": "ARTICLE-TEXT", "box": [46, 10, 90, 90]},
          {"class": "ARTICLE-TITLE", "box": [0, 0, 100, 5]}]
    r = row("Teklia/Newspapers-finlam", width=1400, height=2000, text_regions=tr,
            nontext_regions=[{"box": [10, 10, 20, 20], "type": "picture"},
                             {"box": [30, 30, 40, 40], "type": "announcement"}])
    ref = refs.row_reference(r)
    assert len(ref["gutters"]) == 1
    x0, x1, y0, y1 = ref["gutters"][0][0]
    assert (x0, x1, y0, y1) == (630.0, 644.0, 200.0, 1800.0)
    assert ref["excluded"] == [[140.0, 200.0, 280.0, 400.0]]  # the announcement is not in the mask


def test_liberte_uses_stored_column_gutters_with_cap():
    r = row("Teklia/Newspapers-finlam-La-Liberte", column_gutters=[[[100, 110, 0, 50]], [[100, 400, 0, 50]]])
    assert refs.row_reference(r)["gutters"] == [[[100, 110, 0, 50]]]


def test_liberte_draw_is_hash_ordered():
    ids = ["a", "b", "c"]
    assert sorted(ids, key=refs.ll_order) == sorted(ids, key=lambda i: __import__("hashlib").sha256(("2107:ll:" + i).encode()).hexdigest())


def test_pool_is_equal_weight_per_set():
    import numpy as np
    big = np.array([[1000.0, 1000.0, 1000.0, 1000.0]] * 10)   # P 1, R 1
    small = np.array([[10.0, 0.0, 10.0, 0.0]])                # P 0, R 0
    p, r, f, prs = score.pool({"big": big, "small": small})
    assert (p, r) == (0.5, 0.5) and abs(f - 0.5) < 1e-12


def test_pool_bootstrap_deterministic_and_within_set():
    import numpy as np
    v = {"a": np.array([[10.0, 8, 10, 7], [10, 9, 10, 6]]), "b": np.array([[5.0, 5, 5, 5]])}
    assert score.pool_bootstrap(v) == score.pool_bootstrap(v)


def test_score_sets_private_set_has_no_per_page():
    ref = {"p1": {"gutters": [G(100, 120, 0, 1000)], "excluded": [], "width": 1000, "height": 1000}}
    det = {"p1": {"gutters": [G(100, 120, 0, 1000)]}}
    res = score.score_sets({"pub": {"detections": det, "refs": ref, "for_publication": True},
                            "priv": {"detections": det, "refs": ref, "for_publication": False}})
    assert "per_page" in res["per_set"]["pub"] and "per_page" not in res["per_set"]["priv"]
    assert res["f1"] == 1.0


def test_pagecache_roundtrip(tmp_path):
    from PIL import Image
    p = tmp_path / "a.png"
    Image.fromarray(page(w=300, h=400)).save(p)
    g, size = pagecache.to_gray(str(p), 2000)
    assert g.shape == (400, 300) and size == (300, 400)
    pagecache.save(str(tmp_path), "a", 2000, g, size)
    g2, s2 = columns.load_cached(str(tmp_path), "a", 2000)
    assert (g2 == g).all() and s2 == (300, 400)


# ---- amended metric: height coverage

def test_height_union_counts_each_gap_once():
    assert score.height_union([[100, 120, 0, 500], [102, 118, 300, 800]]) == 800  # overlap counted once
    assert score.height_union([[100, 120, 0, 500], [400, 420, 0, 500]]) == 1000  # two gutters side by side
    assert score.height_union([]) == 0.0


def test_one_long_detection_covers_stacked_reference_pieces():
    ref = [G(100, 112, y, y + 90) for y in range(0, 1000, 100)]          # ten stacked pieces
    det = [G(102, 110, 0, 1000)]
    s = score.match_page(det, ref, 1000, [])
    assert s["cov_ref"] / s["cov_ref_union"] == 1.0 and s["cov_det"] / s["cov_det_union"] == 0.9  # 90 of every 100 px
    # one-to-one credits one piece only
    assert s["matched_ref_height"] / s["ref_height"] == 0.1


def test_coverage_precision_counts_only_detected_height_inside_a_reference_gap():
    ref = [G(100, 120, 0, 500)]
    det = [G(100, 120, 0, 1000)]
    s = score.match_page(det, ref, 1000, [])
    assert s["cov_det"] == 500 and s["cov_det_union"] == 1000 and s["cov_ref"] == 500


def test_coverage_tolerance_is_one_percent_of_width_horizontally_only():
    ref = [G(100, 120, 0, 500)]
    assert score.match_page([G(92, 128, 0, 500)], ref, 1000, [])["cov_ref"] == 500    # 8 px < 10 px
    assert score.match_page([G(85, 128, 0, 500)], ref, 1000, [])["cov_ref"] == 0      # 15 px > 10 px
    assert score.match_page([G(100, 120, 500, 900)], ref, 1000, [])["cov_ref"] == 0   # no y overlap


def test_coverage_overlapping_detections_count_once():
    ref = [G(100, 120, 0, 500)]
    det = [G(100, 120, 0, 400), G(102, 118, 200, 500)]
    s = score.match_page(det, ref, 1000, [])
    assert s["cov_det_union"] == 500 and s["cov_det"] == 500 and s["cov_ref"] == 500


def test_score_sets_reports_coverage_primary_and_one_to_one_secondary():
    ref = {"p1": {"gutters": [G(100, 112, y, y + 90) for y in range(0, 1000, 100)],
                  "excluded": [], "width": 1000, "height": 1000}}
    det = {"p1": {"gutters": [G(102, 110, 0, 1000)]}}
    res = score.score_sets({"a": {"detections": det, "refs": ref}})
    assert res["recall"] == 1.0 and res["one_to_one"]["recall"] < 0.2


def test_finlam_pixel_rows_are_not_rescaled():
    tr = [{"class": "ARTICLE-TEXT", "box": [140, 200, 630, 1800]}, {"class": "ARTICLE-TEXT", "box": [644, 200, 1260, 1800]}]
    r = row("Teklia/Newspapers-finlam", width=1400, height=2000, text_regions=tr)
    assert refs.row_reference(r)["gutters"][0][0][:2] == [630.0, 644.0]


# ---- American Stories contender (no weights needed: letterbox, decode, NMS, gutters from boxes)

def test_letterbox_centres_and_pads():
    pytest.importorskip("cv2")
    import asmodel
    img = np.full((400, 200, 3), 50, np.uint8)   # tall page
    im, r, (left, top) = asmodel.letterbox(img, size=640)
    assert im.shape == (640, 640, 3) and r == 1.6 and top == 0 and left == 160
    assert im[0, 0, 0] == 114 and im[320, 320, 0] == 50


def test_decode_maps_boxes_back_to_the_page():
    import asmodel
    out = np.zeros((1, 14, 3), np.float32)
    out[0, :4, 0] = [320, 320, 200, 100]   # centre box in the letterboxed frame
    out[0, 4, 0] = 0.9                      # class article
    out[0, :4, 1] = [322, 322, 198, 100]   # overlapping duplicate, lower score, same class
    out[0, 4, 1] = 0.5
    out[0, :4, 2] = [100, 100, 20, 20]     # below the confidence threshold
    out[0, 5, 2] = 0.001
    d = asmodel.decode(out, 1.6, (160, 0), (400, 200))
    assert len(d) == 1
    x0, y0, x1, y1, conf, k = d[0]
    assert abs(x0 - (220 - 160) / 1.6) < 1e-3 and abs(x1 - (420 - 160) / 1.6) < 1e-3 and k == 0


def test_nms_is_class_agnostic():
    import asmodel
    b = np.array([[0, 0, 100, 100], [5, 5, 100, 100], [200, 0, 300, 100]], np.float32)
    keep = asmodel.nms_agnostic(b, np.array([0.9, 0.8, 0.7]), 0.1, 2000)
    assert sorted(keep.tolist()) == [0, 2]


def test_gutters_from_boxes_use_article_only_and_ignore_nontext():
    import asmodel
    dets = [(10, 10, 450, 900, 0.9, 0), (470, 10, 900, 900, 0.9, 0), (0, 0, 1000, 5, 0.9, 3)]  # headline: neither
    g = asmodel.gutters_from_boxes(dets, (1000, 1000), (1.0, 1.0))
    assert len(g) == 1 and g[0][0][:2] == [450.0, 470.0]
    dets.append((440, 0, 480, 1000, 0.9, 9))   # a table covering the gutter: the gutter is ignored
    assert asmodel.gutters_from_boxes(dets, (1000, 1000), (1.0, 1.0)) == []


# ---- second amendment: precision a / (a + b)

def test_gutter_through_the_middle_of_a_text_zone_is_b():
    zones = [[100, 0, 400, 1000]]
    s = score.match_page([G(240, 260, 100, 600)], [], 1000, [], zones)
    assert s["a_det"] == 0 and s["b_det"] == 500 and s["ab_det"] == 500   # precision 0


def test_full_height_gutter_past_headings_is_a_plus_c():
    ref = [G(100, 120, 0, 300), G(100, 120, 700, 1000)]                  # reference breaks around headings
    zones = [[0, 0, 98, 300], [122, 0, 300, 300], [0, 700, 98, 1000], [122, 700, 300, 1000]]
    s = score.match_page([G(104, 116, 0, 1000)], ref, 1000, [], zones)
    assert s["a_det"] == 600 and s["b_det"] == 0 and s["ab_det"] == 600       # a / (a + b) = 1
    assert s["cov_det_union"] == 1000                                         # (c) is the 400 px of headings
    p = s["a_det"] / s["ab_det"]
    assert p == 1.0 and s["cov_det"] / s["cov_det_union"] == 0.6             # coverage precision is the secondary


def test_a_wins_where_a_and_b_overlap():
    ref = [G(100, 120, 0, 500)]
    zones = [[50, 0, 170, 500]]       # the zone shrunk by 10 px still covers x 60..160, the gutter is inside it too
    s = score.match_page([G(104, 116, 0, 500)], ref, 1000, [], zones)
    assert s["a_det"] == 500 and s["b_det"] == 0


def test_zone_shrunk_by_the_tolerance_excludes_a_gutter_at_its_edge():
    zones = [[100, 0, 400, 1000]]          # shrunk to x 110..390 at width 1000
    s = score.match_page([G(105, 115, 0, 1000)], [], 1000, [], zones)
    assert s["b_det"] == 0                  # the gutter reaches outside the shrunk zone
    s = score.match_page([G(112, 118, 0, 1000)], [], 1000, [], zones)
    assert s["b_det"] == 1000


def test_page_with_no_a_or_b_adds_nothing_to_precision():
    import numpy as np
    page1 = {"det_height": 10.0, "matched_det_height": 0.0, "ref_height": 0.0, "matched_ref_height": 0.0,
             "ab_det": 0.0, "a_det": 0.0, "cov_ref_union": 0.0, "cov_ref": 0.0}
    page2 = dict(page1, ab_det=100.0, a_det=80.0, cov_ref_union=100.0, cov_ref=50.0)
    v = score._vec([page1, page2], "primary")
    p, r = score.set_pr(v)
    assert p == 0.8 and r == 0.5


def test_score_sets_primary_precision_is_a_over_ab_with_secondaries():
    ref = {"p1": {"gutters": [G(100, 120, 0, 300), G(100, 120, 700, 1000)], "excluded": [], "width": 1000,
                  "height": 1000, "zones": [[0, 0, 98, 300], [122, 0, 300, 300]]}}
    det = {"p1": {"gutters": [G(104, 116, 0, 1000)]}}
    res = score.score_sets({"a": {"detections": det, "refs": ref}})
    assert res["precision"] == 1.0 and abs(res["coverage"]["precision"] - 0.6) < 1e-9
    assert "one_to_one" in res and abs(res["per_set"]["a"]["unscored_share_c"] - 0.4) < 1e-9
