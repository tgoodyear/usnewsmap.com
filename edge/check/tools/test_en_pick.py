import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import en_pick  # noqa: E402
import en_pick_analysis as an  # noqa: E402

ALTO = """<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#"><Layout><Page WIDTH="1000" HEIGHT="500">
<PrintSpace><TextBlock><TextLine HPOS="0" VPOS="0" WIDTH="1000" HEIGHT="20">
{line1}
</TextLine><TextLine>{line2}</TextLine></TextBlock></PrintSpace></Page></Layout></alto>"""


def s(text, x, y):
    return f'<String CONTENT="{text}" HPOS="{x}" VPOS="{y}" WIDTH="60" HEIGHT="20"/><SP/>'


class Args:
    min_similarity, max_block, min_agree, skip_punctuation_only = 0.5, 3, 0.5, True


class Align(unittest.TestCase):
    def test_certain_pair_between_agreeing_words(self):
        pairs, c = en_pick.aligned_words("a b tbe c d".split(), "a b the c d".split(), 0.5, 3)
        self.assertEqual(pairs, [(2, 2)])

    def test_edges_splits_and_dissimilar_are_counted_not_paired(self):
        pairs, c = en_pick.aligned_words("tbe b c".split(), "the b c".split(), 0.5, 3)
        self.assertEqual((pairs, c["left_out_at_crop_edge"]), ([], 1))
        pairs, c = en_pick.aligned_words("a b cat dog e".split(), "a b catdog e".split(), 0.5, 3)
        self.assertEqual((pairs, c["left_out_split_joined_or_dropped"]), ([], 2))
        pairs, c = en_pick.aligned_words("a b xqz c d".split(), "a b the c d".split(), 0.5, 3)
        self.assertEqual((pairs, c["left_out_dissimilar"]), ([], 1))

    def test_offsetting_split_and_join_are_not_paired(self):
        a = "we visited New York today at noon".split()
        b = "we visited NewYork to day at noon".split()
        pairs, c = en_pick.aligned_words(a, b, 0.5, 3)
        self.assertEqual(pairs, [])
        self.assertEqual(c["left_out_dissimilar"] + c["left_out_split_joined_or_dropped"], 3)

    def test_one_word_per_distinct_pair_on_a_page_across_crops(self):
        page = {"page": "p", "date": "1900-01-01"}
        rows = [{"id": f"en/t/p_c{i}~1", "page": page, "loc": "tbe", "xiaomi": "the"} for i in range(3)]
        rows.append({"id": "en/t/p_c9~2", "page": page, "loc": "wltb", "xiaomi": "with"})
        got = en_pick.sample(rows, type("A", (), {"seed": "1", "per_page": 8, "per_stratum": 0})())
        self.assertEqual(sorted((r["loc"], r["xiaomi"]) for r in got), [("tbe", "the"), ("wltb", "with")])

    def test_truth_replacement_with_offsetting_split_and_join_is_unmapped(self):
        got = en_pick.truth_for("we visited New York today at noon".split(), "we visited NewYork to day at noon".split())
        self.assertNotIn(4, got)
        self.assertEqual(got[0], "we")

    def test_truth_side(self):
        self.assertEqual(en_pick.who_is_right("tbe", "the", "the"), "xiaomi")
        self.assertEqual(en_pick.who_is_right("the", "tbe", "the"), "loc")
        self.assertEqual(en_pick.who_is_right("tbe", "thc", "the"), "neither")

    def test_truth_for_maps_replacements(self):
        self.assertEqual(en_pick.truth_for("a tbe c".split(), "a the c".split()), {0: "a", 1: "the", 2: "c"})

    def test_side_is_fixed_by_seed_and_id_and_mixed(self):
        sides = [en_pick.side("1", f"id{i}") for i in range(200)]
        self.assertEqual(sides, [en_pick.side("1", f"id{i}") for i in range(200)])
        self.assertTrue(60 < sides.count("xiaomi") < 140)
        self.assertNotEqual(sides, [en_pick.side("2", f"id{i}") for i in range(200)])

    def test_bands(self):
        self.assertEqual([en_pick.band(x) for x in (None, 0.1, 0.2, 0.3)], ["unknown", "fine", "moderate", "bad"])

    def test_alto_words_in_crop_with_hyphen_and_scaling(self):
        xml = ALTO.format(line1=s("The", 0, 0) + s("quick", 100, 0) + '<HYP CONTENT="-"/>', line2=s("fox", 0, 100)).encode()
        words = en_pick.parse_alto(xml, (500, 250))  # half the ALTO's extent
        self.assertEqual([w["text"] for w in words], ["The", "quick-", "fox"])
        self.assertEqual(words[1]["box"], [50.0, 0.0, 30.0, 10.0])
        self.assertEqual([w["text"] for w in en_pick.in_crop(words, [0, 0, 200, 40])], ["The", "quick-"])


try:
    import PIL  # noqa: F401
except ImportError:
    PIL = None


@unittest.skipUnless(PIL, "needs Pillow")
class Build(unittest.TestCase):
    def test_tasks_are_blind_and_carry_truth(self):
        from PIL import Image
        with tempfile.TemporaryDirectory() as d:
            Image.new("L", (1000, 500), 255).save(os.path.join(d, "p.jpg"))
            words = "a b tbe c d e f g".split()
            line = "".join(s(w, 60 * i + 10, 10) for i, w in enumerate(words))
            open(os.path.join(d, "p.xml"), "w").write(ALTO.format(line1=line, line2=""))
            page = {"page": "sn1/1900-01-01/ed-1/seq-1", "source": "u", "date": "1900-01-01", "label": "L", "alto": "p.xml",
                    "image": "p.jpg", "damage_rate": 0.3, "crops": [
                        {"id": "c1", "box": [0, 0, 1000, 40], "xiaomi": "a b the c d e f g", "truth": "a b the c d e f g"}]}
            open(os.path.join(d, "pages.jsonl"), "w").write(json.dumps(page) + "\n")
            cfg = os.path.join(HERE, "..", "languages", "en.json")
            subprocess.run([sys.executable, os.path.join(HERE, "en_pick.py"), "--pages", os.path.join(d, "pages.jsonl"), "--set", "t",
                            "--language-config", cfg, "--out-dir", os.path.join(d, "out")], check=True, capture_output=True)
            (task,) = [json.loads(l) for l in open(os.path.join(d, "out", "tasks.jsonl"))]
            self.assertEqual(sorted([task["a"], task["b"]]), ["tbe", "the"])
            self.assertNotIn("a_is", task)
            p = task["private"]
            self.assertEqual((p["truth"], p["strata"]), ("xiaomi", {"decade": 1900, "damage_band": "bad"}))
            self.assertEqual({p["a_is"], p["b_is"]}, {"xiaomi", "loc"})
            self.assertEqual(task["id"][:5], "en/t/")
            self.assertTrue(os.path.exists(os.path.join(d, "out", "clips", task["word"])))
            self.assertTrue(os.path.exists(os.path.join(d, "out", "clips", task["line"])))
            self.assertFalse(any(k in json.load(open(os.path.join(d, "out", "language.json"))) for k in ("a_is", "private")))


class Analysis(unittest.TestCase):
    def rows(self):
        out = []
        for i in range(40):
            xi_a = i % 2 == 0
            out.append({"id": f"t{i}", "set": "s", "page": f"p{i % 8}", "settled": True, "outcome": "a" if (i % 5) else "b",
                        "a_is": "xiaomi" if xi_a else "loc", "b_is": "loc" if xi_a else "xiaomi",
                        "strata": {"decade": 1890 + 10 * (i % 2), "damage_band": "bad"}, "truth": "xiaomi"})
        return out

    def test_verdict_maps_side_to_reader(self):
        self.assertEqual(an.verdict({"settled": True, "outcome": "b", "a_is": "loc", "b_is": "xiaomi"}), "xiaomi")
        self.assertEqual(an.verdict({"settled": True, "outcome": "neither"}), "neither")
        self.assertIsNone(an.verdict({"settled": False, "outcome": None}))

    def test_truth_rows_are_not_pilot_rows(self):
        rows = self.rows()
        self.assertTrue(all(r.get("truth") for r in rows))  # the fixture rows are all calibration items

    def test_rate_and_bootstrap(self):
        rows = self.rows()
        rate, x, l = an.win_rate(rows)
        self.assertEqual(x + l, 40)
        lo, hi = an.bootstrap(rows, 300, "1")
        self.assertTrue(0 <= lo <= rate <= hi <= 1)
        self.assertEqual(an.bootstrap(rows, 300, "1"), an.bootstrap(rows, 300, "1"))


if __name__ == "__main__":
    unittest.main()
