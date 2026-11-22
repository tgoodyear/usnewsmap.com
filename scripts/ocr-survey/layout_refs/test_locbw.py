"""Tests for the biglam/loc_beyond_words converter on synthetic fixtures (#2107).

Dataset: biglam/loc_beyond_words (CC0), Lee et al. 2020, arXiv:2005.01583. No real data is used.
"""
import io
import json
import os
import sys
import zipfile

import pytest

sys.path.insert(0, os.path.dirname(__file__))
import common  # noqa: E402
import conv_locbw  # noqa: E402

pytest.importorskip("PIL")

URL = "http://s3.amazonaws.com/ndnp-jpeg-surrogates/pst_davey_ver01/data/sn83045211/00237287691/1917070201/0805.jpg"


def test_identity_and_boxes():
    assert conv_locbw.parse_identity(URL) == {
        "batch": "pst_davey_ver01", "date": "1917-07-02", "edition": 1, "frame": 805,
        "lccn": "sn83045211", "reel": "00237287691", "year": 1917}
    assert conv_locbw.parse_identity("http://x/y.jpg") is None
    img = {"id": 2, "file_name": "2.jpg", "width": 800, "height": 1000, "url": URL}
    anns = [{"id": 1, "bbox": [10, 20, 100, 200], "category_id": 0, "bw_id": "a"},
            {"id": 2, "bbox": [10, 300, 500, 40], "category_id": 5},
            {"id": 3, "bbox": [700, 900, 300, 300], "category_id": 6},
            {"id": 4, "bbox": [5, 5, 0, 10], "category_id": 1}]
    r = conv_locbw.row_from_coco(img, anns, "train", "/z!images/2.jpg")
    assert common.check_row(r) == []
    assert [x["class"] for x in r["nontext_regions"]] == ["Photograph", "Advertisement"]
    assert r["nontext_regions"][1]["box"] == [700.0, 900.0, 800.0, 1000.0]  # clamped
    assert r["text_regions"][0]["class"] == "Headline" and r["gutters"] == []
    assert r["meta"]["ca_identity_parsed"] is True and r["meta"]["year"] == 1917
    assert len(r["meta"]["problems"]) == 1


def test_convert(tmp_path):
    from PIL import Image
    d = tmp_path / "ds" / "data"
    d.mkdir(parents=True)
    url = "http://s3/x/data/sn1/0001/1918010101/0002.jpg"
    for fn, iid in (("train_80_percent.json", 1), ("val_20_percent.json", 2)):
        (d / fn).write_text(json.dumps({
            "images": [{"id": iid, "file_name": "%d.jpg" % iid, "width": 50, "height": 60, "url": url}],
            "annotations": [{"id": 9 + iid, "image_id": iid, "bbox": [1, 1, 10, 10], "category_id": 0}]}))
    with zipfile.ZipFile(d / "images.zip", "w") as z:
        for iid in (1, 2):
            b = io.BytesIO()
            Image.new("L", (50, 60)).save(b, "JPEG")
            z.writestr("images/%d.jpg" % iid, b.getvalue())
    out = tmp_path / "out"
    out.mkdir()
    s = conv_locbw.convert(str(tmp_path / "ds"), str(out), check_parquet=False)
    assert s == {"train": 1, "val": 1}
    row = next(common.read_jsonl(str(out / "biglam__loc_beyond_words__val.jsonl")))
    assert row["meta"]["problems"] == [] and row["image_path"].endswith("images.zip!images/2.jpg")
