import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import gutter_analysis as an  # noqa: E402
import gutter_tasks as gt  # noqa: E402

LANGUAGE = os.path.join(HERE, "..", "languages", "en-gutter.json")


class Geometry(unittest.TestCase):
    def test_margin_is_one_and_a_half_columns_when_known(self):
        box = gt.crop_box(1000, 500, 1500, 3000, 4000, column_width=200)
        self.assertEqual(box, (700, 380, 1301, 1621))

    def test_margin_is_twelve_percent_of_the_page_width_otherwise(self):
        box = gt.crop_box(1000, 500, 1500, 3000, 4000)
        self.assertEqual(box, (640, 380, 1361, 1621))

    def test_crop_stays_inside_the_page(self):
        self.assertEqual(gt.crop_box(10, 5, 3990, 3000, 4000, column_width=200), (0, 0, 311, 4000))
        self.assertEqual(gt.crop_box(2990, 5, 3990, 3000, 4000), (2630, 0, 3000, 4000))

    def test_line_fractions_are_of_the_crop(self):
        box = (700, 380, 1300, 1620)
        f = gt.line_fractions(1000, 500, 1500, box)
        self.assertEqual(f, {"x": 0.5, "y0": round(120 / 1240, 5), "y1": round(1120 / 1240, 5)})

    def test_slugs_have_only_characters_a_clip_key_allows(self):
        key = gt.clip_key("en-gutter", "s1", {"page_id": "sn83030213/1910-01-02/ed-1/seq-1", "segment_id": "c 2/x~y"})
        self.assertEqual(key, "en-gutter/s1/crops/sn83030213_1910-01-02_ed-1_seq-1_sc_2_x_y.jpg")
        self.assertRegex(key, r"^[A-Za-z0-9._\-/]+$")


class Validation(unittest.TestCase):
    ROW = {"segment_id": "s1", "page_id": "p", "image_path": "p.png", "x": 100, "y0": 10, "y1": 90, "method": "m", "role": "r"}

    def test_a_good_row_passes(self):
        gt.validate(self.ROW, (200, 100), 1)
        gt.validate({**self.ROW, "truth": "no", "column_width": 40}, (200, 100), 1)

    def test_bad_rows_are_refused(self):
        for bad in ({"truth": "maybe"}, {"x": 201}, {"y1": 101}, {"y0": 90}, {"column_width": 0}, {"method": ""}, {"x": "5"}, {"role": None}):
            with self.assertRaises(ValueError, msg=str(bad)):
                gt.validate({**self.ROW, **bad}, (200, 100), 1)


class Task(unittest.TestCase):
    def test_task_body_keeps_method_and_role_private(self):
        row = {"segment_id": "s1", "page_id": "sn1/1910-01-02/ed-1/seq-1", "x": 1000, "y0": 500, "y1": 1500, "method": "projection",
               "role": "pilot", "truth": "yes", "source": "https://example.test/page"}
        task, box = gt.build_task(row, "en-gutter", "g1", (3000, 4000))
        self.assertEqual(task["id"], "en-gutter/g1/sn1/1910-01-02/ed-1/seq-1~s1")
        self.assertEqual((task["kind"], task["lang"], task["set"]), ("gutter", "en-gutter", "g1"))
        self.assertEqual(task["private"], {"role": "pilot", "segment_id": "s1", "page_id": row["page_id"], "method": "projection", "truth": "yes"})
        public = {k: v for k, v in task.items() if k != "private"}
        for secret in ("projection", "pilot", "truth"):
            self.assertNotIn(secret, json.dumps(public))
        self.assertEqual(set(task["line"]), {"x", "y0", "y1"})
        self.assertEqual(box, (640, 380, 1361, 1621))

    def test_no_truth_means_no_truth_key(self):
        task, _ = gt.build_task({"segment_id": "s", "page_id": "p", "x": 5, "y0": 1, "y1": 9, "method": "m", "role": "r"}, "en-gutter", "g", (10, 10))
        self.assertNotIn("truth", task["private"])


@unittest.skipUnless(__import__("importlib").util.find_spec("PIL"), "needs Pillow")
class EndToEnd(unittest.TestCase):
    def test_builds_crops_tasks_and_language(self):
        from PIL import Image
        with tempfile.TemporaryDirectory() as d:
            Image.new("L", (3000, 4000), 200).save(os.path.join(d, "p.png"))
            rows = [{"segment_id": "s2", "page_id": "pg/1", "image_path": "p.png", "x": 1500, "y0": 200, "y1": 3800, "method": "m1", "role": "pilot"},
                    {"segment_id": "s1", "page_id": "pg/1", "image_path": "p.png", "x": 900, "y0": 400, "y1": 1000, "method": "m2", "role": "gold",
                     "truth": "no", "column_width": 300, "source": "https://example.test/p"}]
            with open(os.path.join(d, "segments.jsonl"), "w") as f:
                f.write("\n".join(json.dumps(r) for r in rows) + "\n")
            out = os.path.join(d, "out")
            gt.main(["--segments", os.path.join(d, "segments.jsonl"), "--set", "g1", "--language-config", LANGUAGE, "--out-dir", out, "--max-side", "1000"])
            tasks = [json.loads(l) for l in open(os.path.join(out, "tasks.jsonl"))]
            self.assertEqual(sorted(t["id"] for t in tasks), ["en-gutter/g1/pg/1~s1", "en-gutter/g1/pg/1~s2"])
            for t in tasks:
                with Image.open(os.path.join(out, "clips", t["crop"])) as im:
                    self.assertLessEqual(max(im.size), 1000)
                    self.assertEqual(im.format, "JPEG")
            lang = json.load(open(os.path.join(out, "language.json")))
            self.assertEqual(lang["examples"], {})
            self.assertIn("question", lang)
            report = json.load(open(os.path.join(out, "report.json")))
            self.assertEqual((report["segments"], report["with_truth"], report["by_method"]), (2, 1, {"m1": 1, "m2": 1}))

    def test_duplicate_segments_are_refused(self):
        from PIL import Image
        with tempfile.TemporaryDirectory() as d:
            Image.new("L", (100, 100), 200).save(os.path.join(d, "p.png"))
            r = {"segment_id": "s1", "page_id": "p", "image_path": "p.png", "x": 50, "y0": 10, "y1": 90, "method": "m", "role": "r"}
            with open(os.path.join(d, "s.jsonl"), "w") as f:
                f.write(json.dumps(r) + "\n" + json.dumps(r) + "\n")
            with self.assertRaises(SystemExit):
                gt.main(["--segments", os.path.join(d, "s.jsonl"), "--set", "g", "--language-config", LANGUAGE, "--out-dir", os.path.join(d, "o")])

    def test_takes_no_positional_arguments(self):
        p = subprocess.run([sys.executable, "-I", os.path.join(HERE, "gutter_tasks.py"), "segments.jsonl"], capture_output=True, text=True)
        self.assertNotEqual(p.returncode, 0)


def export_row(i, outcome, settled=True, **extra):
    n = {"a": 0, "b": 0, "neither": 0, "unsure": 0, "notext": 0}
    if outcome in n:
        n[outcome] = 3
    r = {"id": f"en-gutter/g1/p~s{i}", "set": "g1", "kind": "gutter", "line": {"x": .5, "y0": .1, "y1": .9}, "counts": n,
         "settled": settled, "outcome": outcome if settled else None}
    if settled:
        r.update(segment_id=f"s{i}", page_id="p", method="m", role="pilot")
    return {**r, **extra}


class Analysis(unittest.TestCase):
    def test_outcomes_are_per_segment_and_unsure_and_disputed_are_apart(self):
        export = [export_row(1, "a"), export_row(2, "b"), export_row(3, "unsure"), export_row(4, "disputed"), export_row(5, None, settled=False),
                  export_row(6, "a", held=True), {"id": "en/x", "kind": None, "settled": True, "outcome": "a"}, export_row(7, "a", set="other")]
        rows, c = an.segment_rows(export, "g1")
        self.assertEqual({r["segment_id"]: r["outcome"] for r in rows}, {"s1": "yes", "s2": "no", "s3": "unsure", "s4": "disputed"})
        self.assertEqual((c["tasks"], c["settled"], c["yes"], c["no"], c["unsure"], c["disputed"], c["unsettled"], c["held_or_withdrawn"]), (6, 4, 1, 1, 1, 1, 1, 1))
        self.assertEqual(rows[0]["method"], "m")

    def test_gold_accuracy_reads_truth_from_the_dump_so_unsettled_tasks_count(self):
        def task(i, truth, kind="gutter"):
            return {"table": "tasks", "row": {"id": f"t{i}", "set_name": "g1", "body": json.dumps({"kind": kind, "private": {"truth": truth}})}}
        def ans(i, who, choice):
            return {"table": "answers", "row": {"task_id": f"t{i}", "contributor": who, "choice": choice}}
        dump = [task(1, "yes"), task(2, "no"), task(3, "yes", kind="pick"), {"table": "tasks", "row": {"id": "t4", "set_name": "g1", "body": json.dumps({"kind": "gutter", "private": {}})}},
                ans(1, "vol-one-abcdef", "a"), ans(2, "vol-one-abcdef", "a"), ans(1, "vol-two-abcdef", "unsure"), ans(3, "vol-one-abcdef", "a"), ans(4, "vol-one-abcdef", "b")]
        g = an.gold_accuracy(dump, "g1")
        self.assertEqual(g["calibration_tasks"], 2)
        self.assertEqual(g["volunteers"]["vol-one-"], {"answers": 2, "matched": 1, "missed": 1, "unsure": 0, "accuracy": 0.5})
        self.assertEqual(g["volunteers"]["vol-two-"], {"answers": 1, "matched": 0, "missed": 0, "unsure": 1, "accuracy": None})


if __name__ == "__main__":
    unittest.main()
