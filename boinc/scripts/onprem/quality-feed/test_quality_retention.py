"""Tests for the quality retention step, with no database or production state."""
import hashlib
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
        self.eligible = self.base / "eligible.jsonl"
        self.journal = self.base / "journal.jsonl"
        self.eligible.write_text("")

    def unit(self, n, docs, page=b"bundle-bytes", copy=None, age=OLD):
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
        with open(self.eligible, "a") as f:
            f.write(json.dumps({"wu_id": n, "wu_name": wu, "file": name}) + "\n")
        return path, unit

    def prune(self, *extra):
        argv = ["prune", "--eligible", str(self.eligible), "--download", str(self.download),
                "--quality-root", str(self.quality), "--journal", str(self.journal),
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

    def test_deletes_a_copy_that_is_a_hard_link_to_the_unit(self):
        path, unit = self.l.unit(1, DOCS)
        path.unlink()
        os.link(unit / "page.jpg", path)
        got = self.l.prune("--delete")
        self.assertEqual((got["files"], got["bytes"]), (1, 0))
        self.assertFalse(path.exists())
        self.assertEqual((unit / "page.jpg").read_bytes(), b"bundle-bytes")

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

    def test_byte_limit_is_checked_before_each_file(self):
        path, _ = self.l.unit(1, DOCS)
        got = self.l.prune("--delete", "--max-bytes", "5")
        self.assertEqual(got["files"], 0)
        self.assertTrue(path.exists())

    def test_leaves_single_page_units(self):
        path, unit = self.l.unit(1, DOCS[:1])
        job = json.loads((unit / "quality.json").read_text())
        job["pages"] = []
        (unit / "quality.json").write_text(json.dumps(job))
        got = self.l.prune("--delete")
        self.assertEqual(got["kept"], {"not a bundle": 1})
        self.assertTrue(path.exists())

    def test_skip_cmp_is_refused_with_delete(self):
        self.l.unit(1, DOCS)
        with self.assertRaises(SystemExit):
            self.l.prune("--delete", "--skip-cmp")

    def test_a_pruned_unit_still_frees_its_download_copy(self):
        path, unit = self.l.unit(1, DOCS)
        job = json.loads((unit / "quality.json").read_text())
        job["inputs"]["page"]["sha256"] = hashlib.sha256(b"bundle-bytes").hexdigest()
        (unit / "quality.json").write_text(json.dumps(job))
        (unit / "page.jpg").unlink()
        (unit / "dims.json").write_text("{}")
        self.assertEqual(self.l.prune("--skip-cmp")["files"], 1)
        got = self.l.prune("--delete")
        self.assertEqual((got["files"], got["bytes"]), (1, len(b"bundle-bytes")))
        self.assertFalse(path.exists())

    def test_a_pruned_unit_keeps_a_download_copy_that_is_not_its_input(self):
        path, unit = self.l.unit(1, DOCS)
        job = json.loads((unit / "quality.json").read_text())
        job["inputs"]["page"]["sha256"] = "0" * 64
        (unit / "quality.json").write_text(json.dumps(job))
        (unit / "page.jpg").unlink()
        (unit / "dims.json").write_text("{}")
        got = self.l.prune("--delete")
        self.assertEqual(got["kept"], {"bytes differ from the job's page input": 1})
        self.assertTrue(path.exists())


def jpeg(width, height, pad=0):
    """The first bytes of a baseline JPEG: SOI, an APP0 block of `pad` bytes, SOF0."""
    app = b"\xff\xe0" + (pad + 2).to_bytes(2, "big") + b"\0" * pad
    sof = (b"\xff\xc0\x00\x0b\x08" + height.to_bytes(2, "big") + width.to_bytes(2, "big")
           + b"\x01\x01\x11\x00")
    return b"\xff\xd8" + app + sof + b"\xff\xda\x00\x02" + b"scan" * 8 + b"\xff\xd9"


class UnitLayout:
    """A campaign's bundle units and the partial loader's state, as prune-units reads them."""

    def __init__(self, base):
        self.base = Path(base)
        self.quality = self.base / "quality"
        self.results = self.base / "scoped"
        self.journal = self.base / "journal.jsonl"
        self.inventory = {}
        self.loaded = {}
        self.generated_at = time.time()
        self.scans = self.base / "scans"
        self.index = []

    def unit(self, n, sizes=((1000, 1400), (1200, 1680)), loaded=True, dims=True, age=None,
             passthrough=()):
        docs = [f"sn{n}_1900-01-01_ed-1_seq-{i + 1}" for i in range(len(sizes))]
        name = f"bundle-{n:05d}-{docs[0]}"
        unit = self.quality / "camp" / "units" / name
        unit.mkdir(parents=True)
        blob, pages = b"", []
        for doc, (w, h) in zip(docs, sizes):
            page = jpeg(w, h, pad=n * 7)
            pages.append({"doc_id": doc, "inputs": {"page": {
                "bytes": len(page), "offset": len(blob),
                "sha256": hashlib.sha256(page).hexdigest(), "source": "x"}}})
            blob += page
        for i, p in enumerate(pages):
            sha = p["inputs"]["page"]["sha256"] if i in passthrough else "f" * 64
            self.index.append({"doc_id": p["doc_id"], "batch": "b_x", "sha256": sha,
                               "bytes": p["inputs"]["page"]["bytes"]})
        job_id = f"{n:064x}"
        (unit / "page.jpg").write_bytes(blob)
        (unit / "submitted").write_text(f"usnm_quality_{job_id}")
        job = {"id": job_id, "doc_id": docs[0], "pages": pages,
               "inputs": {"page": {"bytes": len(blob)}}}
        (unit / "quality.json").write_text(json.dumps(job))
        if dims:
            record = {"job": job_id, "v": 1, "pages": [
                {"doc_id": p["doc_id"], "height": h, "sha256": p["inputs"]["page"]["sha256"],
                 "width": w} for p, (w, h) in zip(pages, sizes)]}
            (unit / "dims.json").write_text(json.dumps(record))
            t = age if age is not None else self.generated_at - 3 * qr.RECORD_SETTLE_SECS
            os.utime(unit / "dims.json", (t, t))
        fp = f"{n:064x}"
        self.inventory[name] = {"status": "canonical-present", "fingerprint": fp}
        if loaded:
            self.loaded[name] = fp
        return unit

    def prune(self, *extra):
        state = self.results / "camp"
        state.mkdir(parents=True, exist_ok=True)
        (state / "inventory.json").write_text(json.dumps(
            {"schema": 1, "generated_at": self.generated_at, "units": self.inventory}))
        (state / "frontier.json").write_text(json.dumps({"schema": 1, "loaded": self.loaded}))
        index = self.base / "index.jsonl"
        index.write_text("".join(json.dumps(r) + "\n" for r in self.index))
        argv = ["prune-units", "--quality-root", str(self.quality), "--campaign", "camp",
                "--results-root", str(self.results), "--journal", str(self.journal),
                "--scan-index", str(index), "--scan-root", str(self.scans),
                "--commit", "abc123", *extra]
        out = []
        real = sys.stdout.write
        sys.stdout.write = out.append
        try:
            qr.main(argv)
        finally:
            sys.stdout.write = real
        return json.loads("".join(out))


class PruneUnitsTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.l = UnitLayout(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def test_jpeg_size_reads_the_frame_header(self):
        self.assertEqual(qr.jpeg_size(jpeg(1000, 1400, pad=300)), (1000, 1400))
        self.assertIsNone(qr.jpeg_size(b"not a jpeg"))
        self.assertIsNone(qr.jpeg_size(jpeg(1000, 1400, pad=300)[:40]))

    def test_dry_run_changes_nothing(self):
        unit = self.l.unit(1)
        got = self.l.prune()
        self.assertEqual((got["mode"], got["units"]), ("dry-run", 1))
        self.assertEqual(got["bytes"], (unit / "page.jpg").stat().st_size)
        self.assertTrue((unit / "page.jpg").exists())
        self.assertFalse(self.l.journal.exists() and self.l.journal.read_text())

    def test_delete_removes_only_page_jpg_and_journals_it(self):
        unit = self.l.unit(1)
        got = self.l.prune("--delete")
        self.assertEqual(got["units"], 1)
        self.assertFalse((unit / "page.jpg").exists())
        for f in ("dims.json", "quality.json", "submitted"):
            self.assertTrue((unit / f).exists())
        rows = [json.loads(x) for x in self.l.journal.read_text().splitlines()]
        self.assertEqual([r["action"] for r in rows], ["deleting", "deleted"])
        self.assertEqual((rows[1]["unit"], rows[1]["commit"]), (unit.name, "abc123"))
        self.assertEqual(self.l.prune("--delete")["kept"], {"already pruned": 1})

    def test_keeps_units_that_are_not_ready(self):
        self.l.unit(1, dims=False)
        self.l.unit(2, loaded=False)
        self.l.unit(3, age=self.l.generated_at - 60)
        u4 = self.l.unit(4)
        self.l.inventory[u4.name]["status"] = "pending"
        u5 = self.l.unit(5)
        (u5 / "submitted").unlink()
        got = self.l.prune("--delete")
        self.assertEqual(got["units"], 0)
        self.assertEqual(got["kept"], {
            "no dims.json": 1, "not loaded at its current fingerprint": 1,
            "inventory not newer than dims.json": 1,
            "no canonical result in the inventory": 1, "not submitted": 1})
        self.assertEqual(len(list(self.l.quality.rglob("page.jpg"))), 5)

    def test_keeps_a_unit_whose_record_does_not_match(self):
        u1 = self.l.unit(1)
        record = json.loads((u1 / "dims.json").read_text())
        record["pages"][1]["width"] += 1
        t = (u1 / "dims.json").stat().st_mtime
        (u1 / "dims.json").write_text(json.dumps(record))
        os.utime(u1 / "dims.json", (t, t))
        u2 = self.l.unit(2)
        record = json.loads((u2 / "dims.json").read_text())
        record["pages"][0]["sha256"] = "0" * 64
        (u2 / "dims.json").write_text(json.dumps(record))
        u3 = self.l.unit(3)
        with open(u3 / "page.jpg", "ab") as f:
            f.write(b"more")
        got = self.l.prune("--delete")
        self.assertEqual(got["kept"], {
            "page header differs from dims.json": 1, "dims.json is not this job's": 1,
            "page.jpg is not the size quality.json gives": 1})
        self.assertEqual(len(list(self.l.quality.rglob("page.jpg"))), 3)

    def test_leaves_single_page_units(self):
        unit = self.l.unit(1)
        job = json.loads((unit / "quality.json").read_text())
        job["pages"] = []
        (unit / "quality.json").write_text(json.dumps(job))
        self.assertEqual(self.l.prune("--delete")["kept"], {"not a bundle": 1})

    def test_limits(self):
        for n in (1, 2, 3):
            self.l.unit(n)
        self.assertEqual(self.l.prune("--delete", "--max-units", "2")["units"], 2)
        size = (next(self.l.quality.rglob("page.jpg"))).stat().st_size
        self.assertEqual(self.l.prune("--delete", "--max-bytes", str(size - 1))["units"], 0)
        self.assertEqual(len(list(self.l.quality.rglob("page.jpg"))), 1)

    def test_keeps_loc_bytes_without_their_original(self):
        u1 = self.l.unit(1, passthrough=(1,))
        u2 = self.l.unit(2, passthrough=(0,))
        u3 = self.l.unit(3)
        self.l.index = [r for r in self.l.index if not r["doc_id"].startswith("sn3_")]
        # Unit 2's passed-through page has its original on disk at its size.
        r = next(r for r in self.l.index if r["doc_id"] == "sn2_1900-01-01_ed-1_seq-1")
        orig = self.l.scans / "keep" / "b_x" / "sn2" / "1900-01-01" / "1" / "1.jpg"
        orig.parent.mkdir(parents=True)
        orig.write_bytes(b"x" * r["bytes"])
        got = self.l.prune("--delete")
        self.assertEqual(got["units"], 1)
        self.assertEqual(got["kept"], {
            "a page is LoC's own JPEG and its original is not on disk": 1,
            "a page's scan is not in the scan index": 1})
        self.assertTrue((u1 / "page.jpg").exists() and (u3 / "page.jpg").exists())
        self.assertFalse((u2 / "page.jpg").exists())

    def test_a_linked_page_counts_no_bytes(self):
        unit = self.l.unit(1)
        os.link(unit / "page.jpg", self.l.base / "linked")
        got = self.l.prune("--delete")
        self.assertEqual((got["units"], got["bytes"]), (1, 0))
        self.assertTrue((self.l.base / "linked").exists())


if __name__ == "__main__":
    unittest.main()
