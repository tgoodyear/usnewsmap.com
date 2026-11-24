"""Tests for bwcap.py (#2122 Beyond Words caption test). Run: python3 -m pytest scripts/ocr-survey/segfirst/test_bwcap.py"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import bwcap  # noqa: E402


def region(i, lccn, seq, caption="A caption that is long enough here", cat="Photograph", nested=False):
    d = {"caption": caption, "category": cat}
    return {"data": {"values": [d], "task_prompt": "x"} if nested else d, "height": 1000, "id": f"r{i:03d}",
            "location": {"standard": f"http://s3.amazonaws.com/ndnp-jpeg-surrogates/xx_b_ver01/data/{lccn}/reel/1917010101/{seq:04d}.jpg"},
            "meta_data": {"alto": "a", "subject_url": f"http://chroniclingamerica.loc.gov/lccn/{lccn}/1917-01-01/ed-1/seq-{seq}/"},
            "region": {"height": 100, "width": 200, "x": 10, "y": 20}, "width": 800}


def test_pages_and_title_cap():
    regs = [region(i, "sn1" if i < 15 else "sn2", i, nested=i % 2 == 0) for i in range(20)]
    regs.append(region(99, "sn3", 1, caption="too short!"))
    regs.append(region(98, "sn3", 2, cat="Map"))
    pages, st = bwcap.bw_pages(regs)
    assert st["captions"] == 20 and len(pages) == 22
    assert pages["sn1_1917-01-01_ed-1_seq-3"]["captions"][0]["box_bw"] == [10, 20, 210, 120]
    chosen, ranking = bwcap.select(pages)
    assert len(chosen) == 15 and sum(pages[d]["lccn"] == "sn1" for d in chosen) == 10
    assert [r["rank_key"] for r in ranking] == sorted(r["rank_key"] for r in ranking)


def test_inside_and_join():
    ws = [{"block": 1, "box": [0, 0, 10, 10], "line": 1, "text": "a"}, {"block": 1, "box": [12, 0, 20, 10], "line": 1, "text": "b-"},
          {"block": 1, "box": [0, 12, 10, 20], "line": 2, "text": "c"}, {"block": 2, "box": [0, 30, 10, 40], "line": 3, "text": "d"},
          {"block": 2, "box": [50, 50, 60, 60], "line": 3, "text": "out"}]
    sel = [w for w in ws if bwcap.inside(w, [0, 0, 30, 41])]
    assert bwcap.join_words(sel) == "a b-\nc\n\nd"
    assert bwcap.score2122.tokens(bwcap.join_words(sel)) == ["a", "bc", "d"]


def test_bootstrap_paired_and_seeded():
    rows = [{"L": [1, 2, 3, 4, 1, 10], "W": [2, 2, 4, 4, 0, 10], "page": f"p{i}", "title": f"t{i % 3}", "id": str(i)} for i in range(9)]
    a, b = bwcap.cluster_bootstrap(rows), bwcap.cluster_bootstrap(rows)
    assert a == b
    assert a["W-L"]["trigram_recall"] == [0.5, 0.5]


def test_large_type():
    rows = [{"id": str(i), "type_size": float(i)} for i in range(25)] + [{"id": "none", "type_size": None}]
    assert bwcap.large_type(rows) == {"24", "23"}
