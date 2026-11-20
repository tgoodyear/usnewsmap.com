"""load.py xiaomi: the held page-mode rows into quality.ocr_outputs and quality.vlm_page_cost (#1998)."""
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import load as m  # noqa: E402

DOC = "sn83030214_1900-01-01_ed-1_seq-1"


def ocr(**kw):
    row = {"doc_id": DOC, "reader": "ours-xiaomi", "engine": "xiaomi-ocr-0/bbd8cf8d21f2",
           "source": "boinc-page", "ocred_at": "2026-11-20T00:00:00+00:00", "text_chars": 1234,
           "damage_rate": 0.02}
    row.update(kw)
    return row


def cost(**kw):
    row = {"doc_id": DOC, "campaign": "xiaomi-pilot", "job_id": "j1", "result_name": "r_0",
           "plan_class": "usnm_qwen_x08p", "gpu": "GPU", "gpu_mem_total_mib": 8192, "slots": 2,
           "ctx_size": 8192, "load_ms": 4000, "engine_ms": 60000, "page_share": 0.5,
           "gpu_seconds": 32.0, "regions": 7, "tokens": 900, "chars_out": 1200,
           "gpu_mem_peak_mib": None, "outcome": "ok", "validated": True, "canonical": False}
    row.update(kw)
    return row


class Xiaomi(unittest.TestCase):
    def test_xiaomi_is_one_of_our_readers(self):
        self.assertIn("ours-xiaomi", m.OURS)
        self.assertEqual(m.reader_name("xiaomi-ocr-0/bbd8cf8d21f2", "boinc-page"), "ours-xiaomi")

    def test_ocr_rows_keep_the_reader_and_drop_other_pages(self):
        rows = m.xiaomi_ocr_rows([ocr(), ocr(doc_id="not a page"), ocr(ocred_at="")])
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0][:2], (DOC, "ours-xiaomi"))
        self.assertEqual(rows[0][5:], (1234, 0.02))

    def test_a_row_of_another_reader_is_refused(self):
        with self.assertRaises(SystemExit):
            m.xiaomi_ocr_rows([ocr(reader="ours-surya")])

    def test_cost_rows_follow_the_columns_and_are_unique_by_result(self):
        rows = m.xiaomi_cost_rows([cost(), cost(), cost(result_name="r_1", canonical=True)])
        self.assertEqual(len(rows), 2)
        self.assertEqual(len(rows[0]), len(m.COST_COLS))
        by = dict(zip(m.COST_COLS, rows[0]))
        self.assertEqual((by["load_ms"], by["page_share"], by["gpu_mem_peak_mib"]), (4000, 0.5, None))
        self.assertIs(by["validated"], True)

    def test_cost_is_loaded_append_only(self):
        sql = m.upsert_sql("vlm_page_cost", m.COST_COLS, ["doc_id", "result_name"],
                           m.xiaomi_cost_rows([cost()]), update=False)
        self.assertIn("DO NOTHING", sql)
        self.assertNotIn("DO UPDATE", sql)

    def test_read_jsonl_names_the_bad_line(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "x.jsonl")
            with open(p, "w") as f:
                f.write(json.dumps(ocr()) + "\n\nnot json\n")
            with self.assertRaises(SystemExit) as e:
                m.read_jsonl(p)
            self.assertIn(":3:", str(e.exception))


if __name__ == "__main__":
    unittest.main()
