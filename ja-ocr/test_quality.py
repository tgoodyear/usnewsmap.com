"""Tests for quality.py: python3 -m unittest discover ja-ocr (pyarrow for the Parquet ones)."""

import csv
import datetime as dt
import gzip
import io
import json
import os
import re
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
    def fixture(self, d, english=None, german=None):
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
        english = english or "The council met on Tuesday and voted for the new school"
        german = german or "Der Rath hat am Dienstag die neue Sclaverei beschlossen"
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
            got = quality.quality(ref, cur, sample_pct=100, min_pages=5, workers=1, loader=loader, metric="v1")
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
            first = quality.quality(ref, cur, sample_pct=30, min_pages=1, workers=1, loader=loader, metric="v1")
            self.assertTrue(0 < first["summary"]["pages_sampled"] < 140)
            # The lock is fresh: another replica leaves the work to the first.
            self.assertIsNone(quality.quality(ref, cur, sample_pct=30, min_pages=1, workers=1, loader=loader,
                                              metric="v1"))
            os.environ["JAOCR_AUDIT_LOCK_MINUTES"] = "-1"
            try:
                again = quality.quality(ref, cur, sample_pct=30, min_pages=1, workers=2, loader=loader, metric="v1")
            finally:
                del os.environ["JAOCR_AUDIT_LOCK_MINUTES"]
            self.assertEqual(again["summary"]["pages_sampled"], first["summary"]["pages_sampled"])
            self.assertEqual([r["pages"] for r in again["by_language_decade"]],
                             [r["pages"] for r in first["by_language_decade"]])

    def test_a_bad_part_is_counted_not_fatal(self):
        with tempfile.TemporaryDirectory() as d:
            ref, cur = self.fixture(d)
            cur.write("pages/b2/part-0.parquet", b"not parquet")
            got = quality.quality(ref, cur, sample_pct=100, min_pages=1, workers=1, loader=loader, metric="v1")
            self.assertEqual((got["summary"]["parts_failed"], got["summary"]["pages_read"]), (1, 100))

    def test_bad_arguments(self):
        with tempfile.TemporaryDirectory() as d:
            ref, cur = self.fixture(d)
            for pct in (0, -1, 101, 0.001):
                with self.assertRaises(ValueError, msg=pct):
                    quality.quality(ref, cur, sample_pct=pct, workers=1, loader=loader, metric="v1")
            for n in (0, -5):
                with self.assertRaises(ValueError, msg=n):
                    quality.quality(ref, cur, min_pages=n, workers=1, loader=loader, metric="v1")
            ref.write("current.json", json.dumps({"reference": "../x"}).encode())
            with self.assertRaises(ValueError):
                quality.quality(ref, cur, workers=1, loader=loader, metric="v1")


# ---------------------------------------------------------------- v2

ENGLISH_TEXT = (
    "The council of the city met on Tuesday evening at the court house, and after a long debate it was voted "
    "that the new school should be built on the lot which the board had bought from Mr. Smith last year. The "
    "mayor said that he would not sign the bill unless the cost of the work was paid from the general fund, and "
    "the members agreed with him.")
GERMAN_TEXT = (
    "Der Stadtrath hat am Dienstag beschlossen, dass die neue Schule auf dem Grundstück gebaut werden soll, "
    "welches die Behörde im vorigen Jahre von Herrn Schmidt gekauft hat. Der Bürgermeister sagte, er werde das "
    "Gesetz nicht unterzeichnen, wenn die Kosten nicht aus der allgemeinen Kasse bezahlt würden, und die "
    "Mitglieder waren mit ihm einverstanden. Es ist noch nicht bekannt, wann die Arbeit beginnen wird.")
FRENCH_TEXT = (
    "Le conseil de la ville s'est réuni mardi soir au palais de justice, et après un long débat il a été décidé "
    "que la nouvelle école sera construite sur le terrain que la commission avait acheté l'année dernière. Le "
    "maire a dit qu'il ne signerait pas le projet si le coût des travaux n'était pas payé par le fonds général, "
    "et les membres étaient d'accord avec lui. On ne sait pas encore quand les travaux vont commencer.")
SPANISH_TEXT = (
    "El consejo de la ciudad se reunió el martes por la noche en la casa de justicia, y después de un largo "
    "debate se decidió que la nueva escuela será construida en el terreno que la junta había comprado el año "
    "pasado. El alcalde dijo que no firmaría la ley si el costo de la obra no fuera pagado por el fondo general, y "
    "los miembros estuvieron de acuerdo con él. Todavía no se sabe cuándo comenzará el trabajo.")
POLISH_TEXT = (
    "Rada miejska zebrała się we wtorek wieczorem w sądzie i po długiej debacie postanowiono, że nowa szkoła "
    "będzie zbudowana na placu, który zarząd kupił w zeszłym roku. Burmistrz powiedział, że nie podpisze ustawy, "
    "jeżeli koszt pracy nie będzie pokryty z ogólnego funduszu, a członkowie zgodzili się z nim. Jeszcze nie "
    "wiadomo, kiedy praca się rozpocznie, ale wszyscy są tego zdania, że to jest dobra rzecz dla miasta i dla "
    "jego mieszkańców.")
YIDDISH_TEXT = (
    "דער שטאט ראט האט זיך פֿארזאמלט דינסטיק אין אוונט און נאך א לאנגע וויכוח האט מען באשלאסן אז די נייע "
    "שול וועט געבויט ווערן אויף דעם פלאץ וואס די קאמיסיע האט געקויפט פאר א יאר. דער מעיאר האט געזאגט אז ער "
    "וועט נישט אונטערשרייבן דעם געזעץ אויב די קאסטן וועלן נישט באצאלט ווערן פון דער אלגעמיינער קאסע.")
HAWAIIAN_TEXT = (
    "Ua hiki mai ka moku mai Kaleponi mai i ka la mua o keia mahina, a ua lawe mai oia i na leka he nui loa no "
    "na kanaka o keia aina. Ua olelo ia mai e hele ana ke Alii i Hawaii i keia pule aku, a e noho ana oia ma "
    "Hilo no kekahi manawa. Ua nui ka ua ma Honolulu i keia pule, a ua piha na kahawai a pau.")


@unittest.skipIf(wordfreq is None, "wordfreq not installed")
class Detection(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.models = quality.Models()

    def detect(self, text, languages):
        return quality.score_v2(text, languages, self.models)

    def test_each_language_among_the_titles(self):
        for text, languages, want in (
                (ENGLISH_TEXT, ("eng", "ger"), "eng"), (GERMAN_TEXT, ("ger", "eng"), "ger"),
                (GERMAN_TEXT, ("eng", "ger"), "ger"), (FRENCH_TEXT, ("fre", "spa", "eng"), "fre"),
                (SPANISH_TEXT, ("spa", "fre"), "spa"), (POLISH_TEXT, ("eng", "pol"), "pol"),
                (ENGLISH_TEXT, (), "eng"), (ENGLISH_TEXT, ("haw",), "eng")):
            got = self.detect(text, languages)
            self.assertEqual((got["decision"], got["lang"]), ("detected", want), (want, languages))
            self.assertGreater(got["function_share"], 0.3, want)
            self.assertEqual(got["damage_rate"], 0.0, want)

    def test_runner_up_is_recorded(self):
        got = self.detect(SPANISH_TEXT, ("spa", "fre"))
        self.assertEqual(got["runner_up"], "fre")
        self.assertGreater(got["share"], got["runner_up_share"])

    def test_a_language_the_title_does_not_list_is_not_found(self):
        # A German page in a title catalogued as English only: English doesn't fit, so und.
        self.assertEqual(self.detect(GERMAN_TEXT, ("eng",))["decision"], "und")

    def test_languages_without_a_word_list_are_und(self):
        got = self.detect(HAWAIIAN_TEXT, ("haw", "eng"))
        self.assertEqual((got["decision"], got["lang"], got["function_share"], got["damage_rate"]),
                         ("und", "und", None, None))

    def test_too_short_is_und(self):
        got = self.detect("The council met on Tuesday and voted for the new school.", ("eng",))
        self.assertEqual((got["decision"], got["lang"]), ("und", "und"))

    def test_garbled_is_und(self):
        got = self.detect(" ".join(["tlrc 8ii nrw Iii fkm Jtk lttk abr tbW4i kikf Jíl"] * 10), ("eng", "cze"))
        self.assertEqual(got["decision"], "und")

    def test_bilingual_page_is_mixed(self):
        text = " ".join([GERMAN_TEXT] * 3 + [ENGLISH_TEXT] * 3)
        got = self.detect(text, ("ger", "eng"))
        self.assertEqual((got["decision"], got["lang"], got["mixed_with"]), ("mixed", "ger", "eng"))
        self.assertGreater(got["function_share"], 0.3)
        # A few English lines on a German page don't make it mixed.
        got = self.detect(" ".join([GERMAN_TEXT] * 6 + [ENGLISH_TEXT]), ("ger", "eng"))
        self.assertEqual((got["decision"], got["lang"]), ("detected", "ger"))

    def test_hebrew_script(self):
        yiddish = self.detect(YIDDISH_TEXT, ("eng", "yid"))
        self.assertEqual((yiddish["decision"], yiddish["lang"], yiddish["script"]), ("detected", "yid", "hebrew"))
        self.assertIsNone(yiddish["damage_rate"])  # wordfreq has no Yiddish list
        # Hebrew script in a title that lists no language written in it.
        self.assertEqual(self.detect(YIDDISH_TEXT, ("eng",))["decision"], "und")
        # Listing both: Hebrew's word list doesn't fit Yiddish, so the one without a list.
        self.assertEqual(self.detect(YIDDISH_TEXT, ("heb", "yid"))["lang"], "yid")
        hebrew = self.detect(YIDDISH_TEXT, ("heb",))
        self.assertEqual((hebrew["lang"], hebrew["script"]), ("heb", "hebrew"))
        # Pointed letters (combining marks) don't split or drop a word.
        self.assertIn("פארזאמלט", quality.text_words("פֿארזאמלט")[1])
        # An English column beside the Yiddish: mixed.
        both = self.detect(YIDDISH_TEXT + " " + ENGLISH_TEXT, ("yid", "eng"))
        self.assertEqual((both["decision"], {both["lang"], both["mixed_with"]}), ("mixed", {"eng", "yid"}))

    def test_cyrillic_serbian_has_no_word_list(self):
        # wordfreq's Serbo-Croatian list is in Latin script: a Cyrillic page is Serbian, unscored.
        text = ("Општински одбор састао се у уторак увече у судници и после дуге расправе решено је да се "
                "нова школа сагради на плацу који је управа купила прошле године. Председник општине рекао је "
                "да неће потписати закон ако трошкови не буду плаћени из општег фонда.")
        got = self.detect(text, ("srp", "eng"))
        self.assertEqual((got["decision"], got["lang"], got["script"], got["function_share"]),
                         ("detected", "srp", "cyrillic", None))
        self.assertEqual(self.models.get("srp").script, "latin")

    def test_hyphenated_line_breaks_are_joined(self):
        self.assertEqual(quality.text_words("turn-\ning the well-known")[1], ["turning", "the"])


@unittest.skipIf(wordfreq is None, "wordfreq not installed")
class DamageRate(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.models = quality.Models()
        cls.en, cls.de = cls.models.get("eng"), cls.models.get("ger")

    def test_english_near_misses(self):
        for miss, word in (("tbe", "the"), ("tlie", "the"), ("thc", "the"), ("ihe", "the"), ("lhe", "the"),
                           ("tne", "the"), ("aud", "and"), ("nnd", "and"), ("ot", "of"), ("iu", "in")):
            self.assertEqual(self.en.near.get(miss), word, miss)

    def test_german_near_misses(self):
        for miss, word in (("ift", "ist"), ("fich", "sich"), ("bie", "die"), ("unb", "und"), ("dcr", "der")):
            self.assertEqual(self.de.near.get(miss), word, miss)
        self.assertIn(self.de.near.get("fie"), ("sie", "die"))  # one edit from either
        # Two substitutions ("d" as "b" and "e" as "c", "n" as "u"): not one edit, not counted.
        self.assertNotIn("bcr", self.de.near)
        self.assertNotIn("beu", self.de.near)

    def test_real_words_are_not_near_misses(self):
        for word in ("she", "an", "or", "he", "then", "hand", "they", "tho", "is", "on"):
            self.assertNotIn(word, self.en.near, word)
        for word in ("sie", "die", "ein", "den", "dem", "bei"):
            self.assertNotIn(word, self.de.near, word)

    def test_rate(self):
        # the, the, and, of: 4 right; tbe, aud, ot: 3 near-misses; "she" and "an" are words.
        d = quality.damage(self.en, "the tbe the and aud of ot she an council".split())
        self.assertEqual((d.head, d.near, d.words), (4, 3, 10))
        self.assertAlmostEqual(d.damage_rate(), 3 / 7)
        self.assertAlmostEqual(d.function_share(), 6 / 10)  # the x2, and, of, she, an
        d = quality.damage(self.de, "die bie der dcr und unb ist ift sich fich".split())
        self.assertEqual(d.damage_rate(), 0.5)
        self.assertIsNone(quality.damage(self.en, ["council"]).damage_rate())

    def test_a_real_word_of_another_candidate_language_is_not_damage(self):
        # "tie" is one edit from "die", but an English word.
        self.assertEqual(quality.damage(self.de, ["tie"]).near, 1)
        self.assertEqual(quality.damage(self.de, ["tie"], (self.en,)).near, 0)

    def test_dano_norwegian_spellings_are_not_damage(self):
        # "af" (av) is Danish, "aar" (år) and "paa" (på) the period's spelling; "fom" (som) is a misread.
        nor = self.models.get("nor")
        self.assertNotIn("af", nor.near)
        self.assertNotIn("aar", nor.near)
        self.assertIn("paa", nor.top)
        self.assertEqual(nor.near.get("fom"), "som")

    def test_damaged_page(self):
        damaged = ENGLISH_TEXT.replace("the ", "tbe ").replace("and ", "aud ")
        got = quality.score_v2(damaged, ("eng",), self.models)
        self.assertEqual(got["lang"], "eng")
        self.assertGreater(got["damage_rate"], quality.BADLY_DAMAGED)

    def test_one_edit(self):
        got = list(quality.one_edit("ab", "abx", "i", "il"))
        for want in ("b", "a", "xb", "ax", "iab", "aib", "abi", "iib", "lib", "ail"):
            self.assertIn(want, got)
        self.assertNotIn("xab", got)  # only thin letters are inserted
        self.assertNotIn("ab", got)
        self.assertLess(got.index("xb"), got.index("b"))  # substitutions first, then deletions,
        self.assertLess(got.index("b"), got.index("iab"))  # insertions
        self.assertLess(got.index("iab"), got.index("lib"))  # and splits


class FakeModels(quality.Models):
    """Word lists without wordfreq: each language's 'ranked' words."""

    def __init__(self):
        ranked = {
            "en": "the of and to in is for that it on was with he as at by this be not are".split()
            + [f"en{i}" for i in range(100)],
            "de": "die der und in den von zu das mit sich des auf ist im dem nicht ein eine als auch".split()
            + [f"de{i}" for i in range(100)],
        }
        super().__init__(lambda code: ranked.get(code))


def page2(lccn, decade, lang="eng", decision="detected", function_share=0.4, damage_rate=0.0, mixed_with=None,
          status="ok", garbage_share=0.0):
    p = {"lccn": lccn, "decade": decade, "status": status}
    if status == "ok":
        p.update(lang=lang, decision=decision, mixed_with=mixed_with, function_share=function_share,
                 damage_rate=damage_rate, garbage_share=garbage_share, chars=100, tokens=20)
    return p


class Aggregation2(unittest.TestCase):
    def test_tables(self):
        titles = quality.title_infos([
            {"lccn": "e1", "languages": ["eng"]}, {"lccn": "g1", "languages": ["ger", "eng"]},
            {"lccn": "y1", "languages": ["eng", "yid"]}, {"lccn": "j1", "languages": ["eng", "jpn"]}])
        catalog = {"g1": {"name": "Der Westbote", "languages": ["ger", "eng"], "state": "Ohio"}}
        t = quality.Tally2(titles)
        t.add("b1_ver01", [page2("e1", 1880, damage_rate=x) for x in (0.0, 0.05, 0.2)]
              + [page2("e1", 1880, status="empty")])
        t.add("b2_ver01", [page2("g1", 1880, "ger", damage_rate=x) for x in (0.3, 0.12, 0.4)]
              + [page2("g1", 1880, "eng", damage_rate=0.0),
                 page2("g1", 1890, "ger", "mixed", damage_rate=0.1, mixed_with="eng"),
                 page2("g1", 1890, "und", "und", function_share=None, damage_rate=None)])
        t.add("b3_ver01", [page2("y1", 1900, "yid", function_share=None, damage_rate=None),
                           page2("y1", 1900, "und", "und", function_share=None, damage_rate=None),
                           page2("j1", 1940, status="empty"), page2("zz", 1900, "eng")])
        got = t.tables(catalog, min_pages=2)

        lang = {r["language"]: r for r in got["by_language"]}
        self.assertEqual(set(lang), {"eng", "ger", "mixed", "und", "yid", "no_text"})
        e = lang["eng"]
        self.assertEqual((e["pages"], e["text_pages"], e["damage_pages"], e["metric"]), (5, 5, 5, "v2"))
        self.assertEqual((e["damage_rate_median"], e["damaged_share"], e["badly_damaged_share"]), (0.0, 0.2, 0.0))
        g = lang["ger"]
        self.assertEqual((g["damage_rate_median"], g["damaged_share"], g["badly_damaged_share"]),
                         (0.3, 1.0, round(2 / 3, 4)))
        self.assertEqual((g["function_share_p10"], g["function_share_p90"], g["wordlist"]), (0.4, 0.4, "de"))
        self.assertEqual((lang["und"]["und_share"], lang["mixed"]["mixed_share"]), (1.0, 1.0))
        self.assertEqual((lang["yid"]["scored_pages"], lang["yid"]["damage_rate_median"]), (0, None))
        self.assertEqual((lang["no_text"]["pages"], lang["no_text"]["empty_share"]), (1, 1.0))
        self.assertEqual((t.japanese.pages, t.untitled), (1, 1))

        dec = {r["decade"]: r for r in got["by_decade"]}
        self.assertEqual((dec[1880]["pages"], dec[1880]["text_pages"], dec[1880]["empty_share"]), (8, 7, 0.125))
        self.assertEqual((dec[1890]["und_share"], dec[1890]["mixed_share"]), (0.5, 0.5))
        ld = {(r["language"], r["decade"]) for r in got["by_language_decade"]}
        self.assertIn(("ger", 1880), ld)

        cross = {(r["title_language"], r["detected"]): (r["pages"], r["share"]) for r in got["language_agreement"]}
        self.assertEqual(cross[("ger", "ger")], (3, 0.5))
        self.assertEqual(cross[("ger", "mixed:ger+eng")], (1, round(1 / 6, 4)))
        self.assertEqual(cross[("eng", "yid")], (1, round(1 / 6, 4)))
        self.assertEqual(cross[("eng", "no_text")], (1, round(1 / 6, 4)))
        self.assertEqual(cross[("und", "eng")], (1, 1.0))

        summ = {r["scope"]: r for r in got["agreement_summary"]}
        self.assertEqual([r["scope"] for r in got["agreement_summary"]][:3],
                         ["all", "single_language_titles", "multilingual_titles"])
        # 12 text pages: e1 3 eng; g1 3 ger, 1 eng, 1 mixed, 1 und; y1 1 yid, 1 und; zz (no title) 1 eng.
        a = summ["all"]
        self.assertEqual(a["text_pages"], 12)
        self.assertEqual((a["same_share"], a["differs_share"]), (0.5, 0.5))
        self.assertEqual((a["und_share"], a["mixed_share"], a["other_language_share"]),
                         (round(2 / 12, 4), round(1 / 12, 4), 0.25))
        m = summ["multilingual_titles"]  # g1 and y1: 8 text pages
        self.assertEqual((m["text_pages"], m["same_share"], m["mixed_with_first_share"]), (8, 0.375, 0.125))
        self.assertEqual(summ["single_language_titles"]["same_share"], 1.0)
        self.assertEqual(summ["title_language:ger"]["text_pages"], 6)

        # By median damage rate; y1 has no scored pages, zz is untitled with one.
        self.assertEqual([(r["lccn"], r["damage_rate_median"]) for r in got["worst_titles"]],
                         [("g1", 0.12), ("e1", 0.05)])
        w = got["worst_titles"][0]
        self.assertEqual((w["name"], w["languages"], w["multilingual"], w["detected"]),
                         ("Der Westbote", "ger eng", True, "ger:3 eng:1 mixed:1 und:1"))
        self.assertEqual([r["batch"] for r in got["worst_batches"]], ["b2_ver01", "b1_ver01"])
        self.assertEqual(got["worst_batches"][1]["detected"], "eng:3 no_text:1")
        self.assertEqual([(r["lccn"], r["und_share"]) for r in got["undetermined_titles"]],
                         [("y1", 0.5), ("g1", round(1 / 6, 4))])


@unittest.skipIf(pa is None, "pyarrow not installed")
class Run2(unittest.TestCase):
    def test_full_sample(self):
        with tempfile.TemporaryDirectory() as d:
            ref, cur = Run.fixture(None, d, english=ENGLISH_TEXT, german=GERMAN_TEXT)
            got = quality.quality(ref, cur, sample_pct=100, min_pages=5, workers=1, loader=None)
            s = got["summary"]
            self.assertEqual((s["metric"], s["pages_sampled"], s["japanese_pages_skipped"]), ("v2", 140, 30))
            self.assertEqual((s["differs_share_all"], s["und_share_all"]), (0.0, 0.0))
            lang = {r["language"]: r for r in got["by_language"]}
            self.assertEqual((lang["eng"]["text_pages"], lang["ger"]["text_pages"], lang["no_text"]["pages"]),
                             (60, 40, 10))
            self.assertEqual(lang["eng"]["damage_rate_median"], 0.0)
            for name in ("by-language-decade", "by-language", "by-decade", "language-agreement", "agreement-summary",
                         "worst-titles", "worst-batches", "undetermined-titles"):
                self.assertTrue(cur.exists(f"audit/ocr-quality-v2-v1-100pct-{name}.csv"), name)
            report = json.loads(cur.read("audit/ocr-quality-v2-v1-100pct.json"))
            self.assertEqual(report["method"]["min_share"], quality.MIN_SHARE)
            rows = list(csv.DictReader(io.StringIO(cur.read("audit/ocr-quality-v2-v1-100pct-language-agreement.csv")
                                                   .decode())))
            self.assertEqual({(r["title_language"], r["detected"], r["pages"]) for r in rows},
                             {("eng", "eng", "60"), ("eng", "no_text", "10"), ("ger", "ger", "40")})
            self.assertFalse(cur.exists("audit/ocr-quality-v1-100pct.json"))
            pages = list(csv.DictReader(io.StringIO(gzip.decompress(
                cur.read("audit/ocr-quality-v2-v1-100pct-pages.csv.gz")).decode())))
            self.assertEqual(len(pages), 110)  # not the 30 Japanese
            self.assertEqual(list(pages[0]), quality.PAGE_COLUMNS)
            ger = next(p for p in pages if p["lccn"] == "sn2")
            self.assertEqual((ger["language"], ger["decision"], ger["title_language"], ger["batch"], ger["damage_rate"],
                              ger["date"]), ("ger", "detected", "ger", "b2_ver02", "0", "1870-01-01"))
            empty = next(p for p in pages if p["status"] == "empty")
            self.assertEqual((empty["language"], empty["decision"], empty["function_share"]),
                             ("no_text", "no_text", ""))
            # Under 200 sampled pages, no language is listed on the status page.
            status = json.loads(ref.read(quality.STATUS))
            self.assertEqual((status["summary"]["languages"], status["batches"]), ([], {"done": 3, "total": 3}))

    def test_status_file(self):
        class Recording(jaocr.LocalStore):
            def write(self, path, data):
                if path == quality.STATUS:
                    writes.append(data)
                super().write(path, data)

        iso = re.compile(r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$")
        keys = ["batches", "finished_at", "metric", "pages_sampled", "sample_pct", "started_at", "summary",
                "updated_at", "version"]
        writes = []
        with tempfile.TemporaryDirectory() as d:
            ref, cur = Run.fixture(None, d, english=ENGLISH_TEXT, german=GERMAN_TEXT)
            ref = Recording(os.path.join(d, "ref"))
            old = (quality.PROGRESS_BATCHES, quality.STATUS_MIN_PAGES)
            quality.PROGRESS_BATCHES, quality.STATUS_MIN_PAGES = 1, 40
            try:
                quality.quality(ref, cur, sample_pct=100, min_pages=5, workers=1)
            finally:
                quality.PROGRESS_BATCHES, quality.STATUS_MIN_PAGES = old
            bodies = [json.loads(w) for w in writes]
            self.assertGreaterEqual(len(bodies), 3)  # the start, progress, the end
            for raw, b in zip(writes, bodies):
                self.assertEqual(list(b), keys)
                self.assertEqual(list(json.loads(raw, object_pairs_hook=lambda kv: [k for k, _ in kv])), keys)
                self.assertEqual((b["metric"], b["sample_pct"], b["version"]), ("v2", 100.0, "v1"))
                self.assertEqual(list(b["batches"]), ["done", "total"])
                self.assertEqual(b["batches"]["total"], 3)
                self.assertRegex(b["started_at"], iso)
                self.assertRegex(b["updated_at"], iso)
            for b in bodies[:-1]:
                self.assertEqual((b["finished_at"], b["summary"]), (None, None))
            start, end = bodies[0], bodies[-1]
            self.assertEqual((start["batches"]["done"], start["pages_sampled"]), (1, 0))  # b3 has no parts
            self.assertEqual((end["batches"]["done"], end["pages_sampled"]), (3, 140))
            self.assertRegex(end["finished_at"], iso)
            self.assertEqual(json.loads(ref.read(quality.STATUS)), end)
            summary = end["summary"]
            self.assertEqual(list(summary), ["agreement", "languages"])
            self.assertEqual(summary["agreement"], {"differs_share": 0.0, "mixed_share": 0.0,
                                                    "multilingual_differs_share": None, "und_share": 0.0})
            self.assertEqual(summary["languages"], [
                {"damage_rate_median": 0.0, "damaged_share": 0.0, "function_share_median": 0.5441,
                 "language": "eng", "pages": 60},
                {"damage_rate_median": 0.0, "damaged_share": 0.0, "function_share_median": 0.4762,
                 "language": "ger", "pages": 40}])

    def test_v1_writes_no_status(self):
        with tempfile.TemporaryDirectory() as d:
            ref, cur = Run.fixture(None, d)
            quality.quality(ref, cur, sample_pct=100, min_pages=5, workers=1, loader=loader, metric="v1")
            self.assertFalse(ref.exists(quality.STATUS))

    def test_bad_metric(self):
        with tempfile.TemporaryDirectory() as d:
            ref, cur = Run.fixture(None, d)
            with self.assertRaises(ValueError):
                quality.quality(ref, cur, workers=1, metric="v3")

    def test_fake_models(self):
        m = FakeModels()
        self.assertEqual(m.get("eng").near.get("tbe"), "the")
        self.assertIsNone(m.get("haw"))
        got = quality.score_v2(" ".join(["die der und tbe"] * 3 + ["the of and to in is for that it on"] * 3),
                               ("eng",), m)
        self.assertEqual(got["lang"], "eng")


if __name__ == "__main__":
    unittest.main()
