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
    def test_renew_only_by_the_owner(self):
        with tempfile.TemporaryDirectory() as d:
            cur = jaocr.LocalStore(d)
            cur.write("l", json.dumps({"owner": "r1"}).encode())
            self.assertTrue(cur.renew("l", json.dumps({"owner": "r1", "n": 2}).encode(), "r1"))
            self.assertFalse(cur.renew("l", json.dumps({"owner": "r2"}).encode(), "r2"))
            self.assertEqual(json.loads(cur.read("l"))["n"], 2)

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
                # A crashed run left a fresh lock and no CSV: a new run waits it out.
                cur.write("audit/loc-pages-v1-non-english.lock", b"{}")
                audit.audit(ref, cur, all_languages=False)
                self.assertEqual(calls, [])
                # Once the lock is stale, the next run takes it over and finishes.
                import os
                os.environ["JAOCR_AUDIT_LOCK_MINUTES"] = "-1"
                try:
                    audit.audit(ref, cur, all_languages=False)
                finally:
                    del os.environ["JAOCR_AUDIT_LOCK_MINUTES"]
                audit.audit(ref, cur, all_languages=False)  # done: the CSV exists
                n = len(calls)
                # A full audit is a different scope: it runs, and covers the English title too.
                audit.audit(ref, cur, all_languages=True)
                self.assertEqual(len(calls), n + 3)
            finally:
                jaocr.fetch = orig
            # Default scope: 2 requests (listing + 1 non-English title); full scope: 3.
            self.assertEqual(len(calls), 2 + 3)
            rows = list(csv.DictReader(io.StringIO(cur.read("audit/loc-pages-v1-non-english.csv").decode())))
            self.assertEqual([(r["lccn"], r["loc_pages"], r["our_pages"], r["missing"]) for r in rows],
                             [("a", "1012", "256", "756")])


if __name__ == "__main__":
    unittest.main()
