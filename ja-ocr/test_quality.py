"""Tests for quality.py: python3 -m unittest discover ja-ocr (pyarrow for the Parquet ones)."""

import csv
import datetime as dt
import io
import json
import os
import tempfile
import unittest

import jaocr
import quality

try:
    import pyarrow as pa
    import pyarrow.parquet as pq
except ImportError:  # the Parquet tests need it; the rest don't
    pa = pq = None

try:
    import wordfreq
except ImportError:  # the image has it; the unit tests use their own word lists
    wordfreq = None

ENGLISH = frozenset("the council met on tuesday and voted for new school".split())
GERMAN = frozenset("der rath hat am dienstag die neue schule beschlossen".split())


def loader(code):
    return {"en": ENGLISH, "de": GERMAN}.get(code)


class Tokens(unittest.TestCase):
    def test_dict_share_counts_letter_words_only(self):
        # "1896" and "&" aren't words, "a" is too short; "Tuesday," loses its comma and case.
        s = quality.score("The council met on Tuesday, 1896 & a quorum.", ENGLISH, "eng")
        self.assertEqual((s.words, s.tokens), (6, 9))
        self.assertAlmostEqual(s.dict_share, 5 / 6)  # "quorum" isn't in the list

    def test_no_word_list_or_no_words(self):
        self.assertIsNone(quality.score("The council met", None, "haw").dict_share)
        self.assertIsNone(quality.score("1896 1897 $5", ENGLISH, "eng").dict_share)
        self.assertIsNone(quality.score("", ENGLISH, "eng").garbage_share)

    def test_case_folding(self):
        # Capitals and the German sharp s fold as wordfreq's lists do ("straße" is "strasse").
        self.assertEqual(quality.score("SCHULE Strasse Straße", frozenset({"schule", "strasse"}), "ger").dict_share, 1.0)

    def test_hyphenated_and_inner_punctuation_are_not_words(self):
        s = quality.score("well-known don't the", ENGLISH, "eng")
        self.assertEqual(s.words, 1)


class Garbage(unittest.TestCase):
    def flagged(self, tok, lang="eng"):
        m = quality.CORE.search(tok)
        return quality.garbage(tok, m.group(0) if m else None, lang)

    def test_rules(self):
        for tok in ("x" * 41, "%$#a", ".,;:", "aaab", "Mrrrning", "bcdfg", "aeiou", "tHe", "ThE", "eNGLISH",
                    "HeLLoS"):
            self.assertTrue(self.flagged(tok), tok)

    def test_real_words(self):
        for tok in ("McKay", "MacDonald", "USA", "Tuesday,", "(the)", "$5", "1,000", "III", "XXXIII", "rhythm",
                    "1000", "&", "—", "Schiff", "co.", "A", "WAR", "Kansas.City"):
            self.assertFalse(self.flagged(tok), tok)

    def test_no_vowel_words_in_czech(self):
        self.assertTrue(self.flagged("smrt", "eng"))
        self.assertFalse(self.flagged("smrt", "cze"))
        self.assertFalse(self.flagged("čtvrt", "cze"))

    def test_garbage_share(self):
        s = quality.score("the council tHe %$#a", ENGLISH, "eng")
        self.assertEqual(s.garbage_share, 0.5)


class Sampling(unittest.TestCase):
    def test_deterministic_and_close_to_the_rate(self):
        ids = [f"sn{i % 97}_1890-01-{i % 28 + 1:02d}_ed-1_seq-{i}" for i in range(20000)]
        a = [d for d in ids if quality.sampled(d, 200)]
        self.assertEqual(a, [d for d in ids if quality.sampled(d, 200)])
        self.assertTrue(300 < len(a) < 500, len(a))
        # A larger sample holds the smaller one.
        self.assertTrue(set(a) <= {d for d in ids if quality.sampled(d, 500)})
        self.assertTrue(all(quality.sampled(d, 10000) for d in ids[:100]))


class Languages(unittest.TestCase):
    def test_first_language_and_multilingual(self):
        self.assertEqual(quality.title_language({"languages": ["ger", "eng"]}), ("ger", True))
        self.assertEqual(quality.title_language({"languages": ["eng"]}), ("eng", False))
        self.assertEqual(quality.title_language({"languages": []}), ("und", False))
        self.assertEqual(quality.title_language(None), ("und", False))

    def test_codes(self):
        self.assertEqual([quality.wordlist_code(x) for x in ("eng", "ger", "nor", "hrv", "srp", "cze")],
                         ["en", "de", "nb", "sh", "sh", "cs"])
        for none in ("haw", "yid", "cho", "dak", "chr", "nav", "lat", "wel", "chi", "pennsylvania german"):
            self.assertIsNone(quality.wordlist_code(none), none)

    def test_title_infos(self):
        got = quality.title_infos([{"lccn": "a", "languages": ["eng", "jpn"]},
                                   {"lccn": "b", "languages": ["dak", "eng"]}])
        self.assertTrue(got["a"].japanese)
        self.assertEqual((got["b"].lang, got["b"].multilingual, got["b"].wordlist), ("dak", True, None))

    def test_word_lists_load_once(self):
        calls = []
        lists = quality.WordLists(lambda c: calls.append(c) or loader(c))
        lists.get("en"), lists.get("en"), lists.get("xx"), lists.get("xx"), lists.get(None)
        self.assertEqual(calls, ["en", "xx"])

    @unittest.skipIf(wordfreq is None, "wordfreq not installed")
    def test_every_mapped_code_has_its_own_wordfreq_list(self):
        have = wordfreq.available_languages("best")
        for lang, code in quality.WORDFREQ.items():
            self.assertIn(code, have, lang)
        self.assertIn("the", quality.wordfreq_words("en"))
        self.assertIsNone(quality.wordfreq_words("yi"))  # wordfreq would fall back to English


def page(lccn, decade, status="ok", dict_share=None, garbage_share=0.0):
    p = {"lccn": lccn, "decade": decade, "status": status}
    if status == "ok":
        p.update(dict_share=dict_share, garbage_share=garbage_share, chars=100, tokens=20)
    return p


class Aggregation(unittest.TestCase):
    def test_tables(self):
        titles = quality.title_infos([
            {"lccn": "e1", "languages": ["eng"]}, {"lccn": "e2", "languages": ["eng"]},
            {"lccn": "g1", "languages": ["ger", "eng"]}, {"lccn": "h1", "languages": ["haw"]},
            {"lccn": "j1", "languages": ["eng", "jpn"]}])
        catalog = {"g1": {"name": "Der Westbote", "languages": ["ger", "eng"], "place_of_publication": "Columbus, Ohio",
                          "state": "Ohio"}}
        t = quality.Tally(titles)
        t.add("b1_ver01", [page("e1", 1880, dict_share=x) for x in (0.9, 0.8, 0.95)] + [page("e1", 1880, "empty")])
        t.add("b1_ver01", [page("e2", 1890, dict_share=0.6), page("e2", 1890, "short")])
        t.add("b2_ver01", [page("g1", 1880, dict_share=x, garbage_share=0.2) for x in (0.3, 0.4, 0.65, 0.2)])
        t.add("b3_ver02", [page("h1", 1880, dict_share=None), page("j1", 1940, "empty"), page("zz", 1900, "empty")])
        got = t.tables(catalog, min_pages=2)

        ld = {(r["language"], r["decade"]): r for r in got["by_language_decade"]}
        e = ld[("eng", 1880)]
        self.assertEqual((e["pages"], e["empty_share"], e["scored_pages"]), (4, 0.25, 3))
        self.assertEqual((e["dict_share_median"], e["poor_share"], e["fair_share"]), (0.9, 0.0, 0.0))
        g = ld[("ger", 1880)]
        self.assertEqual((g["dict_share_median"], g["poor_share"], g["fair_share"]), (0.35, 0.75, 1.0))
        self.assertEqual((g["multilingual_pages"], g["garbage_share_mean"], g["wordlist"]), (4, 0.2, "de"))
        self.assertIsNone(ld[("haw", 1880)]["dict_share_median"])
        self.assertEqual(ld[("und", 1900)]["pages"], 1)
        self.assertNotIn("jpn", {r["language"] for r in got["by_language"]})
        self.assertEqual((t.japanese.pages, t.untitled), (1, 1))

        lang = {r["language"]: r for r in got["by_language"]}
        self.assertEqual((lang["eng"]["pages"], lang["eng"]["scored_pages"], lang["eng"]["dict_share_median"]),
                         (6, 4, 0.85))
        dec = {r["decade"]: r for r in got["by_decade"]}
        self.assertEqual((dec[1880]["pages"], dec[1880]["scored_pages"]), (9, 7))
        self.assertEqual([r["decade"] for r in got["by_decade"]], [1880, 1890, 1900])

        # e2 has one scored page: under --min-pages. Hawaiian has no word list.
        self.assertEqual([(r["lccn"], r["dict_share_median"]) for r in got["worst_titles"]],
                         [("g1", 0.35), ("e1", 0.9)])
        w = got["worst_titles"][0]
        self.assertEqual((w["rank"], w["name"], w["place"], w["state"], w["multilingual"], w["languages"]),
                         (1, "Der Westbote", "Columbus, Ohio", "Ohio", True, "ger eng"))
        self.assertEqual([(r["batch"], r["languages"]) for r in got["worst_batches"]],
                         [("b2_ver01", "ger:4"), ("b1_ver01", "eng:6")])
        self.assertEqual(quality.Tally(titles).tables({}, 1)["worst_titles"], [])

    def test_thresholds_are_strict_at_the_boundaries(self):
        # 0.5 and 0.7 aren't exact in 32-bit floats: stored as such, 0.7 would read as fair.
        t = quality.Tally(quality.title_infos([{"lccn": "e1", "languages": ["eng"]}]))
        t.add("b1_ver01", [page("e1", 1880, dict_share=x) for x in (0.5, 0.7)])
        r = t.tables({}, 1)["by_language_decade"][0]
        self.assertEqual((r["poor_share"], r["fair_share"]), (0.0, 0.5))


@unittest.skipIf(pa is None, "pyarrow not installed")
class Run(unittest.TestCase):
    def fixture(self, d):
        ref, cur = jaocr.LocalStore(os.path.join(d, "ref")), jaocr.LocalStore(os.path.join(d, "cur"))
        ref.write("current.json", json.dumps({"reference": "v1"}).encode())
        ref.write("v1/titles.json", json.dumps([
            {"lccn": "sn1", "name": "One", "languages": ["eng"], "state": "Ohio"},
            {"lccn": "sn2", "name": "Zwei", "languages": ["ger"], "state": "Ohio"}]).encode())
        ref.write("catalog/titles.json", json.dumps([
            {"lccn": "sn1", "name": "One (catalog)", "languages": ["eng"]},
            {"lccn": "sn3", "name": "Japanese", "languages": ["jpn", "eng"]}]).encode())
        ref.write("v1/batches.json", json.dumps([
            {"batch": "b1", "curated": {"version": 1, "parts": ["pages/b1/part-0.parquet", "pages/b1/part-1.parquet"]}},
            {"batch": "b2", "curated": {"version": 2, "parts": ["pages/b2/part-0.parquet"]}},
            {"batch": "b3", "curated": {"version": 1, "parts": []}},
        ]).encode())
        english = "The council met on Tuesday and voted for the new school"
        german = "Der Rath hat am Dienstag die neue Sclaverei beschlossen"
        rows = {
            "pages/b1/part-0.parquet": [("sn1", y, i, "ok", english) for i in range(30) for y in (1880, 1890)],
            "pages/b1/part-1.parquet": [("sn3", 1940, i, "ok", "Br H rJjr Jjw-Ini") for i in range(30)]
            + [("sn1", 1880, 100 + i, "empty", None) for i in range(10)],
            "pages/b2/part-0.parquet": [("sn2", 1870, i, "ok", german) for i in range(40)],
        }
        for path, rs in rows.items():
            t = pa.Table.from_pylist([{
                "doc_id": f"{l}_{y}-01-01_ed-1_seq-{s + 1}", "page_key": f"{l}/{y}-01-01/ed-1/seq-{s + 1}",
                "lccn": l, "date": dt.date(y, 1, 1), "edition": 1, "seq": s + 1, "text_status": st, "text": tx,
            } for l, y, s, st, tx in rs])
            buf = io.BytesIO()
            pq.write_table(t, buf)
            cur.write(path, buf.getvalue())
        return ref, cur

    def test_full_sample(self):
        with tempfile.TemporaryDirectory() as d:
            ref, cur = self.fixture(d)
            got = quality.quality(ref, cur, sample_pct=100, min_pages=5, workers=1, loader=loader)
            s = got["summary"]
            self.assertEqual((s["pages_read"], s["pages_sampled"], s["japanese_pages_skipped"], s["parts_failed"]),
                             (140, 140, 30, 0))
            lang = {r["language"]: r for r in got["by_language"]}
            self.assertEqual(lang["eng"]["dict_share_median"], 1.0)
            self.assertEqual((lang["eng"]["pages"], lang["eng"]["empty_share"]), (70, round(10 / 70, 4)))
            self.assertEqual(lang["ger"]["dict_share_median"], 0.8889)  # "Sclaverei": an old spelling
            self.assertEqual([r["batch"] for r in got["worst_batches"]], ["b2_ver02", "b1_ver01"])
            self.assertEqual(got["worst_titles"][0]["name"], "Zwei")
            # The published titles win over the working catalog's.
            self.assertEqual(got["worst_titles"][1]["name"], "One")
            report = json.loads(cur.read("audit/ocr-quality-v1-100pct.json"))
            self.assertEqual(report["summary"]["pages_sampled"], 140)
            rows = list(csv.DictReader(io.StringIO(cur.read("audit/ocr-quality-v1-100pct-by-language-decade.csv").decode())))
            self.assertEqual({(r["language"], r["decade"]) for r in rows},
                             {("eng", "1880"), ("eng", "1890"), ("ger", "1870")})
            for name in ("by-language", "by-decade", "worst-titles", "worst-batches"):
                self.assertTrue(cur.exists(f"audit/ocr-quality-v1-100pct-{name}.csv"), name)

    def test_sample_is_stable_and_a_second_replica_waits(self):
        with tempfile.TemporaryDirectory() as d:
            ref, cur = self.fixture(d)
            first = quality.quality(ref, cur, sample_pct=30, min_pages=1, workers=1, loader=loader)
            self.assertTrue(0 < first["summary"]["pages_sampled"] < 140)
            # The lock is fresh: another replica leaves the work to the first.
            self.assertIsNone(quality.quality(ref, cur, sample_pct=30, min_pages=1, workers=1, loader=loader))
            os.environ["JAOCR_AUDIT_LOCK_MINUTES"] = "-1"
            try:
                again = quality.quality(ref, cur, sample_pct=30, min_pages=1, workers=2, loader=loader)
            finally:
                del os.environ["JAOCR_AUDIT_LOCK_MINUTES"]
            self.assertEqual(again["summary"]["pages_sampled"], first["summary"]["pages_sampled"])
            self.assertEqual([r["pages"] for r in again["by_language_decade"]],
                             [r["pages"] for r in first["by_language_decade"]])

    def test_a_bad_part_is_counted_not_fatal(self):
        with tempfile.TemporaryDirectory() as d:
            ref, cur = self.fixture(d)
            cur.write("pages/b2/part-0.parquet", b"not parquet")
            got = quality.quality(ref, cur, sample_pct=100, min_pages=1, workers=1, loader=loader)
            self.assertEqual((got["summary"]["parts_failed"], got["summary"]["pages_read"]), (1, 100))

    def test_bad_arguments(self):
        with tempfile.TemporaryDirectory() as d:
            ref, cur = self.fixture(d)
            for pct in (0, -1, 101, 0.001):
                with self.assertRaises(ValueError, msg=pct):
                    quality.quality(ref, cur, sample_pct=pct, workers=1, loader=loader)
            for n in (0, -5):
                with self.assertRaises(ValueError, msg=n):
                    quality.quality(ref, cur, min_pages=n, workers=1, loader=loader)
            ref.write("current.json", json.dumps({"reference": "../x"}).encode())
            with self.assertRaises(ValueError):
                quality.quality(ref, cur, workers=1, loader=loader)


if __name__ == "__main__":
    unittest.main()
