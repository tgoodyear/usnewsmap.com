"""scripts/data-assets.sh and ops/data-assets.json: the sets, the index and the pack layout."""

import json
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "data-assets.sh"
INDEX = ROOT / "ops" / "data-assets.json"
SETS = ("bench-assets", "report-data")


def run(*args, check=False):
    return subprocess.run([str(SCRIPT), *args], capture_output=True, text=True, check=check)


class DataAssets(unittest.TestCase):
    def test_the_index_names_every_set_with_sorted_keys_and_a_digest_or_nothing(self):
        text = INDEX.read_text(encoding="utf-8")
        index = json.loads(text)
        self.assertEqual(sorted(index), list(SETS))
        self.assertEqual(json.dumps(index, indent=2, sort_keys=True) + "\n", text, "keys sorted, two-space indent")
        for name, entry in index.items():
            self.assertEqual(sorted(entry), ["asset", "members", "sha256", "tag"])
            self.assertEqual(entry["asset"], f"{name}.tar.gz")
            self.assertEqual((entry["tag"] is None), (entry["sha256"] is None), "a tag comes with its digest")
            if entry["sha256"] is not None:
                self.assertRegex(entry["sha256"], r"^[0-9a-f]{64}$")
                self.assertRegex(entry["tag"], rf"^data-{name}-\d{{8}}-[0-9a-f]{{7,}}$")

    def test_members_are_tracked_files_of_the_right_kind(self):
        for name in SETS:
            with self.subTest(name):
                members = run("--members", "--set", name, check=True).stdout.split()
                tracked = set(subprocess.run(["git", "-C", str(ROOT), "ls-files"], capture_output=True, text=True, check=True).stdout.split())
                for m in members:
                    self.assertIn(m, tracked)
                    self.assertFalse(re.search(r"\.(md|py|sh|css|svg)$", m), m)
                    self.assertTrue(m.startswith(("docs/reports/", "docs/data/") if name == "report-data" else "ocr/bench/"), m)

    def test_pack_writes_a_tarball_its_members_and_digests(self):
        # --previous none: the tracked members alone, with no network.
        for name in SETS:
            with self.subTest(name):
                members = run("--members", "--set", name, check=True).stdout.split()
                with tempfile.TemporaryDirectory() as d:
                    done = run("--pack", "--set", name, "--out", d, "--previous", "none")
                    if not members:
                        self.assertNotEqual(done.returncode, 0)
                        self.assertIn("is empty", done.stderr)
                        continue
                    self.assertEqual(done.returncode, 0, done.stderr)
                    listed = subprocess.run(["tar", "-tzf", f"{d}/{name}.tar.gz"], capture_output=True, text=True, check=True).stdout.split()
                    self.assertEqual(listed, members)
                    self.assertEqual(Path(d, f"{name}.members.txt").read_text().split(), members)
                    sums = Path(d, "SHA256SUMS").read_text()
                    self.assertRegex(sums, rf"^[0-9a-f]{{64}}  {name}\.tar\.gz\n[0-9a-f]{{64}}  {name}\.members\.txt\n$")
                    # Reproducible across checkouts: the same bytes when a member's mode differs
                    # (a checkout under another umask), and when packed again.
                    victim = ROOT / members[0]
                    mode = victim.stat().st_mode & 0o777
                    try:
                        victim.chmod(mode | 0o020)
                        with tempfile.TemporaryDirectory() as e:
                            run("--pack", "--set", name, "--out", e, "--previous", "none", check=True)
                            self.assertEqual(Path(e, "SHA256SUMS").read_text(), sums)
                    finally:
                        victim.chmod(mode)

    def test_pack_is_cumulative_over_the_previous_release(self):
        # A previous tarball holding a file the tree no longer tracks, and an older copy of a
        # tracked one: the pack keeps the first and takes the tree's version of the second.
        name = "report-data"
        members = run("--members", "--set", name, check=True).stdout.split()
        if not members:
            self.skipTest("no tracked report data in this checkout")
        kept, replaced = "docs/reports/2000-01-old/data/old.csv", members[0]
        with tempfile.TemporaryDirectory() as prev, tempfile.TemporaryDirectory() as out:
            for rel, text in ((kept, "a,b\n1,2\n"), (replaced, "stale\n")):
                Path(prev, "tree", rel).parent.mkdir(parents=True, exist_ok=True)
                Path(prev, "tree", rel).write_text(text)
            subprocess.run(["tar", "-C", f"{prev}/tree", "-czf", f"{prev}/previous.tar.gz", "."], check=True)
            done = run("--pack", "--set", name, "--out", out, "--previous", f"{prev}/previous.tar.gz")
            self.assertEqual(done.returncode, 0, done.stderr)
            packed = Path(out, f"{name}.members.txt").read_text().split()
            self.assertEqual(packed, sorted(set(members) | {kept}))
            self.assertIn(f"files (2 from the previous release, {len(members)} tracked)", done.stdout)
            subprocess.run(["tar", "-C", out, "-xzf", f"{out}/{name}.tar.gz", replaced], check=True)
            self.assertEqual(Path(out, replaced).read_bytes(), (ROOT / replaced).read_bytes())

    def test_fetch_refuses_an_unpublished_set_and_bad_flags(self):
        for name in SETS:
            if json.loads(INDEX.read_text())[name]["tag"] is None:
                done = run("--fetch", "--set", name)
                self.assertNotEqual(done.returncode, 0)
                self.assertIn("not published yet", done.stderr)
        for args in (("--set", "report-data"), ("--pack", "--set", "report-data"), ("--members",), ("--members", "--set", "other"),
                     ("--pack", "--set", "report-data", "--out", "/nonexistent/x", "--previous", "/nonexistent/previous.tar.gz")):
            with self.subTest(args):
                self.assertNotEqual(run(*args).returncode, 0)


if __name__ == "__main__":
    unittest.main()
