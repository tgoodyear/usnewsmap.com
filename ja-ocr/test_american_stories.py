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

    def test_a_scan_with_no_page_joins_by_its_image(self):
        with tempfile.TemporaryDirectory() as d:
            ref, cur, _ = self.fixture(d)
            name, body = scan("sn1", "1865-02-01", 2, CLEAN)
            name = name.replace("_p2_", "_pNone_")
            first, body1 = scan("sn1", "1865-02-01", 1, CLEAN)
            tgz = tarball([(first, body1), (name, body)])
            got = ams.american_stories(ref, cur, [1865], sample_pct=100, run="exec-2",
                                       opener=lambda req: contextlib.closing(io.BytesIO(tgz)))
            s = got["summary"][0]
            self.assertEqual((s["with_american_stories"], s["scans_page_from_image"]), (2, 1))

    def test_compare(self):
        with tempfile.TemporaryDirectory() as d:
            ref, cur, tgz = self.fixture(d)
            asked = []

            def opener(req):
                asked.append((req.full_url, req.headers.get("Dnt")))
                return contextlib.closing(io.BytesIO(tgz))

            got = ams.american_stories(ref, cur, [1865], sample_pct=100, run="exec-1", opener=opener)
            self.assertEqual(asked, [(ams.URL.format(year=1865), "1")] * 2)  # names, then text
            s = got["summary"][0]
            # 4 English pages of ours; 3 have a scan; the scan of sn9 has no page of ours.
            self.assertEqual((s["our_sampled_english_pages"], s["with_american_stories"], s["join_share"]),
                             (4, 3, 0.75))
            self.assertEqual((s["american_stories_sampled_scans"], s["american_stories_without_our_page"]), (4, 1))
            self.assertEqual((s["scans_scans"], s["scans_unparsed"]), (4, 0))
            self.assertEqual(s["damage_median_american_stories"], 0.0)
            self.assertGreater(s["damage_median_loc"], 0.25)
            self.assertEqual((s["badly_damaged_share_loc"], s["badly_damaged_share_american_stories"]), (1.0, 0.0))
            self.assertEqual((s["scored_pages_loc"], s["scored_pages_american_stories"]), (3, 3))
            recall = {r["term"]: r for r in got["recall"]}
            # "railr0ad" in LoC's text: only American Stories finds railroad.
            self.assertEqual((recall["railroad"]["loc"], recall["railroad"]["american_stories"],
                              recall["railroad"]["only_american_stories"]), (0, 3, 3))
            self.assertEqual((recall["cotton"]["loc"], recall["cotton"]["either"], recall["cotton"]["gain_share"]),
                             (3, 3, 0.0))
            only = [o for o in got["only_american_stories"] if o["term"] == "railroad"]
            self.assertEqual(len(only), 3)
            self.assertIn("The railroad company met", only[0]["context"])
            self.assertTrue(only[0]["url"].startswith("https://www.loc.gov/resource/sn1/1865-0"))
            bands = {r["illegible_regions"]: r["pages"] for r in got["legibility"]}
            self.assertEqual(bands, {"lt_0.25": 2, "0.5-0.75": 1})
            self.assertTrue(cur.exists("audit/american-stories-v1-exec-1.json"))
            # A replica of the finished execution does nothing.
            self.assertIsNone(ams.american_stories(ref, cur, [1865], sample_pct=100, run="exec-1", opener=opener))

    def test_names_place_scans_by_page_or_by_image(self):
        self.assertEqual(ams.parse("faro_1793/1793-09-09_p4_sn84038410_01000424529_1793090901_0024.json"),
                         ("sn84038410", "1793-09-09", 1, 24, 4))
        self.assertEqual(ams.parse("faro_1865/1865-07-31_pNone_sn82014064_no_reel_1865073101_0003.json"),
                         ("sn82014064", "1865-07-31", 1, 3, None))
        self.assertIsNone(ams.parse("faro_1793/notes.json"))
        self.assertIsNone(ams.parse("faro_1865/1865-07-31_p1_sn1_x_1865080101_0001.json"))  # dates disagree
        names = [
            # An issue on a reel, all pages named: image 17 is p1.
            "1793-07-01_p1_sn1_0100_1793070101_0017.json", "1793-07-01_p2_sn1_0100_1793070101_0018.json",
            "1793-07-01_p3_sn1_0100_1793070101_0019.json",
            # No pages named: the issue's first image is p1.
            "1865-07-31_pNone_sn2_no_reel_1865073101_0001.json", "1865-07-31_pNone_sn2_no_reel_1865073101_0002.json",
            # Some named: they set the offset for the rest (p2 is image 41, so image 43 is p4).
            "1865-08-01_p2_sn2_0200_1865080101_0041.json", "1865-08-01_pNone_sn2_0200_1865080101_0043.json",
            # A second edition is its own issue.
            "1865-08-01_pNone_sn2_0200_1865080102_0050.json",
            # A second copy of the reel issue's p2 (the same page in another batch): skipped.
            "1793-07-01_p2_sn1_0300_1793070101_0018.json",
            "junk.json",
        ]
        ids, stats = ams.place(names)
        self.assertEqual(ids["1793-07-01_p3_sn1_0100_1793070101_0019.json"], "sn1_1793-07-01_ed-1_seq-3")
        self.assertEqual(ids["1865-07-31_pNone_sn2_no_reel_1865073101_0002.json"], "sn2_1865-07-31_ed-1_seq-2")
        self.assertEqual(ids["1865-08-01_pNone_sn2_0200_1865080101_0043.json"], "sn2_1865-08-01_ed-1_seq-4")
        self.assertEqual(ids["1865-08-01_pNone_sn2_0200_1865080102_0050.json"], "sn2_1865-08-01_ed-2_seq-1")
        self.assertEqual((stats["unparsed"], stats["page_named"], stats["page_from_image"], stats["duplicates"]),
                         (1, 5, 4, 1))
        self.assertEqual(ids["1793-07-01_p2_sn1_0100_1793070101_0018.json"], "sn1_1793-07-01_ed-1_seq-2")
        self.assertNotIn("1793-07-01_p2_sn1_0300_1793070101_0018.json", ids)
        self.assertEqual(len(ids), 8)  # 9 names parse, one is a second copy
        # The image rule on the named pages: right for the reel issue's 4 (the copy too), wrong for p2 at
        # image 41 (the issue's first image), so 4 of 5.
        self.assertEqual((stats["image_rule_checked"], stats["image_rule_agrees"]), (5, 4))

    def test_years_and_sample_are_checked(self):
        with tempfile.TemporaryDirectory() as d:
            ref, cur, _ = self.fixture(d)
            for years, pct in (([1990], 10), ([1773], 10), ([], 10), ([1865], 0), ([1865], -1), ([1865], 101),
                               ([1865], 0.001)):
                with self.assertRaises(ValueError, msg=(years, pct)):
                    ams.american_stories(ref, cur, years, sample_pct=pct, run="r")

    def test_the_cli_default_sample_is_per_command(self):
        from unittest import mock

        import quality

        env = {"USNM_REFERENCE_URL": "/r", "USNM_CURATED_URL": "/c"}
        with mock.patch.dict(os.environ, env), mock.patch.object(jaocr, "store", lambda url: url), \
                mock.patch.object(ams, "american_stories") as a, mock.patch.object(quality, "quality") as q:
            for argv in (["jaocr.py", "american-stories", "--year", "1865", "--year", "1925"],
                         ["jaocr.py", "american-stories", "--year", "1865", "--sample-pct", "2"],
                         ["jaocr.py", "quality"], ["jaocr.py", "quality", "--sample-pct", "10"]):
                with mock.patch("sys.argv", argv):
                    jaocr.main()
        self.assertEqual([c.args[2:] for c in a.call_args_list], [([1865, 1925], 10.0), ([1865], 2.0)])
        self.assertEqual([c.args[2] for c in q.call_args_list], [2.0, 10.0])

    def test_scan_text_and_legibility(self):
        name, body = scan("sn1", "1865-01-01", 1, "Body.", ("Illegible", "Legible", "Questionable"))
        body["bboxes"].append({"class": "ad", "legibility": "Illegible"})
        text, leg = ams.scan_text(body)
        self.assertEqual(text, "NEWS\nBody.")
        self.assertEqual(leg, {"illegible": 1, "legible": 1, "questionable": 1})  # ads aren't text regions


if __name__ == "__main__":
    unittest.main()
