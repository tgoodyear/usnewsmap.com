"""Tests for the Yiddish newspaper line test set (#1479): synthetic scans and transcriptions only."""
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("yiddish_newspaper", Path(__file__).with_name("yiddish_newspaper.py"))
yn = importlib.util.module_from_spec(spec)
spec.loader.exec_module(yn)

DATA = Path(__file__).resolve().parents[2] / "ocr/bench/yiddish-newspaper"


def jpeg(width, height):
    app0 = b"\xff\xe0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00"
    sof0 = b"\xff\xc0\x00\x11\x08" + height.to_bytes(2, "big") + width.to_bytes(2, "big") + b"\x03" + b"\x01\x22\x00\x02\x11\x01\x03\x11\x01"
    return b"\xff\xd8" + app0 + sof0 + b"\xff\xd9"


def select_args(**over):
    args = dict(seed="s", lang="yid", campaign="c", image_path="p", pages_per_cell=3, reserves_per_cell=2,
                anchors_per_page=4, blocks_per_page=2, lines_per_block=2)
    args.update(over)
    return type("Args", (), args)


def inventory_row(doc_id, width=1000, height=1400):
    return {"doc_id": doc_id, **yn.parse_doc_id(doc_id), "damage": "high", "sha256": hashlib.sha256(doc_id.encode()).hexdigest(),
            "bytes": 10, "width": width, "height": height}


class ScanTests(unittest.TestCase):
    def test_jpeg_size_reads_the_frame_header(self):
        self.assertEqual(yn.jpeg_size(jpeg(2542, 3507)), (2542, 3507))
        with self.assertRaises(ValueError):
            yn.jpeg_size(b"\x89PNG\r\n")
        with self.assertRaises(ValueError):
            yn.jpeg_size(b"\xff\xd8\xff\xd9")
        with self.assertRaisesRegex(ValueError, "second SOI"):
            yn.jpeg_size(b"\xff\xd8\xff\xd8" + jpeg(10, 10)[2:])

    def test_scan_facts_streams_in_chunks(self):
        data = jpeg(2542, 3507) + bytes(5000)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "page.jpg"
            path.write_bytes(data)
            self.assertEqual(yn.scan_facts(path, chunk=7), (hashlib.sha256(data).hexdigest(), len(data), 2542, 3507))
            path.write_bytes(b"\xff\xd8" + bytes(50))
            with self.assertRaisesRegex(ValueError, "bad JPEG marker"):
                yn.scan_facts(path, chunk=7)

    def test_inventory_lists_only_held_scans_in_the_language(self):
        with tempfile.TemporaryDirectory() as tmp:
            units = Path(tmp) / "units"
            for name, lang, image in [
                ("sn1_1910-01-02_ed-1_seq-3", "yid", True),
                ("sn1_1910-01-02_ed-1_seq-4", "heb", True),
                ("sn1_1910-01-03_ed-1_seq-1", "yid", False),
            ]:
                (units / name).mkdir(parents=True)
                (units / name / "sample.json").write_text(json.dumps({"lang": lang, "decade": 1910, "damage": "high"}))
                if image:
                    (units / name / "page.jpg").write_bytes(jpeg(640, 480))
            (units / "no-sample").mkdir()
            out = Path(tmp) / "inventory.jsonl"
            self.assertEqual(yn.main(["inventory", "--units", str(units), "--out", str(out)]), 0)
            rows = [json.loads(line) for line in out.read_text().splitlines()]
        self.assertEqual([r["doc_id"] for r in rows], ["sn1_1910-01-02_ed-1_seq-3"])
        self.assertEqual((rows[0]["width"], rows[0]["height"], rows[0]["decade"]), (640, 480, 1910))
        self.assertEqual(rows[0]["sha256"], hashlib.sha256(jpeg(640, 480)).hexdigest())


class SelectTests(unittest.TestCase):
    titles = (None, [
        {"lccn": "sn1", "title": "A", "first_page_date": "1910-01-01", "last_page_date": "1921-12-31", "pages": 9},
        {"lccn": "sn2", "title": "B", "first_page_date": "1895-01-01", "last_page_date": "1896-12-31", "pages": 9},
    ])

    def inventory(self):
        rows = [inventory_row(f"sn1_1910-01-0{day}_ed-1_seq-{seq}") for day in range(1, 5) for seq in range(1, 4)]
        rows += [inventory_row("sn1_1921-05-01_ed-1_seq-1"), inventory_row("sn1_1921-05-02_ed-1_seq-1"),
                 inventory_row("sn9_1910-01-01_ed-1_seq-1")]
        return "".join(json.dumps(r) + "\n" for r in rows).encode()

    def test_cells_reserves_gaps_and_distinct_issues(self):
        m = yn.build_manifest(self.inventory(), self.titles, select_args())
        cells = {c["cell"]: (c["held"], c["selected"], c["reserves"], c["status"]) for c in m["cells"]}
        self.assertEqual(cells, {"sn1/1910": (12, 3, 2, "ok"), "sn1/1920": (2, 2, 0, "short"), "sn2/1890": (0, 0, 0, "gap")})
        self.assertEqual(m["source"]["pages_outside_titles"], 1)
        self.assertEqual(m["totals"]["target_lines"], 5 * 2 * 2)
        drawn = [p for p in m["pages"] if p["cell"] == "sn1/1910"]
        self.assertEqual([p["role"] for p in drawn], ["selected"] * 3 + ["reserve"] * 2)
        self.assertEqual(len({p["date"] for p in drawn[:4]}), 4, "one page per issue before any second page")
        for p in m["pages"]:
            for x, y in p["anchors"]:
                self.assertTrue(50 <= x < 950 and 70 <= y < 1330)

    def test_draw_is_reproducible_and_follows_the_seed(self):
        a = yn.build_manifest(self.inventory(), self.titles, select_args())
        self.assertEqual(a, yn.build_manifest(self.inventory(), self.titles, select_args()))
        orders = {tuple(p["doc_id"] for p in yn.build_manifest(self.inventory(), self.titles, select_args(seed=s))["pages"])
                  for s in ("s", "t", "u", "v")}
        self.assertGreater(len(orders), 1)


def manifest():
    page = {"lccn": "sn1", "date": "1910-01-02", "edition": 1, "damage": "high", "bytes": 10, "width": 1000, "height": 1400}
    return {
        "params": {"pages_per_cell": 1, "reserves_per_cell": 1, "anchors_per_page": 4, "blocks_per_page": 2, "lines_per_block": 2},
        "cells": [{"cell": "sn1/1910"}],
        "pages": [
            {**page, "doc_id": "sn1_1910-01-02_ed-1_seq-1", "cell": "sn1/1910", "rank": 1, "role": "selected", "seq": 1,
             "sha256": "a" * 64, "anchors": [[100, 100], [120, 105], [400, 500], [700, 800]]},
            {**page, "doc_id": "sn1_1910-01-02_ed-1_seq-2", "cell": "sn1/1910", "rank": 2, "role": "reserve", "seq": 2,
             "sha256": "b" * 64, "anchors": [[100, 100], [200, 200], [300, 300], [400, 400]]},
        ],
    }


def done(records):
    """Fill the selected page with a reviewed transcription that follows the rules."""
    rec = records[0]
    rec.update(status="reviewed", transcribed_by="reader-a", reviewed_by="reader-b", evidence="second reading 2026-11-05")
    rec["blocks"] = [
        {"anchor": 0, "region": "body", "column": [40, 320], "lines": [
            {"box": [50, 90, 300, 112], "text": "דער טאָג איז געקומען"},
            {"box": [50, 115, 300, 137], "text": "װאָס \ufffd האָט", "illegible_spans": [{"start": 4, "end": 5, "reason": "torn"}]},
        ]},
        {"anchor": 2, "region": "headline", "column": [340, 620], "lines": [{"box": [350, 480, 600, 520], "text": "נײַעס"}],
         "short_reason": "headline is one line"},
    ]
    rec["skipped_anchors"] = [{"anchor": 1, "reason": "already-used"}]
    return records


class CheckTests(unittest.TestCase):
    def check(self, records, complete=True):
        return yn.check_records(manifest(), records, complete=complete)

    def assertError(self, records, text, complete=True):
        errors, _, _ = self.check(records, complete)
        self.assertTrue(any(text in e for e in errors), f"{text!r} not in {errors}")

    def test_template_is_valid_but_incomplete(self):
        records = yn.template_records(manifest())
        self.assertEqual(self.check(records, complete=False)[0], [])
        self.assertError(records, "every active page must be reviewed")

    def test_reviewed_page_passes_and_counts_visible_characters(self):
        errors, warnings, summary = self.check(done(yn.template_records(manifest())))
        self.assertEqual((errors, warnings), ([], []))
        cell = summary["sn1/1910"]
        self.assertEqual((cell["blocks"], cell["lines"], cell["skip:already-used"]), (2, 3, 1))
        self.assertEqual(cell["characters"], len("דער טאָג איז געקומען") + len("װאָס \ufffd האָט") - 1 + len("נײַעס"))

    def test_anchors_cannot_be_passed_over(self):
        records = done(yn.template_records(manifest()))
        records[0]["skipped_anchors"] = []
        self.assertError(records, "anchors must be taken in order")

    def test_anchors_after_the_last_block_stay_unused(self):
        records = done(yn.template_records(manifest()))
        records[0]["skipped_anchors"].append({"anchor": 3, "reason": "non-text"})
        self.assertError(records, "leave later anchors unused")

    def test_first_line_must_be_at_the_anchor(self):
        records = done(yn.template_records(manifest()))
        records[0]["blocks"][1]["lines"][0]["box"] = [350, 440, 600, 470]
        self.assertError(records, "first line must be the line at anchor 2")

    def test_a_short_first_line_in_the_anchors_column_passes(self):
        records = done(yn.template_records(manifest()))
        records[0]["blocks"][1]["lines"][0]["box"] = [450, 480, 600, 520]
        self.assertEqual(self.check(records)[:2], ([], []))

    def test_lines_stay_in_the_column(self):
        records = done(yn.template_records(manifest()))
        records[0]["blocks"][0]["lines"][1]["box"] = [350, 115, 600, 137]
        self.assertError(records, "every line must lie within the block's column")
        records = done(yn.template_records(manifest()))
        records[0]["blocks"][1]["column"] = [450, 620]
        self.assertError(records, "must contain anchor 2")
        records[0]["blocks"][1].pop("column")
        self.assertError(records, "column must be two integers")

    def test_a_gap_needs_a_reason(self):
        for first, lines in [([350, 600, 600, 640], None), ([350, 480, 600, 520], [350, 600, 600, 640])]:
            with self.subTest(first=first, second=lines):
                records = done(yn.template_records(manifest()))
                block = records[0]["blocks"][1]
                block["lines"][0]["box"] = first
                if lines:
                    block["lines"].append({"box": lines, "text": "נײַעס"})
                    block.pop("short_reason")
                self.assertError(records, "needs a gap_reason")
                block["gap_reason"] = "paragraph break; no line between"
                errors, warnings, summary = self.check(records)
                self.assertEqual(errors, [])
                self.assertTrue(any("paragraph break" in w for w in warnings))
                self.assertEqual(summary["sn1/1910"]["gaps"], 1)

    def test_a_line_repeated_in_its_own_block_fails(self):
        records = done(yn.template_records(manifest()))
        records[0]["blocks"][0]["lines"][1]["box"] = [50, 91, 300, 113]
        self.assertError(records, "repeats a line")

    def test_a_record_without_image_or_anchors_reports_instead_of_crashing(self):
        records = done(yn.template_records(manifest()))
        del records[0]["image"]
        records[0]["anchors"] = None
        records[0]["blocks"][0]["lines"].append("not a line")
        errors, _, _ = self.check(records)
        for text in ["image differs from the manifest", "anchors differs from the manifest", "line 2: must be an object"]:
            self.assertTrue(any(text in e for e in errors), f"{text!r} not in {errors}")

    def test_line_rules(self):
        cases = [
            ("box", [50, 90, 1300, 112], "box must be four integers"),
            ("text", "דער\u200fטאָג", "control characters"),
            ("text", "דער\nטאָג", "control characters"),
            ("text", "   ", "text is empty"),
            ("illegible_spans", [{"start": 3, "end": 40, "reason": "torn"}], "illegible span 0"),
        ]
        for field, value, text in cases:
            with self.subTest(field=field, value=value):
                records = done(yn.template_records(manifest()))
                records[0]["blocks"][0]["lines"][0][field] = value
                self.assertError(records, text)

    def test_review_must_be_independent(self):
        records = done(yn.template_records(manifest()))
        records[0]["reviewed_by"] = "reader-a"
        self.assertError(records, "reviewer must not be the transcriber")

    def test_prefill_is_allowed_with_a_bias_warning(self):
        records = done(yn.template_records(manifest()))
        records[0]["prefill"] = "tesseract-5.5/yid"
        errors, warnings, summary = self.check(records)
        self.assertEqual(errors, [])
        self.assertIn("biased toward tesseract-5.5/yid", warnings[0])
        self.assertEqual(summary["sn1/1910"]["prefilled"], 1)

    def test_reserves_only_replace_rejected_pages_in_rank_order(self):
        base = done(yn.template_records(manifest()))
        extra = copy.deepcopy(base)
        extra[1]["status"] = "todo"
        self.assertError(extra, "at most 1, a reserve only replaces a rejected page")
        unreplaced = copy.deepcopy(base)
        unreplaced[0].update(status="rejected", reject_reason="no-yiddish-text", blocks=[], skipped_anchors=[])
        self.assertError(unreplaced, "activate the next reserve")
        swapped = copy.deepcopy(base)
        swapped[0]["status"] = "reserve"
        self.assertError(swapped, "cannot be put back in reserve")
        self.assertError(swapped, "reserve page has no transcription")


class CommittedDrawTests(unittest.TestCase):
    """The committed manifest and template are what the script makes from the recorded seed."""

    @classmethod
    def setUpClass(cls):
        cls.manifest = json.loads((DATA / "manifest.json").read_text(encoding="utf-8"))
        cls.template = (DATA / "template.jsonl").read_text(encoding="utf-8")

    def test_anchors_follow_the_seed(self):
        m = self.manifest
        for p in m["pages"]:
            self.assertEqual(p["anchors"], yn.anchors_for(m["seed"], p["doc_id"], p["width"], p["height"], m["params"]["anchors_per_page"]))

    def test_cells_and_totals_agree(self):
        m = self.manifest
        for c in m["cells"]:
            ranked = sorted((p for p in m["pages"] if p["cell"] == c["cell"]), key=lambda p: p["rank"])
            self.assertEqual([p["rank"] for p in ranked], list(range(1, len(ranked) + 1)))
            self.assertEqual([p["role"] for p in ranked], ["selected"] * c["selected"] + ["reserve"] * c["reserves"])
            order = [p["doc_id"] for p in yn.order_cell(m["seed"], ranked)]
            self.assertEqual(order, [p["doc_id"] for p in ranked], "rank follows the seeded order")
        expected = {f"{t['lccn']}/{d}" for t in m["titles"] for d in yn.title_decades(t)}
        self.assertLessEqual(expected, {c["cell"] for c in m["cells"]})
        selected = sum(c["selected"] for c in m["cells"])
        self.assertEqual(m["totals"]["target_lines"], selected * m["params"]["blocks_per_page"] * m["params"]["lines_per_block"])
        self.assertTrue(300 <= m["totals"]["target_lines"] <= 500)

    def test_template_matches_and_checks(self):
        self.assertEqual(self.template, yn.dump_records(yn.template_records(self.manifest)))
        records = [json.loads(line) for line in self.template.splitlines()]
        self.assertEqual(yn.check_records(self.manifest, records)[0], [])


if __name__ == "__main__":
    unittest.main()
