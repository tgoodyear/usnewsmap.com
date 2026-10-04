"""Tests for audit.py: python3 -m unittest discover ja-ocr."""

import csv
import io
import json
import tempfile
import unittest

import audit
import jaocr


def facets(pages):
    return {"facets": [{"type": "object-type", "filters": [
        {"title": "Issues", "count": 9}, {"title": "Pages (Full Text)", "count": pages}]}]}


class Audit(unittest.TestCase):
    def test_loc_pages(self):
        self.assertEqual(audit.loc_pages(facets(1012)), 1012)
        self.assertIsNone(audit.loc_pages({"facets": []}))

    def test_rows(self):
        titles = [{"lccn": "a", "name": "A", "languages": ["jpn", "eng"]},
                  {"lccn": "b", "name": "B", "languages": ["heb"]},
                  {"lccn": "c", "name": "C", "languages": ["ger"]}]
        got = audit.rows(titles, {"a": 256, "b": 128, "c": 50}, {"a": 1012, "b": 831, "c": None},
                         {"x_ver01"}, [{"batch": "x_ver01", "lccns": ["a"]}, {"batch": "y_ver02", "lccns": ["b"]}])
        self.assertEqual([(r["lccn"], r["missing"], r["unpublished_batches"]) for r in got],
                         [("a", 756, ""), ("b", 703, "y_ver02"), ("c", "", "")])
        self.assertEqual(got[0]["missing_share"], 0.747)

    def test_audit_writes_a_csv_once(self):
        with tempfile.TemporaryDirectory() as d:
            ref, cur = jaocr.LocalStore(d + "/ref"), jaocr.LocalStore(d + "/cur")
            ref.write("current.json", json.dumps({"reference": "v1"}).encode())
            ref.write("v1/title_pages.json", json.dumps({"a": 256, "e": 10}).encode())
            ref.write("v1/batches.json", json.dumps([{"batch": "x", "curated": {"version": 1}}]).encode())
            ref.write("catalog/titles.json", json.dumps([
                {"lccn": "a", "name": "A", "languages": ["jpn", "eng"]},
                {"lccn": "e", "name": "E", "languages": ["eng"]}]).encode())
            calls = []

            def fake_fetch(url, pacer):
                calls.append(url)
                if url == jaocr.COLLECTION:
                    return json.dumps({"datasets": [{"batch": "x_ver01", "lccns": ["a", "e"]}]}).encode()
                return json.dumps(facets(1012)).encode()

            orig = jaocr.fetch
            jaocr.fetch = fake_fetch
            try:
                audit.audit(ref, cur, all_languages=False)
                audit.audit(ref, cur, all_languages=False)  # locked: does nothing
            finally:
                jaocr.fetch = orig
            # English-only titles are skipped by default; the second run made no requests.
            self.assertEqual(len(calls), 2)
            rows = list(csv.DictReader(io.StringIO(cur.read("audit/loc-pages-v1.csv").decode())))
            self.assertEqual([(r["lccn"], r["loc_pages"], r["our_pages"], r["missing"]) for r in rows],
                             [("a", "1012", "256", "756")])


if __name__ == "__main__":
    unittest.main()
