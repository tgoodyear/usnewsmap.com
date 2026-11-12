"""Tests for quality_receive.py's filing step, with no share or BOINC host (#1447)."""
import contextlib
import hashlib
import importlib.util
import io
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("quality_receive", HERE / "quality_receive.py")
qr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qr)

CAMPAIGN = "wave-x"
OLD_DOC = "sn11111111_1900-01-01_ed-1_seq-1"
NEW_DOCS = ["sn22222222_1901-02-03_ed-1_seq-1", "sn22222222_1901-02-03_ed-1_seq-2"]


class ReceiveTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.share = Path(tmp.name)
        self.camp = self.share / "quality" / CAMPAIGN
        (self.share / "quality" / "v1-wave9").mkdir(parents=True)
        (self.share / "quality" / "v1-wave9" / "places.txt").write_text("place\n")
        # Entries an earlier chunk's run left: filed, owned, and not to be looked at again.
        self.old = [self.camp / "scans" / f"{OLD_DOC}.jpg", self.camp / "texts" / f"{OLD_DOC}.json"]
        for p in self.old:
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text("old")
        self.other_chunk = self.camp / "incoming" / "c00001"
        self.other_chunk.mkdir(parents=True)
        (self.other_chunk / "sample.jsonl").write_text("{}\n")
        (self.camp / "units" / "u1").mkdir(parents=True)
        self.inc = self.camp / "incoming" / "c00002"
        for sub in ("scans", "texts"):
            (self.inc / sub).mkdir(parents=True)
        sums = []
        rows = []
        for d in NEW_DOCS:
            rows.append(json.dumps({"doc_id": d}))
            for rel, body in ((f"scans/{d}.jpg", b"jpg"), (f"scans/{d}.xml", b"xml"), (f"texts/{d}.json", b"{}")):
                (self.inc / rel).write_bytes(body)
                sums.append(f"{hashlib.sha256(body).hexdigest()}  {rel}")
        (self.inc / "sample.jsonl").write_text("\n".join(rows) + "\n")
        body = b"eng,fra"
        (self.inc / "langs.txt").write_bytes(body)
        sums.append(f"{hashlib.sha256(body).hexdigest()}  langs.txt")
        (self.inc / "SHA256SUMS").write_text("\n".join(sums) + "\n")
        (self.inc / "READY").write_text("")

    def run_receive(self):
        """Runs the receive with lchown recorded; reports every path chowned and lstat'd."""
        chowned, statted = [], []
        real_lstat = os.lstat

        def fake_lstat(path, *args, **kw):
            statted.append(str(path))
            info = real_lstat(path, *args, **kw)
            return os.stat_result((info.st_mode, info.st_ino, info.st_dev, info.st_nlink,
                                   0, 0, info.st_size, info.st_atime, info.st_mtime, info.st_ctime))

        with mock.patch.object(qr.os, "lchown", lambda p, u, g: chowned.append(str(p))), \
                mock.patch.object(qr.os, "lstat", fake_lstat), \
                contextlib.redirect_stdout(io.StringIO()):
            qr.main(["--campaign", CAMPAIGN, "--chunk", "c00002", "--share", str(self.share)])
        return chowned, statted

    def test_files_are_moved_and_everything_the_run_wrote_is_owned(self):
        chowned, _ = self.run_receive()
        for d in NEW_DOCS:
            for rel in (f"scans/{d}.jpg", f"scans/{d}.xml", f"texts/{d}.json"):
                self.assertTrue((self.camp / rel).is_file())
                self.assertFalse((self.inc / rel).exists())
                self.assertIn(str(self.camp / rel), chowned)
        for p in (self.camp, self.camp / "scans", self.camp / "texts", self.camp / "places.txt",
                  self.camp / "incoming", self.inc, self.inc / "sample.jsonl", self.inc / "langs.txt",
                  self.inc / "READY", self.inc / "SHA256SUMS"):
            self.assertIn(str(p), chowned)

    def test_entries_from_earlier_runs_are_not_walked_again(self):
        chowned, statted = self.run_receive()
        untouched = [str(p) for p in self.old] + [str(self.other_chunk), str(self.other_chunk / "sample.jsonl"),
                                                  str(self.camp / "units"), str(self.camp / "units" / "u1")]
        for p in untouched:
            self.assertNotIn(p, chowned)
            self.assertNotIn(p, statted)

    def test_an_owned_entry_is_not_chowned_again(self):
        with mock.patch.object(qr.os, "lchown") as lchown, \
                mock.patch.object(qr.os, "lstat") as lstat:
            lstat.return_value = mock.Mock(st_uid=qr.OWNER, st_gid=qr.OWNER)
            qr.own("/nowhere")
            lchown.assert_not_called()
            lstat.return_value = mock.Mock(st_uid=0, st_gid=0)
            qr.own("/nowhere")
            lchown.assert_called_once_with("/nowhere", qr.OWNER, qr.OWNER)

    def test_the_summary_line_is_unchanged(self):
        out = io.StringIO()
        with mock.patch.object(qr.os, "lchown"), contextlib.redirect_stdout(out):
            qr.main(["--campaign", CAMPAIGN, "--chunk", "c00002", "--share", str(self.share)])
        self.assertEqual(out.getvalue().strip(),
                         f"/project/quality/{CAMPAIGN}/incoming/c00002/sample.jsonl\teng,fra\t2")


if __name__ == "__main__":
    unittest.main()
