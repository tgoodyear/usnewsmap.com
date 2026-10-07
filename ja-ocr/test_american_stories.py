"""american_stories.py: joining American Stories' scans to our pages and comparing the two texts."""

import contextlib
import datetime as dt
import io
import json
import os
import tarfile
import tempfile
import unittest

import american_stories as ams
import jaocr

try:
    import pyarrow as pa
    import pyarrow.parquet as pq
    import wordfreq
except ImportError:  # pragma: no cover
    pa = wordfreq = None

CLEAN = ("The railroad company met on Tuesday and the president said that the line to the river would be built "
         "before the election, and that the cotton farmers of the county would ship their crop by rail. ") * 3
DAMAGED = CLEAN.replace(" the ", " tbe ").replace(" and ", " aud ").replace("railroad", "railr0ad")


def scan(lccn, date, page, text, legibility=("Legible",)):
    return f"faro_1865/{date}_p{page}_{lccn}_00000000000_{date.replace('-', '')}01_{page:04d}.json", {
        "lccn": {"lccn": lccn}, "edition": {"edition": "ed-01", "date": date}, "page_number": "na",
        "scan": {"raw_data_loc": f"https://chroniclingamerica.loc.gov/data/batches/b1_ver01/data/{lccn}/x.jp2"},
        "bboxes": [{"id": i, "class": "article", "legibility": leg, "raw_text": ""} for i, leg in enumerate(legibility)],
        "full articles": [{"headline": "NEWS", "byline": "", "article": text}],
    }


def tarball(scans) -> bytes:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tar:
        for name, body in scans:
            data = json.dumps(body).encode()
            info = tarfile.TarInfo(name)
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
    return buf.getvalue()


@unittest.skipIf(pa is None, "pyarrow or wordfreq not installed")
class AmericanStories(unittest.TestCase):
    def fixture(self, d):
        ref, cur = jaocr.LocalStore(os.path.join(d, "ref")), jaocr.LocalStore(os.path.join(d, "cur"))
        ref.write("current.json", json.dumps({"reference": "v1"}).encode())
        ref.write("v1/titles.json", json.dumps([
            {"lccn": "sn1", "name": "One", "languages": ["eng"]},
            {"lccn": "sn2", "name": "Zwei", "languages": ["ger"]}]).encode())
        ref.write("v1/batches.json", json.dumps([
            {"batch": "b1", "curated": {"version": 1, "first": "1865-01-01", "last": "1865-12-31",
                                        "parts": ["pages/b1/p0.parquet"]}},
            # Outside the year: never read.
            {"batch": "b2", "curated": {"version": 1, "first": "1900-01-01", "last": "1900-12-31",
                                        "parts": ["pages/b2/p0.parquet"]}},
        ]).encode())
        rows = [{"doc_id": f"sn1_1865-0{m}-01_ed-1_seq-{s}", "lccn": "sn1", "date": dt.date(1865, m, 1), "batch": "b1",
                 "text_status": "ok", "text": DAMAGED} for m in (1, 2) for s in (1, 2)]
        rows.append({"doc_id": "sn2_1865-01-01_ed-1_seq-1", "lccn": "sn2", "date": dt.date(1865, 1, 1),
                     "batch": "b1", "text_status": "ok", "text": "Der Rath"})
        buf = io.BytesIO()
        pq.write_table(pa.Table.from_pylist(rows), buf)
        cur.write("pages/b1/p0.parquet", buf.getvalue())
        cur.write("pages/b2/p0.parquet", b"not parquet")
        scans = [scan("sn1", "1865-01-01", 1, CLEAN), scan("sn1", "1865-01-01", 2, CLEAN, ("Illegible", "Legible")),
                 scan("sn1", "1865-02-01", 1, CLEAN), scan("sn9", "1865-03-01", 1, CLEAN)]
        return ref, cur, tarball(scans)

    def test_compare(self):
        with tempfile.TemporaryDirectory() as d:
            ref, cur, tgz = self.fixture(d)
            asked = []

            def opener(req):
                asked.append((req.full_url, req.headers.get("Dnt")))
                return contextlib.closing(io.BytesIO(tgz))

            got = ams.american_stories(ref, cur, [1865], sample_pct=100, run="exec-1", opener=opener)
            self.assertEqual(asked, [(ams.URL.format(year=1865), "1")])
            s = got["summary"][0]
            # 4 English pages of ours; 3 have a scan; the scan of sn9 has no page of ours.
            self.assertEqual((s["our_sampled_english_pages"], s["with_american_stories"], s["join_share"]),
                             (4, 3, 0.75))
            self.assertEqual((s["american_stories_sampled_scans"], s["american_stories_without_our_page"]), (4, 1))
            self.assertEqual((s["scans_scans"], s["scans_unparsed"]), (4, 0))
            self.assertEqual(s["damage_median_american_stories"], 0.0)
            self.assertGreater(s["damage_median_loc"], 0.25)
            self.assertEqual((s["badly_damaged_share_loc"], s["badly_damaged_share_american_stories"]), (1.0, 0.0))
            recall = {r["term"]: r for r in got["recall"]}
            # "railr0ad" in LoC's text: only American Stories finds railroad.
            self.assertEqual((recall["railroad"]["loc"], recall["railroad"]["american_stories"],
                              recall["railroad"]["only_american_stories"]), (0, 3, 3))
            self.assertEqual((recall["cotton"]["loc"], recall["cotton"]["either"], recall["cotton"]["gain_share"]),
                             (3, 3, 0.0))
            bands = {r["illegible_regions"]: r["pages"] for r in got["legibility"]}
            self.assertEqual(bands, {"lt_0.25": 2, "0.5-0.75": 1})
            self.assertTrue(cur.exists("audit/american-stories-v1-exec-1.json"))
            # A replica of the finished execution does nothing.
            self.assertIsNone(ams.american_stories(ref, cur, [1865], sample_pct=100, run="exec-1", opener=opener))

    def test_doc_id_and_years(self):
        self.assertEqual(ams.doc_id("faro_1793/1793-09-09_p4_sn84038410_01000424529_1793090901_0024.json", "ed-01"),
                         "sn84038410_1793-09-09_ed-1_seq-4")
        self.assertIsNone(ams.doc_id("faro_1793/notes.json", "ed-01"))
        self.assertIsNone(ams.doc_id("faro_1793/1793-09-09_p4_sn84038410_x.json", "edition one"))
        with tempfile.TemporaryDirectory() as d:
            ref, cur, _ = self.fixture(d)
            with self.assertRaises(ValueError):
                ams.american_stories(ref, cur, [1990], run="r")

    def test_scan_text_and_legibility(self):
        name, body = scan("sn1", "1865-01-01", 1, "Body.", ("Illegible", "Legible", "Questionable"))
        body["bboxes"].append({"class": "ad", "legibility": "Illegible"})
        text, leg = ams.scan_text(body)
        self.assertEqual(text, "NEWS\nBody.")
        self.assertEqual(leg, {"illegible": 1, "legible": 1, "questionable": 1})  # ads aren't text regions


if __name__ == "__main__":
    unittest.main()
