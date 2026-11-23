"""Tests for the ALTO-vs-columns signals, the planted merge, the join and test B, on synthetic pages.

Method sources: step 1 of #2107 (#2112; after jscancella/NYTribuneOCRExperiments findText_usingSums.py, GPL-3.0,
reimplemented, no code copied); #2103; Beyond Words (LoC, CC0, biglam/loc_beyond_words@6c7f5fb3);
American Stories (Dell et al., arXiv:2308.12477).
"""
import json
import os
import sys

import numpy as np
import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "layout_columns"))
import altolayout  # noqa: E402
import join_bw  # noqa: E402
import signals  # noqa: E402
import testa  # noqa: E402
import testb  # noqa: E402

W, H = 1000, 1000
G = [[[495, 505, 0, 1000]]]  # one full-height gutter, centre x 500


def alto(lines, blocks=None, page=(2000, 2000)):
    """ALTO v2 text. lines: [(hpos, vpos, width, height, [words])] in ALTO units; blocks: list of lists of line indexes."""
    out = [f'<alto xmlns="http://www.loc.gov/standards/alto/ns-v2#"><Layout><Page WIDTH="{page[0]}" HEIGHT="{page[1]}">'
           '<PrintSpace>']
    blocks = blocks if blocks is not None else [list(range(len(lines)))]
    for b in blocks:
        out.append('<TextBlock>')
        for i in b:
            x, y, w, h, words = lines[i]
            out.append(f'<TextLine HPOS="{x}" VPOS="{y}" WIDTH="{w}" HEIGHT="{h}">')
            cx = x
            for t in words:
                out.append(f'<String CONTENT="{t}" HPOS="{cx}" VPOS="{y}" WIDTH="40" HEIGHT="{h}" WC="0.9"/>')
                cx += 50
            out.append('</TextLine>')
        out.append('</TextBlock>')
    out.append('</PrintSpace></Page></Layout></alto>')
    return "".join(out).encode()


# ---------------------------------------------------------------- crossing

def test_crossing_basic_and_one_percent_margin_is_inclusive():
    segs = signals.segments(G)
    assert signals.box_crosses([400, 0, 600, 10], segs, W)
    assert signals.box_crosses([490, 0, 600, 10], segs, W)        # exactly 1% (10 px) left of the centre
    assert not signals.box_crosses([490.5, 0, 600, 10], segs, W)  # a little less
    assert signals.box_crosses([400, 0, 510, 10], segs, W)        # exactly 1% right
    assert not signals.box_crosses([400, 0, 509.5, 10], segs, W)
    assert not signals.box_crosses([600, 0, 700, 10], segs, W)    # gutter centre not inside


def test_crossing_half_height_is_inclusive():
    segs = signals.segments([[[495, 505, 0, 100]]])
    assert signals.box_crosses([400, 50, 600, 150], segs, W)      # gutter covers 50 of 100: exactly half
    assert not signals.box_crosses([400, 51, 600, 151], segs, W)  # 49 of 100
    assert not signals.box_crosses([400, 100, 600, 200], segs, W)  # touches only


def test_crossing_unions_segments_and_ignores_zero_height():
    # two segments of one gutter, a hole between: 30 + 30 of a 100-high box is 60% (union), the hole is not covered
    segs = signals.segments([[[495, 505, 0, 30], [495, 505, 70, 100]]])
    assert signals.box_crosses([400, 0, 600, 100], segs, W)
    segs2 = signals.segments([[[495, 505, 0, 30], [495, 505, 30, 60]]])
    assert signals.box_crosses([400, 0, 600, 120], segs2, W)      # touching segments: 60 of 120, exactly half
    # overlapping segments are not double counted
    segs3 = signals.segments([[[495, 505, 0, 40]], [[495, 505, 20, 60]]])
    assert not signals.box_crosses([400, 0, 600, 130], segs3, W)  # union 60 of 130
    assert not signals.box_crosses([400, 5, 600, 5], segs, W)
    assert not signals.box_crosses(None, segs, W)


def test_crossing_uses_each_segments_own_centre():
    # the gutter drifts: its upper segment's centre is left of the box, its lower one's inside
    segs = signals.segments([[[100, 110, 0, 50], [495, 505, 50, 100]]])
    assert not signals.box_crosses([400, 0, 600, 60], segs, W)    # only 10 of 60 inside
    assert signals.box_crosses([400, 40, 600, 100], segs, W)      # 50 of 60


def test_page_width_sets_the_margin():
    segs = signals.segments(G)
    assert signals.box_crosses([480, 0, 520, 10], segs, 2000)     # 1% of 2000 is 20: exactly 20 each side
    assert not signals.box_crosses([480, 0, 520, 10], segs, 2001)


# ---------------------------------------------------------------- cross_share

def test_cross_share_is_height_weighted():
    segs = signals.segments(G)
    boxes = [[400, 0, 600, 30], [100, 40, 300, 50], None, [0, 60, 10, 60]]
    assert signals.cross_share_of(boxes, segs, W) == pytest.approx(30 / 40)
    assert signals.cross_share_of([None], segs, W) is None


# ---------------------------------------------------------------- ALTO parsing

def test_parse_maps_by_proportion_and_numbers_lines_in_document_order():
    xml = alto([(100, 200, 800, 40, ["a", "b"]), (1100, 200, 800, 40, ["c"])], blocks=[[0], [1]])
    p = altolayout.parse(xml, (1000, 1000))
    assert [ln["idx"] for ln in p["lines"]] == [0, 1]
    assert p["lines"][0]["box"] == [50, 100, 450, 120]
    assert p["lines"][0]["words"][0]["box"] == [50, 100, 70, 120]
    assert [ln["block"] for ln in p["lines"]] == [0, 1]
    assert len(p["blocks"]) == 2 and p["blocks"][1]["box"] == [550, 100, 950, 120]


def test_parse_line_without_attributes_uses_string_union_and_empty_line_keeps_its_index():
    xml = (b'<alto><Layout><Page WIDTH="100" HEIGHT="100"><TextBlock><TextLine/><TextLine>'
           b'<String CONTENT="x" HPOS="10" VPOS="20" WIDTH="5" HEIGHT="6"/><String CONTENT="y" HPOS="30" VPOS="21" WIDTH="5" HEIGHT="6"/>'
           b'</TextLine></TextBlock></Page></Layout></alto>')
    p = altolayout.parse(xml, (100, 100))
    assert p["lines"][0]["box"] is None and p["lines"][1]["idx"] == 1
    assert p["lines"][1]["box"] == [10, 20, 35, 27]


def test_parse_rejects_missing_page_size_and_page_dims_reads_the_head():
    with pytest.raises(ValueError):
        altolayout.parse(b"<alto><Layout><Page/></Layout></alto>", (10, 10))
    assert altolayout.page_dims(alto([(0, 0, 10, 10, ["a"])], page=(1234, 5678))) == (1234.0, 5678.0)
    assert altolayout.page_dims(b"<not xml") is None


# ---------------------------------------------------------------- column segments

def test_column_segments_one_gutter_and_a_short_one():
    rects, n = signals.column_segments([[[495, 505, 0, 1000]]], W, H)
    assert n == 2 and sorted((r[1], r[2]) for r in rects) == [(0.0, 500.0), (500.0, 1000.0)]
    # a second gutter over the lower half adds a third column there, and only there
    rects, n = signals.column_segments([[[495, 505, 0, 1000]], [[795, 805, 500, 1000]]], W, H)
    assert n == 4  # left, right-upper, then (500..800) and (800..1000) below; the left column spans both halves
    left = [r for r in rects if r[1] == 0.0]
    assert len({r[0] for r in left}) == 1 and sum(r[4] - r[3] for r in left) == 1000


def test_column_segments_none_without_gutters_and_rows_outside_are_not_columns():
    assert signals.column_segments([], W, H) == ([], 0)
    rects, n = signals.column_segments([[[495, 505, 200, 700]]], W, H)
    assert n == 2 and min(r[3] for r in rects) == 200 and max(r[4] for r in rects) == 700


def test_block_split_counts_blocks_by_centre_per_segment():
    rects, n = signals.column_segments(G, W, H)
    blocks = [{"box": [10, 10, 100, 100]}, {"box": [10, 200, 100, 300]}, {"box": [600, 10, 700, 100]}, {"box": None}]
    assert signals.block_split_of(blocks, rects, n) == pytest.approx(3 / 2)
    assert signals.block_split_of(blocks, [], 0) is None


def test_missed_ink_counts_ink_without_a_word_box():
    ink = np.zeros((100, 100), dtype=bool)
    ink[10:20, 10:40] = True   # 300 px, covered by a word
    ink[30:40, 10:40] = True   # 300 px, not covered
    ink[80:90, 10:40] = True   # 300 px, in rows with no gutter
    rects = [(0, 0.0, 50.0, 0.0, 50.0), (1, 50.0, 100.0, 0.0, 50.0)]
    m = signals.missed_ink_of(ink, [[10, 10, 40, 20]], rects, (100, 100))
    assert m == pytest.approx(0.5)
    assert signals.missed_ink_of(ink, [], [], (100, 100)) is None
    assert signals.missed_ink_of(np.zeros((10, 10), bool), [], rects, (10, 10)) is None


def test_page_signals_end_to_end():
    xml = alto([(100, 100, 1800, 40, ["a", "b"]), (100, 200, 700, 40, ["c"]), (1100, 200, 700, 40, ["d"])],
               blocks=[[0], [1], [2]])
    p = altolayout.parse(xml, (W, H))
    s = signals.page_signals(p, G, (W, H), np.zeros((H, W), bool))
    assert s["line_cross"] == {0: True, 1: False, 2: False}
    assert s["cross_share"] == pytest.approx(1 / 3)
    assert s["block_split"] == pytest.approx(3 / 2) and s["missed_ink"] is None  # no ink at all


# ---------------------------------------------------------------- planted merge

def test_plant_merge_pairs_lines_across_the_gutter_and_raises_cross_share():
    boxes = [[50, 100, 480, 120], [520, 100, 950, 120], [50, 130, 480, 150], [520, 131, 950, 151]]
    segs = signals.segments(G)
    assert signals.cross_share_of(boxes, segs, W) == 0
    new, merges = signals.plant_merge(boxes, G)
    assert merges == 2 and len(new) == 2
    assert signals.cross_share_of(new, segs, W) == 1.0
    assert [50, 100, 950, 120] in new and [50, 130, 950, 151] in new


def test_plant_merge_needs_vertical_overlap_and_a_gutter_at_that_height():
    g = [[[495, 505, 0, 300]]]
    # beside, but not overlapping by half of the shorter height
    new, merges = signals.plant_merge([[50, 100, 480, 120], [520, 115, 950, 135]], g)
    assert merges == 0 and len(new) == 2
    # no gutter at y=500
    new, merges = signals.plant_merge([[50, 500, 480, 520], [520, 500, 950, 520]], g)
    assert merges == 0
    # exactly half of the shorter height overlaps: merges
    new, merges = signals.plant_merge([[50, 100, 480, 120], [520, 110, 950, 130]], g)
    assert merges == 1


def test_nearest_gutter_ties_go_left_and_each_line_merges_once():
    # gutters centred at 300 and 700; the line's right edge is 500: a tie, the left gutter (300) wins
    g = [[[295, 305, 0, 1000]], [[695, 705, 0, 1000]]]
    boxes = [[250, 0, 500, 20], [100, 0, 290, 20], [710, 0, 900, 20]]
    new, merges = signals.plant_merge(boxes, g)
    # visiting order is (vertical centre, x0): the line at x 100 goes first, its right edge 290 is nearest gutter 300;
    # its partner is on the other side of 300: the line [250, 500] (centre 375). The line [710, 900] stays alone.
    assert merges == 1 and [100, 0, 500, 20] in new and [710, 0, 900, 20] in new


def test_plant_merge_picks_the_partner_nearest_the_gutter():
    g = [[[495, 505, 0, 1000]]]
    boxes = [[50, 0, 480, 20], [900, 0, 990, 20], [520, 0, 700, 20]]
    new, merges = signals.plant_merge(boxes, g)
    assert merges == 1 and [50, 0, 700, 20] in new and [900, 0, 990, 20] in new


# ---------------------------------------------------------------- AUC

def test_auc_ties_and_missing_class():
    assert signals.auc([0, 0, 1, 1], [0.1, 0.2, 0.3, 0.4]) == 1.0
    assert signals.auc([0, 1], [0.5, 0.5]) == 0.5
    assert signals.auc([1, 1], [0.1, 0.2]) is None
    assert signals.auc([0, 0, 1, 1], [0.4, 0.3, 0.2, 0.1]) == 0.0


# ---------------------------------------------------------------- the join

def test_decide_statuses_and_tolerance():
    g = {"seq-1": (1000.0, 1000.0), "seq-2": (1000.0, 1300.0)}
    assert join_bw.decide(500, 1000, {}) == ("issue_not_in_tar", None, 0)
    assert join_bw.decide(769, 1000, g) == ("joined", "seq-2", 1)
    assert join_bw.decide(1000, 1000, g) == ("joined", "seq-1", 1)
    assert join_bw.decide(500, 1000, g)[0] == "no_aspect_match"
    assert join_bw.decide(1000, 1000, {"a": (1000.0, 1000.0), "b": (1005.0, 1000.0)})[0] == "ambiguous"
    # 1% exactly: 1010/1000 against 1.0 is within, 1011 is not
    assert join_bw.decide(1000, 1000, {"a": (1010.0, 1000.0)})[0] == "joined"
    assert join_bw.decide(1000, 1000, {"a": (1011.0, 1000.0)})[0] == "no_aspect_match"


def test_parse_member_and_sha_order():
    assert join_bw.parse_member("sn83045211/1918/11/06/ed-1/seq-3/ocr.xml") == (("sn83045211", "1918-11-06", 1), "seq-3")
    assert join_bw.parse_member("sn83045211/1918/11/06/ed-1/seq-3/ocr.txt") is None
    assert join_bw.parse_member("a/b/ocr.xml") is None
    assert join_bw.sha_key("6") == __import__("hashlib").sha256(b"2107:s2:6").hexdigest()


def test_scan_batch_reads_a_tar_stream_and_writes_joined_alto(tmp_path):
    import bz2
    import io
    import tarfile
    tp = tmp_path / "b_ver01.tar.bz2"
    with tarfile.open(tp, "w:bz2") as tf:
        for name, page in [("sn1/1900/01/02/ed-1/seq-1/ocr.xml", (1000, 1400)), ("sn1/1900/01/02/ed-1/seq-1/ocr.txt", None),
                           ("sn1/1900/01/02/ed-1/seq-2/ocr.xml", (1000, 1000)), ("sn1/1900/01/03/ed-1/seq-1/ocr.xml", (1000, 1400)),
                           ("sn1/1900/01/04/ed-1/seq-1/ocr.xml", (1000, 1000)), ("sn1/1900/01/04/ed-1/seq-2/ocr.xml", (1000, 1000))]:
            data = alto([(0, 0, 10, 10, ["a"])], page=page) if page else b"text"
            ti = tarfile.TarInfo(name)
            ti.size = len(data)
            tf.addfile(ti, io.BytesIO(data))
    out_alto = tmp_path / "alto"
    out_alto.mkdir()
    pages = [{"page_id": "p1", "split": "train", "batch": "b_ver01", "lccn": "sn1", "date": "1900-01-02", "edition": 1, "width": 714, "height": 1000},
             {"page_id": "p2", "split": "train", "batch": "b_ver01", "lccn": "sn1", "date": "1900-01-04", "edition": 1, "width": 1000, "height": 1000},
             {"page_id": "p3", "split": "val", "batch": "b_ver01", "lccn": "sn1", "date": "1900-01-09", "edition": 1, "width": 1000, "height": 1000}]
    _, res = join_bw.scan_batch(("b_ver01", str(tp), pages, str(out_alto)))
    assert res["p1"]["status"] == "joined" and res["p1"]["seq"] == "seq-1" and res["p1"]["issue_pages"] == 2
    assert res["p2"]["status"] == "ambiguous" and res["p2"]["within_1pct"] == 2
    assert res["p3"]["status"] == "issue_not_in_tar"
    assert os.listdir(out_alto) == ["p1.xml"]
    assert altolayout.page_dims((out_alto / "p1.xml").read_bytes()) == (1000.0, 1400.0)


# ---------------------------------------------------------------- test B and test A on synthetic pages

def make_join(tmp_path, pages):
    """pages: {page id: ALTO bytes}; every page is 1000 x 1000 with no ink."""
    os.makedirs(tmp_path / "alto")
    os.makedirs(tmp_path / "cache")
    join = {"_meta": {"counts": {}}, "pages": {}}
    for pid, xml in pages.items():
        (tmp_path / "alto" / f"{pid}.xml").write_bytes(xml)
        np.savez(tmp_path / "cache" / f"{pid}.2000.npz", gray=np.full((50, 50), 255, np.uint8), size=np.array([W, H]))
        join["pages"][pid] = {"status": "joined", "lccn": "sn1", "date": "1900-01-01"}
    return join


def two_column_alto(n=6):
    ls = []
    for i in range(n):
        ls.append((100, 100 + 100 * i, 800, 40, ["a", "b"]))    # ALTO units are 2x pixels: x 50..450, a left column line
        ls.append((1100, 100 + 100 * i, 800, 40, ["c", "d"]))   # x 550..950
    return alto(ls, blocks=[list(range(len(ls)))])


def test_test_b_separates_planted_pages_and_counts_skips(tmp_path):
    join = make_join(tmp_path, {"a": two_column_alto(), "b": two_column_alto(4), "c": two_column_alto(3)})
    gut = {"a": {"gutters": G}, "b": {"gutters": G}, "c": {"gutters": []}}
    r = testb.test_b(str(tmp_path), join, gut, 300)
    assert r["pages_scored"] == 2 and r["skipped"]["no_gutter"] == 1
    assert r["auc"] == 1.0 and r["passes"]
    assert r["mean_cross_share_original"] == 0 and r["mean_cross_share_planted"] == 1
    # a page with a gutter but nothing beside it
    join2 = make_join(tmp_path / "x", {"d": alto([(100, 100, 800, 40, ["a"])])})
    r2 = testb.test_b(str(tmp_path / "x"), join2, {"d": {"gutters": G}}, 300)
    assert r2["pages_scored"] == 0 and r2["skipped"]["no_line_beside_gutter"] == 1 and not r2["passes"]


def test_test_b_limit_takes_the_first_in_sha_order(tmp_path):
    join = make_join(tmp_path, {str(i): two_column_alto() for i in range(5)})
    gut = {str(i): {"gutters": G} for i in range(5)}
    r = testb.test_b(str(tmp_path), join, gut, 3)
    assert r["pages_selected"] == 3 and len(r["per_page"]) == 3
    first3 = sorted(join["pages"], key=join_bw.sha_key)[:3]
    assert sorted(r["per_page"]) == sorted(first3)


def test_distributions_and_top20(tmp_path):
    join = make_join(tmp_path, {"a": two_column_alto(), "b": alto([(100, 100, 1800, 40, ["x"])] + [(100, 200, 700, 40, ["y"])])})
    gut = {"a": {"gutters": G}, "b": {"gutters": G}}
    d, rows = testb.distributions(str(tmp_path), join, gut, ["a", "b"], progress=False)
    assert d["pages"] == 2 and d["top20_cross_share"][0]["page_id"] == "b"
    assert rows["a"]["cross_share"] == 0 and rows["b"]["cross_share"] == pytest.approx(0.5)
    assert d["missed_ink"]["none"] == 2  # no ink on the synthetic pages


def test_add_layout_gives_word_line_and_page_features(tmp_path):
    xml = alto([(100, 100, 1800, 40, ["a", "b", "c"]), (100, 200, 700, 40, ["d"])], blocks=[[0, 1]])
    (tmp_path / "p.xml").write_bytes(xml)
    gut = {"pg": {"gutters": G, "width": W, "height": H}}
    mk = lambda i, line, box: {"page_id": "pg", "source_image": "p.jpg", "line": line, "box": box, "label": 1, "word_index": i}
    # words: line 0 string 'a' (x 50..70), 'b' (75..95), 'c' (100..120): none of them spans the gutter on its own;
    # a long synthetic word box that does, and one on the uncrossed line
    rows = [mk(0, 0, [50, 50, 20, 20]), mk(1, 0, [400, 50, 200, 20]), mk(2, 1, [50, 100, 20, 20]), mk(3, 1, [50, 50, 20, 20])]
    st, mismatch = testa.add_layout(rows, str(tmp_path), gut)
    assert [r["word_cross"] for r in rows] == [0.0, 1.0, 0.0, 0.0]
    assert [r["on_cross_line"] for r in rows] == [1.0, 1.0, 0.0, 0.0]
    assert rows[0]["cross_share"] == pytest.approx(0.5) and st["pg"]["crossing_lines"] == 1
    assert mismatch == 1  # the last word names line 1 but sits on line 0


def test_testa_is_withheld_when_test_b_fails(tmp_path):
    tb = tmp_path / "tb.json"
    tb.write_text(json.dumps({"banded": {"test_b": {"auc": 0.5, "passes": False}}}))
    out = tmp_path / "o.json"
    testa.main(["--set", "x,y,z", "--gutters", "g", "--render-compare-dir", "d", "--testb-json", str(tb), "--out", str(out)])
    r = json.load(open(out))
    assert r["reported"] is False and "banded" not in r


def test_bootstrap_gain_is_deterministic(tmp_path):
    cmp_dir = os.environ.get("RENDER_COMPARE_DIR")
    if not cmp_dir:
        pytest.skip("needs #2103's compare.py (RENDER_COMPARE_DIR)")
    sys.path.insert(0, cmp_dir)
    rng = np.random.default_rng(1)
    n = 400
    groups = np.array([f"p{i % 8}" for i in range(n)])
    y = (rng.random(n) < 0.3).astype(int)
    sb = rng.random(n) + 0.5 * y
    sc = sb + 0.2 * y
    masks = {"all": np.ones(n, bool)}
    a = testa.bootstrap_gain(sb, sc, y, groups, masks, 50, 2107)
    b = testa.bootstrap_gain(sb, sc, y, groups, masks, 50, 2107)
    assert a == b and a["all"][0] > 0


def test_json_keys_are_sorted_in_testb_output(tmp_path):
    join = make_join(tmp_path, {"a": two_column_alto()})
    (tmp_path / "join.json").write_text(json.dumps(join))
    (tmp_path / "g.json").write_text(json.dumps({"a": {"gutters": G, "width": W, "height": H}}))
    testb.main(["--join-dir", str(tmp_path), "--gutters", str(tmp_path / "g.json"), "--out", str(tmp_path / "o.json")])
    text = (tmp_path / "o.json").read_text()
    d = json.loads(text)
    assert list(d) == sorted(d) and list(d["banded"]) == sorted(d["banded"])


def test_scan_batch_handles_an_issue_split_in_the_tar(tmp_path):
    import io
    import tarfile
    tp = tmp_path / "b_ver01.tar.bz2"
    names = [("sn1/1900/01/02/ed-1/seq-1/ocr.xml", (1000, 1400)), ("sn1/1900/01/03/ed-1/seq-1/ocr.xml", (1000, 1000)),
             ("sn1/1900/01/02/ed-1/seq-2/ocr.xml", (1000, 1000))]
    with tarfile.open(tp, "w:bz2") as tf:
        for name, page in names:
            data = alto([(0, 0, 10, 10, ["a"])], page=page)
            ti = tarfile.TarInfo(name)
            ti.size = len(data)
            tf.addfile(ti, io.BytesIO(data))
    out_alto = tmp_path / "alto"
    out_alto.mkdir()
    pages = [{"page_id": "p1", "split": "train", "batch": "b_ver01", "lccn": "sn1", "date": "1900-01-02", "edition": 1, "width": 1000, "height": 1000}]
    _, res = join_bw.scan_batch(("b_ver01", str(tp), pages, str(out_alto)))
    assert res["p1"]["status"] == "joined" and res["p1"]["issue_pages"] == 2 and res["p1"]["seq"] == "seq-2"
    assert not any(n.startswith("spill") for n in os.listdir(tmp_path))


# ---------------------------------------------------------------- METS join

METS = """<mets xmlns="http://www.loc.gov/METS/" xmlns:mods="http://www.loc.gov/mods/v3" xmlns:xlink="http://www.w3.org/1999/xlink">
<dmdSec ID="issueModsBib"><mdWrap><xmlData><mods:mods/></xmlData></mdWrap></dmdSec>
%s
<fileSec>%s</fileSec>
<structMap><div TYPE="np:issue">%s</div></structMap></mets>"""


def write_mets(path, pages):
    """pages: [(reel, reel seq)] -> an issue METS with np:page divs in that order."""
    dmd = "".join(f'<dmdSec ID="pageModsBib{i}"><mdWrap><xmlData><mods:mods><mods:relatedItem>'
                  f'<mods:identifier type="reel number">{r}</mods:identifier>'
                  f'<mods:identifier type="reel sequence number">{s}</mods:identifier></mods:relatedItem></mods:mods></xmlData></mdWrap></dmdSec>'
                  for i, (r, s) in enumerate(pages, 1))
    fs = "".join(f'<fileGrp ID="g{i}"><file ID="m{i}" USE="master"><FLocat xlink:href="./{int(s):04d}.tif"/></file>'
                 f'<file ID="ocrFile{i}" USE="ocr"><FLocat xlink:href="./{int(s):04d}.xml"/></file></fileGrp>' for i, (r, s) in enumerate(pages, 1))
    st = "".join(f'<div TYPE="np:page" DMDID="pageModsBib{i}"><fptr FILEID="m{i}"/><fptr FILEID="ocrFile{i}"/></div>' for i in range(1, len(pages) + 1))
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(METS % (dmd, fs, st))


def test_mets_join_matches_reel_and_sequence_exactly(tmp_path):
    import join_mets
    mets = tmp_path / "mets"
    write_mets(str(mets / "b_ver01/mets/sn1/1900010201.xml"), [("R1", "5"), ("R1", "6"), ("R2", "6"), ("R1", "7")])
    write_mets(str(mets / "b_ver01/mets/sn1/1900010301.xml"), [("R1", "5"), ("R1", "5")])
    refs = tmp_path / "refs"
    refs.mkdir()
    def row(pid, batch, date, reel, frame, ed=1):
        return {"page_id": pid, "split": "train", "width": 10, "height": 10, "image_path": "x",
                "meta": {"ca_identity": {"batch": batch, "lccn": "sn1", "date": date, "edition": ed, "reel": reel, "frame": frame}}}
    rows = [row("a", "b_ver01", "1900-01-02", "R1", 6), row("b", "b_ver01", "1900-01-02", "R2", 6), row("c", "b_ver01", "1900-01-02", "R9", 6),
            row("d", "b_ver01", "1900-01-03", "R1", 5), row("e", "b_ver01", "1900-01-09", "R1", 5), row("f", "other_ver01", "1900-01-02", "R1", 6),
            row("g", "b_ver01", "1900-01-02", "R1", 5, ed=2)]
    (refs / "biglam__loc_beyond_words__train.jsonl").write_text("\n".join(json.dumps(r) for r in rows))
    (refs / "biglam__loc_beyond_words__val.jsonl").write_text("")
    out = join_mets.mets_join(str(refs), str(mets), 300)
    st = {p: v["status"] for p, v in out["pages"].items()}
    assert st == {"a": "joined", "b": "joined", "c": "no_page_match", "d": "multiple_match", "e": "no_mets_issue",
                  "f": "no_mets_batch", "g": "no_mets_issue"}
    assert out["pages"]["a"]["seq"] == 2 and out["pages"]["b"]["seq"] == 3 and out["pages"]["a"]["ocr"] == "./0006.xml"
    assert out["pages"]["a"]["mets_pages"] == 4
    assert join_mets.member_name(out["pages"]["a"]) == "sn1/1900/01/02/ed-1/seq-2/ocr.xml"
    assert out["_meta"]["selected"] == sorted(["a", "b"], key=join_mets.sha_key)
    assert join_mets.mets_join(str(refs), str(mets), 1)["_meta"]["selected"] == out["_meta"]["selected"][:1]


def test_fetch_batch_writes_the_named_member_and_counts_issue_pages(tmp_path):
    import io
    import tarfile
    import join_mets
    tp = tmp_path / "b_ver01.tar.bz2"
    with tarfile.open(tp, "w:bz2") as tf:
        for k in (1, 2, 3):
            data = alto([(0, 0, 10, 10, ["a"])], page=(1000 + k, 1400))
            ti = tarfile.TarInfo(f"sn1/1900/01/02/ed-1/seq-{k}/ocr.xml")
            ti.size = len(data)
            tf.addfile(ti, io.BytesIO(data))
    (tmp_path / "alto").mkdir()
    v = {"lccn": "sn1", "date": "1900-01-02", "edition": 1, "seq": 2}
    _, out = join_mets.fetch_batch(("b_ver01", str(tp), {"p": v, "q": dict(v, seq=9)}, str(tmp_path / "alto")))
    assert out["p"] == {"alto_size": (1002.0, 1400.0), "tar_issue_pages": 3, "found": True}
    assert out["q"]["found"] is False and sorted(os.listdir(tmp_path / "alto")) == ["p.xml"]


def test_subset_aucs():
    pp = {"a": {"cross_share": 0.1, "cross_share_planted": 0.9}, "b": {"cross_share": 0.1, "cross_share_planted": 0.2},
          "c": {"cross_share": 0.5, "cross_share_planted": 0.4}, "d": {"skipped": "no_gutter"}}
    r = testb.subset_aucs(pp, {"a", "b"})
    assert r["in_subset"]["pages_scored"] == 2 and r["in_subset"]["auc"] == 1.0
    assert r["not_in_subset"]["pages_scored"] == 1 and r["not_in_subset"]["auc"] == 0.0
