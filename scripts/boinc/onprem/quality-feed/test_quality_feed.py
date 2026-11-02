"""Tests for the on-prem quality feeder (#962): the parts that need no Pillow, pyarrow or network."""
import contextlib
import hashlib
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
                 "doc_id": "sn1_1900-01-01_ed-1_seq-1", "lang": "eng",
                 "bytes": 6, "sha256": hashlib.sha256(b"source").hexdigest()}
            texts = {"page": None, "readers": [{"name": "loc-hist", "text": "x"}]}
            feed.write_chunk(d, "c00001", [p], {p["doc_id"]: (b"jpg", b"<alto/>", texts, None)})
            c = Path(d, "c00001")
            self.assertEqual(c.joinpath("READY").read_text(), "1\n")
            self.assertEqual(feed.pending_chunks(d), [str(c)])
            sums = c.joinpath("SHA256SUMS").read_text().split("\n")
            self.assertIn(f"scans/{p['doc_id']}.jpg", " ".join(sums))
            receipt = json.loads(c.joinpath("texts", p["doc_id"] + ".image-provenance.json").read_text())
            self.assertEqual(receipt["source"]["sha256"], p["sha256"])
            self.assertEqual(receipt["jpeg"]["sha256"], hashlib.sha256(b"jpg").hexdigest())
            self.assertIn(f"texts/{p['doc_id']}.image-provenance.json", " ".join(sums))
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


class FollowTest(unittest.TestCase):
    def test_page_of_names_the_set(self):
        row = {"doc_id": "sn1_1900-01-02_ed-1_seq-3", "batch": "b", "batch_version": 1, "bytes": 5,
               "sha256": "x", "format": "jp2", "fetched_at": "t"}
        p = feed.page_of(row, "buffer", {"sn1_1900-01-02_ed-1_seq-3": {"lang": "ger", "damage_rate": 0.4}})
        self.assertEqual(p["scan"], "buffer/b/sn1/1900-01-02/1/3.jp2")
        self.assertEqual((p["lang"], p["damage_rate"]), ("deu", 0.4))

    def test_only_shipped_quality_buffer_scans_are_deleted(self):
        with tempfile.TemporaryDirectory() as d:
            camp = Path(d, "camp")
            (camp / "c00002").mkdir(parents=True)  # still in the outbox
            done = Path(d, "done.txt")
            done.write_text("q1\tc00001\tbuffer/b/q1.jp2\n"   # shipped, quality, buffer: deleted
                            "o1\tc00001\tbuffer/b/o1.jp2\n"   # shipped, OCR's page: kept
                            "q2\tc00001\tkeep/b/q2.jp2\n"     # keep set: kept
                            "q3\tc00002\tbuffer/b/q3.jp2\n")  # not shipped yet: kept
            calls = []

            class Resp:
                def __enter__(self):
                    return self

                def __exit__(self, *a):
                    return False

            def fake(req, timeout=0):
                calls.append((req.get_method(), req.full_url))
                return Resp()

            a = type("A", (), {"scan_server": "http://s"})()
            with mock.patch.object(feed.urllib.request, "urlopen", fake):
                n = feed.delete_shipped(a, str(camp), str(done), {"q1", "q2", "q3"}, str(Path(d, "deleted.txt")))
            self.assertEqual(n, 1)
            self.assertEqual(calls, [("DELETE", "http://s/scans/buffer/b/q1.jp2")])
            with mock.patch.object(feed.urllib.request, "urlopen", fake):
                self.assertEqual(feed.delete_shipped(a, str(camp), str(done), {"q1"}, str(Path(d, "deleted.txt"))), 0)


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

    def test_agreement_rows_map_loc_and_as_to_a_and_b(self):
        x = {"doc_id": "sn1_1900-01-01_ed-1_seq-1", "batch": "b", "batch_version": 1, "metric_version": 1,
             "cer_loc": 0.7, "cer_as": 0.6, "local_cer_sym": 0.05, "matched_loc": 0.9, "matched_as": 0.99,
             "loc_words": 100, "as_words": 90}
        rows = loader.agreement_rows([x, dict(x, local_cer_sym=0.06), {"doc_id": "bad"}], "text-agreement-v1")
        self.assertEqual(len(rows), 1)
        r = dict(zip(loader.AGREE_COLS, rows[0]))
        self.assertEqual((r["reader_a"], r["reader_b"]), ("loc-hist", "as"))
        self.assertEqual((r["cer_a"], r["cer_b"], r["local_cer_sym"]), (0.7, 0.6, 0.06))
        self.assertEqual((r["matched_a"], r["words_b"]), (0.9, 90))

    def test_hat_v2_tables_keep_both_versions_and_v1_tables_fill_v1(self):
        base = {"doc_id": "sn1_1900-01-01_ed-1_seq-1", "reader": "loc-hist", "outcome": "ok", "job_id": "j",
                "blocks": 1, "pairs_json": None}
        v2 = dict(base, hat_version=2, est_cer="0.1", est_cer_v1="0.4", matched="0.7",
                  hat_determined=True, hat_determined_v1=False, hat_pairs_json='{"b": 1, "a": 2}')
        units, scores = loader.result_rows([v2], "v1-test")
        u = dict(zip(loader.UNIT_COLS, units[0]))
        r = dict(zip(loader.SCORE_COLS, scores[0]))
        self.assertEqual((u["hat_version"], u["hat_determined_v1"], u["hat_pairs"]), (2, False, '{"a": 2, "b": 1}'))
        self.assertEqual((r["est_cer"], r["est_cer_v1"], r["matched"]), (0.1, 0.4, 0.7))
        units, scores = loader.result_rows([dict(base, est_cer="0.3", hat_determined=True)], "v1-old")
        u = dict(zip(loader.UNIT_COLS, units[0]))
        r = dict(zip(loader.SCORE_COLS, scores[0]))
        self.assertEqual((u["hat_version"], u["hat_determined_v1"]), (1, True))
        self.assertEqual((r["est_cer"], r["est_cer_v1"], r["matched"]), (0.3, 0.3, None))

    def test_loc_word_confidence_rows(self):
        x = {"doc_id": "sn1_1900-01-01_ed-1_seq-1", "batch": "b", "batch_version": 2, "words": 10,
             "words_with_wc": 9, "wc_mean": 0.81, "wc_median": 0.9, "wc_below": 0.1, "wc_threshold": 0.5,
             "software": "ABBYY", "archive": "b_ver02.tar.bz2"}
        rows = loader.locwc_rows([x, dict(x, wc_mean=0.7), {"doc_id": "nope", "batch": "b"}])
        self.assertEqual(len(rows), 1)
        r = dict(zip(loader.LOCWC_COLS, rows[0]))
        self.assertEqual((r["batch_version"], r["wc_mean"], r["words"]), (2, 0.7, 10))

    def test_damage_keeps_one_row_a_page(self):
        import datetime

        x = {"doc_id": "sn1_1900-01-01_ed-1_seq-1", "batch": "b", "batch_version": 1, "lccn": "sn1",
             "date": datetime.date(1900, 1, 1), "edition": 1, "seq": 1, "lang": "eng", "damage_rate": 0.4}
        pages, damage = loader.damage_rows([x, dict(x, damage_rate=0.5)], "damage-v2")
        self.assertEqual(len(pages), 1)
        self.assertEqual(damage[0][loader.DAMAGE_COLS.index("damage_rate")], 0.5)


class ReceiveOwnerTest(unittest.TestCase):
    def test_a_file_already_the_owners_is_not_chowned(self):
        # Each chown on the share is a synchronous write; a chunk's walk meets thousands of files.
        with tempfile.TemporaryDirectory() as d:
            path = Path(d, "page.json")
            path.write_text("{}")
            info = os.lstat(path)
            with mock.patch.object(receive.os, "lchown") as chown:
                with mock.patch.object(receive, "OWNER", info.st_uid), \
                        mock.patch.object(receive.os, "lstat", return_value=mock.Mock(st_uid=info.st_uid, st_gid=info.st_uid)):
                    receive.own(str(path))
                chown.assert_not_called()
                with mock.patch.object(receive, "OWNER", info.st_uid + 1):
                    receive.own(str(path))
                chown.assert_called_once_with(str(path), info.st_uid + 1, info.st_uid + 1)


if __name__ == "__main__":
    unittest.main()
