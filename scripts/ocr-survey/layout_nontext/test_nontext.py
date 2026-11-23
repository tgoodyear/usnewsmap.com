"""Tests on synthetic boxes for nontext.py (#2107). Run: python -m pytest test_nontext.py"""
import os
import sys

import numpy as np
import pytest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import nontext as nt  # noqa: E402


def test_union_not_sum():
    assert nt.union_area([[0, 0, 10, 10], [5, 5, 15, 15]]) == 175
    assert nt.union_area([[0, 0, 10, 10], [0, 0, 10, 10]]) == 100
    assert nt.union_area([[0, 0, 10, 10], [2, 2, 4, 4]]) == 100
    assert nt.union_area([[0, 0, 5, 5], [10, 10, 12, 12]]) == 29
    assert nt.union_area([]) == 0


def test_inter_of_unions():
    a = [[0, 0, 10, 10], [5, 0, 15, 10]]  # union 15 x 10
    b = [[10, 0, 20, 10]]
    assert nt.inter_union_area(a, b) == 50


def test_mapping_all_seven_classes():
    row = {"page_id": "p", "nontext_regions": [{"class": c, "box": [0, 0, 1, 1]} for c in
           ["Photograph", "Illustration", "Map", "Comics/Cartoon", "Editorial Cartoon", "Advertisement"]],
           "text_regions": [{"class": "Headline", "box": [0, 0, 1, 1]}]}
    r = nt.ref_boxes(row)
    assert (len(r[nt.PHOTO]), len(r[nt.CARTOON]), len(r[nt.HEADLINE])) == (1, 5, 1)
    with pytest.raises(ValueError):
        nt.ref_boxes({"page_id": "q", "nontext_regions": [{"class": "Poem", "box": [0, 0, 1, 1]}]})


def test_cut_and_headline_excluded():
    dets = [[0, 0, 10, 10, 0.30, nt.PHOTO], [0, 0, 10, 10, 0.20, nt.CARTOON], [0, 0, 50, 50, 0.9, nt.HEADLINE]]
    refs = {nt.PHOTO: [[0, 0, 10, 5]], nt.CARTOON: [], nt.HEADLINE: [[0, 0, 50, 50]]}
    s = nt.page_stats(refs, dets)["nontext"]
    assert (s["ref_area"], s["pred_area"], s["inter_area"]) == (50, 100, 50)


def test_overlapping_predictions_count_once():
    dets = [[0, 0, 10, 10, 0.9, nt.PHOTO], [0, 0, 10, 10, 0.8, nt.CARTOON]]
    refs = {nt.PHOTO: [[0, 0, 10, 10]], nt.CARTOON: [], nt.HEADLINE: []}
    s = nt.page_stats(refs, dets)["nontext"]
    assert (s["ref_area"], s["pred_area"], s["inter_area"]) == (100, 100, 100)


def test_pooling_not_mean_of_ratios():
    big = {"ref_area": 100.0, "pred_area": 100.0, "inter_area": 100.0, "n_gt": 0,
           "confs": np.zeros(0), "tp": np.zeros(0, bool)}
    small = {"ref_area": 10.0, "pred_area": 10.0, "inter_area": 0.0, "n_gt": 0,
             "confs": np.zeros(0), "tp": np.zeros(0, bool)}
    t = nt.totals([big, small], [0, 1])
    assert t["mask_recall"] == pytest.approx(100 / 110)  # the mean of 1.0 and 0.0 would be 0.5
    assert t["mask_precision"] == pytest.approx(100 / 110)


def test_ap_values():
    assert nt.average_precision(np.array([0.9, 0.8]), np.array([True, True]), 2) == pytest.approx(1.0)
    # one of two reference boxes found, no false positives: precision 1 up to recall 0.5, 51 of 101 points
    assert nt.average_precision(np.array([0.9]), np.array([True]), 2) == pytest.approx(51 / 101)
    # a false positive ranked first halves precision at every recall point
    assert nt.average_precision(np.array([0.9, 0.8]), np.array([False, True]), 1) == pytest.approx(0.5)
    assert nt.average_precision(np.zeros(0), np.zeros(0, bool), 3) == 0.0


def test_matching_one_to_one():
    gt = [[0, 0, 10, 10]]
    confs, tp = nt.match_page(gt, [(0.5, [0, 0, 10, 10]), (0.9, [0, 0, 10, 10])])
    assert list(confs) == [0.9, 0.5] and list(tp) == [True, False]
    _, tp = nt.match_page(gt, [(0.9, [0, 0, 10, 4])])  # IoU 0.4
    assert list(tp) == [False]


def test_recall_at_cut_uses_prefix():
    refs = {nt.PHOTO: [[0, 0, 10, 10], [20, 0, 30, 10]], nt.CARTOON: [], nt.HEADLINE: []}
    dets = [[0, 0, 10, 10, 0.9, nt.PHOTO], [20, 0, 30, 10, 0.1, nt.PHOTO]]
    p = nt.page_stats(refs, dets)
    t = nt.totals([p["photograph"]], [0])
    assert t["box_recall"] == 0.5 and t["n_pred_boxes"] == 1


def test_bootstrap_is_seeded():
    st = [{"ref_area": 10.0, "pred_area": 10.0, "inter_area": float(i % 2) * 10, "n_gt": 0,
           "confs": np.zeros(0), "tp": np.zeros(0, bool)} for i in range(20)]
    a = nt.bootstrap({"x": st}, 20, draws=50)
    b = nt.bootstrap({"x": st}, 20, draws=50)
    assert a == b and a["x"]["mask_recall"][0] < a["x"]["mask_recall"][1]
