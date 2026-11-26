"""Tests for names_report.py on a synthetic export: python3 -m unittest discover -s edge/check/tools."""
import json
import os
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(__file__))
import names_report as nr  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))


def task(tid, counts, names, a_is=None, **extra):
    t = {"id": tid, "a": "ᏒᎯ", "b": "ᎡᎯ", "counts": dict({"a": 0, "b": 0, "neither": 0, "unsure": 0, "notext": 0}, **counts)}
    if names is not None:
        t["names_counts"] = dict({"shown": 0, "not_shown": 0, "unknown": 0}, **names)
    if a_is:
        t["a_is"], t["b_is"] = a_is, ("loc" if a_is == "reference" else "reference")
    t.update(extra)
    return t


LOOK = [{"a": 0, "a_label": "sv", "b": 0, "b_label": "e"}]
MARK = [{"a": 0, "a_marks": "x", "b": 0, "b_marks": "", "base": "y"}]


class Report(unittest.TestCase):
    def tasks(self):
        out = []
        for i in range(5):  # shown: 5 words x 5 answers: 3 a, 1 b, 1 unsure; a is the reference; look-alike pairs
            out.append(task(f"chr/s/s{i}", {"a": 3, "b": 1, "unsure": 1}, {"shown": 5}, a_is="reference", lookalikes=LOOK))
        for i in range(5):  # not shown: 5 x 5 answers: 2 a, 2 b, 1 unsure; b is the reference
            out.append(task(f"chr/s/n{i}", {"a": 2, "b": 2, "unsure": 1}, {"not_shown": 5}, a_is="loc", lookalikes=LOOK))
        for i in range(5):  # mixed, with a mark-only pair
            out.append(task(f"chr/s/m{i}", {"a": 4, "unsure": 1}, {"shown": 2, "not_shown": 3}, mark_only=MARK))
        out.append(task("chr/s/old", {"a": 30}, {"unknown": 30}))  # answers from before the option
        out.append(task("chr/s/older", {"b": 25}, None))  # an export from before names_counts
        out.append(task("chr/s/few", {"a": 3}, {"shown": 3}))
        return out

    def test_a_bad_crop_word_is_left_out(self):
        bad = task("chr/s/bad", {"a": 1, "unsure": 1, "badcrop": 4}, {"shown": 6}, outcome="bad_crop", settled=True)
        self.assertEqual(nr.report(self.tasks() + [bad], 20), nr.report(self.tasks(), 20))

    def test_words_are_grouped_by_what_all_their_answers_carry(self):
        g = nr.names_group
        self.assertEqual(g({"names_counts": {"shown": 5, "not_shown": 0, "unknown": 0}}), "shown")
        self.assertEqual(g({"names_counts": {"shown": 0, "not_shown": 5, "unknown": 0}}), "not_shown")
        self.assertEqual(g({"names_counts": {"shown": 2, "not_shown": 3, "unknown": 0}}), "mixed")
        self.assertEqual(g({"names_counts": {"shown": 2, "not_shown": 0, "unknown": 3}}), "mixed")
        self.assertEqual(g({"names_counts": {"shown": 0, "not_shown": 0, "unknown": 4}}), "unknown")
        self.assertEqual(g({"names_counts": {"shown": 0, "not_shown": 0, "unknown": 0}}), "unknown", "a word with no answers")
        self.assertEqual(g({}), "unknown", "an export from before the option")

    def test_rates_per_group_overall_and_for_lookalike_and_mark_only_words(self):
        rep = nr.report(self.tasks(), 20)["chr"]
        shown = rep["all"]["shown"]
        self.assertEqual((shown["words"], shown["answers"], shown["unsure"], shown["cant_tell_rate"]), (6, 28, 5, 0.179))
        look = rep["lookalikes"]
        self.assertEqual(sorted(look), ["not_shown", "shown"])
        s = look["shown"]
        self.assertEqual((s["words"], s["answers"], s["unsure"], s["cant_tell_rate"]), (5, 25, 5, 0.2))
        # per word: 10 pairs, agreeing 3 a (3) + 1 b (0) + 1 unsure (0) = 3
        self.assertEqual((s["agree_pairs"], s["pairs"], s["agreement"]), (15, 50, 0.3))
        # a is the reference in every shown word: 15 of the 20 answers that picked a reading
        self.assertEqual((s["reference_picks"], s["reference_answers"], s["reference_agreement"]), (15, 20, 0.75))
        n = look["not_shown"]
        self.assertEqual((n["agree_pairs"], n["pairs"], n["cant_tell_rate"]), (5 * 2, 50, 0.2))  # 2 a (1) + 2 b (1) + 1 unsure (0) = 2 per word
        self.assertEqual((n["reference_picks"], n["reference_answers"]), (10, 20), "b is the reference when a_is is loc")
        m = rep["mark_only"]["mixed"]
        self.assertEqual((m["words"], m["answers"], m["unsure"]), (5, 25, 5))
        self.assertEqual(m["reference_agreement"], None, "no reference known for these words")
        self.assertEqual(sorted(rep["mark_only"]), ["mixed"])

    def test_old_answers_and_old_exports_are_unknown(self):
        unk = nr.report(self.tasks(), 20)["chr"]["all"]["unknown"]
        self.assertEqual((unk["words"], unk["answers"], unk["unsure"]), (2, 55, 0))

    def test_under_twenty_answers_print_too_few(self):
        rep = nr.report([task("chr/s/x", {"a": 3}, {"shown": 3}, lookalikes=LOOK)], 20)["chr"]
        self.assertEqual(rep["all"]["shown"], {"words": 1, "answers": 3, "note": "too few answers"})
        self.assertEqual(rep["lookalikes"]["shown"]["note"], "too few answers")
        self.assertEqual(nr.report([task("chr/s/x", {"a": 3}, {"shown": 3})], 3)["chr"]["all"]["shown"]["answers"], 3)

    def test_languages_are_kept_apart(self):
        rep = nr.report([task("chr/s/1", {"a": 25}, {"shown": 25}), task("sr/s/1", {"b": 25}, {"not_shown": 25})], 20)
        self.assertEqual(sorted(rep), ["chr", "sr"])
        self.assertEqual(list(rep["sr"]["all"]), ["not_shown"])

    def test_command_line(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "export.jsonl")
            with open(path, "w", encoding="utf-8") as f:
                f.write("\n".join(json.dumps(t, ensure_ascii=False) for t in self.tasks()) + "\n")
            run = lambda *extra: subprocess.run([sys.executable, os.path.join(HERE, "names_report.py"), "--export", path, *extra], check=True, capture_output=True, text=True).stdout
            out = run()
            self.assertIn("chr: words by whether letter names were showing", out)
            self.assertIn("too few answers (under 30)", run("--min-answers", "30"))
            self.assertIn("picked the reference 15 of 20 (75%)", out)
            self.assertEqual(json.loads(run("--json"))["chr"]["lookalikes"]["shown"]["words"], 5)
            self.assertIn("too few answers", run("--min-answers", "1000"))


if __name__ == "__main__":
    unittest.main()
