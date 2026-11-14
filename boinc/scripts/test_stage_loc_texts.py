import bz2
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import time
import unittest
import urllib.error
from types import SimpleNamespace
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("stage", ROOT / "boinc/scripts/stage_loc_texts.py")
S = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(S)

NOW = 1763121600.0  # 2026-11-14 12:00 UTC


def page(n, batch="nn_carson", sn="sn83030272", date="1876-10-17", seq=4):
    return {"job": f"j{n}", "host": "1", "page": f"{sn}/{date}/ed-1/seq-{seq}", "batch": batch}


def fake_check(pages):
    return SimpleNamespace(selected_images=lambda trust, image: ["sha256:x"],
                           returned_pages=lambda *a, **k: pages,
                           sample=lambda pgs, date, count: pgs[:count])


class StageTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        t = Path(self.tmp.name)
        self.handoff, self.scans, self.state = t / "h", t / "s", t / "st"
        self.out, self.archives = t / "out", t / "arc"
        for d in (self.handoff / "spool", self.handoff / "dispatch/jobs", self.scans, self.state, self.archives):
            d.mkdir(parents=True)

    def args(self, **kw):
        a = dict(handoff=self.handoff, scans=self.scans, state_dir=self.state, out=self.out,
                 archives=self.archives, trust_file=Path("unused"), image=None, hosts=None, count=10,
                 archive_budget_secs=60, loc_max=10, no_loc_gov=False)
        a.update(kw)
        return SimpleNamespace(**a)

    def make_archive(self, name, members):
        buf = io.BytesIO()
        with tarfile.open(fileobj=buf, mode="w") as tar:
            for member, text in members.items():
                data = text.encode()
                info = tarfile.TarInfo(member)
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
        (self.archives / f"{name}.tar.bz2").write_bytes(bz2.compress(buf.getvalue()))

    def run_stage(self, pages, **kw):
        sleeps = []
        counts = S.stage(self.args(**kw), check=fake_check(pages), sleep=sleeps.append, now=NOW)
        return counts, sleeps

    def test_empty_sample_is_clean(self):
        counts, _ = self.run_stage([])
        self.assertEqual(counts, dict.fromkeys(S.SOURCES, 0))
        self.assertFalse(self.out.exists())

    def test_missing_handoff_stages_nothing(self):
        (self.handoff / "spool").rmdir()
        counts, _ = self.run_stage([page(1)])
        self.assertEqual(sum(counts.values()), 0)

    def test_invalid_pages_are_not_staged(self):
        p = page(1)
        p["invalid"] = "text is not UTF-8"
        counts, _ = self.run_stage([p], no_loc_gov=True)
        self.assertEqual(sum(counts.values()), 0)

    def test_local_sidecar_first(self):
        d = self.scans / "keep/nn_carson/sn83030272/1876-10-17/1"
        d.mkdir(parents=True)
        (d / "4.txt").write_text("local text")
        self.make_archive("nn_carson_ver02", {"sn83030272/1876/10/17/ed-1/seq-4/ocr.txt": "archive text"})
        counts, _ = self.run_stage([page(1)])
        self.assertEqual(counts["local"], 1)
        self.assertEqual(counts["archive"], 0)
        self.assertEqual((self.out / "sn83030272/1876-10-17/ed-1/seq-4.txt").read_text(), "local text")

    def test_archive_one_pass_for_all_pages_and_highest_version(self):
        self.make_archive("nn_carson_ver01", {"sn83030272/1876/10/17/ed-1/seq-4/ocr.txt": "old"})
        self.make_archive("nn_carson_ver02", {
            "sn83030272/1876/10/17/ed-1/seq-1/ocr.txt": "one",
            "sn83030272/1876/10/17/ed-1/seq-1/ocr.xml": "<x/>",
            "sn83030272/1876/10/17/ed-1/seq-4/ocr.txt": "four"})
        counts, sleeps = self.run_stage([page(1, seq=4), page(2, seq=1)])
        self.assertEqual((counts["archive"], counts["missing"]), (2, 0))
        self.assertEqual(sleeps, [])
        self.assertEqual((self.out / "sn83030272/1876-10-17/ed-1/seq-4.txt").read_text(), "four")
        self.assertEqual((self.out / "sn83030272/1876-10-17/ed-1/seq-1.txt").read_text(), "one")

    def test_a_kept_file_is_counted_and_not_rewritten(self):
        f = self.out / "sn83030272/1876-10-17/ed-1/seq-4.txt"
        f.parent.mkdir(parents=True)
        f.write_text("kept")
        counts, _ = self.run_stage([page(1)], no_loc_gov=True)
        self.assertEqual(counts["have"], 1)
        self.assertEqual(f.read_text(), "kept")

    def test_loc_gov_last_resort_waits_and_keeps_answers(self):
        item = json.dumps({"resources": [{"files": [[{"mimetype": "image/jp2"}]] * 3 + [[
            {"mimetype": "image/jp2"},
            {"fulltext_service": "https://tile.loc.gov/text-services/x?segment=/a/0787.xml&full_text=1"}]]}]})
        ft = json.dumps({"/a/0787.xml": {"full_text": "from loc", "height": 1, "width": 1}})
        seen = []

        def fake_open(req, timeout):
            seen.append((req.full_url, req.get_header("User-agent"), timeout))
            body = item if "/item/" in req.full_url else ft
            return mock.MagicMock(__enter__=lambda s: s, __exit__=lambda *a: False,
                                  read=lambda n=-1: body.encode())

        with mock.patch.object(S.urllib.request, "urlopen", fake_open):
            counts, sleeps = self.run_stage([page(1)])
        self.assertEqual((counts["loc.gov"], counts["missing"]), (1, 0))
        self.assertEqual(sleeps, [5.0, 5.0])
        self.assertTrue(seen[0][0].startswith("https://www.loc.gov/item/sn83030272/1876-10-17/ed-1/"))
        self.assertTrue(all(ua == S.USER_AGENT and t for _, ua, t in seen))
        self.assertEqual((self.out / "sn83030272/1876-10-17/ed-1/seq-4.txt").read_text(), "from loc")
        self.assertTrue((self.out / "_loc-gov/sn83030272/1876-10-17/ed-1/seq-4.json").is_file())

    def test_pushback_ends_loc_gov_for_the_run(self):
        calls = []

        def fake_open(req, timeout):
            calls.append(req.full_url)
            raise urllib.error.HTTPError(req.full_url, 429, "slow down", {}, None)

        with mock.patch.object(S.urllib.request, "urlopen", fake_open):
            counts, _ = self.run_stage([page(1), page(2, seq=5)])
        self.assertEqual(len(calls), 1)
        self.assertEqual((counts["loc.gov"], counts["missing"]), (0, 2))

    def test_no_loc_gov_flag_and_cap(self):
        with mock.patch.object(S.urllib.request, "urlopen", side_effect=AssertionError("network")):
            counts, _ = self.run_stage([page(1)], no_loc_gov=True)
            self.assertEqual(counts["missing"], 1)
            counts, _ = self.run_stage([page(2, seq=6)], loc_max=0)
            self.assertEqual(counts["missing"], 1)

    def test_corrupt_archive_falls_through(self):
        (self.archives / "nn_carson_ver02.tar.bz2").write_bytes(b"not bzip2")
        counts, _ = self.run_stage([page(1)], no_loc_gov=True)
        self.assertEqual(counts["missing"], 1)

    def test_main_fails_soft(self):
        with mock.patch.object(S, "stage", side_effect=RuntimeError("boom")):
            self.assertEqual(S.main(["--handoff", "h", "--scans", "s", "--state-dir", "d", "--out", "o",
                                     "--archives", "a", "--trust-file", "t"]), 0)

    def test_real_check_with_no_returns_is_an_empty_sample(self):
        trust = ROOT / "boinc/project/native-build-trust.json"
        a = self.args(trust_file=trust, check_script=ROOT / "boinc/scripts/native_crossbuild_check.py")
        counts = S.stage(a, now=time.time())
        self.assertEqual(sum(counts.values()), 0)


if __name__ == "__main__":
    unittest.main()
