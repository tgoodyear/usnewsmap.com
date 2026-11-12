"""Tests for the scan confirmation list, with a throwaway scan tree."""
import contextlib
import importlib.util
import io
import json
import os
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("scan_confirm", HERE / "scan_confirm.py")
sc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sc)

SHA = {n: f"{n:064x}" for n in range(1, 12)}


class Tree:
    def __init__(self, base):
        self.base = Path(base)
        self.hot = self.base / "hot"
        self.cold = self.base / "cold"
        for root in (self.hot, self.cold):
            (root / "keep").mkdir(parents=True)
            (root / "buffer").mkdir()
        self.rows = []

    def scan(self, root, set_, n, *, batch="b_one", seq=1, ext="jp2", sidecar_sha=None, data=b"x" * 10,
             listed=True, fmt=None, link=None):
        doc = f"sn1_1890-01-0{n % 9 + 1}_ed-1_seq-{seq}"
        rel = Path(set_) / batch / "sn1" / f"1890-01-0{n % 9 + 1}" / "1"
        if link:  # the batch directory is a symlink into another root, as scan-cold-mover leaves it
            d = root / set_ / batch
            if not d.exists() and not d.is_symlink():
                os.symlink(link / set_ / batch, d)
            real = link / rel
        else:
            real = root / rel
        real.mkdir(parents=True, exist_ok=True)
        (real / f"{seq}.{ext}").write_bytes(data)
        (real / f"{seq}.{ext}.json").write_text(json.dumps({"sha256": sidecar_sha or SHA[n]}))
        if listed:
            self.rows.append({"doc_id": doc, "batch": batch, "format": fmt or ("jp2" if ext == "jp2" else "jpeg"),
                              "sha256": SHA[n], "bytes": len(data)})

    def publish(self):
        index = self.base / "index.jsonl"
        index.write_text("".join(json.dumps(r) + "\n" for r in self.rows) + '{"torn": ')
        out = self.base / "out" / "confirmed.txt"
        out.parent.mkdir(exist_ok=True)
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            sc.main(["publish", "--index", str(index), "--root", str(self.hot), "--root", str(self.cold),
                     "--out", str(out), "--workers", "2"])
        lines = out.read_text().splitlines()
        return json.loads(lines[0][2:]), lines[1:], json.loads(buf.getvalue())


class Publish(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.t = Tree(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def test_keep_buffer_jpeg_and_cold_scans_are_confirmed(self):
        self.t.scan(self.t.hot, "keep", 1)
        self.t.scan(self.t.hot, "buffer", 2, batch="b_two")
        self.t.scan(self.t.hot, "keep", 3, ext="jpg")
        self.t.scan(self.t.cold, "buffer", 4, batch="b_cold")
        # moved to /cold, with a symlink left in the hot tree
        self.t.scan(self.t.hot, "buffer", 5, batch="b_moved", link=self.t.cold)
        header, shas, printed = self.t.publish()
        self.assertEqual(shas, sorted(SHA[n] for n in range(1, 6)))
        self.assertEqual((header["rows"], header["confirmed"], header["missing"]), (5, 5, 0))
        self.assertEqual(printed["step"], "scan-confirm")

    def test_a_listed_scan_with_no_file_is_left_out_and_counted(self):
        self.t.scan(self.t.hot, "keep", 1)
        self.t.scan(self.t.hot, "buffer", 2, listed=True)
        for p in (self.t.hot / "buffer").rglob("*.jp2*"):
            p.unlink()  # deleted before 8 November: the index still lists it
        header, shas, _ = self.t.publish()
        self.assertEqual(shas, [SHA[1]])
        self.assertEqual((header["confirmed"], header["missing"]), (1, 1))

    def test_a_file_of_another_size_or_digest_is_not_confirmed(self):
        self.t.scan(self.t.hot, "keep", 1, sidecar_sha=SHA[9])  # sidecar names another digest
        self.t.scan(self.t.hot, "keep", 2, batch="b_two")
        self.t.rows[-1]["bytes"] = 99  # the index gives another size
        self.t.scan(self.t.hot, "keep", 3, batch="b_three", data=b"")  # empty
        self.t.rows[-1]["bytes"] = 10
        header, shas, _ = self.t.publish()
        self.assertEqual(shas, [])
        self.assertEqual(header["missing"], 3)

    def test_a_missing_sidecar_is_not_confirmed(self):
        self.t.scan(self.t.hot, "keep", 1)
        (next((self.t.hot / "keep").rglob("*.json"))).unlink()
        self.assertEqual(self.t.publish()[1], [])

    def test_a_digest_listed_twice_is_confirmed_if_any_copy_is(self):
        self.t.scan(self.t.hot, "keep", 1)
        self.t.rows.append(dict(self.t.rows[0], doc_id="sn1_1890-01-02_ed-1_seq-9"))  # no such file
        header, shas, _ = self.t.publish()
        self.assertEqual((shas, header["missing"]), ([SHA[1]], 0))

    def test_odd_rows_are_missing_not_fatal_and_cannot_escape_the_root(self):
        self.t.scan(self.t.hot, "keep", 1)
        self.t.rows += [{"doc_id": "../../x_1890-01-01_ed-1_seq-1", "batch": "b", "sha256": SHA[2], "bytes": 1},
                        {"doc_id": "sn1_1890-01-01_ed-1_seq-1", "batch": "../keep/b_one", "sha256": SHA[3], "bytes": 1},
                        {"doc_id": None, "batch": 5, "sha256": "short", "bytes": "x"}]
        header, shas, _ = self.t.publish()
        self.assertEqual(shas, [SHA[1]])
        self.assertEqual(header["missing"], 3)

    def test_a_root_without_keep_or_buffer_is_refused(self):
        with self.assertRaises(SystemExit):
            sc.main(["publish", "--index", str(self.t.base / "i"), "--root", self.tmp.name,
                     "--out", str(self.t.base / "o")])

    def test_the_list_is_what_the_retention_step_reads(self):
        """The two halves agree on the format: handoff_retention accepts what this writes."""
        hr_spec = importlib.util.spec_from_file_location("handoff_retention", HERE / "handoff_retention.py")
        hr = importlib.util.module_from_spec(hr_spec)
        hr_spec.loader.exec_module(hr)
        self.t.scan(self.t.hot, "keep", 1)
        self.t.publish()
        import time
        digests = hr.load_confirmed(str(self.t.base / "out" / "confirmed.txt"), 3600, time.time())
        self.assertEqual(digests, {SHA[1]})


if __name__ == "__main__":
    unittest.main()
