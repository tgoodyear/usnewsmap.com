"""Tests for the on-prem quality feeder (#962): the parts that need no Pillow, pyarrow or network."""
import contextlib
import importlib.util
import io
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, HERE / file)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


feed = load("quality_feed", "quality_feed.py")
receive = load("quality_receive", "quality_receive.py")
loader = load("quality_load", "../../../onprem/quality/load.py")


class FeedTest(unittest.TestCase):
    def test_scan_path_follows_the_keep_layout(self):
        row = {"doc_id": "sn88084068_1917-11-01_ed-1_seq-12", "batch": "arhi_cosmic", "format": "jp2"}
        self.assertEqual(feed.scan_path(row), "keep/arhi_cosmic/sn88084068/1917-11-01/1/12.jp2")
        row.pop("format")
        self.assertTrue(feed.scan_path(row).endswith("/12.jpg"))

    def test_sample_row_matches_the_runner(self):
        p = {"batch": "rp_barbeddevil", "batch_version": 2, "damage_rate": 0.62, "date": "1894-08-01",
             "doc_id": "sn92063918_1894-08-01_ed-1_seq-2", "lang": "spa"}
        r = feed.sample_row(p)
        self.assertEqual((r["damage"], r["decade"], r["prefix"], r["year"]), ("high", 1890, "rp", 1894))
        p["damage_rate"] = None
        self.assertEqual(feed.sample_row(p)["damage_rate"], 0.15)

    def test_languages_and_readers(self):
        self.assertEqual(feed.lang_of("ger"), "deu")
        self.assertEqual(feed.lang_of("xxx"), "eng")
        self.assertEqual(feed.reader_name("surya 0.17", ""), "ours-surya")
        self.assertEqual(feed.reader_name("tesseract 5.5", "ocr-out"), "ours-tesseract")

    def test_empty_alto_carries_the_page_size(self):
        self.assertIn(b'WIDTH="4952" HEIGHT="6341"', feed.empty_alto(4952, 6341))

    def test_text_is_cleaned_and_capped(self):
        self.assertEqual(feed.normalize_text("a\x00b\nc"), "ab\nc")
        self.assertEqual(len(feed.normalize_text("x" * 600_000).encode()), 500_000)

    def test_chunk_is_written_with_sums_and_ready_last(self):
        with tempfile.TemporaryDirectory() as d:
            p = {"batch": "b", "batch_version": 1, "damage_rate": 0.3, "date": "1900-01-01",
                 "doc_id": "sn1_1900-01-01_ed-1_seq-1", "lang": "eng"}
            texts = {"page": None, "readers": [{"name": "loc-hist", "text": "x"}]}
            feed.write_chunk(d, "c00001", [p], {p["doc_id"]: (b"jpg", b"<alto/>", texts, None)})
            c = Path(d, "c00001")
            self.assertEqual(c.joinpath("READY").read_text(), "1\n")
            self.assertEqual(feed.pending_chunks(d), [str(c)])
            sums = c.joinpath("SHA256SUMS").read_text().split("\n")
            self.assertIn(f"scans/{p['doc_id']}.jpg", " ".join(sums))
            # The receiver accepts it.
            share = Path(d, "share")
            inc = share / "quality" / "v1-test" / "incoming"
            inc.mkdir(parents=True)
            os.rename(c, inc / "c00001")
            (share / "quality" / "v1-wave9").mkdir()
            (share / "quality" / "v1-wave9" / "places.txt").write_text("Albany\n")
            buf = io.StringIO()
            with mock.patch.object(receive.os, "lchown"), contextlib.redirect_stdout(buf):
                receive.main(["--campaign", "v1-test", "--chunk", "c00001", "--share", str(share)])
            self.assertEqual(buf.getvalue().split("\t")[1:], ["eng", "1\n"])
            self.assertTrue((share / "quality" / "v1-test" / "scans" / f"{p['doc_id']}.jpg").exists())

    def test_receiver_refuses_a_page_without_its_scan(self):
        with tempfile.TemporaryDirectory() as d:
            inc = Path(d, "quality", "v1-test", "incoming", "c00001")
            (inc / "scans").mkdir(parents=True)
            (inc / "texts").mkdir()
            (inc / "READY").write_text("1\n")
            (inc / "sample.jsonl").write_text(json.dumps({"doc_id": "sn1_1900-01-01_ed-1_seq-1"}) + "\n")
            (inc / "SHA256SUMS").write_text("")
            with self.assertRaises(SystemExit) as e:
                receive.main(["--campaign", "v1-test", "--chunk", "c00001", "--share", d])
            self.assertIn("would ask LoC", str(e.exception))


    def test_after_submit_frees_copies_once_every_bundle_is_submitted(self):
        with tempfile.TemporaryDirectory() as d:
            camp = Path(d, "quality", "v1-test")
            doc = "sn1_1900-01-01_ed-1_seq-1"
            inc = camp / "incoming" / "c00001"
            inc.mkdir(parents=True)
            (inc / "sample.jsonl").write_text(json.dumps({"doc_id": doc}) + "\n")
            for rel in (f"scans/{doc}.jpg", f"texts/{doc}.json", f"pages/{doc}/page.jpg", f"pages/{doc}/quality.json",
                        "units/bundle-00001/page.jpg"):
                (camp / rel).parent.mkdir(parents=True, exist_ok=True)
                (camp / rel).write_text("x")
            args = ["--campaign", "v1-test", "--chunk", "c00001", "--share", d, "--after-submit"]
            with self.assertRaises(SystemExit):
                receive.main(args)
            (camp / "units/bundle-00001/submitted").write_text("wu\n")
            with contextlib.redirect_stdout(io.StringIO()):
                receive.main(args)
            self.assertFalse((camp / f"scans/{doc}.jpg").exists())
            self.assertFalse((camp / f"pages/{doc}/page.jpg").exists())
            self.assertTrue((camp / f"pages/{doc}/quality.json").exists())
            self.assertTrue((camp / "units/bundle-00001/page.jpg").exists())
            self.assertFalse(inc.exists())


class LoadTest(unittest.TestCase):
    def test_numbers_from_strings_and_nan(self):
        self.assertEqual(loader.num("0.25"), 0.25)
        self.assertIsNone(loader.num(float("nan")))
        self.assertIsNone(loader.num("none"))
        self.assertEqual(loader.integer("12.0"), 12)

    def test_upsert_sql_copies_then_inserts(self):
        sql = loader.upsert_sql("pages", ["doc_id", "lang"], ["doc_id"], [("a", None), ("b", "eng")])
        self.assertIn("COPY t_load (doc_id, lang) FROM STDIN WITH (FORMAT csv);\na,\nb,eng\n\\.\n", sql)
        self.assertIn("ON CONFLICT (doc_id) DO UPDATE SET lang = EXCLUDED.lang", sql)
        self.assertEqual(loader.upsert_sql("pages", ["doc_id"], ["doc_id"], []), "")

    def test_results_split_into_pages_and_readers(self):
        rows = [
            {"doc_id": "sn1_1900-01-01_ed-1_seq-1", "reader": "loc-hist", "blocks": 0, "est_cer": "0.2",
             "pairs_json": '{"as": {"cer_sym": 0.5}}', "job_id": "j", "outcome": "ok"},
            {"doc_id": "sn1_1900-01-01_ed-1_seq-1", "reader": "as", "blocks": 0, "est_cer": None,
             "pairs_json": None, "job_id": "j", "outcome": "ok"},
        ]
        units, scores = loader.result_rows(rows, "v1-test")
        self.assertEqual(len(units), 1)
        self.assertEqual(units[0][loader.UNIT_COLS.index("readers")], 2)
        self.assertFalse(units[0][loader.UNIT_COLS.index("has_alto")])
        self.assertEqual([s[2] for s in scores], ["loc-hist", "as"])
        self.assertEqual(scores[0][loader.SCORE_COLS.index("est_cer")], 0.2)

    def test_damage_keeps_one_row_a_page(self):
        import datetime

        x = {"doc_id": "sn1_1900-01-01_ed-1_seq-1", "batch": "b", "batch_version": 1, "lccn": "sn1",
             "date": datetime.date(1900, 1, 1), "edition": 1, "seq": 1, "lang": "eng", "damage_rate": 0.4}
        pages, damage = loader.damage_rows([x, dict(x, damage_rate=0.5)], "damage-v2")
        self.assertEqual(len(pages), 1)
        self.assertEqual(damage[0][loader.DAMAGE_COLS.index("damage_rate")], 0.5)


if __name__ == "__main__":
    unittest.main()
