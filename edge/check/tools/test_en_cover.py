import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import en_cover  # noqa: E402


def string(text, x, y, w=60, h=20):
    return f'<String CONTENT="{text}" HPOS="{x}" VPOS="{y}" WIDTH="{w}" HEIGHT="{h}"/><SP/>'


def line(words, x0, y):
    return "<TextLine>" + "".join(string(t, x0 + 70 * i, y) for i, t in enumerate(words)) + "</TextLine>"


def alto(blocks, width=1000, height=1000):
    body = "".join("<TextBlock>" + "".join(b) + "</TextBlock>" for b in blocks)
    return (f'<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#"><Layout><Page WIDTH="{width}" HEIGHT="{height}">'
            f"<PrintSpace>{body}</PrintSpace></Page></Layout></alto>").encode()


WORDS = "the quick brown fox jumps over".split()


class Args:
    lang, set, seed = "en-cover", "cover-v1", "1"
    max_lines, min_words, max_overlap, low, max_low, per_page = 8, 6, 0.6, 0.5, 24, 16


def two_columns(ours_right=True, scale=1):
    """LoC: two columns of 3 lines. Ours: the left column, and the right one unless ours_right is False."""
    left = [line(WORDS, 0, 100 + 30 * i) for i in range(3)]
    right = [line(WORDS, 500, 100 + 30 * i) for i in range(3)]
    loc = alto([left, right])
    ours_blocks = [[line(WORDS, 0, 100 + 30 * i) for i in range(3)]]
    if ours_right:
        ours_blocks.append([line(WORDS, 500, 100 + 30 * i) for i in range(3)])
    # Our ALTO may be in other units than LoC's: the page extent maps both onto the image.
    ours = alto(ours_blocks)
    if scale != 1:
        import re
        ours = re.sub(rb'(HPOS|VPOS|WIDTH|HEIGHT)="(\d+)"', lambda m: m.group(1) + b'="' + str(int(m.group(2)) * scale).encode() + b'"', ours)
    return loc, ours


class Chunks(unittest.TestCase):
    def run_page(self, loc, ours, args=Args):
        from collections import Counter
        stats = Counter()
        cands = en_cover.chunks({"page": "sn1/1876-10-08/ed-1/seq-7"}, en_cover.blocks(loc, (2000, 2000)),
                                en_cover.words(ours, (2000, 2000)), args, stats)
        return cands, stats

    def test_a_missed_column_has_ratio_zero_and_the_other_full(self):
        cands, _ = self.run_page(*two_columns(ours_right=False))
        self.assertEqual([c["ratio"] for c in cands], [1.0, 0.0])
        self.assertEqual(cands[1]["text"], "")
        self.assertEqual(cands[0]["text"], "\n".join([" ".join(WORDS)] * 3))

    def test_our_alto_in_other_units_maps_by_page_extent(self):
        cands, _ = self.run_page(*two_columns(scale=2))
        self.assertEqual([c["ratio"] for c in cands], [1.0, 1.0])

    def test_long_blocks_are_cut_and_short_or_stacked_chunks_left_out(self):
        tall = [line(WORDS, 0, 30 * i) for i in range(10)]
        stacked = [line(WORDS, 0, 30 * i) for i in range(8)]  # LoC's second reading of the same region
        short = [line(["a", "b"], 500, 900)]
        cands, stats = self.run_page(alto([tall, stacked, short]), alto([tall]))
        self.assertEqual([c["id"].split("~")[1] for c in cands], ["b0c0", "b0c1"])
        self.assertEqual((stats["left_out_overlap"], stats["left_out_few_words"]), (1, 1))

    def test_our_alto_with_block_geometry_is_refused(self):
        _, ours = two_columns()
        for g, refused in (("block", True), ("line", True), ("page", True), ("word", False)):
            xml = ours.replace(b"<Layout>", f"<Description><Processing><processingStep><processingStepSettings>ocr_engine=x\ngranularity={g}</processingStepSettings></processingStep></Processing></Description><Layout>".encode())
            if refused:
                with self.assertRaises(en_cover.NotWordGeometry):
                    en_cover.words(xml, (2000, 2000))
            else:
                self.assertEqual(len(en_cover.words(xml, (2000, 2000))), 36)

    def test_pick_keeps_every_low_chunk_and_fills_with_others(self):
        cands = [{"id": f"x~b{i}c0", "ratio": 0.1 if i < 3 else 1.0} for i in range(10)]

        class A(Args):
            per_page = 5
        chosen = en_cover.pick(cands, A)
        self.assertEqual(sum(c["ratio"] < 0.5 for c in chosen), 3)
        self.assertEqual(len(chosen), 5)
        self.assertEqual(chosen, en_cover.pick(cands, A))  # the draw is seeded


class Build(unittest.TestCase):
    def test_writes_cover_tasks_with_strata_kept_private(self):
        from PIL import Image
        loc, ours = two_columns(ours_right=False)
        with tempfile.TemporaryDirectory() as d:
            Image.new("L", (2000, 2000), 255).save(os.path.join(d, "p.jpg"))
            open(os.path.join(d, "loc.xml"), "wb").write(loc)
            open(os.path.join(d, "ours.xml"), "wb").write(ours)
            open(os.path.join(d, "en.json"), "w").write(json.dumps({"name": "English", "intro": "x", "min_examples": 3}))
            page = {"page": "sn1/1876-10-08/ed-1/seq-7", "date": "1876-10-08", "image": "p.jpg", "loc_alto": "loc.xml",
                    "ours_alto": "ours.xml", "seed": "known-miss", "source": "https://example.org/p"}
            open(os.path.join(d, "pages.jsonl"), "w").write(json.dumps(page) + "\n")
            out = os.path.join(d, "out")
            subprocess.run([sys.executable, os.path.join(HERE, "en_cover.py"), "--pages", os.path.join(d, "pages.jsonl"),
                            "--set", "cover-v1", "--language-config", os.path.join(d, "en.json"), "--out-dir", out],
                           check=True, capture_output=True)
            tasks = [json.loads(l) for l in open(os.path.join(out, "tasks.jsonl"))]
            self.assertEqual(len(tasks), 2)
            t = tasks[1]
            self.assertEqual((t["kind"], t["text"], t["id"]), ("cover", "", "en-cover/cover-v1/sn1/1876-10-08/ed-1/seq-7~b1c0"))
            self.assertEqual(t["private"]["strata"], {"decade": 1870})
            self.assertEqual(t["private"]["measure"]["ratio_band"], "none")
            self.assertEqual(t["private"]["measure"]["seed"], "known-miss")
            self.assertNotIn("ratio", {k for k in t if k != "private"})
            for key in (t["block"], t["pageview"]):
                self.assertTrue(os.path.exists(os.path.join(out, "clips", key)), key)
            self.assertTrue(all(0 <= v <= 1 for v in t["box"]))
            lang = json.load(open(os.path.join(out, "language.json")))
            self.assertNotIn("min_examples", lang)
            report = json.load(open(os.path.join(out, "report.json")))
            self.assertEqual(report["tasks_by_ratio_band"], {"full": 1, "none": 1})

    def test_a_chunk_with_almost_nothing_of_ours_and_many_loc_words_is_labelled_by_rule(self):
        from PIL import Image
        many = [f"w{i}" for i in range(20)]
        loc = alto([[line(many[:10], 0, 100), line(many[10:], 0, 130)], [line(WORDS, 500, 100), line(WORDS, 500, 130)]])
        ours = alto([[line(["w1", "w2"], 0, 100)], [line(WORDS, 500, 100), line(WORDS, 500, 130)]])
        with tempfile.TemporaryDirectory() as d:
            Image.new("L", (2000, 2000), 255).save(os.path.join(d, "p.jpg"))
            open(os.path.join(d, "loc.xml"), "wb").write(loc)
            open(os.path.join(d, "ours.xml"), "wb").write(ours)
            open(os.path.join(d, "en.json"), "w").write(json.dumps({"name": "English", "intro": "x"}))
            page = {"page": "sn1/1876-10-08/ed-1/seq-7", "date": "1876-10-08", "image": "p.jpg", "loc_alto": "loc.xml",
                    "ours_alto": "ours.xml", "seed": "known-miss"}
            open(os.path.join(d, "pages.jsonl"), "w").write(json.dumps(page) + "\n")
            out = os.path.join(d, "out")
            subprocess.run([sys.executable, os.path.join(HERE, "en_cover.py"), "--pages", os.path.join(d, "pages.jsonl"),
                            "--set", "cover-v1", "--language-config", os.path.join(d, "en.json"), "--out-dir", out,
                            "--clip-tag", "fr1"], check=True, capture_output=True)
            tasks = [json.loads(l) for l in open(os.path.join(out, "tasks.jsonl"))]
            self.assertEqual([t["id"].rsplit("~", 1)[1] for t in tasks], ["b1c0"])
            self.assertIn("/blocks-fr1/", tasks[0]["block"])
            self.assertIn("/pages-fr1/", tasks[0]["pageview"])
            ruled = [json.loads(l) for l in open(os.path.join(out, "rule_labelled.jsonl"))]
            self.assertEqual([(r["id"].rsplit("~", 1)[1], r["outcome"], r["reason"]) for r in ruled],
                             [("b0c0", "missing", "ours<=3 words, loc>=20")])
            self.assertFalse(os.path.exists(os.path.join(out, "clips", "en-cover/cover-v1/blocks-fr1/sn1_1876-10-08_ed-1_seq-7_b0c0.jpg")))
            only = os.path.join(d, "only.txt")
            open(only, "w").write(tasks[0]["id"] + "\n" + ruled[0]["id"] + "\n")
            subprocess.run([sys.executable, os.path.join(HERE, "en_cover.py"), "--pages", os.path.join(d, "pages.jsonl"),
                            "--set", "cover-v1", "--language-config", os.path.join(d, "en.json"), "--out-dir", out + "2",
                            "--only-ids", only], check=True, capture_output=True)
            again = [json.loads(l) for l in open(os.path.join(out + "2", "tasks.jsonl"))]
            self.assertEqual([t["id"] for t in again], [tasks[0]["id"]])

    def test_a_wide_block_clip_keeps_the_scans_pixels(self):
        from PIL import Image
        row = [f"w{i}" for i in range(30)]
        both = alto([[line(row, 0, 100), line(row, 0, 130)]], width=4000, height=1000)
        with tempfile.TemporaryDirectory() as d:
            Image.new("L", (4000, 1000), 255).save(os.path.join(d, "p.jpg"))
            open(os.path.join(d, "loc.xml"), "wb").write(both)
            open(os.path.join(d, "ours.xml"), "wb").write(both)
            open(os.path.join(d, "en.json"), "w").write(json.dumps({"name": "English", "intro": "x"}))
            page = {"page": "sn1/1876-10-08/ed-1/seq-7", "date": "1876-10-08", "image": "p.jpg", "loc_alto": "loc.xml",
                    "ours_alto": "ours.xml"}
            open(os.path.join(d, "pages.jsonl"), "w").write(json.dumps(page) + "\n")
            out = os.path.join(d, "out")
            subprocess.run([sys.executable, os.path.join(HERE, "en_cover.py"), "--pages", os.path.join(d, "pages.jsonl"),
                            "--set", "cover-v1", "--language-config", os.path.join(d, "en.json"), "--out-dir", out],
                           check=True, capture_output=True)
            t = json.loads(open(os.path.join(out, "tasks.jsonl")).readline())
            x0, y0, x1, y1 = t["block_box"]
            self.assertGreater(x1 - x0, 1200)
            self.assertGreater(Image.open(os.path.join(out, "clips", t["block"])).size[0], x1 - x0)


if __name__ == "__main__":
    unittest.main()
