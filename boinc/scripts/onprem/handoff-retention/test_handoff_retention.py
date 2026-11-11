"""Tests for the hand-off retention step, with no BOINC state."""
import contextlib
import importlib.util
import io
import json
import os
import tempfile
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("handoff_retention", HERE / "handoff_retention.py")
hr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hr)

OLD = time.time() - 5 * 3600


class Root:
    def __init__(self, base):
        self.root = Path(base) / "handoff"
        (self.root / "jobs").mkdir(parents=True)
        (self.root / "bundles").mkdir()
        self.journal = Path(base) / "journal.jsonl"

    def bundle(self, n, pages=b"scans", age=OLD):
        bid = f"{n:064x}"
        d = self.root / "bundles" / bid
        d.mkdir()
        (d / "ocr_bundle.json").write_text("{}")
        (d / "pages").write_bytes(pages)
        os.utime(d / "pages", (age, age))
        return bid, d

    def job(self, n, bundle=None, stage="submitted"):
        d = self.root / "jobs" / f"{n:064x}"
        d.mkdir()
        record = {"job": {}, "stage": stage}
        if bundle:
            record["bundle"] = bundle
        (d / "record.json").write_text(json.dumps(record))
        (d / "page").write_bytes(b"scan")
        return d

    def run(self, *extra):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            hr.main(["prune-bundles", "--root", str(self.root), "--journal", str(self.journal),
                     *extra])
        return json.loads(out.getvalue())


class PruneBundles(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.r = Root(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def test_dry_run_changes_nothing(self):
        _, d = self.r.bundle(1)
        summary = self.r.run()
        self.assertEqual((summary["bundles"], summary["deleted"]), (1, False))
        self.assertTrue((d / "pages").exists())
        self.assertFalse(self.r.journal.exists())

    def test_frees_pages_only_and_journals_it(self):
        bid, d = self.r.bundle(1, pages=b"x" * 100)
        job = self.r.job(1, bid)
        summary = self.r.run("--delete", "--commit", "abc")
        self.assertEqual((summary["bundles"], summary["deleted"]), (1, True))
        self.assertFalse((d / "pages").exists())
        self.assertTrue((d / "ocr_bundle.json").exists())
        self.assertTrue((job / "page").exists() and (job / "record.json").exists())
        lines = [json.loads(x) for x in self.r.journal.read_text().splitlines()]
        self.assertEqual([x["action"] for x in lines], ["deleting", "deleted"])
        self.assertEqual({x["commit"] for x in lines}, {"abc"})
        self.assertEqual(lines[0]["bytes"], 100)

    def test_a_bundle_no_journal_names_is_freed(self):
        _, d = self.r.bundle(1)
        self.r.job(9)
        self.assertEqual(self.r.run("--delete")["bundles"], 1)
        self.assertFalse((d / "pages").exists())

    def test_keeps_a_bundle_with_a_member_still_being_submitted(self):
        for n, stage in enumerate(["prepared", "reserved", "submitting"], 1):
            bid, d = self.r.bundle(n)
            self.r.job(n, bid, stage)
            self.r.job(n + 10, bid, "submitted")
        summary = self.r.run("--delete")
        self.assertEqual(summary["bundles"], 0)
        self.assertEqual(summary["kept"], {"a member is still before submitted": 3})

    def test_frees_when_every_member_is_submitted_or_later(self):
        bid, d = self.r.bundle(1)
        for n, stage in enumerate(["submitted", "published", "failed"], 1):
            self.r.job(n, bid, stage)
        self.assertEqual(self.r.run("--delete")["bundles"], 1)

    def test_a_new_bundle_is_kept(self):
        _, d = self.r.bundle(1, age=time.time() - 60)
        summary = self.r.run("--delete")
        self.assertEqual(summary["bundles"], 0)
        self.assertTrue((d / "pages").exists())

    def test_hard_linked_or_linked_pages_are_kept(self):
        _, d = self.r.bundle(1)
        os.link(d / "pages", Path(self.tmp.name) / "other")
        _, d2 = self.r.bundle(2)
        (d2 / "pages").unlink()
        os.symlink(Path(self.tmp.name) / "other", d2 / "pages")
        summary = self.r.run("--delete")
        self.assertEqual(summary["bundles"], 0)
        self.assertEqual(sum(summary["kept"].values()), 2)

    def test_an_unreadable_journal_stops_the_run_untouched(self):
        _, d = self.r.bundle(1)
        job = self.r.job(1)
        (job / "record.json").write_text("{not json")
        with self.assertRaises(SystemExit):
            self.r.run("--delete")
        self.assertTrue((d / "pages").exists())

    def test_a_job_directory_without_its_record_stops_the_run(self):
        _, d = self.r.bundle(1)
        job = self.r.job(1)
        (job / "record.json").unlink()
        with self.assertRaises(SystemExit):
            self.r.run("--delete")
        self.assertTrue((d / "pages").exists())
        (job / "record.json").symlink_to(Path(self.tmp.name) / "missing")
        with self.assertRaises(SystemExit):
            self.r.run("--delete")

    def test_a_stray_file_in_jobs_stops_the_run(self):
        _, d = self.r.bundle(1)
        (self.r.root / "jobs" / "stray").write_text("x")
        with self.assertRaises(SystemExit):
            self.r.run("--delete")
        self.assertTrue((d / "pages").exists())

    def test_bounds(self):
        for n in range(1, 5):
            self.r.bundle(n, pages=b"x" * 10)
        summary = self.r.run("--delete", "--max-bundles", "2")
        self.assertEqual((summary["bundles"], summary["stopped"]), (2, "bound reached"))
        summary = self.r.run("--delete", "--max-bytes", "15")
        self.assertEqual((summary["bundles"], summary["gib"] < 1), (1, True))
        self.assertEqual(sum(1 for p in (self.r.root / "bundles").glob("*/pages")), 1)

    def test_already_freed_and_odd_names_are_skipped(self):
        bid, d = self.r.bundle(1)
        (d / "pages").unlink()
        (self.r.root / "bundles" / "not-a-bundle").mkdir()
        self.assertEqual(self.r.run("--delete")["bundles"], 0)

    def test_refuses_something_that_is_not_a_handoff_root(self):
        with self.assertRaises(SystemExit):
            hr.main(["prune-bundles", "--root", self.tmp.name, "--journal", "/dev/null"])
        with self.assertRaises(SystemExit):
            self.r.run("--min-age-hours", "0")


if __name__ == "__main__":
    unittest.main()
