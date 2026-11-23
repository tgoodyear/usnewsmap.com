"""Tests on synthetic labels: class maps, IoU drop, YOLO format, sha order (#2107)."""
import hashlib
import os
import sys

import pytest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import classmap as cm  # noqa: E402


def row(ds, regions, nontext=()):
    return {"dataset": ds, "text_regions": [{"class": c, "box": b} for c, b in regions],
            "nontext_regions": [{"class": c, "box": b} for c, b in nontext]}


def test_names_match_teacher_order():
    assert cm.NAMES[0] == "article" and cm.NAMES[9] == "table" and len(cm.NAMES) == 10


def test_every_target_is_a_known_class():
    for m in cm.MAPS.values():
        assert set(v for v in m.values() if v) <= set(cm.NAMES)


def test_finlam_and_liberte_maps():
    r = row("Teklia/Newspapers-finlam", [("ARTICLE-TEXT", [0, 0, 10, 10]), ("ARTICLE-SUBTITLE", [0, 0, 5, 5])],
            [("ANNOUNCEMENT", [1, 1, 4, 4]), ("ARTICLE-ILLUSTRATION", [2, 2, 8, 8])])
    assert [b[4] for b in cm.mapped_boxes(r)] == ["article", "headline", "article", "cartoon_or_advertisement"]
    r = row("Teklia/Newspapers-finlam-La-Liberte", [("ILLUSTRATEDTEXT", [0, 0, 9, 9]), ("TEXT", [0, 0, 9, 9])],
            [("TABLECONTENT", [0, 0, 3, 3]), ("TABLE", [0, 0, 9, 9])])
    assert [b[4] for b in cm.mapped_boxes(r)] == ["article", "table"]


def test_beyond_words_map():
    cls = ["Photograph", "Illustration", "Map", "Comics/Cartoon", "Editorial Cartoon", "Advertisement", "Headline"]
    r = row("biglam/loc_beyond_words", [], [(c, [0, 0, 5, 5]) for c in cls])
    assert [b[4] for b in cm.mapped_boxes(r)] == ["photograph"] + ["cartoon_or_advertisement"] * 5 + ["headline"]


def test_unknown_class_raises():
    with pytest.raises(KeyError):
        cm.mapped_boxes(row("Teklia/Newspapers-finlam", [("WEIRD", [0, 0, 1, 1])]))


def test_iou_drop_same_class_only():
    ver = [(0, 0, 100, 100, "headline")]
    teacher = [(0, 0, 100, 40, "headline"),      # IoU 0.4: dropped
               (0, 0, 100, 25, "headline"),      # IoU 0.25: kept
               (0, 0, 100, 100, "article"),      # other class: kept
               (200, 200, 300, 300, "headline")]  # no overlap: kept
    kept = cm.drop_overlapping(teacher, ver)
    assert kept == teacher[1:]


def test_iou_exactly_threshold_is_kept():
    assert cm.drop_overlapping([(0, 0, 100, 30, "headline")], [(0, 0, 100, 100, "headline")]) != []


def test_teacher_cut():
    dets = [[0, 0, 10, 10, 0.25, "article"], [0, 0, 10, 10, 0.2499, "table"]]
    assert cm.teacher_boxes(dets) == [(0, 0, 10, 10, "article")]


def test_yolo_line_format():
    assert cm.yolo_line("headline", [10, 20, 30, 60], 100, 200) == "3 0.200000 0.200000 0.200000 0.200000"
    assert cm.yolo_line("article", [-5, -5, 50, 50], 100, 100) == "0 0.250000 0.250000 0.500000 0.500000"
    assert cm.yolo_line("table", [10, 10, 11, 50], 100, 100) is None


def test_sha_order_and_holdout():
    ids = [str(i) for i in range(623)]
    o = cm.sha_order(ids)
    assert o[0] == min(ids, key=lambda p: hashlib.sha256(("2107:tr:" + p).encode()).hexdigest())
    tr, va = cm.split_in_order(o)
    assert (len(tr), len(va)) == (561, 62) and va == o[-62:]
    assert cm.holdout_count(2000) == 200 and cm.holdout_count(2846) == 284
