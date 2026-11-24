"""Synthetic tests for banded v2 (#2107): refit, scoring adapter, false-gutter measure."""
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "layout_columns"))
import refit  # noqa: E402
import bwfalse  # noqa: E402
import score  # noqa: E402


def test_adapter_keeps_v1_pieces():
    g = [[[100, 117, 0, 40], [101, 118, 40, 80]], [[300, 309, 10, 90]]]
    assert refit.check_adapter(g)
    assert refit.scoring_gutters(refit.v1_lines(g)) == [[[100.0, 117.0, 0.0, 40.0], [101.0, 118.0, 40.0, 80.0]], [[300.0, 309.0, 10.0, 90.0]]]


def test_adapter_v1_scores_equal():
    g = [[[100, 117, 0, 40], [101, 118, 40, 80]]]
    ref = {"p": {"gutters": [[[98, 120, 0, 80]]], "excluded": [], "width": 1000, "height": 100, "zones": [[0, 0, 98, 80]]}}
    a = score.page_rows({"p": {"gutters": g}}, ref)
    b = score.page_rows({"p": {"gutters": refit.scoring_gutters(refit.v1_lines(g))}}, ref)
    assert a == b


def test_adapter_uses_line_x_at_band_centre():
    ln = {"x0": 90, "x1": 110, "y0": 0, "y1": 100, "half": 10.0, "x_top": 95.0, "x_bottom": 105.0}
    assert refit.scoring_gutters([[ln]]) == [[[90.0, 110.0, 0, 100]]]


def test_refit_snaps_to_rule_and_follows_tilt():
    ink = np.zeros((400, 1000), dtype=bool)
    ink[:, 503:505] = True  # a 2 px rule beside the v1 gutter's centre (500)
    ink[:, 420:490] = True  # text left
    ink[:, 515:590] = True  # text right
    r = refit.refit_gutters(ink, [[[495, 505, 0, 400]]])[0][0]
    assert r["fit"] == "rule" and r["theta"] == 0.0 and r["offset"] == 3.5 and r["dark_best"] == 0
    ink = np.zeros((400, 1000), dtype=bool)
    for y in range(400):  # a white channel leaning right going down, 1 degree
        c = int(round(500 + np.tan(np.radians(1.0)) * (y - 200)))
        ink[y, :c - 2] = True  # a 5 px channel: too narrow for a vertical 3 px line over 400 rows
        ink[y, c + 3:] = True
    r = refit.refit_gutters(ink, [[[495, 505, 0, 400]]])[0][0]
    # the smallest |theta| that clears the channel wins (0.6 here), not the channel's own 1 degree
    assert r["fit"] == "white" and r["dark_vertical"] > 0 and r["dark_best"] == 0 and 0.5 <= r["theta"] <= 1.0


def test_inside_length_vertical_and_tilted():
    ln = {"y0": 0.0, "y1": 100.0, "x_top": 50.0, "x_bottom": 50.0}
    assert bwfalse.inside_length(ln, [[40, 20, 60, 80]]) == 60.0
    assert bwfalse.inside_length(ln, [[40, 20, 60, 80], [45, 70, 55, 90]]) == 70.0
    assert bwfalse.inside_length(ln, [[60, 0, 70, 100]]) == 0.0
    ln = {"y0": 0.0, "y1": 100.0, "x_top": 0.0, "x_bottom": 100.0}  # x = y
    assert abs(bwfalse.inside_length(ln, [[0, 0, 50, 100]]) - 50.0) < 1e-9


def test_page_false_half_rule():
    boxes = [[40, 0, 60, 50]]
    lines = [[{"y0": 0.0, "y1": 100.0, "x_top": 50.0, "x_bottom": 50.0}],  # half inside: counts
             [{"y0": 0.0, "y1": 100.0, "x_top": 80.0, "x_bottom": 80.0}]]
    assert bwfalse.page_false(lines, boxes) == [100.0, 200.0]


def test_marked_boxes_headline_width():
    row = {"width": 1000, "nontext_regions": [{"type": "photograph", "box": [0, 0, 10, 10]}, {"type": "other", "box": [0, 0, 5, 5]}],
           "text_regions": [{"type": "headline", "box": [0, 0, 249, 10]}, {"type": "headline", "box": [0, 0, 250, 10]}]}
    assert bwfalse.marked_boxes(row) == [[0, 0, 10, 10], [0, 0, 250, 10]]


def test_iiif_url_from_source_url():
    u = "http://s3.amazonaws.com/ndnp-jpeg-surrogates/dlc_jetblue_ver01/data/sn84026749/00280764450/1918021501/0238.jpg"
    assert bwfalse.scan_stem(u) == "dlc_jetblue_ver01__sn84026749__00280764450__1918021501__0238"
    assert bwfalse.iiif_url(*bwfalse.scan_tail(u)) == ("https://tile.loc.gov/image-services/iiif/service:ndnp:dlc:batch_dlc_jetblue_ver01:"
                                                        "data:sn84026749:00280764450:1918021501:0238/full/full/0/default.jpg")
