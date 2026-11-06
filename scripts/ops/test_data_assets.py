"""scripts/ops/data-assets.sh and ops/data-assets.json: the sets, the index and the pack layout."""

import hashlib
import json
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
INDEX = ROOT / "ops" / "data-assets.json"
SETS = tuple(sorted(json.loads(INDEX.read_text(encoding="utf-8"))))
REPORT_SETS = tuple(name for name in SETS if name.startswith("report-") and name != "report-data")


def run(*args, root=ROOT, check=False):
    return subprocess.run([str(root / "scripts" / "ops" / "data-assets.sh"), *args], capture_output=True, text=True, check=check)


def fixture(base, approved_extra=None):
    root = Path(base, "repo")
    scripts = root / "scripts" / "ops"
    scripts.mkdir(parents=True)
    for name in ("data-assets.sh", "verify-report-data.py"):
        shutil.copy2(ROOT / "scripts" / "ops" / name, scripts / name)
    (root / "ops").mkdir()
    shutil.copy2(INDEX, root / "ops" / "data-assets.json")
    tracked = {
        "docs/reports/2001-01-new/data/new.csv": b"current\n",
        "docs/reports/report-data-rights.txt": b"synthetic fixture notice\n",
        "ocr/bench/fixture.png": b"synthetic fixture image\n",
    }
    for name, content in tracked.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
    approved = {name: content for name, content in tracked.items() if name.startswith("docs/")}
    approved.update(approved_extra or {})
    manifest = {"version": 1, "files": {name: hashlib.sha256(content).hexdigest() for name, content in sorted(approved.items())}}
    (root / "docs" / "reports" / "report-data-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    subprocess.run(["git", "init", "-q", str(root)], check=True)
    subprocess.run(["git", "-C", str(root), "add", "."], check=True)
    return root


class DataAssets(unittest.TestCase):
    def test_the_index_names_every_set_with_sorted_keys_and_a_digest_or_nothing(self):
        text = INDEX.read_text(encoding="utf-8")
        index = json.loads(text)
        self.assertTrue({"bench-assets", "report-data"} <= set(index))
        for name in REPORT_SETS:
            self.assertRegex(name, r"^report-\d{4}-\d{2}-[a-z0-9-]+$")
            self.assertTrue((ROOT / "docs" / "reports" / name.removeprefix("report-")).is_dir(), name)
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
                    if name in REPORT_SETS:
                        own = f"docs/reports/{name.removeprefix('report-')}/data/"
                        self.assertTrue(m.startswith(own) or m == "docs/reports/report-data-rights.txt", m)
                        continue
                    self.assertTrue(m.startswith(("docs/reports/", "docs/data/") if name == "report-data" else "ocr/bench/"), m)
                    for other in REPORT_SETS:
                        self.assertFalse(m.startswith(f"docs/reports/{other.removeprefix('report-')}/data/"), m)

    def test_pack_writes_a_tarball_its_members_and_digests(self):
        # An isolated tracked fixture, with no network or changes to the checkout.
        with tempfile.TemporaryDirectory() as repo:
            root = fixture(repo)
            for name in SETS:
                with self.subTest(name):
                    members = run("--members", "--set", name, root=root, check=True).stdout.split()
                    with tempfile.TemporaryDirectory() as d:
                        done = run("--pack", "--set", name, "--out", d, "--previous", "none", root=root)
                        self.assertEqual(done.returncode, 0, done.stderr)
                        listed = subprocess.run(["tar", "-tzf", f"{d}/{name}.tar.gz"], capture_output=True, text=True, check=True).stdout.split()
                        self.assertEqual(listed, members)
                        self.assertEqual(Path(d, f"{name}.members.txt").read_text().split(), members)
                        sums = Path(d, "SHA256SUMS").read_text()
                        self.assertRegex(sums, rf"^[0-9a-f]{{64}}  {name}\.tar\.gz\n[0-9a-f]{{64}}  {name}\.members\.txt\n$")
                        # A different checkout umask must not change the archive.
                        victim = root / members[0]
                        mode = victim.stat().st_mode & 0o777
                        try:
                            victim.chmod(mode | 0o020)
                            with tempfile.TemporaryDirectory() as e:
                                run("--pack", "--set", name, "--out", e, "--previous", "none", root=root, check=True)
                                self.assertEqual(Path(e, "SHA256SUMS").read_text(), sums)
                        finally:
                            victim.chmod(mode)

    def test_pack_is_cumulative_over_the_previous_release(self):
        # A previous tarball holding a file the tree no longer tracks, and an older copy of a
        # tracked one: the pack keeps the first and takes the tree's version of the second.
        name = "report-data"
        kept = "docs/reports/2000-01-old/data/old.csv"
        replaced = "docs/reports/2001-01-new/data/new.csv"
        with tempfile.TemporaryDirectory() as repo, tempfile.TemporaryDirectory() as prev, tempfile.TemporaryDirectory() as out:
            root = fixture(repo, {kept: b"a,b\n1,2\n"})
            members = run("--members", "--set", name, root=root, check=True).stdout.split()
            for rel, text in ((kept, "a,b\n1,2\n"), (replaced, "stale\n")):
                Path(prev, "tree", rel).parent.mkdir(parents=True, exist_ok=True)
                Path(prev, "tree", rel).write_text(text)
            subprocess.run(["tar", "-C", f"{prev}/tree", "-czf", f"{prev}/previous.tar.gz", "."], check=True)
            done = run("--pack", "--set", name, "--out", out, "--previous", f"{prev}/previous.tar.gz", root=root)
            self.assertEqual(done.returncode, 0, done.stderr)
            packed = Path(out, f"{name}.members.txt").read_text().split()
            self.assertEqual(packed, sorted(set(members) | {kept}))
            self.assertIn(f"files (2 from the previous release, {len(members)} tracked)", done.stdout)
            subprocess.run(["tar", "-C", out, "-xzf", f"{out}/{name}.tar.gz", replaced], check=True)
            self.assertEqual(Path(out, replaced).read_bytes(), (root / replaced).read_bytes())

    def test_report_repack_after_tracked_data_is_migrated(self):
        data = "docs/reports/2001-01-new/data/new.csv"
        with tempfile.TemporaryDirectory() as repo, tempfile.TemporaryDirectory() as first, tempfile.TemporaryDirectory() as second:
            root = fixture(repo)
            run("--pack", "--set", "report-data", "--out", first, "--previous", "none", root=root, check=True)
            subprocess.run(["git", "-C", str(root), "update-index", "--force-remove", data], check=True)
            (root / data).unlink()
            run("--pack", "--set", "report-data", "--out", second, "--previous", f"{first}/report-data.tar.gz", root=root, check=True)
            self.assertEqual(Path(first, "SHA256SUMS").read_text(), Path(second, "SHA256SUMS").read_text())

    def test_pack_rejects_an_empty_benchmark_set(self):
        with tempfile.TemporaryDirectory() as repo, tempfile.TemporaryDirectory() as out:
            root = fixture(repo)
            subprocess.run(["git", "-C", str(root), "update-index", "--force-remove", "ocr/bench/fixture.png"], check=True)
            done = run("--pack", "--set", "bench-assets", "--out", out, "--previous", "none", root=root)
            self.assertNotEqual(done.returncode, 0)
            self.assertIn("is empty", done.stderr)

    def test_report_pack_rejects_unapproved_or_changed_files(self):
        extra = "docs/reports/2000-01-old/data/unapproved.csv"
        with tempfile.TemporaryDirectory() as repo, tempfile.TemporaryDirectory() as prev, tempfile.TemporaryDirectory() as out:
            root = fixture(repo)
            Path(prev, "tree", extra).parent.mkdir(parents=True)
            Path(prev, "tree", extra).write_text("unreviewed\n")
            subprocess.run(["tar", "-C", f"{prev}/tree", "-czf", f"{prev}/previous.tar.gz", "."], check=True)
            done = run("--pack", "--set", "report-data", "--out", out, "--previous", f"{prev}/previous.tar.gz", root=root)
            self.assertNotEqual(done.returncode, 0)
            self.assertIn("unapproved files", done.stderr)
            self.assertFalse(Path(out, "report-data.tar.gz").exists())

            Path(root, "docs/reports/2001-01-new/data/new.csv").write_text("changed\n")
            done = run("--pack", "--set", "report-data", "--out", out, "--previous", "none", root=root)
            self.assertNotEqual(done.returncode, 0)
            self.assertIn("digest mismatch", done.stderr)
            self.assertFalse(Path(out, "report-data.tar.gz").exists())

    def test_report_pack_rejects_missing_approved_files(self):
        kept = "docs/reports/2000-01-old/data/old.csv"
        with tempfile.TemporaryDirectory() as repo, tempfile.TemporaryDirectory() as out:
            root = fixture(repo, {kept: b"old\n"})
            done = run("--pack", "--set", "report-data", "--out", out, "--previous", "none", root=root)
            self.assertNotEqual(done.returncode, 0)
            self.assertIn("missing approved files", done.stderr)

    def test_inventory_is_a_candidate_not_an_automatic_approval(self):
        name = "docs/reports/2001-01-new/data/new.csv"
        with tempfile.TemporaryDirectory() as repo, tempfile.TemporaryDirectory() as out:
            root = fixture(repo)
            (root / name).write_text("changed\n")
            done = run("--inventory", "--set", "report-data", "--out", out, "--previous", "none", root=root)
            self.assertEqual(done.returncode, 0, done.stderr)
            candidate = Path(out, "report-data-manifest.json")
            self.assertEqual(json.loads(candidate.read_text())["files"][name], hashlib.sha256(b"changed\n").hexdigest())
            self.assertFalse(Path(out, "report-data.tar.gz").exists())
            self.assertNotEqual(run("--pack", "--set", "report-data", "--out", out, "--previous", "none", root=root).returncode, 0)
            shutil.copyfile(candidate, root / "docs" / "reports" / "report-data-manifest.json")
            done = run("--pack", "--set", "report-data", "--out", out, "--previous", "none", root=root)
            self.assertEqual(done.returncode, 0, done.stderr)

    def test_a_report_set_takes_its_data_out_of_report_data(self):
        with tempfile.TemporaryDirectory() as repo:
            root = fixture(repo)
            index_path = root / "ops" / "data-assets.json"
            index = json.loads(index_path.read_text())
            index["report-2001-01-new"] = {"asset": "report-2001-01-new.tar.gz", "members": "fixture", "sha256": None, "tag": None}
            index_path.write_text(json.dumps(index, indent=2, sort_keys=True) + "\n")
            extra = root / "docs/reports/2001-01-new/data/sample.ids"
            extra.write_text("a\n")
            subprocess.run(["git", "-C", str(root), "add", "."], check=True)
            own = run("--members", "--set", "report-2001-01-new", root=root, check=True).stdout.split()
            self.assertEqual(own, ["docs/reports/2001-01-new/data/new.csv", "docs/reports/2001-01-new/data/sample.ids",
                                   "docs/reports/report-data-rights.txt"])
            shared = run("--members", "--set", "report-data", root=root, check=True).stdout.split()
            self.assertNotIn("docs/reports/2001-01-new/data/new.csv", shared)
            with tempfile.TemporaryDirectory() as d:
                done = run("--pack", "--set", "report-2001-01-new", "--out", d, root=root)
                self.assertEqual(done.returncode, 0, done.stderr)
                self.assertEqual(Path(d, "report-2001-01-new.members.txt").read_text().split(), own)
            self.assertNotEqual(run("--members", "--set", "report-2001-01-old", root=root).returncode, 0)

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
