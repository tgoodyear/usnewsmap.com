"""scripts/ci/check-engine-pins.py: the pins that are written in more than one place agree."""

import importlib.util
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("check", HERE / "check-engine-pins.py")
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)

FILES = [
    f"{check.SURYA}/engine.lock",
    f"{check.SURYA}/weights.lock",
    f"{check.SURYA}/Containerfile",
    check.NDLOCR_LOCK,
    *check.NDLOCR_IMAGES,
    check.NDLOCR_NOTICE,
    check.NDLOCR_ADAPTER,
    check.CATALOG,
]


def current(pattern: str, rel: str) -> str:
    """What the repository writes today, so a legitimate bump of a pin does not break these tests."""
    return re.search(pattern, (check.ROOT / rel).read_text()).group(1)


COMMIT = current(r'pub const COMMIT: &str = "([0-9a-f]{40})";', check.NDLOCR_ADAPTER)
ARCHIVE_SHA = current(r"--checksum=sha256:([0-9a-f]{64})", check.NDLOCR_IMAGES[0])
PINS = check.read_pins((check.ROOT / check.SURYA / "engine.lock").read_text(), "engine.lock", [])
OTHER_COMMIT = "0123456789abcdef0123456789abcdef01234567"


class Pins(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.reset()

    def reset(self):
        """The repository's own files again, undoing every edit."""
        for rel in FILES:
            (self.root / rel).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(check.ROOT / rel, self.root / rel)

    def edit(self, rel: str, old: str, new: str, count: int | None = 1):
        """Replace `old` with `new`: exactly `count` times, or (None) wherever it is, at least once."""
        path = self.root / rel
        text = path.read_text()
        if count is None:
            self.assertIn(old, text, rel)
        else:
            self.assertEqual(text.count(old), count, f"{rel}: {old!r}")
        path.write_text(text.replace(old, new))

    def problems(self) -> str:
        return "\n".join(check.problems(self.root))

    def test_the_repository_agrees(self):
        self.assertEqual(check.problems(check.ROOT), [])
        proc = subprocess.run([sys.executable, str(HERE / "check-engine-pins.py")], capture_output=True, text=True)
        self.assertEqual((proc.returncode, proc.stdout.strip()), (0, "engine pins agree"), proc.stderr)

    def test_the_cli_names_every_finding_and_fails(self):
        self.edit(f"{check.SURYA}/engine.lock", "SURYA_VERSION=", "SURYA_VERSION=v")
        proc = subprocess.run(
            [sys.executable, str(HERE / "check-engine-pins.py"), "--root", str(self.root)],
            capture_output=True, text=True,
        )
        self.assertEqual(proc.returncode, 1)
        self.assertIn("error: boinc/apps/surya/engine.lock: SURYA_VERSION=v", proc.stderr)

    # The Surya image.

    def lock_edit(self, old: str, new: str):
        self.edit(f"{check.SURYA}/engine.lock", old, new)

    def test_the_lock_must_hold_each_key_well_formed_and_no_other(self):
        lock = (self.root / check.SURYA / "engine.lock").read_text()
        version = next(line for line in lock.splitlines() if line.startswith("SURYA_VERSION="))
        self.lock_edit(version + "\n", "")
        self.assertIn("engine.lock: no SURYA_VERSION", self.problems())
        hex_digest = PINS["LLAMA_DIGEST"].removeprefix("sha256:")
        self.lock_edit(f"LLAMA_DIGEST=sha256:{hex_digest}", f"LLAMA_DIGEST=sha256:{hex_digest.upper()}")
        self.assertIn(f"LLAMA_DIGEST=sha256:{hex_digest.upper()} is not", self.problems())
        hex_digest = PINS["LLAMA_VULKAN_DIGEST"].removeprefix("sha256:")
        self.lock_edit(f"LLAMA_VULKAN_DIGEST=sha256:{hex_digest}", f"LLAMA_VULKAN_DIGEST={hex_digest}")
        self.assertIn(f"LLAMA_VULKAN_DIGEST={hex_digest} is not", self.problems())
        (self.root / check.SURYA / "engine.lock").write_text(lock + "EXTRA=1\nSURYA_WEIGHTS=again\nnot a pin\n")
        found = self.problems()
        self.assertIn("EXTRA is not a pin the Containerfile takes", found)
        self.assertIn("SURYA_WEIGHTS is set twice", found)
        self.assertIn("not a KEY=value line: 'not a pin'", found)

    def test_a_lock_with_crlf_line_endings_is_refused_not_misread(self):
        path = self.root / check.SURYA / "engine.lock"
        path.write_bytes(path.read_bytes().replace(b"\n", b"\r\n"))
        self.assertIn(f"not a KEY=value line: 'SURYA_VERSION={PINS['SURYA_VERSION']}\\r'", self.problems())

    def test_the_weights_digest_is_the_hash_of_the_weights_lock(self):
        # A changed weights.lock line without the new digest in engine.lock.
        self.edit(f"{check.SURYA}/weights.lock", "surya-2.gguf https", "surya-2.gguf  https")
        found = self.problems()
        self.assertIn(f"SURYA_WEIGHTS={PINS['SURYA_WEIGHTS']} but", found)
        self.assertIn("weights.lock that are not comments hash to sha256:", found)

    def test_a_comment_in_the_weights_lock_does_not_change_the_digest(self):
        path = self.root / check.SURYA / "weights.lock"
        path.write_text("# a new comment\n" + path.read_text())
        self.assertEqual(check.surya_problems(self.root), [])

    def test_the_containerfile_takes_each_pin_as_an_argument_without_a_default(self):
        self.edit(f"{check.SURYA}/Containerfile", "ARG LLAMA_DIGEST\n", f"ARG LLAMA_DIGEST=sha256:{'0' * 64}\n", count=2)
        found = self.problems()
        self.assertIn("ARG LLAMA_DIGEST has a default", found)
        self.assertIn("holds a digest", found)
        self.edit(f"{check.SURYA}/Containerfile", "ARG SURYA_VERSION\n", "")
        self.assertIn("no `ARG SURYA_VERSION`", self.problems())

    def test_the_containerfile_holds_no_copy_of_a_pin(self):
        self.edit(f"{check.SURYA}/Containerfile", 'surya-ocr=="$SURYA_VERSION"', "surya-ocr==1.2.3", count=2)
        self.assertIn("holds a pinned surya-ocr version", self.problems())
        self.edit(f"{check.SURYA}/Containerfile", "USNM_SURYA_VERSION=${SURYA_VERSION}", "USNM_SURYA_VERSION=1.2.3")
        self.assertIn("holds a pinned USNM_SURYA_VERSION", self.problems())

    def test_a_comment_in_the_containerfile_may_name_a_value(self):
        path = self.root / check.SURYA / "Containerfile"
        path.write_text(f"# was surya-ocr==1.2.3 on sha256:{'a' * 64}\n" + path.read_text())
        self.assertEqual(check.surya_problems(self.root), [])

    # NDLOCR-Lite.

    def test_one_commit_everywhere_passes(self):
        self.assertEqual(check.ndlocr_problems(self.root), [])

    def test_every_place_the_commit_is_written_is_compared(self):
        places = [
            (check.NDLOCR_LOCK, f"# NDLOCR-Lite at {COMMIT}", f"# NDLOCR-Lite at {OTHER_COMMIT}", 1, "models.lock header"),
            (check.NDLOCR_LOCK, f"/ndlocr-lite/{COMMIT}/", f"/ndlocr-lite/{OTHER_COMMIT}/", None, "models.lock urls"),
            (check.NDLOCR_NOTICE, f"Commit: {COMMIT}", f"Commit: {OTHER_COMMIT}", 1, check.NDLOCR_NOTICE),
            (check.NDLOCR_ADAPTER, f'COMMIT: &str = "{COMMIT}"', f'COMMIT: &str = "{OTHER_COMMIT}"', 1, check.NDLOCR_ADAPTER),
            (check.NDLOCR_IMAGES[0], f"ARG NDLOCR_COMMIT={COMMIT}", f"ARG NDLOCR_COMMIT={OTHER_COMMIT}", 1,
             check.NDLOCR_IMAGES[0]),
            (check.NDLOCR_IMAGES[1], f"ARG NDLOCR_COMMIT={COMMIT}", f"ARG NDLOCR_COMMIT={OTHER_COMMIT}", 1,
             check.NDLOCR_IMAGES[1]),
            (check.CATALOG, f"ndlocr-lite/tree/{COMMIT}", f"ndlocr-lite/tree/{OTHER_COMMIT}", 1, "engines.toml (ndlocr-lite)"),
            (check.CATALOG, f"ndlocr-lite/blob/{COMMIT}/LICENCE", f"ndlocr-lite/blob/{OTHER_COMMIT}/LICENCE", None,
             "engines.toml (ndlocr-lite)"),
            (check.CATALOG, f'version = "{COMMIT}"', f'version = "{OTHER_COMMIT}"', None, "engines.toml (ndlocr-lite)"),
        ]
        for rel, old, new, count, where in places:
            with self.subTest(file=rel, change=new):
                self.reset()
                self.edit(rel, old, new, count)
                found = self.problems()
                self.assertIn("is not one commit", found)
                self.assertIn(f"{OTHER_COMMIT} in", found)
                self.assertIn(where, found)

    def test_one_models_licence_link_with_another_commit_is_reported(self):
        # The engine's own terms link is the first occurrence; the PARSeq models' links follow.
        # Change the second occurrence only, so only one model's licence commit moves.
        path = self.root / check.CATALOG
        old = f"ndlocr-lite/blob/{COMMIT}/LICENCE"
        text = path.read_text()
        self.assertGreaterEqual(text.count(old), 3)
        second = text.index(old, text.index(old) + 1)
        path.write_text(text[:second] + f"ndlocr-lite/blob/{OTHER_COMMIT}/LICENCE" + text[second + len(old):])
        found = self.problems()
        self.assertIn("is not one commit", found)
        self.assertIn(f"{OTHER_COMMIT} in ocr/engines/engines.toml (ndlocr-lite)", found)
        # DINOv3's licence has its own revision and is not the NDLOCR pin.
        self.reset()
        self.edit(check.CATALOG, "facebookresearch/dinov3/blob/", "facebookresearch/dinov3/blob/0000000000000000000000000000000000000000/", None)
        self.assertNotIn("is not one commit", self.problems())

    def test_a_literal_copy_of_the_commit_left_in_a_dockerfile_is_compared_too(self):
        self.edit(check.NDLOCR_IMAGES[0], "NDLOCR_COMMIT=${NDLOCR_COMMIT}", f"NDLOCR_COMMIT={OTHER_COMMIT}")
        self.assertIn("is not one commit", self.problems())

    def test_the_source_archive_checksum_must_be_the_same_in_both_dockerfiles(self):
        self.edit(check.NDLOCR_IMAGES[1], ARCHIVE_SHA, "0" * 64)
        found = self.problems()
        self.assertIn("source archive's sha256 differs between the Dockerfiles", found)
        self.assertIn(f"{ARCHIVE_SHA} in {check.NDLOCR_IMAGES[0]}", found)

    def test_a_file_that_names_no_commit_is_reported(self):
        self.edit(check.NDLOCR_NOTICE, f"Commit: {COMMIT}", "Commit: unknown")
        self.assertIn(f"{check.NDLOCR_NOTICE}: names no commit", self.problems())
        (self.root / check.NDLOCR_ADAPTER).unlink()
        self.assertIn(f"{check.NDLOCR_ADAPTER}: not readable", self.problems())


if __name__ == "__main__":
    unittest.main()
