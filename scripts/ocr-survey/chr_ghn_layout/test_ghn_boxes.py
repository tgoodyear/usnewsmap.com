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
import line_clf_v2  # noqa: E402

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
        frozen = line_clf_v2.load_frozen()
        clf = ghn_boxes.classifier_version("t" * 64, frozen)
        self.assertIn("8b82fe7e", clf)
        self.assertIn("77d692ae", clf)
        self.assertIn("5bda2baa808a", clf)
        self.assertIn("p_line >= 0.46", clf)
        for v in (finder, clf):
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

    def test_rows_are_a_page_row_then_lines_called_cherokee_at_the_ensemble_threshold(self):
        lines = [{"box": [10, 10, 90, 20], "tid": "r0.1.1.1", "p_chr": 0.2, "p_v2": 0.9, "p_cnn": 0.8, "p_line": 0.97},
                 {"box": [10, 30, 90, 40], "tid": "r0.1.1.2", "p_chr": 0.9, "p_v2": 0.3, "p_cnn": 0.4, "p_line": 0.4599},
                 {"box": [10, 50, 90, 60], "tid": "r1.1.1.1", "p_chr": 0.5, "p_v2": 0.5, "p_cnn": 0.5, "p_line": 0.46},
                 {"box": [10, 70, 10, 80], "tid": "r1.1.1.2", "p_chr": None},
                 {"box": [10, 80, 90, 90], "tid": "r1.1.1.3", "p_chr": None, "p_v2": None, "p_cnn": None, "p_line": None}]
        rows = ghn_boxes.page_rows(self.rec(lines), self.PAGE, "F", "C", 0.08, 0.46)
        head = rows[0]
        self.assertEqual(head, {"chr_threshold": 0.46, "classifier": "C", "contrast": None, "doc_id": DOC, "finder": "F", "height": 100,
                                "held": False, "held_reasons": [], "kind": "page", "scan_sha256": "a" * 64,
                                "tall_share": 0.01, "width": 100})
        self.assertEqual([(r["id"], r["chr"]) for r in rows[1:]],
                         [("r0b1p1l1", True), ("r0b1p1l2", False), ("r1b1p1l1", True), ("r1b1p1l3", False)])
        self.assertEqual((rows[2]["p_chr"], rows[2]["p_v2"], rows[2]["p_cnn"], rows[2]["p_line"]), (0.9, 0.3, 0.4, 0.4599))
        self.assertIsNone(rows[4]["p_line"])
        # Keys are sorted, as usnm-boinc's lines::boxes reads them.
        with tempfile.TemporaryDirectory() as tmp:
            path = ghn_boxes.boxes_path(tmp, DOC)
            self.assertEqual(path, Path(tmp, "ghn", "sn83020866_1828-02-21_ed-1_seq-1", "boxes.jsonl"))
            ghn_boxes.write_rows(path, rows)
            text = path.read_text().splitlines()
            self.assertTrue(text[0].startswith('{"chr_threshold": 0.46, "classifier": '))
            self.assertTrue(text[1].startswith('{"box": '))
            self.assertEqual(json.loads(text[1])["kind"], "line")

    def test_a_page_over_the_merged_lines_share_is_held(self):
        rows = ghn_boxes.page_rows(self.rec([], share=0.081), self.PAGE, "F", "C", 0.08, 0.46)
        self.assertEqual((rows[0]["held"], rows[0]["held_reasons"]), (True, ["merged_lines"]))
        rows = ghn_boxes.page_rows(self.rec([], share=0.08), self.PAGE, "F", "C", 0.08, 0.46)
        self.assertFalse(rows[0]["held"])

    def test_a_faded_page_is_held_with_its_reason(self):
        rows = ghn_boxes.page_rows(self.rec([]), self.PAGE, "F", "C", 0.08, 0.46, faded=True)
        self.assertEqual((rows[0]["held"], rows[0]["held_reasons"]), (True, ["faded"]))
        rows = ghn_boxes.page_rows(self.rec([], share=0.2), self.PAGE, "F", "C", 0.08, 0.46, faded=True)
        self.assertEqual(rows[0]["held_reasons"], ["merged_lines", "faded"])

    def test_a_box_off_the_page_stops_the_run(self):
        with self.assertRaises(SystemExit):
            ghn_boxes.page_rows(self.rec([{"box": [10, 10, 101, 20], "tid": "r0.1.1.1"}]), self.PAGE,
                                "F", "C", 0.08, 0.46)


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
            for bad in ({**row, "source": "loc", "doc_id": "sn1_1900-01-01_ed-1_seq-1"},
                        {**row, "doc_id": "ghn:../../escaped"}, {**row, "doc_id": "ghn:sn1/1900-01-01_ed-1_seq-1"}):
                path.write_text(json.dumps(bad) + "\n")
                with self.assertRaises(SystemExit):
                    ghn_boxes.read_pages(path)
            path.write_text((json.dumps(row) + "\n") * 2)
            with self.assertRaises(SystemExit):
                ghn_boxes.read_pages(path)
            with self.assertRaises(SystemExit):
                ghn_boxes.boxes_path(tmp, "ghn:../../escaped")


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
        # A page listed twice in the summary counts once.
        self.assertEqual(len(set(ghn_boxes.pilot_pages(pages, summary + summary, 7))), 7)
        with self.assertRaises(SystemExit):
            ghn_boxes.pilot_pages(pages, summary + summary, 8)
        with self.assertRaises(SystemExit):
            ghn_boxes.pilot_pages(pages, [], 5)
        # Fewer usable pages in the batch than the pilot takes: refused, not a shorter pilot.
        with self.assertRaises(SystemExit):
            ghn_boxes.pilot_pages(pages, summary, 8)


class Classifier(unittest.TestCase):
    """The ported v2 ensemble (#2128 amendment 3.3). The reference values are two boxes of the 3.3 gate
    as chr-2128's clf2.py and dev3.py scored them (p_v2 and p_line rounded to 6 places there)."""
    REF = [([-18.49105, -5.1266, 0.06224, 0.32773, 0.0, 1.1831, 0.0032], 0.007385, 0.140298, 0.023893),
           ([-4.06028, -5.49982, 0.91029, 0.47294, 1.0, 0.56338, 0.29998], 0.864772, 0.387505, 0.687688)]

    def test_the_frozen_model_is_the_gated_one(self):
        fr = line_clf_v2.load_frozen()
        self.assertEqual(fr["t"], 0.46)
        self.assertEqual(fr["cnn_sha256"], "5bda2baa808ac5912381bd4cb45cdaf640f1e666840b387e3457dee27cf58937")
        self.assertEqual(fr["v2"]["features"], ["ll_chr", "ll_eng", "conf_chr", "conf_eng", "syl_share", "rel_h", "p_chr"])
        self.assertEqual(fr["v2"]["lm"]["chr"]["sha256"], "b8fb978e48aeaabfe2b5cfdbc3c17c117074d5f2319ab0e5d96d487947ad7650")

    def test_scores_match_the_gate_run(self):
        fr = line_clf_v2.load_frozen()
        for x, p_v2, p_cnn, p_line in self.REF:
            self.assertAlmostEqual(line_clf_v2.v2_prob(x, fr), p_v2, places=5)
            self.assertAlmostEqual(line_clf_v2.ensemble_prob(p_v2, p_cnn, fr), p_line, places=5)

    def test_witten_bell_is_a_distribution_and_prefers_seen_text(self):
        m = line_clf_v2.WittenBell(["ᏣᎳᎩ ᏧᎴᎯᏌᏅᎯ", "ᏣᎳᎩ ᎠᏂᏴᏫᏯ"])
        for hist in ("", "ᏣᎳ", "\x02\x02\x02\x02", "zzzz"):
            total = sum(m.prob(c, hist) for c in m.vocab) + m.prob("\x00", hist)
            self.assertAlmostEqual(total, 1.0)
        self.assertGreater(m.ll("ᏣᎳᎩ"), m.ll("abc"))
        self.assertEqual(m.ll(""), m.unk_lp)

    def test_a_wrong_text_or_model_file_is_refused(self):
        fr = line_clf_v2.load_frozen()
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp, "x.txt")
            t.write_text("ᏣᎳᎩ\n")
            with self.assertRaises(SystemExit):
                line_clf_v2.language_models(t, t, fr)
            for name in ("chr_phoenix.traineddata", "eng.traineddata"):
                Path(tmp, name).write_bytes(b"x")
            with self.assertRaises(SystemExit):
                ghn_boxes.check_tessdata(tmp, tmp, fr)

    def test_the_faded_list_is_the_frozen_quarter_of_the_campaign(self):
        faded = ghn_boxes.faded_pages()
        self.assertEqual(len(faded), 699 // 4)
        self.assertTrue(all(ghn_boxes.DOC_ID.fullmatch(d) for d in faded))
        with tempfile.TemporaryDirectory() as tmp:
            other = Path(tmp, "faded.txt")
            other.write_text("\n".join(sorted(faded)[1:]) + "\n")
            with self.assertRaises(SystemExit):
                ghn_boxes.faded_pages(other)

    def test_faded_is_the_lowest_quarter_by_contrast_then_id(self):
        con = {f"p{i}": float(c) for i, c in enumerate([200, 150, 150, 250, 100, 180, 220, 230, 240])}
        self.assertEqual(line_clf_v2.faded_pages(con), {"p4", "p1"})
        self.assertEqual(line_clf_v2.faded_pages({}), set())


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
