"""Tests for scripts/ocr-bench/fetch against a small release served from file:// URLs."""
import hashlib
import io
import json
import os
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path

FETCH = Path(__file__).resolve().parent / "fetch"
VERSION = "v9.8.7"
NAME = f"usnm-ocr-bench-{VERSION}"


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def release_files() -> dict[str, bytes]:
    manifest = {
        "release": VERSION,
        "samples": [
            {"id": "a-1", "image": {"path": "images/a-1.jpg"}},
            {"id": "a-2", "image": {"path": "images/a-2.jpg"}},
        ],
        "subsets": {"all-2": {"members": ["a-1", "a-2"]}, "one": {"members": ["a-2"]}},
    }
    files = {
        "images/a-1.jpg": b"one",
        "images/a-2.jpg": b"two",
        "references/a-1.txt": b"One",
        "references/a-2.txt": b"Two",
        "manifest.json": json.dumps(manifest).encode(),
    }
    files["SHA256SUMS"] = "".join(f"{sha(d)}  {p}\n" for p, d in sorted(files.items())).encode()
    return files


def make_tar(files: dict[str, bytes], extra: list[tarfile.TarInfo] = ()) -> bytes:
    out = io.BytesIO()
    with tarfile.open(fileobj=out, mode="w", format=tarfile.USTAR_FORMAT) as t:
        for p, d in sorted(files.items()):
            info = tarfile.TarInfo(f"{NAME}/{p}")
            info.size = len(d)
            t.addfile(info, io.BytesIO(d))
        for info in extra:
            t.addfile(info)
    return out.getvalue()


class Fetch(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.serve(make_tar(release_files()))

    def tearDown(self):
        self.tmp.cleanup()

    def serve(self, tar: bytes, digest: str | None = None):
        files = release_files()
        (self.root / "dl" / f"ocr-bench-{VERSION}").mkdir(parents=True, exist_ok=True)
        (self.root / "dl" / f"ocr-bench-{VERSION}" / f"{NAME}.tar").write_bytes(tar)
        index = {"releases": [{
            "archive": {"bytes": len(tar), "name": f"{NAME}.tar", "sha256": sha(tar)},
            "content_digest": digest or sha(files["SHA256SUMS"]),
            "release": VERSION,
            "tag": f"ocr-bench-{VERSION}",
        }]}
        (self.root / "releases.json").write_text(json.dumps(index))

    def run_fetch(self, *args):
        env = dict(os.environ, USNM_OCR_BENCH_BASE_URL=f"file://{self.root / 'dl'}")
        return subprocess.run(
            [str(FETCH), "--version", VERSION, "--index", str(self.root / "releases.json"),
             "--dest", str(self.root / "out"), *args],
            capture_output=True, text=True, env=env)

    def test_fetches_verifies_and_lists_a_subset(self):
        r = self.run_fetch("--verify")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), str(self.root / "out" / NAME))
        self.assertIn("verified", r.stderr)
        r = self.run_fetch("--verify", "--subset", "one")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.splitlines(), [str(self.root / "out" / NAME / "images/a-2.jpg")])
        r = self.run_fetch("--subset", "nope")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("no subset nope", r.stderr)

    def test_archive_checksum_mismatch_fails(self):
        files = release_files()
        files["references/a-1.txt"] = b"Changed"
        tar = make_tar(files)
        (self.root / "dl" / f"ocr-bench-{VERSION}" / f"{NAME}.tar").write_bytes(tar)
        r = self.run_fetch("--verify")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("the index says", r.stderr)
        self.assertFalse((self.root / "out" / NAME).exists())

    def test_tampered_unpacked_file_fails_verify(self):
        self.assertEqual(self.run_fetch().returncode, 0)
        (self.root / "out" / NAME / "references/a-1.txt").write_bytes(b"Edited")
        r = self.run_fetch("--verify")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("differ from SHA256SUMS", r.stderr)

    def test_extra_file_fails_verify(self):
        self.assertEqual(self.run_fetch().returncode, 0)
        (self.root / "out" / NAME / "images/extra.jpg").write_bytes(b"x")
        r = self.run_fetch("--verify")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("differ from SHA256SUMS", r.stderr)

    def test_wrong_content_digest_fails(self):
        self.serve(make_tar(release_files()), digest="0" * 64)
        r = self.run_fetch("--verify")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("content digest", r.stderr)

    def test_links_and_escapes_are_refused(self):
        link = tarfile.TarInfo(f"{NAME}/images/link.jpg")
        link.type = tarfile.SYMTYPE
        link.linkname = "/etc/passwd"
        self.serve(make_tar(release_files(), [link]))
        r = self.run_fetch()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("unexpected entries", r.stderr)
        escape = tarfile.TarInfo(f"{NAME}/../evil.txt")
        self.serve(make_tar(release_files(), [escape]))
        r = self.run_fetch()
        self.assertNotEqual(r.returncode, 0)
        self.assertFalse((self.root / "out" / "evil.txt").exists())

    def test_unknown_version_and_bad_flags(self):
        r = subprocess.run([str(FETCH), "--version", "1.0"], capture_output=True, text=True)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("v1.0.0", r.stderr)
        r = subprocess.run([str(FETCH), "--version", "v0.0.1", "--index", str(self.root / "releases.json")],
                           capture_output=True, text=True)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("is not in", r.stderr)


if __name__ == "__main__":
    unittest.main()
