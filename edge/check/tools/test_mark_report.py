"""Tests for mark_report.py: python3 -m unittest discover -s edge/check/tools. They need Pillow."""
import json
import os
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(__file__))

try:
    from PIL import Image  # noqa: F401
    import lookalike as la
    import mark_report as mr
    HAVE_PIL = True
except ImportError:
    HAVE_PIL = False

HERE = os.path.dirname(os.path.abspath(__file__))
AL, PATAH, QAMATS = "א", "ַ", "ָ"
PE, DAGESH, RAFE = "פ", "ּ", "ֿ"


def task(tid, a, b, counts, fonts=None):
    t = {"id": tid, "a": a, "b": b, "counts": dict({"a": 0, "b": 0, "neither": 0, "unsure": 0, "notext": 0}, **counts)}
    if fonts:
        t["font_counts"] = dict({"loaded": 0, "not_loaded": 0, "none": 0, "unknown": 0}, **fonts)
    return t


@unittest.skipUnless(HAVE_PIL, "Pillow not installed")
class Report(unittest.TestCase):
    def run_report(self, tasks, min_answers=20):
        return mr.report(tasks, la.Fonts(), os.path.join(HERE, "..", "languages"), la.THRESHOLD, min_answers)

    def tasks(self):
        out = []
        for i in range(5):  # patah/qamats: 5 words, 25 answers, 5 can't tell, each word 3 a + 1 b + 1 unsure
            out.append(task(f"yi/t/p{i}", AL + PATAH, AL + QAMATS, {"a": 3, "b": 1, "unsure": 1}, {"none": 5}))
        for i in range(2):  # dagesh/rafe: 2 words, 6 answers: too few
            out.append(task(f"yi/t/d{i}", PE + DAGESH, PE + RAFE, {"a": 3}, {"none": 3}))
        for i in range(10):  # different letters, no look-alike, no marks: 4 answers each, all agree
            out.append(task(f"yi/t/u{i}", "ר", "ד", {"b": 4}, {"loaded": 2, "none": 2}))
        return out

    def test_rates_per_mark_and_the_unflagged_baseline(self):
        rep = self.run_report(self.tasks())["yi"]
        row = rep["marks"]["U+05B7 patah"]["all"]
        self.assertEqual((row["words"], row["answers"], row["unsure"], row["cant_tell_rate"]), (5, 25, 5, 0.2))
        # per word: 5 answers, 10 pairs, agreeing pairs 3 a (3) + 1 b (0) + 1 unsure (0) = 3
        self.assertEqual((row["agree_pairs"], row["pairs"], row["agreement"]), (15, 50, 0.3))
        # a mark that differs is counted under each of the two letters' marks
        self.assertEqual(sorted(rep["marks"]), ["U+05B7 patah", "U+05B8 qamats", "U+05BC dagesh or mapiq", "U+05BF rafe"])
        base = rep["baseline"]["all"]
        self.assertEqual((base["words"], base["answers"], base["unsure"], base["agreement"]), (10, 40, 0, 1.0))

    def test_fewer_than_twenty_answers_print_too_few(self):
        rep = self.run_report(self.tasks())["yi"]
        self.assertEqual(rep["marks"]["U+05BC dagesh or mapiq"]["all"], {"words": 2, "answers": 6, "note": "too few answers"})
        self.assertNotIn("note", rep["marks"]["U+05B7 patah"]["all"])
        out = subprocess.run([sys.executable, os.path.join(HERE, "mark_report.py"), "--export", self.write(self.tasks())],
                             check=True, capture_output=True, text=True).stdout
        self.assertIn("too few answers", out)
        self.assertIn("U+05BC dagesh or mapiq", out)

    def test_split_by_font_counts(self):
        rep = self.run_report(self.tasks())["yi"]
        self.assertEqual(sorted(rep["marks"]["U+05B7 patah"]), ["all", "none"])
        self.assertEqual(sorted(rep["baseline"]), ["all", "mixed"])  # 2 loaded + 2 none per word: mixed
        no_font = [task("yi/t/x", AL + PATAH, AL, {"a": 25})]
        self.assertEqual(sorted(self.run_report(no_font)["yi"]["marks"]["U+05B7 patah"]), ["all", "unknown"])
        self.assertEqual(mr.font_group(task("a/b/c", "x", "y", {}, {"loaded": 4})), "loaded")

    def test_a_word_with_a_mark_pair_and_an_overlap_flag_is_in_neither_group(self):
        try:
            la.Fonts().bitmap(la.BUNDLED["CHEROKEE"], "Ꭰ")
        except OSError as e:
            self.skipTest(f"this Pillow can't read the bundled woff2: {e}")
        both = task("yi/t/z", "ᏒᎯ" + AL + PATAH, "ᎡᎯ" + AL + QAMATS, {"a": 30})
        # threshold 0.0 flags every measured pair, so the Cherokee pair is overlap-flagged beside the mark pair
        rep = mr.report([both], la.Fonts(), os.path.join(HERE, "..", "languages"), 0.0, 1)
        self.assertEqual(rep, {})

    def test_the_command_stops_when_pairs_could_not_be_checked(self):
        try:
            la.Fonts().bitmap(la.BUNDLED["CHEROKEE"], "Ꭰ")
        except OSError as e:
            self.skipTest(f"this Pillow can't read the bundled woff2: {e}")
        path = self.write([task("yi/t/1", "\u05e8", "\u05d3", {"a": 25})])
        # a reference font with no Hebrew glyph: the overlap check can't run, so every pair is unmeasured
        run = lambda *extra: subprocess.run([sys.executable, os.path.join(HERE, "mark_report.py"), "--export", path, "--font", "HEBREW=" + la.BUNDLED["CHEROKEE"], *extra],
                                            capture_output=True, text=True)
        r = run()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("no font or glyph: HEBREW 1", r.stderr)
        self.assertIn("--allow-incomplete", r.stderr)
        ok = run("--allow-incomplete")
        self.assertEqual(ok.returncode, 0)
        self.assertIn("WARNING", ok.stderr)

    def write(self, tasks):
        d = tempfile.mkdtemp()
        path = os.path.join(d, "export.jsonl")
        with open(path, "w", encoding="utf-8") as f:
            f.write("".join(json.dumps(t, ensure_ascii=False) + "\n" for t in tasks))
        return path


if __name__ == "__main__":
    unittest.main()
