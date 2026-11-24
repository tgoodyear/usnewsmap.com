"""Tests for the combined segmenter (#2107) on synthetic pages: pooling, line fit, features, labels, training,
inference and the volunteer-label exporter."""
import json
import os
import sys

import numpy as np
import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "layout_columns"))
import export_engutter  # noqa: E402
import feats  # noqa: E402
import gbdt  # noqa: E402
import lines  # noqa: E402
import pool  # noqa: E402
import score  # noqa: E402
import wordboxes  # noqa: E402

W, H = 1000, 1400
COLS = ((60, 320), (360, 640), (680, 940))


def text_block(img, x0, x1, y0, y1, rng, words, line=8, gap=5):
    y = y0
    while y + line <= y1:
        x = x0
        while x < x1:
            w = int(rng.integers(8, 30))
            img[y:y + line, x:min(x + w, x1)] = 20
            words.append([x, y, min(x + w, x1), y + line, 90.0])
            x += w + int(rng.integers(3, 6))
        y += line + gap


def synth(seed=0, rule=True, headline=True):
    """A three-column page: a headline across the top (y 60-160), body columns from y 200, a printed rule in
    the second gutter. Returns gray, words, reference page and detections."""
    rng = np.random.default_rng(seed)
    img = np.full((H, W), 235, np.uint8)
    words = []
    if headline:
        text_block(img, 60, 940, 60, 160, rng, words, line=30, gap=6)
    for x0, x1 in COLS:
        text_block(img, x0, x1, 200, 1300, rng, words)
    if rule:
        img[200:1300, 659:661] = 10
    ref = {"width": W, "height": H, "excluded": [],
           "zones": [[x0, 200, x1, 1300] for x0, x1 in COLS],
           "gutters": [[[320, 360, 200, 1300]], [[640, 680, 200, 1300]]]}
    det = {"v1": [[[335, 345, 40, 1300]], [[648, 662, 200, 1300]], [[150, 160, 300, 700]]],
           "as": [[[322, 358, 220, 1280]]]}
    boxes = {"as": [[60, 200, 320, 1300, 0.9, "article"], [360, 200, 640, 1300, 0.8, "article"],
                    [60, 60, 940, 160, 0.7, "headline"]], "tr": []}
    return img, {"words": words, "lines": []}, ref, det, boxes


def test_pool_merges_lined_up_pieces():
    _, _, _, det, _ = synth()
    ps, cands = pool.pool_page(det, W, H)
    xs = sorted(round(np.mean([ps[i]["xc"] for i in c["members"]])) for c in cands)
    assert len(cands) == 3
    first = min(cands, key=lambda c: abs(ps[c["members"][0]]["xc"] - 340))
    assert first["dets"] == ["as", "v1"]
    assert xs[0] < 200 < xs[1]


def test_atoms_split_on_detector_change_and_length():
    _, _, _, det, _ = synth()
    ps, cands = pool.pool_page(det, W, H)
    c = next(c for c in cands if c["dets"] == ["as", "v1"])
    dets = [tuple(a["dets"]) for a in c["atoms"]]
    assert ("v1",) in dets and ("as", "v1") in dets
    assert all(a["y1"] - a["y0"] <= pool.MAX_ATOM * H + 1e-6 for a in c["atoms"])
    assert c["atoms"][0]["x_source"] == "v1"


def test_fit_finds_rule_and_white():
    img, *_ = synth()
    ink = img <= lines.otsu(img)
    f = lines.fit(ink, 655, 300, 400, 7, [])
    assert f["fit"] == "rule" and abs(f["x_top"] - 660) <= 1.5
    g = lines.fit(ink, 338, 300, 400, 5, [])
    assert g["fit"] == "white" and g["dark_best"] == 0 and 320 < g["x_top"] < 360


def test_fit_follows_tilt():
    rng = np.random.default_rng(1)
    img = np.full((1200, 400), 235, np.uint8)
    t = np.tan(np.radians(1.0))
    for y in range(1200):
        x = int(round(200 + t * (y - 600)))
        row = np.where(rng.random(400) < 0.5, 20, 235).astype(np.uint8)
        img[y, : x - 3] = row[: x - 3]
        img[y, x + 4:] = row[x + 4:]
    ink = img <= lines.otsu(img)
    f = lines.fit(ink, 200, 0, 1200, 6, [])
    assert f["fit"] == "white" and f["dark_best"] == 0 and abs(f["theta"] - 1.0) <= 0.25


def test_labels_from_reference():
    _, _, ref, _, _ = synth()
    assert feats.label_ref([339, 341, 400, 500], ref)[0] == 1
    assert feats.label_ref([150, 152, 400, 500], ref)[0] == 0
    assert feats.label_ref([339, 341, 60, 150], ref)[0] == -1
    ref2 = dict(ref, excluded=[[300, 0, 380, 1400]])
    assert feats.label_ref([339, 341, 400, 500], ref2)[0] == -1


def test_negatives_and_line_labels():
    neg = [[300, 100, 400, 300, "photograph"], [0, 50, 1000, 90, "headline"], [330, 600, 350, 700, "headline"]]
    assert feats.label_negatives(340, 340, 120, 280, neg, W)[0] == 0
    assert feats.label_negatives(340, 340, 600, 700, neg, W)[0] == -1  # narrow headline does not count
    assert feats.label_negatives(340, 340, 400, 500, neg, W)[0] == -1
    L = [{"x_top": 342, "x_bottom": 342, "y0": 0, "y1": 1000, "label": 1}]
    assert feats.label_lines(340, 340, 100, 200, L, W) == 1
    assert feats.label_lines(400, 400, 100, 200, L, W) == -1


def test_word_cuts():
    words = np.array([[300, 100, 400, 110], [100, 100, 200, 110]], dtype=float)
    f = feats.word_features(words, np.zeros((0, 4)), 350, 350, 90, 130, W)
    assert f["words_cut"] == 1 and 0.2 < f["words_cut_share"] < 0.3
    assert np.isnan(feats.word_features(None, None, 350, 350, 90, 130, W)["words_cut"])


def test_page_rows_labels_and_features():
    img, words, ref, det, boxes = synth()
    rows = feats.page_rows("p0", det, (W, H), img, boxes=boxes, words=words, ref=ref)
    assert rows and all(set(feats.FEATURES) <= set(r["f"]) for r in rows)
    top = [r for r in rows if r["y1"] <= 170 and 300 < r["x_top"] < 380]
    assert top and all(r["label"] == -1 and r["f"]["words_cut"] > 0 for r in top)
    body = [r for r in rows if r["y0"] >= 200 and 300 < r["x_top"] < 380]
    assert body and all(r["label"] == 1 for r in body)
    mid = [r for r in rows if 100 < r["x_top"] < 200]
    assert mid and all(r["label"] == 0 for r in mid)
    ruled = [r for r in rows if 640 < r["x_top"] < 680]
    assert ruled and all(r["fit"] == "rule" for r in ruled)
    assert all(r["f"]["as_cross_headline"] > 0 for r in top)


def _rows(n=6):
    out = []
    for s in range(n):
        img, words, ref, det, boxes = synth(seed=s, rule=bool(s % 2))
        out += feats.page_rows("p%d" % s, det, (W, H), img, boxes=boxes, words=words, ref=ref)
    return out


def test_train_infer_score_deterministic(tmp_path):
    rows = _rows()
    clf1 = gbdt.fit(rows, params={"min_samples_leaf": 2, "max_iter": 60, "learning_rate": 0.3})
    clf2 = gbdt.fit(rows, params={"min_samples_leaf": 2, "max_iter": 60, "learning_rate": 0.3})
    p1, p2 = gbdt.predict(clf1, rows), gbdt.predict(clf2, rows)
    assert np.array_equal(p1, p2)
    lab = np.array([r["label"] for r in rows])
    assert p1[lab == 1].mean() > p1[lab == 0].mean()
    g = gbdt.gutters(rows, p1, 0.5)
    _, _, ref, _, _ = synth()
    refs = {"p%d" % s: ref for s in range(6)}
    det = {p: {"gutters": g.get(p, [])} for p in refs}
    pr = score.page_rows(det, refs)
    P, R = score.set_pr(score._vec(list(pr.values()), "primary"))
    assert P > 0.9 and R > 0.5
    path = str(tmp_path / "m.pkl")
    gbdt.save(clf1, path, {"threshold": 0.5})
    assert np.array_equal(gbdt.predict(gbdt.load(path)["model"], rows), p1)


def test_dev_split_is_stable():
    ids = ["a%d" % i for i in range(100)]
    d1, d2 = gbdt.dev_split(ids, 0.2), gbdt.dev_split(list(reversed(ids)), 0.2)
    assert d1 == d2 and len(d1) == 20


def test_export_mapping():
    tasks = [{"id": "t1", "set": "en2107-l2", "private": {"segment_id": "s1"}},
             {"id": "t2", "set": "en2107-l2", "private": {"segment_id": "s2"}},
             {"id": "t3", "set": "en2107-l2", "private": {"segment_id": "s3"}}]
    segs = [{"segment_id": "s1", "page_id": "p", "x": 10, "x_bottom": 12, "y0": 0, "y1": 100, "method": "banded@337fc0aa"},
            {"segment_id": "s2", "page_id": "p", "x": 50, "y0": 0, "y1": 100, "method": "bw-nontext"},
            {"segment_id": "s3", "page_id": "p", "x": 90, "y0": 0, "y1": 100, "method": "banded@337fc0aa"}]
    export = [{"id": "t1", "set": "en2107-l2", "kind": "gutter", "settled": True, "outcome": "a", "segment_id": "s1", "counts": {}},
              {"id": "t2", "set": "en2107-l2", "kind": "gutter", "settled": True, "outcome": "b", "counts": {}},
              {"id": "t3", "set": "en2107-l2", "kind": "gutter", "settled": True, "outcome": "unsure", "counts": {}},
              {"id": "t4", "set": "en2107-l1", "kind": "gutter", "settled": False}]
    out, c = export_engutter.build(export, tasks, segs, "en2107-l2", {"bw-nontext"})
    assert [(r["segment_id"], r["label"], r["x_bottom"]) for r in out] == [("s1", 1, 12)]
    assert c["excluded_method_bw-nontext"] == 1 and c["settled_unsure"] == 1
    bad = [dict(export[0], segment_id="zz")]
    with pytest.raises(SystemExit):
        export_engutter.build(bad, tasks, segs, "en2107-l2", set())


def test_wordbox_parsers(tmp_path):
    tsv = ("level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n"
           "4\t1\t1\t1\t1\t0\t10\t20\t100\t12\t-1\t\n"
           "5\t1\t1\t1\t1\t1\t10\t20\t40\t12\t91.5\tword\n"
           "5\t1\t1\t1\t1\t2\t60\t20\t40\t12\t-1\t \n")
    w, l_ = wordboxes.parse_tsv(tsv, 2.0, 2.0)
    assert w == [[20.0, 40.0, 100.0, 64.0, 91.5]] and l_ == [[20.0, 40.0, 220.0, 64.0]]
    alto = tmp_path / "a.xml"
    alto.write_text('<alto xmlns="http://www.loc.gov/standards/alto/ns-v2#"><Layout><Page WIDTH="2000" HEIGHT="3000">'
                    '<PrintSpace><TextBlock><TextLine HPOS="100" VPOS="200" WIDTH="400" HEIGHT="30">'
                    '<String HPOS="100" VPOS="200" WIDTH="150" HEIGHT="30" WC="0.9" CONTENT="x"/></TextLine></TextBlock>'
                    '</PrintSpace></Page></Layout></alto>')
    w, l_ = wordboxes.alto_boxes(str(alto), 1000, 1500)
    assert w == [[50.0, 100.0, 125.0, 115.0, 90.0]] and l_ == [[50.0, 100.0, 250.0, 115.0]]


def test_v2_lines_pool_like_the_v2_scoring_adapter():
    import refit
    v1 = [[[640, 660, 200, 700]]]
    ln = refit.v1_lines(v1)[0][0]
    ln = dict(ln, x_top=645.0, x_bottom=655.0, fit="white", theta=0.5)
    ps = pool.pieces({"v2": [[ln]]})
    assert ps[0]["x0"] == 640.0 and ps[0]["x1"] == 660.0 and ps[0]["xc"] == 650.0
    assert [ps[0][k] for k in ("x0", "x1", "y0", "y1")] == refit.scoring_gutters([[ln]])[0][0]
    _, cands = pool.pool_page({"v2": [[ln]], "v1": v1}, W, H)
    assert len(cands) == 1 and cands[0]["atoms"][0]["x_source"] == "v2"


def test_negatives_weight_cap(tmp_path):
    import train
    rows = _rows(6)
    for r in rows[:40]:
        if r["label"] == -1:
            r.update(label=0, label_source="negatives")
    fp = tmp_path / "f.jsonl"
    fp.write_text("".join(json.dumps(r) + "\n" for r in rows))
    _, _, ref, _, _ = synth()
    rp = tmp_path / "refs.json"
    rp.write_text(json.dumps({"pages": {"p%d" % s: ref for s in range(6)}}))
    with pytest.raises(SystemExit):
        train.main(["--features", str(fp), "--out-model", str(tmp_path / "m.pkl"), "--report", str(tmp_path / "r.json"),
                    "--refs", str(rp), "--dev-frac", "0.34", "--baseline-dev-precision", "0.5",
                    "--source-weight", "negatives=1000"])
    train.main(["--features", str(fp), "--out-model", str(tmp_path / "m.pkl"), "--report", str(tmp_path / "r.json"),
                "--refs", str(rp), "--dev-frac", "0.34", "--baseline-dev-precision", "0.5"])
    rep = json.load(open(tmp_path / "r.json"))
    assert rep["negatives_weight_search"]["chosen_factor"] in train.NEG_FACTORS
    assert rep["source_weights"]["negatives"] <= rep["negatives_weight_cap"] + 1e-12


def test_export_pages_filter():
    tasks = [{"id": "t1", "set": "s", "private": {"segment_id": "s1"}}]
    segs = [{"segment_id": "s1", "page_id": "p", "x": 1, "y0": 0, "y1": 9, "method": "m", "role": "banded"}]
    ex = [{"id": "t1", "set": "s", "kind": "gutter", "settled": True, "outcome": "b", "counts": {}}]
    out, c = export_engutter.build(ex, tasks, segs, "s", set(), pages={"q"})
    assert out == [] and c["other_half_page"] == 1
    out, c = export_engutter.build(ex, tasks, segs, "s", set(), pages={"p"})
    assert out[0]["label"] == 0 and c["labelled_non_gold"] == 1


def test_threshold_rule_precision_floor():
    import train
    assert train.GRID == (0.02, 0.05, 0.10, 0.20, 0.30, 0.50)
    ds = {0.02: (0.80, 0.95), 0.05: (0.85, 0.90), 0.10: (0.89, 0.85), 0.20: (0.92, 0.70), 0.30: (0.95, 0.50),
          0.50: (0.97, 0.30)}
    ds = {t: {"precision": p, "recall": r, "f1": 2 * p * r / (p + r)} for t, (p, r) in ds.items()}
    # floor 0.90 - 0.01 = 0.89: 0.10 (F1 0.870) beats 0.20 (0.797) and above; 0.05 has the best F1 (0.874) but fails it
    assert train.select_threshold(ds, 0.90) == (0.10, True)
    assert train.select_threshold(ds, 0.50) == (0.05, True)
    # nobody reaches 0.99 - 0.01: the highest precision wins
    assert train.select_threshold(ds, 0.99) == (0.50, False)


def test_threshold_requires_baseline(tmp_path):
    import train
    rows = _rows(3)
    fp = tmp_path / "f.jsonl"
    fp.write_text("".join(json.dumps(r) + "\n" for r in rows))
    _, _, ref, _, _ = synth()
    rp = tmp_path / "refs.json"
    rp.write_text(json.dumps({"pages": {"p%d" % s: ref for s in range(3)}}))
    with pytest.raises(SystemExit):
        train.main(["--features", str(fp), "--out-model", str(tmp_path / "m.pkl"), "--report", str(tmp_path / "r.json"),
                    "--refs", str(rp)])


def test_build_features_requires_full_resolution(tmp_path):
    import build_features
    with pytest.raises(SystemExit):  # --paths is required
        build_features.main(["--page-cache", str(tmp_path), "--detections-dir", str(tmp_path), "--out", str(tmp_path / "o")])
    img = tmp_path / "a.png"
    img.write_bytes(b"x")
    z = tmp_path / "b.zip"
    z.write_bytes(b"x")
    paths = {"a": str(img), "b": str(z) + "!images/b.jpg", "c": str(tmp_path / "gone.parquet") + "#row=3",
             "d": str(tmp_path / "gone.jpg")}
    have, miss = build_features.split_available(["a", "b", "c", "d", "e"], paths)
    assert have == ["a", "b"] and miss == ["c", "d", "e"]


def test_build_features_lists_missing_pages(tmp_path):
    import build_features
    from PIL import Image
    img, words, ref, det, boxes = synth()
    cache = tmp_path / "cache"
    cache.mkdir()
    np.savez(cache / "p0.2000.npz", gray=img, size=np.array([W, H]))
    Image.fromarray(img).save(tmp_path / "p0.png")
    dd = tmp_path / "det"
    dd.mkdir()
    (dd / "v1.json").write_text(json.dumps({p: {"gutters": det["v1"], "width": W, "height": H} for p in ("p0", "p1")}))
    (tmp_path / "refs.json").write_text(json.dumps({"pages": {"p0": ref, "p1": ref}}))
    (tmp_path / "paths.json").write_text(json.dumps({"p0": str(tmp_path / "p0.png")}))
    out = tmp_path / "f.jsonl"
    build_features.main(["--page-cache", str(cache), "--detections-dir", str(dd), "--paths", str(tmp_path / "paths.json"),
                         "--refs", str(tmp_path / "refs.json"), "--out", str(out), "--workers", "1"])
    miss = json.load(open(str(out) + ".missing.json"))
    assert miss["count"] == 1 and miss["pages"] == ["p1"] and miss["kept"] == 1
    rows = [json.loads(ln) for ln in open(out)]
    assert rows and {r["page_id"] for r in rows} == {"p0"}
