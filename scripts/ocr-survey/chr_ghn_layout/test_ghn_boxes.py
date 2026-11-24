"""Tests for the GHN staging step's line boxes (#1622). The parts that need numpy are skipped without it."""
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import ghn_boxes  # noqa: E402

HAVE_NUMPY = importlib.util.find_spec("numpy") is not None
DOC = "ghn:sn83020866_1828-02-21_ed-1_seq-1"


class FrozenColumns(unittest.TestCase):
    def test_the_frozen_column_finder_is_the_file_s1_was_scored_with(self):
        self.assertEqual(ghn_boxes.sha256_file(ghn_boxes.COLUMNS_PY), ghn_boxes.COLUMNS_SHA256)
        self.assertEqual(ghn_boxes.check_columns(), str(ghn_boxes.COLUMNS_PY))

    def test_another_column_finder_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            other = Path(tmp, "columns.py")
            other.write_bytes(ghn_boxes.COLUMNS_PY.read_bytes() + b"\n")
            with self.assertRaises(SystemExit):
                ghn_boxes.check_columns(other)

    def test_the_versions_name_the_frozen_inputs(self):
        finder = ghn_boxes.finder_version("e" * 64)
        self.assertIn("337fc0aa", finder)
        self.assertIn(ghn_boxes.COLUMNS_SHA256[:12], finder)
        self.assertIn("89fb1017", finder)
        self.assertIn("tesseract 5.5.0", finder)
        self.assertIn("8b82fe7e", ghn_boxes.classifier_version("t" * 64))
        for v in (finder, ghn_boxes.classifier_version("t" * 64)):
            self.assertLessEqual(len(v), 512)


class Rows(unittest.TestCase):
    PAGE = {"doc_id": DOC, "scan": {"sha256": "a" * 64}}

    def rec(self, lines, share=0.01):
        return {"width": 100, "height": 100, "tall_share": share, "lines": lines}

    def test_ids_keep_the_region_and_tesseracts_numbering(self):
        self.assertEqual(ghn_boxes.line_id("r0.1.1.2"), "r0b1p1l2")
        self.assertEqual(ghn_boxes.line_id("r12.3.10.25"), "r12b3p10l25")
        for bad in ("1.1.2", "r0.1.1", "rx.1.1.2", "0.1.1.2"):
            with self.assertRaises(ValueError):
                ghn_boxes.line_id(bad)

    def test_rows_are_a_page_row_then_lines_called_cherokee_at_one_half(self):
        lines = [{"box": [10, 10, 90, 20], "tid": "r0.1.1.1", "p_chr": 0.97},
                 {"box": [10, 30, 90, 40], "tid": "r0.1.1.2", "p_chr": 0.4999},
                 {"box": [10, 50, 90, 60], "tid": "r1.1.1.1", "p_chr": 0.5},
                 {"box": [10, 70, 10, 80], "tid": "r1.1.1.2", "p_chr": None},
                 {"box": [10, 80, 90, 90], "tid": "r1.1.1.3", "p_chr": None}]
        rows = ghn_boxes.page_rows(self.rec(lines), self.PAGE, "F", "C", 0.08)
        head = rows[0]
        self.assertEqual(head, {"classifier": "C", "doc_id": DOC, "finder": "F", "height": 100, "held": False,
                                "kind": "page", "scan_sha256": "a" * 64, "tall_share": 0.01, "width": 100})
        self.assertEqual([(r["id"], r["chr"]) for r in rows[1:]],
                         [("r0b1p1l1", True), ("r0b1p1l2", False), ("r1b1p1l1", True), ("r1b1p1l3", False)])
        self.assertEqual(rows[2]["p_chr"], 0.4999)
        # Keys are sorted, as usnm-boinc's lines::boxes reads them.
        with tempfile.TemporaryDirectory() as tmp:
            path = ghn_boxes.boxes_path(tmp, DOC)
            self.assertEqual(path, Path(tmp, "ghn", "sn83020866_1828-02-21_ed-1_seq-1", "boxes.jsonl"))
            ghn_boxes.write_rows(path, rows)
            text = path.read_text().splitlines()
            self.assertTrue(text[0].startswith('{"classifier": '))
            self.assertTrue(text[1].startswith('{"box": '))
            self.assertEqual(json.loads(text[1])["kind"], "line")

    def test_a_page_over_the_merged_lines_share_is_held(self):
        rows = ghn_boxes.page_rows(self.rec([], share=0.081), self.PAGE, "F", "C", 0.08)
        self.assertTrue(rows[0]["held"])
        rows = ghn_boxes.page_rows(self.rec([], share=0.08), self.PAGE, "F", "C", 0.08)
        self.assertFalse(rows[0]["held"])

    def test_a_box_off_the_page_stops_the_run(self):
        with self.assertRaises(SystemExit):
            ghn_boxes.page_rows(self.rec([{"box": [10, 10, 101, 20], "tid": "r0.1.1.1"}]), self.PAGE,
                                "F", "C", 0.08)


class PageList(unittest.TestCase):
    def test_reads_ghn_pages_only_and_keeps_image_paths_inside(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp, "ghn-pages.jsonl")
            row = {"doc_id": DOC, "file": "images/sn83020866/1828-02-21/ed-1/seq-1.jpg", "source": "ghn"}
            path.write_text(json.dumps(row) + "\n")
            pages = ghn_boxes.read_pages(path)
            self.assertEqual(ghn_boxes.image_of("/x", pages[DOC]),
                             Path("/x/images/sn83020866/1828-02-21/ed-1/seq-1.jpg"))
            with self.assertRaises(SystemExit):
                ghn_boxes.image_of("/x", {**row, "file": "../etc/passwd"})
            select = Path(tmp, "select.txt")
            select.write_text("ghn:sn83020866_1900-01-01_ed-1_seq-1\n")
            with self.assertRaises(SystemExit):
                ghn_boxes.selected(pages, select)
            path.write_text(json.dumps({**row, "source": "loc", "doc_id": "sn1_1900-01-01_ed-1_seq-1"}) + "\n")
            with self.assertRaises(SystemExit):
                ghn_boxes.read_pages(path)


class Pilot(unittest.TestCase):
    def test_the_pilot_is_usable_pages_of_one_batch_in_hash_order(self):
        def page(n, batch):
            return f"ghn:sn8302086{6 if batch == 'gua_cph' else 4}_1830-01-0{n}_ed-1_seq-1"
        pages, summary = {}, []
        for n in range(1, 10):
            for batch in ("gua_cph", "gua_cia"):
                d = page(n, batch)
                pages[d] = {"batch": batch}
                summary.append({"doc_id": d, "chr_lines": 0 if n == 1 else 3, "held": n == 2})
        ids = ghn_boxes.pilot_pages(pages, summary, 5)
        self.assertEqual(len(ids), 5)
        self.assertEqual(len({pages[d]["batch"] for d in ids}), 1)
        self.assertFalse({page(1, "gua_cph"), page(2, "gua_cph"), page(1, "gua_cia"), page(2, "gua_cia")} & set(ids))
        self.assertEqual(ids, ghn_boxes.pilot_pages(pages, list(reversed(summary)), 5))
        with self.assertRaises(SystemExit):
            ghn_boxes.pilot_pages(pages, [], 5)
        # Fewer usable pages in the batch than the pilot takes: refused, not a shorter pilot.
        with self.assertRaises(SystemExit):
            ghn_boxes.pilot_pages(pages, summary, 8)


@unittest.skipUnless(HAVE_NUMPY, "needs numpy")
class Regions(unittest.TestCase):
    def test_a_heading_above_the_gutters_stays_one_region(self):
        import s1_columns
        # Two gutters from y 200 down, at x 300 and 600, on a 900 by 1000 page.
        gutters = [[[295, 305, 200, 600], [296, 306, 600, 1000]], [[598, 602, 200, 1000]]]
        self.assertEqual(s1_columns.regions(gutters, 900, 1000),
                         [[0, 0, 900, 200], [0, 200, 300, 1000], [300, 200, 600, 1000], [600, 200, 900, 1000]])


if __name__ == "__main__":
    unittest.main()
