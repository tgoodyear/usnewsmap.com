"""Tests for merged_lines.py and its use in the line tools: python3 -m unittest discover -s edge/check/tools.
The tool runs need Pillow (and skip without it)."""
import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import merged_lines as ml  # noqa: E402
import serbian_lines as sl  # noqa: E402

try:
    from PIL import Image
except ImportError:  # the tool runs below need it
    Image = None


def lines_of(heights):
    return [{"id": f"L{i}", "box": [0, 100 * i, 500, h]} for i, h in enumerate(heights)]


class Rule(unittest.TestCase):
    def test_a_line_over_the_factor_is_flagged_and_exactly_at_it_is_not(self):
        median, flagged = ml.flag_page(lines_of([50, 50, 50, 50, 50, 81, 80]))
        self.assertEqual(median, 50)
        self.assertEqual([f["id"] for f in flagged], ["L5"])  # 81 > 1.6 * 50, 80 is not
        self.assertEqual(flagged[0]["ratio"], 1.62)

    def test_the_median_is_over_every_line_so_a_few_merged_lines_do_not_move_it(self):
        median, flagged = ml.flag_page(lines_of([40, 40, 40, 40, 100, 100]))
        self.assertEqual(median, 40)
        self.assertEqual(len(flagged), 2)

    def test_a_page_with_few_lines_is_never_flagged(self):
        self.assertEqual(ml.flag_page(lines_of([50, 50, 50, 400])), (None, []))  # 4 lines: no median worth using
        self.assertEqual(ml.flag_page(lines_of([50, 50, 50, 50, 400]))[0], 50)  # 5 lines: checked
        self.assertEqual(len(ml.flag_page(lines_of([50, 50, 50, 50, 400]))[1]), 1)
        self.assertEqual(ml.flag_page(lines_of([50, 50, 50, 400]), min_lines=4)[1][0]["id"], "L3")

    def test_zero_heights_do_not_count_as_lines(self):
        self.assertIsNone(ml.page_median([50, 50, 50, 50, 0, 0, None]))

    def test_the_factor_is_a_parameter(self):
        _, flagged = ml.flag_page(lines_of([50, 50, 50, 50, 50, 70]), factor=1.2)
        self.assertEqual([f["id"] for f in flagged], ["L5"])
        self.assertEqual(ml.flag_page(lines_of([50, 50, 50, 50, 50, 70]))[1], [])

    def test_report_counts_per_page_and_in_total_and_writes_the_lines(self):
        r = ml.Report()
        self.assertEqual(r.page("p1", lines_of([50] * 5 + [120])), {"L5"})
        self.assertEqual(r.page("p2", lines_of([50] * 6)), set())
        self.assertEqual(r.page("p3", lines_of([50, 400])), set())
        s = r.summary()
        self.assertEqual((s["pages_checked"], s["pages_too_few_lines"], s["merged_lines"], s["merged_lines_per_page"]), (3, 1, 1, {"p1": 1}))
        with tempfile.TemporaryDirectory() as d:
            r.write(os.path.join(d, "m.jsonl"))
            rows = [json.loads(x) for x in open(os.path.join(d, "m.jsonl"), encoding="utf-8")]
        self.assertEqual([(x["page"], x["id"], x["height"], x["median"], x["factor"]) for x in rows], [("p1", "L5", 120, 50, 1.6)])


class Serbian(unittest.TestCase):
    def test_candidates_skip_merged_lines(self):
        lines = [{"words": [{"text": t} for t in ("седница", "одржати", "у")]} for _ in range(3)]
        self.assertEqual(sl.candidates(lines, 3), [0, 1, 2])
        self.assertEqual(sl.candidates(lines, 3, {1}), [0, 2])


def alto(width, height, lines, ids=False):
    out = [f'<alto xmlns="http://www.loc.gov/standards/alto/ns-v2#"><Layout><Page WIDTH="{width}" HEIGHT="{height}"><PrintSpace><TextBlock>']
    for i, ((x, y, w, h), words) in enumerate(lines):
        out.append(f'<TextLine ID="P1_TL{i + 1:05d}" HPOS="{x}" VPOS="{y}" WIDTH="{w}" HEIGHT="{h}">')
        for text, (wx, wy, ww, wh) in words:
            out.append(f'<String CONTENT="{text}" HPOS="{wx}" VPOS="{wy}" WIDTH="{ww}" HEIGHT="{wh}" WC="0.99"/>')
        out.append("</TextLine>")
    out.append("</TextBlock></PrintSpace></Page></Layout></alto>")
    return "".join(out)


def run(args, env=None):
    r = subprocess.run([sys.executable, "-I"] + args,  # -I as the Containerfile runs the tools: the script directory is not on the path
                        capture_output=True, text=True, env=env, cwd=HERE)
    if r.returncode:
        raise AssertionError(r.stdout + r.stderr)
    return r


@unittest.skipIf(Image is None, "Pillow not installed")
class Tools(unittest.TestCase):
    def test_phoenix_lines_leaves_a_merged_line_out_and_reports_it(self):
        with tempfile.TemporaryDirectory() as d:
            loc_dir = os.path.join(d, "loc", "sn1", "1828", "03", "06", "ed-1", "seq-1")
            prod_dir = os.path.join(d, "prod")
            os.makedirs(loc_dir)
            os.makedirs(prod_dir)
            words_loc = [("ᎠᎴ", (400, 0, 300, 100)), ("ᎣᏂᏃ", (800, 0, 300, 100)), ("ᎡᎯ", (1200, 0, 300, 100))]
            heights = [160, 160, 400, 160, 160, 160]  # the third TextLine covers two printed lines
            loc = [((400, 800 + 400 * i, 2000, h), [(t, (bx, 800 + 400 * i, bw, bh)) for t, (bx, _, bw, bh) in words_loc]) for i, h in enumerate(heights)]
            open(os.path.join(loc_dir, "ocr.xml"), "w", encoding="utf-8").write(alto(4000, 8000, loc))
            prod = [((100, 200 + 100 * i, 500, 40), [("ᎠᎴ ᎣᏂᏗ ᎡᎯ", (100, 200 + 100 * i, 500, 40))]) for i in range(6)]
            open(os.path.join(prod_dir, "sn1_1828-03-06_ed-1_seq-1.xml"), "w", encoding="utf-8").write(alto(1000, 2000, prod))
            pages = os.path.join(d, "pages.jsonl")
            open(pages, "w").write(json.dumps({"page": "sn1/1828-03-06/ed-1/seq-1", "lines": 6, "test_set": True}) + "\n")
            tests = os.path.join(d, "test-lines.jsonl")
            open(tests, "w").write("")
            out = os.path.join(d, "out")
            r = run(["phoenix_lines.py", "disputes", "--prod-alto", prod_dir, "--loc-alto", os.path.join(d, "loc"), "--pages", pages,
                     "--test-lines", tests, "--no-images", "--out-dir", out])
            made = [json.loads(x)["id"] for x in open(os.path.join(out, "disputes", "lines.jsonl"), encoding="utf-8")]
            self.assertEqual(len(made), 5)
            self.assertNotIn("sn1_1828-03-06_ed-1_seq-1_P1_TL00003", made)
            rows = [json.loads(x) for x in open(os.path.join(out, "disputes", "merged-lines.jsonl"), encoding="utf-8")]
            self.assertEqual([(x["page"], x["id"], x["ratio"]) for x in rows], [("sn1/1828-03-06/ed-1/seq-1", "P1_TL00003", 2.5)])
            report = json.load(open(os.path.join(out, "disputes", "report.json")))
            self.assertEqual(report["merged"]["merged_lines"], 1)
            self.assertEqual(report["merged"]["merged_lines_per_page"], {"sn1/1828-03-06/ed-1/seq-1": 1})
            self.assertIn("1 merged lines left out", r.stderr)
            # the same page with a looser factor keeps every line
            out2 = os.path.join(d, "out2")
            run(["phoenix_lines.py", "disputes", "--prod-alto", prod_dir, "--loc-alto", os.path.join(d, "loc"), "--pages", pages,
                 "--test-lines", tests, "--no-images", "--merged-line-factor", "3", "--out-dir", out2])
            self.assertEqual(len(open(os.path.join(out2, "disputes", "lines.jsonl")).read().splitlines()), 6)
            self.assertEqual(open(os.path.join(out2, "disputes", "merged-lines.jsonl")).read(), "")

    def cached(self, work, doc, xml):
        os.makedirs(os.path.join(work, "loc"))
        base = os.path.join(work, "loc", doc)
        open(base + ".xml", "w", encoding="utf-8").write(xml)
        # fetch() returns the cached copy and its recorded URL without a request
        open(base + ".xml.url", "w").write("https://tile.loc.gov/storage-services/service/ndnp/x/batch/data/" + doc + ".xml")
        Image.new("L", (1000, 1000), 255).save(base + ".jpg")
        open(base + ".jpg.url", "w").write("https://tile.loc.gov/image-services/x")

    def test_yiddish_lines_leaves_a_merged_line_out_of_the_picks(self):
        with tempfile.TemporaryDirectory() as d:
            work = os.path.join(d, "work")
            doc = "sn1_1913-09-01_ed-1_seq-3"
            word = ("שלום", (10, 0, 100, 40))
            ys_h = [(0, 60), (100, 150), (300, 60), (400, 60), (500, 60), (600, 60)]  # the second line is far taller than the rest
            lines = [((0, y, 800, h), [(word[0], (10, y, 100, 40))]) for y, h in ys_h]
            self.cached(work, doc, alto(1000, 1000, lines))
            manifest = os.path.join(d, "manifest.json")
            json.dump({"titles": [{"lccn": "sn1", "title": "Test"}],
                       "pages": [{"role": "selected", "lccn": "sn1", "date": "1913-09-01", "edition": 1, "seq": 3, "doc_id": doc,
                                  "width": 1000, "height": 1000, "anchors": [[20, 20]]}]}, open(manifest, "w"))
            bin_dir = os.path.join(d, "bin")
            os.makedirs(bin_dir)
            tess = os.path.join(bin_dir, "tesseract")
            open(tess, "w").write("#!/bin/sh\necho 'שלום'\n")
            os.chmod(tess, os.stat(tess).st_mode | stat.S_IEXEC)
            env = {**os.environ, "PATH": bin_dir + os.pathsep + os.environ["PATH"]}
            r = run(["yiddish_lines.py", "--manifest", manifest, "--work", work, "--tessdata", d, "--model", "yidj2"], env)
            ids = [json.loads(x)["id"] for x in open(os.path.join(work, "lines.jsonl"), encoding="utf-8")]
            self.assertEqual(ids, [doc + "_l0", doc + "_l2"])
            rows = [json.loads(x) for x in open(os.path.join(work, "merged-lines.jsonl"), encoding="utf-8")]
            self.assertEqual([x["id"] for x in rows], [doc + "_l1"])
            self.assertIn("1 merged lines left out", r.stdout)

    def test_serbian_lines_leaves_a_merged_line_out(self):
        with tempfile.TemporaryDirectory() as d:
            work = os.path.join(d, "work")
            doc = "sn2_1933-04-13_ed-1_seq-4"
            word = ("седница", (10, 0, 100, 40))
            ys_h = [(0, 60), (100, 150), (300, 60), (400, 60), (500, 60), (600, 60)]
            lines = [((0, y, 800, h), [("седница", (10, y, 100, 40)), ("одржати", (130, y, 100, 40)), ("у", (250, y, 40, 40))]) for y, h in ys_h]
            self.cached(work, doc, alto(1000, 1000, lines))
            ours = os.path.join(d, "ours")
            os.makedirs(ours)
            ours_lines = [((0, y, 800, h), [("седница", (10, y, 100, 40)), ("одржати", (130, y, 100, 40)), ("у", (250, y, 40, 40))]) for y, h in ys_h]
            open(os.path.join(ours, doc + ".xml"), "w", encoding="utf-8").write(alto(1000, 1000, ours_lines))
            pages = os.path.join(d, "pages.jsonl")
            open(pages, "w").write(json.dumps({"doc_id": doc, "page": "sn2/1933-04-13/ed-1/seq-4", "alto": "https://tile.loc.gov/x.xml", "title": "Test"}) + "\n")
            r = run(["serbian_lines.py", "--pages", pages, "--ours", ours, "--work", work, "--lines-per-page", "10"])
            ids = {json.loads(x)["id"] for x in open(os.path.join(work, "lines.jsonl"), encoding="utf-8")}
            self.assertEqual(ids, {f"{doc}_l{i}" for i in (0, 2, 3, 4, 5)})
            rows = [json.loads(x) for x in open(os.path.join(work, "merged-lines.jsonl"), encoding="utf-8")]
            self.assertEqual([x["id"] for x in rows], [doc + "_l1"])
            self.assertIn("1 merged lines left out", r.stdout)

    def test_build_tasks_leaves_out_a_row_marked_merged_line(self):
        with tempfile.TemporaryDirectory() as d:
            Image.new("L", (400, 60), 255).save(os.path.join(d, "l.png"))
            row = {"id": "l1", "image": "l.png", "page": "p/1900-01-01/ed-1/seq-1", "source": "s", "label": "x",
                   "a": "שלום עולם", "a_words": [{"text": "שלום", "box": [10, 5, 100, 40]}, {"text": "עולם", "box": [150, 5, 100, 40]}], "b": "שלום עולכ"}
            lines = os.path.join(d, "lines.jsonl")
            open(lines, "w", encoding="utf-8").write(json.dumps(row, ensure_ascii=False) + "\n" + json.dumps({**row, "id": "l2", "merged_line": True}, ensure_ascii=False) + "\n")
            cfg = os.path.join(d, "cfg.json")
            json.dump({"name": "Test"}, open(cfg, "w"))
            r = run(["build_tasks.py", "--lines", lines, "--images", d, "--lang", "yi", "--set", "s", "--a-is", "loc", "--b-is", "m",
                     "--language-config", cfg, "--out-dir", os.path.join(d, "out")])
            ids = [json.loads(x)["id"] for x in open(os.path.join(d, "out", "tasks.jsonl"), encoding="utf-8")]
            self.assertTrue(all("/l1~" in i for i in ids) and ids, ids)
            self.assertIn("1 lines marked merged_line left out", r.stdout)


if __name__ == "__main__":
    unittest.main()
