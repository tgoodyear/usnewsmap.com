"""Tests for jaocr.py: python3 -m unittest discover ja-ocr (pyarrow for the Parquet ones)."""

import io
import json
import os
import tempfile
import unittest
from datetime import timedelta

import jaocr

try:
    import pyarrow as pa
    import pyarrow.parquet as pq
except ImportError:  # the Parquet tests need it; the rest don't
    pa = pq = None

JP2 = ("https://tile.loc.gov/storage-services/service/sgp/sgpbatches/batch_dlc_ballston_ver01/data/"
       "sn83025517/00237288798/1945010101/0365.jp2")


class Classify(unittest.TestCase):
    def test_empty_and_short_pages_need_ocr(self):
        self.assertEqual(jaocr.needs_ocr("empty", None), "empty")
        self.assertEqual(jaocr.needs_ocr("short", "ab"), "short")

    def test_english_text_is_left_alone(self):
        text = "The relocation center held a meeting about the new school for the children " * 3
        self.assertIsNone(jaocr.needs_ocr("ok", text))

    def test_capitals_are_words(self):
        text = "WAR NEWS TODAY ALLIED ARMIES CROSS THE RHINE " * 3
        self.assertIsNone(jaocr.needs_ocr("ok", text))

    def test_garbled_latin_is_japanese(self):
        text = "Br H rJjr Jjw-Ini l iiiTTTTiwtiawyM d j.. s,y fr r vtT r i j TtTrHPPIIHHkVHHVBL"
        self.assertEqual(jaocr.needs_ocr("ok", text), "garbled")

    def test_japanese_titles(self):
        titles = [{"lccn": "a", "languages": ["eng", "jpn"]}, {"lccn": "b", "languages": ["eng"]},
                  {"lccn": "c"}]
        self.assertEqual(jaocr.japanese_lccns(titles), {"a"})


class Archives(unittest.TestCase):
    def test_missing_pages(self):
        names = ["batch_x/data/sn1/1945/01/01/ed-1/seq-1/ocr.txt", "batch_x/data/sn1/1945/01/01/ed-1/seq-1/ocr.xml",
                 "batch_x/data/sn1/1945/01/01/ed-1/seq-2/ocr.xml", "sn1/1945/01/08/ed-2/seq-10/ocr.xml",
                 "sn1/1945/01/08/ed-2/seq-11/ocr.txt", "batch_x/data/batch.xml",
                 # The compact date layout.
                 "./batch_y_ver01/sn1/1945-02-03/ed-1/seq-3/ocr.xml",
                 "./batch_y_ver01/sn1/1945-02-03/ed-1/seq-4/ocr.xml", "./batch_y_ver01/sn1/1945-02-03/ed-1/seq-4/ocr.txt"]
        got = jaocr.missing_pages(names, {"sn1"}, "x")
        self.assertEqual(sorted((r["date"], r["edition"], r["seq"]) for r in got),
                         [("1945-01-01", 1, 2), ("1945-01-08", 2, 10), ("1945-02-03", 1, 3)])
        self.assertIsNotNone(jaocr.page_from_path("sn1/1945/01/01/ed-1/seq-32767/ocr.xml"))
        self.assertEqual(jaocr.page_from_path("sn1/1896-07-10/ed-2/seq-1/ocr.txt"),
                         ("sn1/1896-07-10/ed-2/seq-1", "ocr.txt"))
        for bad in ("sn1/18x6/07/10/ed-1/seq-1/ocr.xml", "sn1/1945/13/40/ed-1/seq-1/ocr.xml",
                    "SN1/1945/01/01/ed-1/seq-1/ocr.xml", "sn1/1945/01/01/ed-1/seq-0/ocr.xml",
                    "sn1/1945-02-30/ed-1/seq-1/ocr.xml", "sn1/1945/01/01/ed-0/seq-1/ocr.xml",
                    "sn1/1945/01/01/ed-1/seq-70000/ocr.xml", "sn1/1945/01/01/ed-1/seq-32768/ocr.xml"):
            self.assertIsNone(jaocr.page_from_path(bad), bad)


class LocGov(unittest.TestCase):
    def test_iiif_base(self):
        self.assertEqual(
            jaocr.iiif_base(JP2),
            "https://tile.loc.gov/image-services/iiif/service:sgp:sgpbatches:batch_dlc_ballston_ver01:"
            "data:sn83025517:00237288798:1945010101:0365")

    def test_page_images_are_in_sequence_order(self):
        item = {"resources": [{"files": [
            [{"mimetype": "application/pdf", "url": "x.pdf"}, {"mimetype": "image/jp2", "url": JP2}],
            [{"mimetype": "text/xml", "url": "y.xml"}],
            [{"mimetype": "image/jp2", "url": JP2.replace("0365", "0367")}],
        ]}]}
        imgs = jaocr.page_images(item)
        self.assertEqual(sorted(imgs), [1, 3])
        self.assertTrue(imgs[3].endswith(":0367"))
        self.assertEqual(jaocr.page_images({}), {})

    def test_issue_key(self):
        self.assertEqual(jaocr.issue_key("sn1", "1945-01-01", 2), "sn1_1945-01-01_ed-2")


class Text(unittest.TestCase):
    def test_normalize_keeps_columns(self):
        self.assertEqual(jaocr.normalize("去年の　大記事 \r\n\n  やはり\x07西歐  "), "去年の 大記事\nやはり西歐")

    def test_status(self):
        self.assertEqual(jaocr.text_status(""), "empty")
        self.assertEqual(jaocr.text_status("去年の大記事"), "short")
        self.assertEqual(jaocr.text_status("去年の大記事は何？やはり西歐大侵略戰。米國通信社の面白い調査"), "ok")


class Stores(unittest.TestCase):
    def test_create_is_create_only(self):
        with tempfile.TemporaryDirectory() as d:
            s = jaocr.LocalStore(d)
            self.assertTrue(s.create("a/b.json", b"1"))
            self.assertFalse(s.create("a/b.json", b"2"))
            self.assertEqual(s.read("a/b.json"), b"1")
            self.assertTrue(s.exists("a/b.json"))
            self.assertEqual(s.list("a"), ["a/b.json"])

    def test_claims(self):
        with tempfile.TemporaryDirectory() as d:
            s = jaocr.LocalStore(d)
            self.assertTrue(jaocr.claim(s, "i1", "r1", timedelta(hours=6)))
            self.assertFalse(jaocr.claim(s, "i1", "r2", timedelta(hours=6)))
            # A claim older than the stale limit is taken over.
            self.assertTrue(jaocr.claim(s, "i1", "r2", timedelta(seconds=-1)))
            self.assertEqual(json.loads(s.read("ocr-ja/claims/i1.json"))["owner"], "r2")


@unittest.skipIf(pa is None, "pyarrow not installed")
class Parquet(unittest.TestCase):
    def fixture(self, d):
        ref, cur = jaocr.LocalStore(os.path.join(d, "ref")), jaocr.LocalStore(os.path.join(d, "cur"))
        ref.write("current.json", json.dumps({"reference": "v1"}).encode())
        ref.write("v1/titles.json", json.dumps([
            {"lccn": "sn1", "languages": ["jpn", "eng"]}, {"lccn": "sn2", "languages": ["eng"]}]).encode())
        ref.write("v1/batches.json", json.dumps([
            {"batch": "b1", "curated": {"lccns": ["sn1", "sn2"], "parts": ["pages/b1/part-0.parquet"]}},
            {"batch": "b2", "curated": {"lccns": ["sn2"], "parts": ["pages/b2/part-0.parquet"]}},
        ]).encode())
        english = "The camp council met on Tuesday and voted for the new school " * 3
        rows = [
            ("sn1/1945-01-01/ed-1/seq-1", "sn1", 1, "ok", english),
            ("sn1/1945-01-01/ed-1/seq-2", "sn1", 2, "empty", None),
            ("sn1/1945-01-01/ed-1/seq-3", "sn1", 3, "ok", "Br H rJjr Jjw-Ini iiiTTTTiwtiawyM d j s,y fr"),
            ("sn2/1945-01-01/ed-1/seq-1", "sn2", 1, "empty", None),
        ]
        import datetime as dt
        t = pa.Table.from_pylist([{
            "doc_id": k.replace("/", "_"), "page_key": k, "lccn": l, "date": dt.date(1945, 1, 1),
            "edition": 1, "seq": s, "batch": "b1", "text_status": st, "text": tx,
        } for k, l, s, st, tx in rows])
        buf = io.BytesIO()
        pq.write_table(t, buf)
        cur.write("pages/b1/part-0.parquet", buf.getvalue())
        return ref, cur

    def test_unsafe_version_is_refused(self):
        with tempfile.TemporaryDirectory() as d:
            ref, cur = self.fixture(d)
            for bad in ("../cur", ".hidden", "-v1", "a/b", ""):
                ref.write("current.json", json.dumps({"reference": bad}).encode())
                with self.assertRaises(ValueError, msg=bad):
                    jaocr.targets(ref, cur, datasets=[])

    def test_targets(self):
        with tempfile.TemporaryDirectory() as d:
            ref, cur = self.fixture(d)
            got = jaocr.targets(ref, cur, datasets=[])
            self.assertEqual([(r["seq"], r["loc_text"]) for r in got], [(2, "empty"), (3, "garbled")])
            self.assertEqual(got[0]["date"], "1945-01-01")

    def test_targets_add_pages_missing_from_the_archives(self):
        with tempfile.TemporaryDirectory() as d:
            ref, cur = self.fixture(d)
            listing = ["sn1/1945/01/01/ed-1/seq-1/ocr.txt", "sn1/1945/01/01/ed-1/seq-1/ocr.xml",
                       "sn1/1945/01/01/ed-1/seq-4/ocr.xml",       # Japanese page: ALTO only
                       "sn2/1945/01/01/ed-1/seq-2/ocr.xml"]       # not a Japanese title
            orig = jaocr.list_archive
            jaocr.list_archive = lambda url, pacer: listing
            try:
                got = jaocr.targets(ref, cur, datasets=[
                    {"batch": "b1_ver01", "url": "u", "lccns": ["sn1"]},
                    {"batch": "b9_ver01", "url": "u", "lccns": ["sn9"]}])
            finally:
                jaocr.list_archive = orig
            self.assertEqual([(r["seq"], r["loc_text"], r["batch"]) for r in got],
                             [(2, "empty", "b1"), (3, "garbled", "b1"), (4, "missing", "b1")])
            self.assertEqual(got[2]["doc_id"], "sn1_1945-01-01_ed-1_seq-4")

    def test_done_is_per_page(self):
        with tempfile.TemporaryDirectory() as d:
            cur = jaocr.LocalStore(d)
            self.assertEqual(jaocr.part_name(cur, "i1"), "i1")
            self.write(cur, "i1", "a")
            self.assertEqual(jaocr.part_name(cur, "i1"), "i1.2")
            self.write(cur, "i1.2", "b")
            self.assertEqual(jaocr.part_name(cur, "i1"), "i1.3")
            self.assertEqual(jaocr.done_pages(cur, cur.list("ocr-ja/pages/")), {"a", "b"})

    def write(self, cur, name, doc_id):
        from datetime import datetime, timezone
        jaocr.write_part(cur, name, [{
            "doc_id": doc_id, "page_key": doc_id, "lccn": "sn1", "date": "1945-01-01", "edition": 1, "seq": 1,
            "batch": "b1", "loc_text": "missing", "ocr_source": jaocr.OCR_SOURCE, "ocr_engine": "t",
            "text_status": "ok", "text": "去年の大記事", "text_chars": 6, "text_sha256": b"\0" * 32,
            "image_url": "u", "ocred_at": datetime.now(timezone.utc)}])

    def test_write_part(self):
        with tempfile.TemporaryDirectory() as d:
            cur = jaocr.LocalStore(d)
            from datetime import datetime, timezone
            jaocr.write_part(cur, "sn1_1945-01-01_ed-1", [{
                "doc_id": "x", "page_key": "x", "lccn": "sn1", "date": "1945-01-01", "edition": 1, "seq": 2,
                "batch": "b1", "loc_text": "empty", "ocr_source": jaocr.OCR_SOURCE, "ocr_engine": "t",
                "text_status": "ok", "text": "去年の大記事", "text_chars": 6, "text_sha256": b"\0" * 32,
                "image_url": "u", "ocred_at": datetime.now(timezone.utc)}])
            t = pq.read_table(io.BytesIO(cur.read("ocr-ja/pages/sn1_1945-01-01_ed-1.parquet")))
            self.assertEqual(t.column("text").to_pylist(), ["去年の大記事"])
            self.assertEqual(str(t.schema.field("date").type), "date32[day]")


if __name__ == "__main__":
    unittest.main()
