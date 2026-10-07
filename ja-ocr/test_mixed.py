"""mixed.py: the bands of LoC's text on Japanese titles' pages, and the English in our OCR."""

import datetime as dt
import io
import json
import os
import tempfile
import unittest
import unittest.mock

import jaocr
import mixed

try:
    import pyarrow as pa
    import pyarrow.parquet as pq
except ImportError:  # pragma: no cover
    pa = None

TOP = frozenset({"the", "council", "met", "and", "voted", "for", "new", "school", "camp", "news"})


def table(rows, **cols):
    return pa.Table.from_pylist([{**r, **cols} for r in rows])


@unittest.skipIf(pa is None, "pyarrow not installed")
class Mixed(unittest.TestCase):
    def fixture(self, d):
        ref, cur = jaocr.LocalStore(os.path.join(d, "ref")), jaocr.LocalStore(os.path.join(d, "cur"))
        ref.write("current.json", json.dumps({"reference": "v1"}).encode())
        ref.write("v1/titles.json", json.dumps([
            {"lccn": "sn1", "name": "Camp News", "languages": ["eng", "jpn"]},
            {"lccn": "sn2", "name": "Town Paper", "languages": ["eng"]}]).encode())
        ref.write("v1/batches.json", json.dumps([
            {"batch": "b1", "curated": {"version": 1, "lccns": ["sn1", "sn2"], "parts": ["pages/b1/p0.parquet"]}},
            {"batch": "b2", "curated": {"version": 1, "lccns": ["sn9"], "parts": ["pages/b2/p0.parquet"]}},
        ]).encode())
        english = "The council met on Tuesday and voted for the new school building"
        mixed_page = english + " " + " ".join(["8ii", "4Jj", "1!r", "9;i", "rJjr"] * 3)

        def page(lccn, i, status, text):
            return {"doc_id": f"{lccn}_1943-01-01_ed-1_seq-{i}", "lccn": lccn, "text_status": status, "text": text}

        rows = ([page("sn1", i, "ok", english) for i in range(1, 6)]
                + [page("sn1", i, "ok", mixed_page) for i in range(6, 9)]
                + [page("sn1", 9, "ok", "8ii 4Jj 1!r 9;i"), page("sn1", 10, "empty", None)]
                + [page("sn2", i, "ok", english) for i in range(1, 5)] + [page("sn2", 5, "ok", mixed_page)])
        buf = io.BytesIO()
        pq.write_table(table(rows), buf)
        cur.write("pages/b1/p0.parquet", buf.getvalue())
        # A batch with no title that lists Japanese isn't read.
        cur.write("pages/b2/p0.parquet", b"not parquet")
        at = dt.datetime(2026, 10, 1, tzinfo=dt.timezone.utc)
        ours = [
            {"doc_id": "sn1_1943-01-02_ed-1_seq-1", "lccn": "sn1", "loc_text": "missing", "text": "日本の新聞 キャンプ", "ocred_at": at},
            {"doc_id": "sn1_1943-01-02_ed-1_seq-2", "lccn": "sn1", "loc_text": "missing",
             "text": "日本 " + " ".join(["the camp news"] * 5), "ocred_at": at},
            # An older reading of the same page is replaced by the newer one.
            {"doc_id": "sn1_1943-01-02_ed-1_seq-3", "lccn": "sn1", "loc_text": "garbled",
             "text": " ".join(["the council met"] * 20), "ocred_at": at},
            {"doc_id": "sn1_1943-01-02_ed-1_seq-3", "lccn": "sn1", "loc_text": "garbled", "text": "日本",
             "ocred_at": at + dt.timedelta(days=1)},
        ]
        for i, r in enumerate(ours):
            buf = io.BytesIO()
            pq.write_table(table([r]), buf)
            cur.write(f"ocr-ja/pages/sn1_1943-01-02_ed-1.{i}.parquet", buf.getvalue())
        return ref, cur

    def test_bands_and_english(self):
        with tempfile.TemporaryDirectory() as d:
            ref, cur = self.fixture(d)
            got = mixed.mixed(ref, cur, top=TOP, run="exec-1")
            # A replica of a finished execution (another, or this one retried) does nothing.
            self.assertIsNone(mixed.mixed(ref, cur, top=TOP, run="exec-1"))
            bands = {r["group"]: r for r in got["loc_text_bands"]}
            jp, control = bands["japanese"], bands["control"]
            self.assertEqual((jp["pages_with_loc_text"], control["pages_with_loc_text"]), (9, 5))
            self.assertEqual((jp["ge_0.9"], jp["lt_0.35"]), (5, 1))
            mid = sum(jp[k] for k in mixed.BAND_NAMES[1:-1])
            self.assertEqual(mid, 3)  # the three pages with a garbled column
            self.assertEqual((control["ge_0.9"], sum(control[k] for k in mixed.BAND_NAMES[1:-1])), (4, 1))
            title = got["loc_text_by_title"]
            self.assertEqual([(t["lccn"], t["name"], t["pages_with_loc_text"]) for t in title],
                             [("sn1", "Camp News", 9)])
            self.assertEqual({r["band"] for r in got["loc_text_samples"]} - set(mixed.BAND_NAMES), set())
            self.assertTrue(got["loc_text_samples"][0]["url"].startswith("https://www.loc.gov/resource/sn1/"))

            ours = {r["loc_text"]: r for r in got["our_ocr_english"]}
            self.assertEqual(ours["missing"]["pages"], 2)
            self.assertEqual((ours["missing"]["0"], ours["missing"]["10-49"]), (1, 1))  # "the camp news" x5: 15
            self.assertEqual((ours["garbled"]["pages"], ours["garbled"]["0"]), (1, 1))  # the newer reading
            # Latin shares: 0 for the Japanese-only page, 55/57 for the page with English.
            self.assertEqual(ours["missing"]["latin_share_over_half"], 0.5)
            self.assertEqual(ours["garbled"]["latin_share_median"], 0.0)
            self.assertEqual(got["our_parts"], 4)
            for name in ("loc-text-bands", "loc-text-by-title", "our-ocr-english", "our-ocr-samples"):
                self.assertTrue(cur.exists(f"audit/mixed-pages-v1-exec-1-{name}.csv"), name)
            self.assertEqual(json.loads(cur.read("audit/mixed-pages-v1-exec-1.json"))["version"], "v1")

    def test_a_retried_owner_takes_its_lock_back_and_a_dead_owners_lock_is_taken_over(self):
        with tempfile.TemporaryDirectory() as d:
            ref, cur = self.fixture(d)
            lock = "audit/mixed-pages-v1.run/exec-2.lock"
            with unittest.mock.patch.dict(os.environ, {"CONTAINER_APP_REPLICA_NAME": "exec-2-aaaaa"}):
                cur.write(lock, json.dumps({"owner": "exec-2-aaaaa"}).encode())  # our own, before a crash
                self.assertIsNotNone(mixed.mixed(ref, cur, top=TOP))
            cur.write("audit/mixed-pages-v1.run/exec-3.lock", b'{"owner": "exec-3-dead1"}')
            with unittest.mock.patch.dict(os.environ, {"CONTAINER_APP_REPLICA_NAME": "exec-3-bbbbb",
                                                       "JAOCR_AUDIT_LOCK_MINUTES": "-1"}):
                self.assertIsNotNone(mixed.mixed(ref, cur, top=TOP))

    def test_helpers(self):
        self.assertEqual(mixed.band(0.2), "lt_0.35")
        self.assertEqual(mixed.band(0.35), "0.35-0.5")
        self.assertEqual(mixed.band(0.95), "ge_0.9")
        self.assertEqual(mixed.band(0, mixed.WORD_BANDS, mixed.WORD_BAND_NAMES), "0")
        self.assertEqual(mixed.band(250, mixed.WORD_BANDS, mixed.WORD_BAND_NAMES), "ge_200")
        self.assertEqual(mixed.english_words("The council MET in 1943; tbe", TOP), 3)
        self.assertEqual(mixed.latin_share("ab日本"), 0.5)
        self.assertIsNone(mixed.latin_share("1943 ..."))
        self.assertEqual(mixed.loc_url("sn1_1943-01-02_ed-1_seq-4"),
                         "https://www.loc.gov/resource/sn1/1943-01-02/ed-1/?sp=4")


if __name__ == "__main__":
    unittest.main()
