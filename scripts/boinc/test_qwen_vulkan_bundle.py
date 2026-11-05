"""Test the native Qwen Linux Vulkan bundle against a synthetic pinned archive; no GPU needed."""
import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest

from scripts.boinc import qwen_vulkan_bundle as bundle

ROOT = Path(__file__).resolve().parents[2]
COMMIT = "a" * 40
LIBS = ["libggml-base.so.0.9.11", "libggml.so.0.9.11", "libggml-vulkan.so", "libllama.so.0.0.8799",
        "libmtmd.so.0.0.8799", "libggml-cpu-x64.so"]
LINKS = {"libggml-base.so.0": "libggml-base.so.0.9.11", "libggml-base.so": "libggml-base.so.0",
         "libggml.so.0": "libggml.so.0.9.11", "libggml.so": "libggml.so.0", "libllama.so": "libllama.so.0.0.8799",
         "libmtmd.so": "libmtmd.so.0.0.8799"}


def add(packed, name, data=None, link=None, kind=tarfile.REGTYPE):
    member = tarfile.TarInfo(name)
    member.type = tarfile.SYMTYPE if link is not None and kind == tarfile.REGTYPE else kind
    member.linkname = link or ""
    member.mtime = 1
    if data is not None:
        member.size = len(data)
    packed.addfile(member, io.BytesIO(data) if data is not None else None)


class Bundle(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(prefix=".qwen-bundle-", dir=ROOT)
        self.dir = Path(self.folder.name)
        self.binary = self.dir / "usnm_qwen"
        self.binary.write_bytes(b"\x7fELF qwen")

    def tearDown(self):
        self.folder.cleanup()

    def archive(self, extra=(), skip=()):
        path = self.dir / "llama-b8799-bin-ubuntu-vulkan-x64.tar.gz"
        with tarfile.open(path, "w:gz") as packed:
            add(packed, "./", kind=tarfile.DIRTYPE)
            for name in ["llama-server", "llama-cli", "LICENSE", *LIBS]:
                if name not in skip:
                    add(packed, f"llama-b8799/{name}", name.encode() * 3)
            for name, target in LINKS.items():
                add(packed, f"llama-b8799/{name}", link=target)
            for args in extra:
                add(packed, *args[:2], **args[2] if len(args) > 2 else {})
        lock = self.dir / "llama.lock"
        lock.write_text(json.dumps({"repository": "ggml-org/llama.cpp", "tag": "b8799", "assets": {
            "ubuntu-vulkan-x64": {"file": path.name, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}}}))
        return path, lock

    def build(self, archive, lock, out="out"):
        return bundle.build(archive, self.binary, COMMIT, self.dir / out, "nvidia_vulkan_2b", lock)

    def test_build_materializes_links_and_verifies_reproducibly(self):
        archive, lock = self.archive()
        release = self.build(archive, lock)
        self.build(archive, lock, "again")
        out = self.dir / "out"
        for path in out.rglob("*"):
            if path.is_file():
                self.assertEqual(path.read_bytes(), (self.dir / "again" / path.relative_to(out)).read_bytes())
        extra = out / "extra"
        self.assertEqual({p.name for p in extra.iterdir()},
                         {"llama-server", "llama.LICENSE", "llama.lock", "weights.lock", *LIBS, *LINKS})
        self.assertFalse(any(p.is_symlink() for p in extra.iterdir()))
        self.assertEqual((extra / "libggml.so").read_bytes(), b"libggml.so.0.9.11" * 3)
        self.assertEqual((extra / "llama.LICENSE").read_bytes(), b"LICENSE" * 3)
        self.assertEqual(release["release"]["plan_class"], "nvidia_vulkan_2b")
        worker = release["worker"]
        self.assertEqual(worker["sha256"], hashlib.sha256(b"\x7fELF qwen").hexdigest())
        self.assertEqual(worker["extra_files"]["llama.lock"]["source_commit"], COMMIT)
        self.assertEqual(worker["extra_files"]["libggml.so"]["source_commit"], bundle.LLAMA_SOURCE["b8799"])
        args = bundle.release_args(out)
        self.assertEqual(args[:4], ["--binary", str(out.resolve() / bundle.BINARY), "--binary-sha256", worker["sha256"]])
        self.assertIn(f"libggml.so={out.resolve() / 'extra/libggml.so'}", args)
        self.assertEqual(args.count("--extra-file"), len(worker["extra_files"]))

    def test_verify_refuses_a_changed_or_unlisted_file_or_another_runtime(self):
        archive, lock = self.archive()
        self.build(archive, lock)
        out = self.dir / "out"
        reviewed = self.dir / "reviewed.json"
        worker = json.loads((out / "release.json").read_text())["worker"]
        # The reviewed release was signed with the locks of its own commit; only the runtime must match.
        worker["extra_files"]["weights.lock"]["sha256"] = "0" * 64
        reviewed.write_text(json.dumps({"worker": worker}))
        bundle.verify(out, reviewed)
        (out / "extra/libggml-vulkan.so").write_bytes(b"other")
        with self.assertRaisesRegex(ValueError, "does not match SHA256SUMS"):
            bundle.verify(out)
        self.build(archive, lock, "second")
        (self.dir / "second/extra/extra.so").write_bytes(b"x")
        with self.assertRaisesRegex(ValueError, "exactly"):
            bundle.verify(self.dir / "second")
        data = json.loads(reviewed.read_text())
        data["worker"]["extra_files"]["libggml-vulkan.so"]["sha256"] = "0" * 64
        reviewed.write_text(json.dumps(data))
        self.build(archive, lock, "third")
        with self.assertRaisesRegex(ValueError, "runtime differs"):
            bundle.verify(self.dir / "third", reviewed)

    def test_build_refuses_an_unpinned_or_unsafe_archive(self):
        archive, lock = self.archive()
        lock.write_text(lock.read_text().replace('"sha256": "', '"sha256": "0'))
        with self.assertRaisesRegex(ValueError, "not the llama.lock-pinned"):
            self.build(archive, lock)
        for extra, error in [
            ([("llama-b8799/libevil.so", None, {"link": "../../etc/passwd"})], "outside the release"),
            ([("../escape", b"x")], "unexpected archive member"),
            ([("llama-b8799/sub/libx.so", b"x")], "nested"),
            ([("llama-b8799/libdev.so", None, {"kind": tarfile.FIFOTYPE})], "not a file or link"),
        ]:
            with self.subTest(error=error):
                archive, lock = self.archive(extra)
                with self.assertRaisesRegex(ValueError, error):
                    self.build(archive, lock, error.replace(" ", "-"))
        archive, lock = self.archive(skip={"libggml-vulkan.so"})
        with self.assertRaisesRegex(ValueError, "lacks libggml-vulkan.so"):
            self.build(archive, lock, "missing")


if __name__ == "__main__":
    unittest.main()
