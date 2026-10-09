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

    def test_diff_tables(self):
        with tempfile.TemporaryDirectory() as d:
            ref, cur, tgz = self.fixture(d)
            got = ams.american_stories(ref, cur, [1865], sample_pct=100, run="exec-d", diff=True,
                                       opener=lambda req: contextlib.closing(io.BytesIO(tgz)))
            summary = {r["k"]: r for r in got["diff_summary"]}
            self.assertEqual(sorted(summary), list(ams.DIFF_KS))
            s0, s2 = summary[0], summary[2]
            self.assertEqual((s0["pages"], s0["pages_identical"], s0["align_gaps_given_up"]), (3, 0, 0))
            # LoC's "tbe", "aud" and "railr0ad", and AS's headline: the delta is a part of AS's text, more with k.
            self.assertLess(0, s0["delta_tokens"])
            self.assertLess(s0["delta_tokens"], s2["delta_tokens"])
            self.assertLessEqual(s2["delta_tokens"], s2["as_tokens"])
            self.assertEqual(s0["delta_share_of_as"], round(s0["delta_tokens"] / s0["as_tokens"], 4))
            for s in summary.values():
                self.assertEqual(s["terms_lost"], 0)  # every term of AS's text is in LoC's or the delta
            self.assertGreater(s0["ngram2_loss"], 0)  # "the railroad": "the" was "tbe", "railroad" was too
            self.assertEqual((s2["ngram2_loss"], s2["ngram3_loss"]), (0.0, 0.0))  # k >= n - 1 keeps them
            phrases = {r["phrase"]: r for r in got["diff_phrase"]}
            self.assertEqual(set(phrases), set(ams.TERMS) | set(ams.COMMON_PHRASES))
            rr = phrases["railroad"]
            self.assertEqual((rr["loc"], rr["only_american_stories"], rr["kept_k0"], rr["loss_k0"]), (0, 3, 3, 0.0))
            groups = {r["group"]: r for r in got["diff_phrase_group"]}
            self.assertEqual(set(groups), {"terms", "common", "context_2_words", "context_3_words"})
            self.assertEqual(groups["terms"]["only_american_stories"], 3)  # railroad, on 3 pages
            # The contexts' n-grams: "the railroad company", for one, matches only in AS's text.
            self.assertGreater(groups["context_3_words"]["only_american_stories"], 0)
            self.assertEqual(groups["context_3_words"]["loss_k2"], 0.0)
            self.assertIn("diff_summary", json.loads(cur.read("audit/american-stories-v1-exec-d.json")))

    def test_the_cli_passes_diff(self):
        from unittest import mock

        env = {"USNM_REFERENCE_URL": "/r", "USNM_CURATED_URL": "/c"}
        with mock.patch.dict(os.environ, env), mock.patch.object(jaocr, "store", lambda url: url), \
                mock.patch.object(ams, "american_stories") as a:
            for argv in (["jaocr.py", "american-stories", "--year", "1865", "--diff"],
                         ["jaocr.py", "american-stories", "--year", "1865"]):
                with mock.patch("sys.argv", argv):
                    jaocr.main()
        self.assertEqual([c.kwargs["diff"] for c in a.call_args_list], [True, False])

    def test_context_reads_hyphenated_words_as_terms_do(self):
        self.assertEqual(ams.context("The rail-\nroad company met.", "railroad", 4), "The railroad com")

    def test_scan_text_and_legibility(self):
        name, body = scan("sn1", "1865-01-01", 1, "Body.", ("Illegible", "Legible", "Questionable"))
        body["bboxes"].append({"class": "ad", "legibility": "Illegible"})
        body["bboxes"].append({"class": "author", "legibility": "Legible"})  # the dataset's name for a byline
        text, leg = ams.scan_text(body)
        self.assertEqual(text, "NEWS\nBody.")
        self.assertEqual(leg, {"illegible": 1, "legible": 2, "questionable": 1})  # ads aren't text regions


def toks(s: str) -> list[str]:
    return s.split()


class Diff(unittest.TestCase):
    """--diff: the delta of American Stories' text against LoC's (#251)."""

    def regions(self, loc: str, ast: str) -> list[tuple[int, int]]:
        a, b = toks(loc), toks(ast)
        blocks, gave_up = ams.matching_blocks(a, b)
        self.assertEqual(gave_up, 0)
        for i, j, n in blocks:
            self.assertEqual(a[i:i + n], b[j:j + n])
        return ams.diff_regions(blocks, len(a), len(b))

    def test_index_tokens_like_the_analyzer(self):
        # usnm_core::text's tokenize test, and normalize_ocr's hyphens, ligatures and long s.
        self.assertEqual(ams.index_tokens("Crucify mankind upon a CROSS of Gold! Café—ſo"),
                         ["crucify", "mankind", "upon", "a", "cross", "of", "gold", "cafe", "so"])
        self.assertEqual(ams.index_tokens("The indus-\ntry ﬁne Æsop x_y well-known 1896-\n97 " + "a" * 41),
                         ["the", "industry", "fine", "aesop", "x", "y", "well", "known", "1896", "97"])

    def test_regions(self):
        self.assertEqual(self.regions("a b c", "a b c"), [])  # identical: nothing differs
        self.assertEqual(self.regions("", "a b"), [(0, 2)])  # no LoC text: all of AS's
        self.assertEqual(self.regions("a b", ""), [(0, 0)])  # no AS text: only LoC's extra tokens
        self.assertEqual(self.regions("a x c d", "a y c d"), [(1, 2)])  # replaced
        self.assertEqual(self.regions("a c d", "a b c d"), [(1, 2)])  # inserted in AS
        self.assertEqual(self.regions("a b x c d", "a b c d"), [(2, 2)])  # LoC has more: a point
        # Common words with no unique anchor between them: difflib aligns the gap.
        self.assertEqual(self.regions("u the of the v", "u the the of v"), [(2, 2), (3, 4)])

    def test_windows(self):
        self.assertEqual(ams.windows([], 10, 3), [])
        self.assertEqual(ams.windows([(4, 5)], 10, 0), [(4, 5)])
        self.assertEqual(ams.windows([(4, 5)], 10, 2), [(2, 7)])
        self.assertEqual(ams.windows([(0, 1), (9, 10)], 10, 2), [(0, 3), (7, 10)])  # clipped to the page
        self.assertEqual(ams.windows([(2, 2)], 10, 0), [])  # LoC's extra tokens: nothing to keep at k = 0
        self.assertEqual(ams.windows([(2, 2)], 10, 1), [(1, 3)])  # the AS tokens around them
        self.assertEqual(ams.windows([(2, 3), (5, 6)], 10, 1), [(1, 7)])  # touching: one window
        self.assertEqual(ams.windows([(2, 3), (6, 7)], 10, 1), [(1, 4), (5, 8)])  # apart
        self.assertEqual(ams.windows([(2, 3), (4, 5)], 10, 2), [(0, 7)])  # overlapping
        self.assertEqual(ams.windows([(0, 2)], 2, 3), [(0, 2)])  # the whole page

    def test_phrase_kept(self):
        ast = toks("the city of new york county voted")
        self.assertTrue(ams.phrase_kept(ast, [(2, 5)], ("new", "york")))
        self.assertFalse(ams.phrase_kept(ast, [(2, 4)], ("new", "york")))  # cut at the window's end
        self.assertFalse(ams.phrase_kept(ast, [(2, 4), (4, 6)], ("new", "york")))  # not across two windows
        self.assertTrue(ams.phrase_kept(ast, [(4, 5)], ("york",)))
        self.assertFalse(ams.phrase_kept(ast, [], ("york",)))

    def test_words_in_both_texts_together_only_in_american_stories(self):
        loc, ast = toks("the city of new hampshire and york county voted"), toks("the city of new york county voted")
        phrases = {2: {("new", "york"), ("york", "county")}, 1: {("york",)}}
        got = ams.page_diff(loc, ast, phrases, ks=(0, 1, 2))
        self.assertEqual(got["phrases"]["as"] - got["phrases"]["loc"], {("new", "york")})
        self.assertEqual(got["phrases"]["edge"], {("new", "york")})
        self.assertEqual(got["ngrams"][2], {"as_only": 1, "edge": 1})  # "new york"
        self.assertEqual(got["ngrams"][3], {"as_only": 2, "edge": 2})  # "of new york", "new york county"
        # LoC's extra "hampshire and" sits between "new" and "york": k = 0 keeps nothing, k = 1 the bigram.
        self.assertEqual([got["by_k"][k]["phrases_kept"] for k in (0, 1, 2)],
                         [set(), {("new", "york")}, {("new", "york")}])
        self.assertEqual([got["by_k"][k]["ngrams"][3]["kept"] for k in (0, 1, 2)], [0, 0, 2])
        self.assertEqual([got["by_k"][k]["delta_tokens"] for k in (0, 1, 2)], [0, 2, 4])
        self.assertEqual(got["by_k"][0]["terms_lost"], 0)

    def test_windows_run_together_could_match_falsely(self):
        loc, ast = toks("a b c x d e f q r s"), toks("a b c y d e f z r s")  # two replaced, apart
        got = ams.page_diff(loc, ast, {2: {("y", "z")}}, ks=(0,))
        self.assertEqual(got["by_k"][0]["delta_tokens"], 2)
        self.assertEqual(got["by_k"][0]["phrases_spurious"], {("y", "z")})  # "y z" joined, in neither text
        self.assertEqual(got["by_k"][0]["ngrams"][2]["spurious_if_joined"], 1)

    def test_summary_arithmetic(self):
        pages = [ams.page_diff(toks(loc), toks(ast), {}, ks=(0, 1)) for loc, ast in (
            ("a b c d e f g h", "a b x d e f g h"),  # one replaced: 1 token at k = 0, 3 at k = 1
            ("a b c d", "a b c d"),  # identical
            ("", "p q"),  # no LoC text
        )]
        rows = {r["k"]: r for r in ams.diff_summary(1865, pages, 2.0, ks=(0, 1))}
        r0, r1 = rows[0], rows[1]
        self.assertEqual((r0["pages"], r0["pages_identical"], r0["pages_without_loc_text"]), (3, 1, 1))
        self.assertEqual((r0["loc_tokens"], r0["as_tokens"], r0["delta_tokens"], r1["delta_tokens"]), (12, 14, 3, 5))
        self.assertEqual((r0["delta_share_of_as"], r0["delta_vs_loc"]), (round(3 / 14, 4), 0.25))
        self.assertEqual((r0["page_delta_share_median"], r0["page_delta_share_p90"]), (0.125, 1.0))
        self.assertEqual((r0["as_terms"], r0["loc_terms"], r0["as_only_terms"]), (14, 12, 3))
        self.assertEqual((r0["delta_terms"], r1["delta_terms"], r0["terms_lost"]), (3, 5, 0))
        # Bigrams only in AS: "b x", "x d", "p q"; k = 0 keeps "p q", k = 1 all three.
        self.assertEqual((r0["ngram2_as_only"], r0["ngram2_kept"], r0["ngram2_loss"]), (3, 1, round(2 / 3, 4)))
        self.assertEqual((r1["ngram2_kept"], r1["ngram2_loss"]), (3, 0.0))
        self.assertEqual((r0["windows"], r0["secs_per_1000_pages"]), (2, 666.7))
        self.assertEqual(ams.diff_summary(1865, [], 0.0, ks=(0,))[0]["secs_per_1000_pages"], None)

    def test_phrase_rows_count_pages(self):
        groups = {"terms": {("york",)}, "common": {("new", "york")}, "context_2_words": {("city", "of")}}
        phrases = {1: groups["terms"], 2: groups["common"] | groups["context_2_words"]}
        pages = [ams.page_diff(toks(loc), toks(ast), phrases, ks=(0, 1)) for loc, ast in (
            ("the city of new hampshire and york", "the city of new york"),  # only in AS, words all in LoC
            ("in new york today", "in new york today"),  # in both
            ("in now yark today", "in new york today"),  # only in AS: both words replaced, kept at k = 0
        )]
        rows, groups_rows = ams.diff_phrases(1865, pages, groups, ks=(0, 1))
        ny = {r["phrase"]: r for r in rows}["new york"]
        self.assertEqual((ny["loc"], ny["american_stories"], ny["only_american_stories"], ny["only_words_all_in_loc"]),
                         (1, 3, 2, 1))
        self.assertEqual((ny["kept_k0"], ny["kept_k1"], ny["edge_kept_k0"], ny["edge_kept_k1"]), (1, 2, 0, 1))
        self.assertEqual((ny["loss_k0"], ny["loss_k1"]), (0.5, 0.0))
        g = {r["group"]: r for r in groups_rows}
        self.assertEqual((g["terms"]["only_american_stories"], g["terms"]["kept_k0"]), (1, 1))  # york, page 3
        self.assertEqual(g["context_2_words"]["american_stories"], 1)


if __name__ == "__main__":
    unittest.main()
