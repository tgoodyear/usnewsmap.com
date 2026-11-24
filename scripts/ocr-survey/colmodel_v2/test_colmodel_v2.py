"""Synthetic-data tests for the #2107 v2 column model pipeline. Run: pytest test_colmodel_v2.py

The torch tests (training smoke, ONNX export and parity) are skipped when torch/onnxruntime are missing.
"""
import csv
import io
import json
import os
import sys
import tarfile

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import alto_weak  # noqa: E402
import colgeom  # noqa: E402
import conv_refs  # noqa: E402
import to_gutters  # noqa: E402

W, H = 2000, 3000
COLS = [(50 + c * 470, 50 + c * 470 + 450) for c in range(4)]  # 4 columns, 20 px gutters
WORDS = "the news of the day and the city council met to vote in a hall that was full for an hour".split()


def column_lines(y_from=300, y_to=2900, pitch=30, h=20, cols=COLS):
    return [[x0, y, x1, y + h] for x0, x1 in cols for y in range(y_from, y_to, pitch)]


def alto_xml(lines, blocks=None, words=True, page=(W, H)):
    out = ['<?xml version="1.0"?><alto xmlns="http://www.loc.gov/standards/alto/ns-v3#"><Layout>',
           '<Page WIDTH="%d" HEIGHT="%d"><PrintSpace>' % page]
    blocks = blocks or [lines]
    for bl in blocks:
        bx = [min(b[0] for b in bl), min(b[1] for b in bl), max(b[2] for b in bl), max(b[3] for b in bl)]
        out.append('<TextBlock HPOS="%d" VPOS="%d" WIDTH="%d" HEIGHT="%d">' % (bx[0], bx[1], bx[2] - bx[0], bx[3] - bx[1]))
        for i, b in enumerate(bl):
            out.append('<TextLine HPOS="%d" VPOS="%d" WIDTH="%d" HEIGHT="%d">' % (b[0], b[1], b[2] - b[0], b[3] - b[1]))
            if words:
                for k in range(4):
                    w = WORDS[(i + k) % len(WORDS)]
                    out.append('<String CONTENT="%s" HPOS="%d" VPOS="%d" WIDTH="50" HEIGHT="%d"/>' % (w, b[0] + k * 60, b[1], b[3] - b[1]))
            out.append('</TextLine>')
        out.append('</TextBlock>')
    out.append('</PrintSpace></Page></Layout></alto>')
    return "".join(out).encode()


def per_column_blocks(lines, size=10):
    blocks = []
    for x0, x1 in COLS:
        col = [b for b in lines if b[0] == x0]
        blocks += [col[i:i + size] for i in range(0, len(col), size)]
    return blocks


# ---------------------------------------------------------------- geometry

def test_columnize_four_columns_under_a_spanning_headline():
    lines = column_lines(y_from=300) + [[50, 200, 1900, 260]]  # masthead-wide line above
    regs = colgeom.columnize(lines, W, H, max_vgap=50, min_members=3)
    assert len(regs) == 4
    for r, (x0, x1) in zip(regs, COLS):
        assert r[0] == x0 and r[2] == x1 and r[1] == 300
    gut = colgeom.gutters_from_regions(regs, W, H)
    assert len(gut) == 3
    for g, (a, b) in zip(sorted(gut, key=lambda g: g[0][0]), zip(COLS, COLS[1:])):
        assert g[0][0] == a[1] and g[0][1] == b[0]


def test_spanning_line_splits_columns_into_sections():
    top = column_lines(y_from=300, y_to=1400)
    bottom = column_lines(y_from=1600, y_to=2900, cols=COLS[:2])
    head = [[50, 1450, 1900, 1550]]
    regs = colgeom.columnize(top + bottom + head, W, H, max_vgap=50, min_members=3)
    assert len(regs) == 6
    assert sum(1 for r in regs if r[1] >= 1600) == 2


def test_clean_alto_page_passes_all_filters():
    lines = column_lines()
    parsed = alto_weak.parse_alto(alto_xml(lines, per_column_blocks(lines)), (W, H))
    lab = alto_weak.label_page(parsed, W, H)
    assert lab["passed"], lab["failed"]
    assert lab["metrics"]["columns"] == 4 and lab["metrics"]["gutters"] == 3
    assert lab["metrics"]["cross_share"] == 0.0


def test_alto_scaled_to_image_pixels():
    lines = column_lines()
    parsed = alto_weak.parse_alto(alto_xml(lines, page=(W // 2, H // 2)), (W, H))
    assert parsed["lines"][0] == [100.0, 600.0, 1000.0, 640.0]


def test_merged_lines_across_gutter_are_caught():
    lines = column_lines()
    merged = []
    for b in lines:
        if b[0] == COLS[1][0] and (b[1] // 30) % 8 == 0:   # every 8th row of columns 0-1 merged into one line
            merged.append([COLS[0][0], b[1], COLS[1][1], b[3]])
        elif b[0] == COLS[0][0] and (b[1] // 30) % 8 == 0:
            continue
        else:
            merged.append(b)
    parsed = alto_weak.parse_alto(alto_xml(merged), (W, H))
    lab = alto_weak.label_page(parsed, W, H)
    assert not lab["passed"]
    assert lab["metrics"]["cross_share"] > colgeom.MAX_CROSS


def test_blocks_across_gutters_fail_block_align():
    lines = column_lines()
    rows = sorted({b[1] for b in lines})
    blocks = [[b for b in lines if b[1] in rows[i:i + 10]] for i in range(0, len(rows), 10)]  # page-wide blocks
    parsed = alto_weak.parse_alto(alto_xml(lines, blocks), (W, H))
    lab = alto_weak.label_page(parsed, W, H)
    assert "block_align" in lab["failed"]


def test_language_and_aspect_filters():
    lines = column_lines()
    good = alto_xml(lines, per_column_blocks(lines))
    german = good
    for en, de in (("the", "der"), ("of", "und"), ("and", "die"), ("to", "zu"), ("a", "ein"), ("in", "im"),
                   ("that", "dass"), ("was", "war"), ("for", "fur")):
        german = german.replace(b'CONTENT="%s"' % en.encode(), b'CONTENT="%s"' % de.encode())
    lab = alto_weak.label_page(alto_weak.parse_alto(german, (W, H)), W, H)
    assert "lang_ok" in lab["failed"] and lab["metrics"]["alto_lang"] is None
    tagged = good.replace(b"<TextBlock ", b'<TextBlock language="ger" ')
    lab = alto_weak.label_page(alto_weak.parse_alto(tagged, (W, H)), W, H)
    assert lab["failed"] == ["lang_ok"] and lab["metrics"]["alto_lang"] == "ger"
    assert alto_weak.label_page(alto_weak.parse_alto(tagged, (W, H)), W, H, lang="ger")["passed"]
    lab = alto_weak.label_page(alto_weak.parse_alto(good, (W, H)), W, int(H * 1.05))
    assert "aspect_err" in lab["failed"]


def test_width_consistency():
    assert colgeom.width_consistency([[0, 0, 100, 1000], [120, 0, 220, 1000]]) == 1.0
    assert colgeom.width_consistency([[0, 0, 100, 1000], [120, 0, 250, 1000]]) == 1.0  # 1.3x: a wider section
    assert colgeom.width_consistency([[0, 0, 100, 1000], [120, 0, 420, 1000], [440, 0, 740, 1000]]) < 0.7


# ---------------------------------------------------------------- ids, exclusions, archives

def test_member_and_doc_round_trip():
    did = "sn84031042_1859-12-17_ed-1_seq-4"
    assert alto_weak.member_name(did) == "sn84031042/1859/12/17/ed-1/seq-4/ocr.xml"
    assert alto_weak.doc_of_member(alto_weak.member_name(did)) == did
    assert alto_weak.image_rel(did) == "sn84031042/1859-12-17/1/4.jpg"
    assert alto_weak.doc_of_member("sn84031042/1859/12/17/ed-1/seq-4/1.jp2") is None


def test_exclusions_page_and_issue(tmp_path):
    p = tmp_path / "en.txt"
    p.write_text("page_id\nsn84031042_1859-12-17_ed-1_seq-4\n")
    q = tmp_path / "bw.jsonl"
    q.write_text(json.dumps({"page_id": "sn1_1900-01-01_ed-2_seq-1"}) + "\n")
    ex = alto_weak.Exclusions(["en2107=%s" % p, "bwval=%s" % q])
    assert ex.reason("sn84031042_1859-12-17_ed-1_seq-4") == "excluded:en2107"
    assert ex.reason("sn84031042_1859-12-17_ed-1_seq-2") == "excluded_issue:en2107"
    assert ex.reason("sn1_1900-01-01_ed-1_seq-1") is None
    assert ex.reason("sn1_1900-01-01_ed-2_seq-9") == "excluded_issue:bwval"
    assert alto_weak.Exclusions(["en2107=%s" % p], issues=False).reason("sn84031042_1859-12-17_ed-1_seq-2") is None


def _tar(path, members):
    with tarfile.open(path, "w:bz2") as tf:
        for name, data in members.items():
            ti = tarfile.TarInfo(name)
            ti.size = len(data)
            tf.addfile(ti, io.BytesIO(data))


def test_scan_archive_takes_only_wanted(tmp_path):
    lines = column_lines()
    xml = alto_xml(lines, per_column_blocks(lines))
    ids = ["sn1_1900-01-01_ed-1_seq-%d" % k for k in (1, 2, 3)]
    _tar(tmp_path / "b_ver01.tar.bz2", {alto_weak.member_name(d): xml for d in ids} | {"sn1/1900/01/01/ed-1/seq-1/1.jp2": b"x"})
    (tmp_path / "alto").mkdir()
    _, got = alto_weak.scan_archive((str(tmp_path / "b_ver01.tar.bz2"), ids[:2], str(tmp_path / "alto")))
    assert sorted(got) == ids[:2]
    assert sorted(os.listdir(tmp_path / "alto")) == [d + ".xml" for d in ids[:2]]


def test_select_and_build_end_to_end(tmp_path):
    pa = pytest.importorskip("pyarrow")
    import pyarrow.parquet as pq
    from PIL import Image
    lines = column_lines()
    good = alto_xml(lines, per_column_blocks(lines))
    bad = alto_xml(lines)  # one page-wide block -> block_align fails
    ids = ["sn1_1900-01-0%d_ed-1_seq-1" % d for d in range(1, 7)]
    (tmp_path / "locbulk").mkdir()
    _tar(tmp_path / "locbulk" / "bb_x_ver01.tar.bz2",
         {alto_weak.member_name(d): (good if i % 2 == 0 else bad) for i, d in enumerate(ids)})
    (tmp_path / "locstats").mkdir()
    pq.write_table(pa.table({"batch": ["bb_x"] * 6, "batch_version": [1] * 6, "doc_id": ids, "words": [100] * 6,
                             "archive": ["bb_x_ver01.tar.bz2"] * 6}), tmp_path / "locstats" / "bb_x_ver01.parquet")
    with open(tmp_path / "index.jsonl", "w") as f:
        for d in ids:
            f.write(json.dumps({"doc_id": d, "batch": "bb_x", "batch_version": 1}) + "\n")
            p = tmp_path / "scans" / "bb_x" / alto_weak.image_rel(d)
            p.parent.mkdir(parents=True, exist_ok=True)
            Image.new("L", (W // 4, H // 4), 255).save(p)
    (tmp_path / "test.txt").write_text(ids[0] + "\n")
    rc = alto_weak.main(["select", "--scans-index", str(tmp_path / "index.jsonl"), "--scans-root", str(tmp_path / "scans"),
                         "--locstats-dir", str(tmp_path / "locstats"), "--locbulk-dir", str(tmp_path / "locbulk"),
                         "--exclude", "test=%s" % (tmp_path / "test.txt"), "--no-exclude-issues", "--pages", "10",
                         "--out", str(tmp_path / "pages.tsv"), "--manifest-out", str(tmp_path / "sel.tsv")])
    assert rc == 0
    with open(tmp_path / "sel.tsv") as f:
        st = {r["doc_id"]: r["status"] for r in csv.DictReader(f, delimiter="\t")}
    assert st[ids[0]] == "excluded:test" and sum(v == "selected" for v in st.values()) == 5
    rc = alto_weak.main(["build", "--pages", str(tmp_path / "pages.tsv"), "--locbulk-dir", str(tmp_path / "locbulk"),
                         "--out-dir", str(tmp_path / "out"), "--workers", "1"])
    assert rc == 0
    summ = json.load(open(tmp_path / "out" / "summary.json"))
    assert summ["accepted"] == 2 and summ["statuses"] == {"accepted": 2, "rejected": 3}
    lab = [json.loads(x) for x in open(tmp_path / "out" / "labels.jsonl")]
    assert all(len(x["columns"]) == 4 and x["width"] == W // 4 for x in lab)


# ---------------------------------------------------------------- reference converters

def test_teklia_rows_only_and_exclude(tmp_path):
    def row(pid):
        tr = [{"box": [x0, 300, x1, 1400], "type": "article"} for x0, x1 in COLS]
        tr += [{"box": [x0, 1400, x1, 2900], "type": "article"} for x0, x1 in COLS]
        tr += [{"box": [50, 100, 1900, 250], "type": "header"}]
        return {"page_id": pid, "split": "train", "width": W, "height": H, "image_path": "x.jpg", "text_regions": tr}
    with open(tmp_path / "refs.jsonl", "w") as f:
        for pid in ("p1", "p2", "p3"):
            f.write(json.dumps(row(pid)) + "\n")
    (tmp_path / "spent.txt").write_text("p1\np2\n")
    (tmp_path / "ex.txt").write_text("p2\n")
    conv_refs.main(["teklia", "--refs", str(tmp_path / "refs.jsonl"), "--name", "liberte", "--out-dir", str(tmp_path / "o"),
                    "--only", "spent=%s" % (tmp_path / "spent.txt"), "--exclude", "test=%s" % (tmp_path / "ex.txt")])
    lab = [json.loads(x) for x in open(tmp_path / "o" / "labels.jsonl")]
    assert [x["page_id"] for x in lab] == ["p1"]
    assert len(lab[0]["columns"]) == 4 and lab[0]["licence"].startswith("MIT")
    with open(tmp_path / "o" / "manifest.tsv") as f:
        st = {r["page_id"]: r["status"] for r in csv.DictReader(f, delimiter="\t")}
    assert st == {"p1": "written", "p2": "excluded:test", "p3": "not_in_only"}


def test_cg_skips_test_split(tmp_path):
    d = tmp_path / "data"
    (d / "annotations").mkdir(parents=True)
    regs = "".join('<TextRegion id="r%d" custom="structure {type:paragraph;}"><Coords points="%d,300 %d,300 %d,2900 %d,2900"/></TextRegion>'
                   % (i, x0, x1, x1, x0) for i, (x0, x1) in enumerate(COLS))
    xml = ('<PcGts xmlns="http://schema.primaresearch.org/PAGE/gts/pagecontent/2013-07-15"><Page imageFilename="a.jpg" '
           'imageWidth="%d" imageHeight="%d">%s<SeparatorRegion id="s"><Coords points="0,0 1,1"/></SeparatorRegion></Page></PcGts>' % (W, H, regs))
    for n in ("A_0001", "B_0001", "C_0001"):
        (d / "annotations" / (n + ".xml")).write_text(xml)
    (d / "split.csv").write_text("Filename,KoeZ,Test,Validation,OutOfDistribution,Advertisements,Pictures\n"
                                 "A_0001,0,1,0,0,0,0\nB_0001,0,0,1,0,0,0\nC_0001,0,0,0,0,0,0\n")
    conv_refs.main(["cg", "--data-dir", str(d), "--out-dir", str(tmp_path / "o")])
    lab = [json.loads(x) for x in open(tmp_path / "o" / "labels.jsonl")]
    assert sorted(x["page_id"] for x in lab) == ["B_0001", "C_0001"]
    assert all(len(x["columns"]) == 4 and "NC" in x["licence"] for x in lab)
    with open(tmp_path / "o" / "manifest.tsv") as f:
        st = {r["page_id"]: r["status"] for r in csv.DictReader(f, delimiter="\t")}
    assert st["A_0001"] == "test_split"


# ---------------------------------------------------------------- evaluation adapter

def test_adapter_reproduces_reference_gutters():
    regs = [[x0, 300, x1, 2900] for x0, x1 in COLS]
    ref = colgeom.gutters_from_regions(regs, W, H)
    boxes = regs + [[COLS[0][0] + 5, 400, COLS[0][1] - 5, 2000]]  # duplicate inside column 0, lower score
    preds = {"p": {"boxes": boxes, "scores": [0.9, 0.8, 0.95, 0.7, 0.6], "width": W, "height": H},
             "q": {"boxes": regs, "scores": [0.9, 0.1, 0.9, 0.9], "width": W, "height": H}, "_meta": {}}
    det = to_gutters.convert(preds, threshold=0.5)
    assert det["p"]["gutters"] == ref and len(det["p"]["regions"]) == 4
    assert len(det["q"]["regions"]) == 3 and len(det["q"]["gutters"]) == 2  # column 1 below threshold: gaps 0|2 and 2|3
    for g in det["p"]["gutters"]:
        for s in g:
            assert len(s) == 4 and s[0] < s[1] and s[2] < s[3]


# ---------------------------------------------------------------- torch: train smoke, ONNX

def _synthetic_labels(tmp_path, n=3):
    from PIL import Image, ImageDraw
    rows = []
    for k in range(n):
        img = Image.new("L", (W // 2, H // 2), 235)
        d = ImageDraw.Draw(img)
        for x0, x1 in COLS:
            for y in range(150, 1450, 15):
                d.rectangle([x0 // 2, y, x1 // 2, y + 8], fill=40)
        p = tmp_path / ("s%d.jpg" % k)
        img.save(p)
        rows.append({"columns": [[x0 / 2, 150, x1 / 2, 1458] for x0, x1 in COLS], "gutters": [], "height": H // 2,
                     "image_path": str(p), "licence": "synthetic", "page_id": "s%d" % k, "source": "synth", "width": W // 2})
    with open(tmp_path / "labels.jsonl", "w") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")
    return tmp_path / "labels.jsonl"


def test_train_export_predict_smoke(tmp_path):
    pytest.importorskip("torch")
    pytest.importorskip("torchvision")
    pytest.importorskip("onnxruntime")
    import export_onnx
    import predict
    import train
    labels = _synthetic_labels(tmp_path)
    assert train.main(["--labels", str(labels), "--out-dir", str(tmp_path / "m"), "--iterations", "2", "--batch-size", "2",
                       "--val-frac", "0.0", "--log-every", "1", "--threads", "2"]) == 0
    meta = json.load(open(tmp_path / "m" / "train.json"))
    assert meta["pages"]["synth"]["train"] == 3 and len(meta["log"]) == 2
    em = export_onnx.export(str(tmp_path / "m" / "model.pt"), str(tmp_path / "m" / "colv2.onnx"))
    assert len(em["checks"]) == 2 and all(c["boxes_torch"] == c["boxes_onnx"] for c in em["checks"])
    assert em["checks"][0]["input"] != em["checks"][1]["input"]
    assert predict.main(["--model", str(tmp_path / "m" / "colv2.onnx"), "--pages", str(labels),
                         "--out", str(tmp_path / "preds.json"), "--floor", "0.0"]) == 0
    preds = json.load(open(tmp_path / "preds.json"))
    assert preds["s0"]["width"] == W // 2 and preds["_meta"]["seconds_per_page"]["mean"] > 0
    det = to_gutters.convert(preds, threshold=0.0)
    assert set(det) == {"s0", "s1", "s2"}


def test_exclusion_lists(tmp_path):
    import exclusions
    (tmp_path / "bw.jsonl").write_text(json.dumps({"meta": {"ca_identity": {"lccn": "sn1", "date": "1900-01-02", "edition": "1"}}}) + "\n")
    exclusions.main(["bwval", "--refs", str(tmp_path / "bw.jsonl"), "--out", str(tmp_path / "bw.txt")])
    assert (tmp_path / "bw.txt").read_text() == "sn1_1900-01-02_ed-1_seq-0\n"
    (tmp_path / "hipe").mkdir()
    (tmp_path / "hipe" / "x_overproof-combined_v1.0_test_en.jsonl").write_text(
        json.dumps({"document_metadata": {"document_id": "sn86063758-0-00271743506-1859080801-0012"}}) + "\n"
        + json.dumps({"document_metadata": {"document_id": "13023000"}}) + "\n")
    exclusions.main(["overproof", "--hipe-dir", str(tmp_path / "hipe"), "--out", str(tmp_path / "op.txt")])
    assert (tmp_path / "op.txt").read_text() == "sn86063758_1859-08-08_ed-1_seq-0\n"
    ex = alto_weak.Exclusions(["overproof=%s" % (tmp_path / "op.txt")])
    assert ex.reason("sn86063758_1859-08-08_ed-1_seq-3") == "excluded_issue:overproof"


def test_held_entry():
    assert alto_weak.held_entry("nbu_x/2017270200/1895-12-26/1/2.jpg") == ("2017270200_1895-12-26_ed-1_seq-2", "nbu_x")
    assert alto_weak.held_entry("nbu_x/2017270200/1895-12-26/1/2.jpg.json") == (None, None)
