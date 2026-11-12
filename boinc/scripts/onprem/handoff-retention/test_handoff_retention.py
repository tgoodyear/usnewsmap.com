"""Tests for the hand-off retention step, with no BOINC state."""
import contextlib
import hashlib
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
        self.confirmed_path = Path(base) / "confirmed.txt"
        # The digests of scans the storage host has confirmed another copy of.
        self.confirmed = set()

    def bundle(self, n, pages=b"scans", age=OLD, scans=None, confirm=True):
        """A bundle holding `scans` (default: one scan, `pages`) back to back, with a manifest."""
        scans = scans or [pages]
        bid = f"{n:064x}"
        d = self.root / "bundles" / bid
        d.mkdir()
        entries, offset = [], 0
        for scan in scans:
            sha = hashlib.sha256(scan).hexdigest()
            entries.append({"job": {"scan": {"sha256": sha, "bytes": len(scan)}}, "offset": offset})
            offset += len(scan)
            if confirm:
                self.confirmed.add(sha)
        (d / "ocr_bundle.json").write_text(json.dumps({"v": 1, "id": bid, "pages": entries}))
        (d / "pages").write_bytes(b"".join(scans))
        os.utime(d / "pages", (age, age))
        return bid, d

    def write_confirmed(self, age=0):
        shas = sorted(self.confirmed)
        header = {"v": 1, "confirmed": len(shas)}
        self.confirmed_path.write_text("# " + json.dumps(header) + "\n" + "".join(x + "\n" for x in shas))
        if age:
            t = time.time() - age
            os.utime(self.confirmed_path, (t, t))

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
        if not self.confirmed_path.exists():
            self.write_confirmed()
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            hr.main(["prune-bundles", "--root", str(self.root), "--journal", str(self.journal),
                     "--confirmed", str(self.confirmed_path), *extra])
        return json.loads(out.getvalue())

    def journal_lines(self):
        return [json.loads(x) for x in self.journal.read_text().splitlines()]


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

    def test_a_record_that_is_not_a_job_stops_the_run(self):
        _, d = self.r.bundle(1)
        for body in ("null", "[]", '"x"', "{}", '{"stage": "submitted", "bundle": 5}'):
            job = self.r.job(1)
            (job / "record.json").write_text(body)
            with self.assertRaises(SystemExit, msg=body):
                self.r.run("--delete")
            self.assertTrue((d / "pages").exists())
            (job / "record.json").unlink()
            (job / "page").unlink()
            job.rmdir()

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

    def test_a_sha_named_file_or_link_in_bundles_is_kept_and_the_run_goes_on(self):
        (self.r.root / "bundles" / f"{90:064x}").write_text("x")
        os.symlink(self.tmp.name, self.r.root / "bundles" / f"{91:064x}")
        _, d = self.r.bundle(1)
        summary = self.r.run("--delete")
        self.assertEqual(summary["bundles"], 1)
        self.assertEqual(summary["kept"], {"bundle directory is not a directory": 2})
        self.assertFalse((d / "pages").exists())

    def test_refuses_something_that_is_not_a_handoff_root(self):
        with self.assertRaises(SystemExit):
            hr.main(["prune-bundles", "--root", self.tmp.name, "--journal", "/dev/null",
                     "--confirmed", str(self.r.confirmed_path)])
        with self.assertRaises(SystemExit):
            self.r.run("--min-age-hours", "0")

    def test_a_scan_with_no_other_copy_keeps_the_bundle_and_is_journalled_once(self):
        ok, d_ok = self.r.bundle(1, scans=[b"one", b"two"])
        lone, d_lone = self.r.bundle(2, scans=[b"three", b"four"], confirm=False)
        self.r.confirmed.add(hashlib.sha256(b"three").hexdigest())  # one of its two scans only
        summary = self.r.run("--delete")
        self.assertEqual(summary["bundles"], 1)
        self.assertEqual(summary["kept"], {"no other copy of a scan": 1})
        self.assertFalse((d_ok / "pages").exists())
        self.assertEqual((d_lone / "pages").read_bytes(), b"threefour")
        kept = [x for x in self.r.journal_lines() if x["action"] == "kept: no other copy"]
        self.assertEqual(len(kept), 1)
        self.assertEqual((kept[0]["bundle"], kept[0]["scans"], kept[0]["unconfirmed"]), (lone, 2, 1))
        self.assertEqual(kept[0]["unconfirmed_sha256"], [hashlib.sha256(b"four").hexdigest()])
        # A second run keeps it again without another journal line.
        self.r.run("--delete")
        kept = [x for x in self.r.journal_lines() if x["action"] == "kept: no other copy"]
        self.assertEqual(len(kept), 1)

    def test_nothing_confirmed_deletes_nothing(self):
        self.r.bundle(1, confirm=False)
        self.r.bundle(2, confirm=False)
        summary = self.r.run("--delete")
        self.assertEqual(summary["bundles"], 0)
        self.assertEqual(len(list((self.r.root / "bundles").glob("*/pages"))), 2)
        self.assertFalse(any(x["action"] in ("deleting", "deleted") for x in self.r.journal_lines()))

    def test_a_dry_run_reports_the_unconfirmed_and_journals_nothing(self):
        self.r.bundle(1, confirm=False)
        summary = self.r.run()
        self.assertEqual((summary["bundles"], summary["kept"]), (0, {"no other copy of a scan": 1}))
        self.assertFalse(self.r.journal.exists())

    def test_a_bundle_without_a_usable_manifest_is_kept(self):
        _, d1 = self.r.bundle(1)
        (d1 / "ocr_bundle.json").unlink()
        _, d2 = self.r.bundle(2)
        (d2 / "ocr_bundle.json").write_text("{not json")
        _, d3 = self.r.bundle(3)
        (d3 / "ocr_bundle.json").write_text(json.dumps({"pages": []}))
        summary = self.r.run("--delete")
        self.assertEqual(summary["bundles"], 0)
        self.assertEqual(sum(summary["kept"].values()), 3)
        for d in (d1, d2, d3):
            self.assertTrue((d / "pages").exists())

    def test_pages_that_differ_from_the_manifest_are_kept(self):
        _, d = self.r.bundle(1, scans=[b"abc", b"def"])
        (d / "pages").write_bytes(b"abcdeX")  # same size, other content
        os.utime(d / "pages", (OLD, OLD))
        _, d2 = self.r.bundle(2, scans=[b"ghi"])
        (d2 / "pages").write_bytes(b"ghij")  # other size
        os.utime(d2 / "pages", (OLD, OLD))
        summary = self.r.run("--delete")
        self.assertEqual(summary["bundles"], 0)
        self.assertTrue((d / "pages").exists() and (d2 / "pages").exists())
        self.assertEqual({x["action"] for x in self.r.journal_lines()},
                         {"kept: pages differs from its manifest's scans",
                          "kept: pages is not the size its manifest gives"})

    def test_a_manifest_that_does_not_cover_every_byte_once_keeps_the_bundle(self):
        # Two 3-byte entries both at offset 0 add up to the size of "abcXYZ", but XYZ is never
        # hashed or confirmed.
        _, d = self.r.bundle(1, scans=[b"abc"])
        (d / "pages").write_bytes(b"abcXYZ")
        os.utime(d / "pages", (OLD, OLD))
        entry = json.loads((d / "ocr_bundle.json").read_text())["pages"][0]
        (d / "ocr_bundle.json").write_text(json.dumps({"pages": [entry, dict(entry)]}))
        # A gap, a zero length and a late start are refused too.
        _, d2 = self.r.bundle(2, scans=[b"abc", b"def"])
        m2 = json.loads((d2 / "ocr_bundle.json").read_text())
        m2["pages"][1]["offset"] = 4
        (d2 / "ocr_bundle.json").write_text(json.dumps(m2))
        _, d3 = self.r.bundle(3, scans=[b"ghi"])
        m3 = json.loads((d3 / "ocr_bundle.json").read_text())
        m3["pages"].append({"job": {"scan": {"sha256": hashlib.sha256(b"").hexdigest(), "bytes": 0}}, "offset": 3})
        (d3 / "ocr_bundle.json").write_text(json.dumps(m3))
        summary = self.r.run("--delete")
        self.assertEqual(summary["bundles"], 0)
        self.assertEqual(summary["kept"], {"ocr_bundle.json does not lay its scans out end to end": 3})
        for d in (d, d2, d3):
            self.assertTrue((d / "pages").exists())

    def test_a_missing_stale_or_torn_confirmation_list_stops_the_run(self):
        _, d = self.r.bundle(1)
        with self.assertRaises(SystemExit):
            hr.main(["prune-bundles", "--root", str(self.r.root), "--journal", str(self.r.journal),
                     "--confirmed", str(self.r.confirmed_path), "--delete"])
        self.r.write_confirmed(age=30 * 3600)
        with self.assertRaises(SystemExit):
            self.r.run("--delete")
        self.r.write_confirmed()
        # A list cut short: its header counts more digests than follow.
        self.r.confirmed_path.write_text("# " + json.dumps({"v": 1, "confirmed": 99}) + "\n")
        with self.assertRaises(SystemExit):
            self.r.run("--delete")
        self.r.confirmed_path.write_text("not a header\n")
        with self.assertRaises(SystemExit):
            self.r.run("--delete")
        self.assertTrue((d / "pages").exists())
        self.assertFalse(self.r.journal.exists())


if __name__ == "__main__":
    unittest.main()
