"""Tests for the layout-reference converters on small synthetic fixtures (#2107).

Datasets the converters cover: Teklia/Newspapers-finlam and -La-Liberte (MIT),
biglam/loc_beyond_words (CC0), biglam/europeana-newspapers-ground-truth (CC0),
NealCaren/co-newspaper-layout-yolo (no licence stated; private tuning only). No real data is used.
"""
import io
import json
import os
import sys

import pytest

sys.path.insert(0, os.path.dirname(__file__))
import common  # noqa: E402
import conv_coyolo  # noqa: E402
import conv_europeana  # noqa: E402
import conv_finlam  # noqa: E402
import inventory  # noqa: E402

PIL = pytest.importorskip("PIL")


def png_bytes(w, h):
    from PIL import Image
    b = io.BytesIO()
    Image.new("L", (w, h), 255).save(b, "PNG")
    return b.getvalue()


def rect(x0, y0, x1, y1):
    return [[x0, y0], [x1, y0], [x1, y1], [x0, y1]]


# ---- common ----

def test_derive_gutters_two_columns():
    g = common.derive_gutters([(0, 0, 100, 500), (120, 0, 220, 500)], 300, 600)
    assert g == [[[100.0, 120.0, 0.0, 500.0]]]


def test_derive_gutters_needs_side_by_side():
    assert common.derive_gutters([(0, 0, 100, 100), (0, 200, 100, 300)], 300, 600) == []
    assert common.derive_gutters([], 300, 600) == []


def test_write_jsonl_sorted_keys(tmp_path):
    row = common.make_row("d", "s", "p", "/x.png", 10, 20, "MIT", True)
    p = tmp_path / "o.jsonl"
    assert common.write_jsonl(str(p), [row]) == 1
    line = p.read_text().strip()
    keys = list(json.loads(line, object_pairs_hook=lambda kv: [k for k, _ in kv]))
    assert keys == sorted(keys) == sorted(common.ROW_KEYS)
    assert common.check_row(json.loads(line)) == []


# ---- finlam ----

def finlam_record():
    names = conv_finlam.CLASS_NAMES
    zones = [  # (class, polygon, order, article)
        ("HEADER-TITLE", rect(0, 0, 1000, 100), 0, -1),
        ("ARTICLE-TITLE", rect(0, 150, 1000, 250), 1, 0),
        ("ARTICLE-TEXT", rect(0, 300, 300, 1500), 2, 0),
        ("ARTICLE-TEXT", rect(340, 300, 640, 1500), 3, 0),
        ("ARTICLE-TEXT", rect(680, 300, 1000, 1500), 4, 0),
        ("ARTICLE-ILLUSTRATION", rect(0, 1600, 500, 1900), 5, 1),
        ("ADVERTISEMENT", rect(520, 1600, 1000, 1900), 6, -1),
    ]
    return {
        "newspaper_name": "Le Test", "newspaper_arkindex_id": "n1", "page_arkindex_id": "pg1",
        "page_index": 0, "zone_arkindex_ids": ["a%d" % i for i in range(len(zones))],
        "zone_polygons": [z[1] for z in zones], "zone_texts": ["abc"] * len(zones),
        "zone_classes": [names.index(z[0]) for z in zones],
        "zone_orders": [z[2] for z in zones], "article_id": [z[3] for z in zones],
    }


def test_finlam_row():
    r = conv_finlam.row_from_record(finlam_record(), "train", "/p.parquet#row=0", 1000, 2000,
                                    "Teklia/Newspapers-finlam")
    assert common.check_row(r) == []
    assert {x["class"] for x in r["nontext_regions"]} == {"ARTICLE-ILLUSTRATION", "ADVERTISEMENT"}
    assert len(r["text_regions"]) == 5
    assert len(r["column_gutters"]) == 2  # two gaps between the three ARTICLE-TEXT zones
    assert [s[:2] for s in (c[0] for c in r["column_gutters"])] == [[300.0, 340.0], [640.0, 680.0]]
    assert r["reading_order"][:3] == ["z000", "z001", "z002"] and len(r["reading_order"]) == 7
    ids = {a["id"]: a["regions"] for a in r["articles"]}
    assert ids[0] == ["z001", "z002", "z003", "z004"] and ids[1] == ["z005"]
    assert r["for_publication"] is True and r["meta"]["problems"] == []


def test_liberte_row():
    rec = {"page_arkindex_id": "L1", "page_index": 3,
           "zone_polygons": [rect(0, 100, 300, 900), rect(340, 100, 640, 900), rect(0, 950, 640, 990),
                             rect(0, 1000, 600, 1100)],
           "zone_classes": [7, 7, 6, 13], "zone_orders": [0, 1, 2, 3], "zone_texts": ["a"] * 4,
           "zone_article_ids": [5, 5, 5, -1], "zone_section_ids": [-1] * 4}
    r = conv_finlam.row_from_record(rec, "train", "x", 700, 1200, "Teklia/Newspapers-finlam-La-Liberte")
    assert common.check_row(r) == []
    assert [x["type"] for x in r["text_regions"]] == ["article", "article", "headline", "mixed"]
    assert len(r["column_gutters"]) == 1
    assert [a["regions"] for a in r["articles"]] == [["z000", "z001", "z002"]]
    assert r["meta"]["title"] == "La Liberte"


def test_liberte_normalised_coordinates():
    rec = {"page_arkindex_id": "L2", "zone_polygons": [rect(0.0, 0.1, 0.4, 0.9), rect(0.5, 0.1, 0.9, 0.9)],
           "zone_classes": [7, 7], "zone_orders": [0, 1], "zone_texts": ["a", "b"],
           "zone_article_ids": [1, 1], "zone_section_ids": [-1, -1]}
    r = conv_finlam.row_from_record(rec, "test", "x", 1000, 2000, "Teklia/Newspapers-finlam-La-Liberte")
    assert r["text_regions"][1]["box"] == [500.0, 200.0, 900.0, 1800.0]
    assert r["column_gutters"] == [[[400.0, 500.0, 200.0, 1800.0]]]


def test_finlam_name_parse():
    assert conv_finlam.parse_newspaper_name("Paris-midi - 1932-01-25 (4729329)") == (
        "Paris-midi", "1932-01-25", 1932)
    assert conv_finlam.parse_newspaper_name("Le Test") == ("Le Test", None, None)
    assert conv_finlam.parse_newspaper_name(None) == (None, None, None)


def test_finlam_degenerate_zone_is_reported():
    rec = finlam_record()
    rec["zone_polygons"][2] = [[1, 1], [2, 2]]
    r = conv_finlam.row_from_record(rec, "val", "x", 1000, 2000, "Teklia/Newspapers-finlam")
    assert len(r["meta"]["problems"]) == 1 and len(r["text_regions"]) == 4


def test_finlam_convert_parquet(tmp_path):
    pa = pytest.importorskip("pyarrow")
    pq = pytest.importorskip("pyarrow.parquet")
    rec = finlam_record()
    d = tmp_path / "ds" / "data"
    d.mkdir(parents=True)
    img = png_bytes(1000, 2000)
    for fn in ("train-00000-of-00001.parquet", "test-00000-of-00001.parquet"):
        cols = {k: [v] for k, v in rec.items()}
        cols["page_image"] = [{"bytes": img, "path": "a.png"}]
        pq.write_table(pa.table(cols), str(d / fn))
    out = tmp_path / "out"
    out.mkdir()
    s = conv_finlam.convert(str(tmp_path / "ds"), str(out), "finlam")
    assert s == {"test": 1, "train": 1}
    rows = list(common.read_jsonl(str(out / "Teklia__Newspapers-finlam__train.jsonl")))
    assert rows[0]["image_path"].endswith("train-00000-of-00001.parquet#row=0")
    assert rows[0]["width"] == 1000 and rows[0]["height"] == 2000


# ---- europeana ----

def europeana_record():
    reg = lambda i, kind, typ, bb: {"id": i, "kind": kind, "type": typ, "text": "t", "bbox": bb,
                                     "polygon": rect(*bb), "n_lines": 0}
    return {
        "page_id": "e1", "image_filename": "e1.tif", "width": 1000, "height": 1500, "text": "gt",
        "ocr_text": "ocr", "reading_order": ["r1", "r2", "r3", "missing"],
        "regions": [
            reg("r1", "TextRegion", "paragraph", [0, 100, 300, 1400]),
            reg("r2", "TextRegion", "paragraph", [340, 100, 640, 1400]),
            reg("r3", "TextRegion", "heading", [680, 100, 1000, 200]),
            reg("s1", "SeparatorRegion", None, [318, 100, 322, 1400]),
            reg("s2", "SeparatorRegion", None, [0, 90, 1000, 94]),
            reg("i1", "ImageRegion", None, [680, 300, 1000, 700]),
        ],
    }


def test_europeana_row():
    r = conv_europeana.row_from_record(europeana_record(), "train", "/e.parquet#row=0", 1000, 1500)
    assert common.check_row(r) == []
    orient = {s["id"]: s["orientation"] for s in r["separators"]}
    assert orient == {"s1": "vertical", "s2": "horizontal"}
    assert r["meta"]["vertical_separators"] == 1
    assert r["reading_order"] == ["r1", "r2", "r3"]
    assert len(r["meta"]["problems"]) == 1  # the unknown reading-order id
    assert len(r["column_gutters"]) == 1 and r["column_gutters"][0][0][:2] == [300.0, 340.0]
    assert r["nontext_regions"][0]["class"] == "ImageRegion"
    assert r["meta"]["ocr_text"] == "ocr"


# ---- co yolo ----

def test_coyolo_labels_and_private(tmp_path):
    txt = "1 0.15 0.5 0.2 0.8\n1 0.45 0.5 0.2 0.8\n0 0.8 0.5 0.2 0.5\n3 0.8 0.9 0.2 0.1\nbad line\n"
    r = conv_coyolo.row_from_labels("ADS19100425_1_1", "train", "/i.jpg", 1000, 2000, txt)
    assert common.check_row(r) == []
    assert r["for_publication"] is False
    assert r["meta"]["year"] == 1910 and r["meta"]["title_code"] == "ADS"
    assert len(r["column_gutters"]) == 1
    assert [x["class"] for x in r["nontext_regions"]] == ["ADVERTISEMENT", "ILLUSTRATION"]
    assert len(r["articles"]) == 2 and len(r["meta"]["problems"]) == 1


def test_coyolo_convert(tmp_path):
    root = tmp_path / "ds" / "newspaper"
    (root / "images").mkdir(parents=True)
    (root / "labels").mkdir()
    from PIL import Image
    for stem in ("ADS19100425_1_1", "ADS19100425_1_2"):
        Image.new("L", (100, 200), 255).save(root / "images" / (stem + ".jpg"))
        (root / "labels" / (stem + ".txt")).write_text("1 0.5 0.5 0.5 0.5\n")
    (root / "train.txt").write_text("images/ADS19100425_1_1.jpg\n")
    (root / "val.txt").write_text("images/ADS19100425_1_2.jpg\n")
    out = tmp_path / "out"
    out.mkdir()
    assert conv_coyolo.convert(str(tmp_path / "ds"), str(out)) == {"train": 1, "val": 1}
    row = next(common.read_jsonl(str(out / "NealCaren__co-newspaper-layout-yolo__val.jsonl")))
    assert row["page_id"] == "ADS19100425_1_2" and row["for_publication"] is False


# ---- inventory ----

def test_inventory_summary():
    r = conv_finlam.row_from_record(finlam_record(), "train", "x", 1000, 2000, "Teklia/Newspapers-finlam")
    s = inventory.summarise([r, r])
    assert s["pages"] == 2 and s["pages_can_serve_as"]["columns"] == 2
    assert s["pages_can_serve_as"]["reading_order"] == 2
    assert s["column_gutters"]["per_page"]["median"] == 2
    # both gaps are 40 px of a 1000 px page = 0.04
    assert s["column_gutters"]["gap_width_frac"]["p50"] == 0.04
    assert s["data_problems"]["rows_failing_check"] == 0


def test_quantile():
    assert inventory.quantile([1, 2, 3, 4, 5], 0.5) == 3
    assert inventory.quantile([], 0.5) is None
