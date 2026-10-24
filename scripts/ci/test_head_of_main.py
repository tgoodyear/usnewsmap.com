"""scripts/ci/head-of-main.sh: only main's head publishes, or an older commit whose newer
commits change nothing it publishes.

Each test commits changes in a scratch clone of this checkout and runs head-of-main.sh there,
with a stub `gh` that reports main's head. Classifying the newer commits runs changes.sh,
which asks cargo which crates the images build, so those cases skip without it.
"""

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CARGO = shutil.which("cargo")


def git(repo: Path, *args: str) -> str:
    return subprocess.run(["git", "-C", str(repo), *args], check=True,
                          capture_output=True, text=True).stdout.strip()


class HeadOfMain(unittest.TestCase):
    def setUp(self):
        # Resolved: cargo metadata reports canonical paths (macOS's /var is a symlink).
        self.tmp = Path(tempfile.mkdtemp()).resolve()
        self.repo = self.tmp / "repo"
        subprocess.run(["git", "clone", "-q", "--no-hardlinks", str(ROOT), str(self.repo)], check=True)
        # The working tree's scripts and Cargo manifests, committed or not.
        for rel in ["scripts/ci/changes.sh", "scripts/ci/head-of-main.sh", "Cargo.toml", "Cargo.lock"]:
            shutil.copy(ROOT / rel, self.repo / rel)
        for manifest in (ROOT / "crates").glob("*/Cargo.toml"):
            rel_dir = manifest.parent.relative_to(ROOT)
            if (self.repo / rel_dir).exists():
                shutil.copy(manifest, self.repo / manifest.relative_to(ROOT))
            else:
                shutil.copytree(manifest.parent, self.repo / rel_dir)
        self.base = self.commit("these scripts")
        bin_dir = self.tmp / "bin"
        bin_dir.mkdir()
        gh = bin_dir / "gh"
        gh.write_text('#!/bin/sh\necho "$FAKE_MAIN"\n')
        gh.chmod(0o755)
        self.path = f"{bin_dir}{os.pathsep}{os.environ['PATH']}"

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def commit(self, message: str) -> str:
        git(self.repo, "add", "-A")
        git(self.repo, "-c", "user.name=t", "-c", "user.email=t@example.com",
            "commit", "-q", "--allow-empty", "-m", message)
        return git(self.repo, "rev-parse", "HEAD")

    def touch(self, path: str) -> str:
        f = self.repo / path
        f.parent.mkdir(parents=True, exist_ok=True)
        with f.open("a") as h:
            h.write("\n// touched\n")
        return self.commit(f"touch {path}")

    def current(self, sha: str, main: str, *args: str) -> bool:
        out = self.tmp / "out"
        out.write_text("")
        env = dict(os.environ, PATH=self.path, FAKE_MAIN=main, GITHUB_SHA=sha,
                   GITHUB_REPOSITORY="o/r", GITHUB_OUTPUT=str(out))
        result = subprocess.run(["scripts/ci/head-of-main.sh", *args], cwd=self.repo, env=env,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        # The scratch worktree is gone again.
        self.assertEqual(len(git(self.repo, "worktree", "list").splitlines()), 1)
        return out.read_text().strip() == "current=true"

    def test_main_s_head_is_current(self):
        sha = self.touch("crates/usnm-api/src/main.rs")
        self.assertTrue(self.current(sha, sha))
        self.assertTrue(self.current(sha, sha, "--unless-changed", "image"))

    def test_an_older_commit_is_not_current_without_the_flag(self):
        sha = self.touch("crates/usnm-api/src/main.rs")
        main = self.touch("docs/notes.md")
        self.assertFalse(self.current(sha, main))

    @unittest.skipUnless(CARGO, "no cargo")
    def test_an_older_commit_publishes_when_main_s_newer_commits_change_no_image(self):
        # 24 Oct 2026: an API change, then a scripts-only merge before its run published.
        sha = self.touch("crates/usnm-api/src/main.rs")
        self.touch("docs/notes.md")
        self.touch("scripts/ops/notes.txt")
        main = self.touch("crates/usnm-boinc/src/lib.rs")
        self.assertTrue(self.current(sha, main, "--unless-changed", "image"))

    @unittest.skipUnless(CARGO, "no cargo")
    def test_a_newer_change_to_a_crate_the_api_builds_publishes_itself(self):
        sha = self.touch("crates/usnm-api/src/main.rs")
        main = self.touch("crates/usnm-search/src/lib.rs")
        self.assertFalse(self.current(sha, main, "--unless-changed", "image"))

    def test_a_newer_image_change_publishes_itself(self):
        sha = self.touch("crates/usnm-api/src/main.rs")
        self.touch("docs/notes.md")
        main = self.touch("web/index.html")
        self.assertFalse(self.current(sha, main, "--unless-changed", "image"))

    @unittest.skipUnless(CARGO, "no cargo")
    def test_every_flag_must_be_unchanged(self):
        sha = self.touch("crates/usnm-api/src/main.rs")
        main = self.touch("ocr/engines/tesseract/notes.txt")
        self.assertTrue(self.current(sha, main, "--unless-changed", "image"))
        self.assertFalse(self.current(sha, main, "--unless-changed", "image,ocr_worker"))

    def test_a_commit_main_does_not_descend_from_is_not_current(self):
        sha = self.touch("crates/usnm-api/src/main.rs")
        git(self.repo, "reset", "-q", "--hard", self.base)
        main = self.touch("docs/notes.md")
        self.assertFalse(self.current(sha, main, "--unless-changed", "image"))

    def test_flag_names_are_checked(self):
        env = dict(os.environ, PATH=self.path, FAKE_MAIN=self.base, GITHUB_SHA=self.base,
                   GITHUB_REPOSITORY="o/r", GITHUB_OUTPUT=os.devnull)
        for flags in ["image;rm", "image,", "Image"]:
            with self.subTest(flags=flags):
                result = subprocess.run(["scripts/ci/head-of-main.sh", "--unless-changed", flags],
                                        cwd=self.repo, env=env, capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
