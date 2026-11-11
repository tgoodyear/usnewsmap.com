"""Tests for the quality retention step, with no database or production state."""
import importlib.util
import json
import os
import sys
import tempfile
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("quality_retention", HERE / "quality_retention.py")
qr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qr)

OLD = time.time() - 2 * qr.MIN_AGE_SECS


class Layout:
    def __init__(self, base):
        self.base = Path(base)
        self.download = self.base / "download"
        self.quality = self.base / "quality"
        self.scans = self.base / "scans"
        self.index = self.base / "index-full.jsonl"
        self.eligible = self.base / "eligible.jsonl"
        self.journal = self.base / "journal.jsonl"
        self.scans.mkdir()
        self.index.write_text("")
        self.eligible.write_text("")

    def unit(self, n, docs, page=b"bundle-bytes", copy=None, scans=True, age=OLD):
        job_id = f"{n:064x}"
        wu = f"usnm_quality_{job_id}"
        unit = self.quality / "camp" / "units" / f"bundle-{n:05d}-{docs[0]}"
        unit.mkdir(parents=True)
        (unit / "page.jpg").write_bytes(page)
        os.utime(unit / "page.jpg", (OLD, OLD))
        (unit / "submitted").write_text(wu)
        job = {"id": job_id, "doc_id": docs[0], "inputs": {"page": {"bytes": len(page)}},
               "pages": [{"doc_id": d} for d in docs]}
        (unit / "quality.json").write_text(json.dumps(job))
        name = f"{job_id}{qr.PAGE_SUFFIX}"
        path = Path(qr.download_path(str(self.download), name))
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(page if copy is None else copy)
        os.utime(path, (age, age))
        with open(self.index, "a") as f:
            for d in docs:
                f.write(json.dumps({"doc_id": d, "batch": "dlc_test"}) + "\n")
                if scans:
                    lccn, date, ed, seq = qr.DOC.fullmatch(d).groups()
                    s = self.scans / "keep" / "dlc_test" / lccn / date / str(int(ed))
                    s.mkdir(parents=True, exist_ok=True)
                    (s / f"{int(seq)}.jp2").write_bytes(b"jp2")
        with open(self.eligible, "a") as f:
            f.write(json.dumps({"wu_id": n, "wu_name": wu, "file": name}) + "\n")
        return path, unit

    def prune(self, *extra):
        argv = ["prune", "--eligible", str(self.eligible), "--download", str(self.download),
                "--quality-root", str(self.quality), "--scan-index", str(self.index),
                "--scan-root", str(self.scans), "--journal", str(self.journal),
                "--commit", "abc123", *extra]
        out = []
        real = sys.stdout.write
        sys.stdout.write = out.append
        try:
            qr.main(argv)
        finally:
            sys.stdout.write = real
        return json.loads("".join(out))

    def journal_rows(self):
        if not self.journal.exists():
            return []
        return [json.loads(line) for line in self.journal.read_text().splitlines()]


DOCS = ["sn1_1900-01-01_ed-1_seq-1", "sn1_1900-01-01_ed-1_seq-2"]


class QualityRetentionTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.l = Layout(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def test_page_inputs_takes_only_quality_page_names(self):
        job = "a" * 64
        xml = (f"<file_info><name>{job}_page_jpg</name></file_info>"
               f"<file_info><name>{job}_loc_xml</name></file_info>"
               "<file_info><name>usnm_quality_places_v2_x</name></file_info>"
               "<file_info><name>bogus_page_jpg</name></file_info>")
        self.assertEqual(qr.page_inputs(xml), [f"{job}_page_jpg"])

    def test_download_path_matches_boinc_fanout(self):
        name = "adb01d08cb67a609f06f73f0e54309bc405c13d84771e5a452248aa263510e35_page_jpg"
        self.assertEqual(qr.download_path("/d", name), f"/d/10d/{name}")

    def test_dry_run_changes_nothing(self):
        path, _ = self.l.unit(1, DOCS)
        got = self.l.prune()
        self.assertEqual((got["mode"], got["files"], got["bundles"]), ("dry-run", 1, 1))
        self.assertTrue(path.exists())
        self.assertEqual(self.l.journal_rows(), [])

    def test_delete_removes_only_the_download_copy_and_journals_it(self):
        path, unit = self.l.unit(1, DOCS)
        got = self.l.prune("--delete")
        self.assertEqual(got["files"], 1)
        self.assertFalse(path.exists())
        self.assertTrue((unit / "page.jpg").exists())
        rows = self.l.journal_rows()
        self.assertEqual([r["action"] for r in rows], ["deleting", "deleted"])
        self.assertEqual(rows[1]["commit"], "abc123")
        self.assertEqual(rows[1]["path"], str(path))

    def test_links_a_copy_whose_original_scan_is_missing(self):
        path, unit = self.l.unit(1, DOCS, scans=False)
        dry = self.l.prune()
        self.assertEqual((dry["files"], dry["linked"]), (0, 1))
        self.assertNotEqual(path.stat().st_ino, (unit / "page.jpg").stat().st_ino)
        got = self.l.prune("--delete")
        self.assertEqual((got["files"], got["linked"]), (0, 1))
        self.assertEqual(path.read_bytes(), b"bundle-bytes")
        self.assertEqual(path.stat().st_ino, (unit / "page.jpg").stat().st_ino)
        self.assertEqual(list(path.parent.glob(".*")), [])
        rows = self.l.journal_rows()
        self.assertEqual([r["action"] for r in rows], ["linking", "linked"])
        self.assertTrue(rows[1]["never_prune"] and rows[1]["only_local_image"])
        self.assertIn("original scan missing for 2 page(s)", rows[1]["reason"])
        again = self.l.prune("--delete")
        self.assertEqual(again["kept"], {"already one copy (hard link)": 1})

    def test_does_not_link_a_copy_that_differs(self):
        path, unit = self.l.unit(1, DOCS, copy=b"bundle-bytez", scans=False)
        got = self.l.prune("--delete")
        self.assertEqual(got["kept"], {"bytes differ from unit page.jpg": 1})
        self.assertNotEqual(path.stat().st_ino, (unit / "page.jpg").stat().st_ino)

    def test_keeps_a_copy_that_differs_from_the_unit(self):
        path, _ = self.l.unit(1, DOCS, copy=b"bundle-bytez")
        got = self.l.prune("--delete")
        self.assertEqual(got["kept"], {"bytes differ from unit page.jpg": 1})
        self.assertTrue(path.exists())

    def test_keeps_a_recent_copy(self):
        path, _ = self.l.unit(1, DOCS, age=time.time())
        got = self.l.prune("--delete")
        self.assertEqual(got["kept"], {"written in the last hour": 1})
        self.assertTrue(path.exists())

    def test_keeps_a_copy_without_its_unit_page(self):
        path, unit = self.l.unit(1, DOCS)
        (unit / "page.jpg").unlink()
        got = self.l.prune("--delete")
        self.assertEqual(got["kept"], {"unit page.jpg missing": 1})
        self.assertTrue(path.exists())

    def test_batch_limit(self):
        for n in (1, 2, 3):
            self.l.unit(n, [f"sn{n}_1900-01-01_ed-1_seq-1"])
        got = self.l.prune("--delete", "--max-files", "2")
        self.assertEqual(got["files"], 2)
        self.assertEqual(sum(1 for _ in self.l.download.rglob("*_page_jpg")), 1)

    def test_skip_cmp_is_refused_with_delete(self):
        self.l.unit(1, DOCS)
        with self.assertRaises(SystemExit):
            self.l.prune("--delete", "--skip-cmp")


if __name__ == "__main__":
    unittest.main()
