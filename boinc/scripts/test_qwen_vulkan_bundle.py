"""Test the native Qwen Linux Vulkan bundle against a synthetic pinned archive; no GPU needed."""
import hashlib
import io
import json
import shutil
import subprocess
from pathlib import Path
import tarfile
import tempfile
import unittest

from boinc.scripts import qwen_vulkan_bundle as bundle

ROOT = Path(__file__).resolve().parents[2]
COMMIT = "a" * 40
# b12084's layout: llama-server is a launcher for libllama-server-impl.so and libllama-common.so.
LIBS = ["libggml-base.so.0.28.0", "libggml.so.0.28.0", "libggml-vulkan.so", "libllama.so.0.8.0",
        "libmtmd.so.0.8.0", "libggml-cpu-x64.so", "libllama-server-impl.so", "libllama-common.so.0.8.0"]
LINKS = {"libggml-base.so.0": "libggml-base.so.0.28.0", "libggml-base.so": "libggml-base.so.0",
         "libggml.so.0": "libggml.so.0.28.0", "libggml.so": "libggml.so.0", "libllama.so": "libllama.so.0.8.0",
         "libmtmd.so": "libmtmd.so.0.8.0", "libllama-common.so.0": "libllama-common.so.0.8.0",
         "libllama-common.so": "libllama-common.so.0"}


def add(packed, name, data=None, link=None, kind=tarfile.REGTYPE):
    member = tarfile.TarInfo(name)
    member.type = tarfile.SYMTYPE if link is not None and kind == tarfile.REGTYPE else kind
    member.linkname = link or ""
    member.mtime = 1
    if data is not None:
        member.size = len(data)
    packed.addfile(member, io.BytesIO(data) if data is not None else None)


GOMP_MEMBER = "./usr/lib/x86_64-linux-gnu/libgomp.so.1.0.0"


def fake_deb(path, library):
    """A Debian package shaped like libgomp1's: an ar archive whose data.tar.zst holds the library."""
    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode="w") as packed:
        add(packed, GOMP_MEMBER, library)
        add(packed, "./usr/lib/x86_64-linux-gnu/libgomp.so.1", link="libgomp.so.1.0.0")
    zst = subprocess.run(["zstd", "-q", "-c"], input=data.getvalue(), capture_output=True, check=True).stdout

    def entry(name, body):
        header = f"{name + '/':<16}{0:<12}{0:<6}{0:<6}{100644:<8}{len(body):<10}`\n".encode()
        return header + body + (b"\n" if len(body) % 2 else b"")
    path.write_bytes(b"!<arch>\n" + entry("debian-binary", b"2.0\n") + entry("data.tar.zst", zst))


@unittest.skipUnless(shutil.which("zstd"), "needs zstd to read the libgomp package")
class Bundle(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(prefix=".qwen-bundle-", dir=ROOT)
        self.dir = Path(self.folder.name)
        self.binary = self.dir / "usnm_qwen"
        self.binary.write_bytes(b"\x7fELF qwen")
        self.gomp = b"\x7fELF libgomp"
        self.deb = self.dir / "libgomp1.deb"
        fake_deb(self.deb, self.gomp)
        self.gomp_lock = self.dir / "libgomp.lock"
        self.gomp_lock.write_text(json.dumps({
            "package": "libgomp1", "version": "12.3.0-test", "url": "http://example.invalid/libgomp1.deb",
            "sha256": hashlib.sha256(self.deb.read_bytes()).hexdigest(), "member": GOMP_MEMBER,
            "member_sha256": hashlib.sha256(self.gomp).hexdigest(),
            "license": "GPL-3.0-or-later WITH GCC-exception-3.1", "source": "gcc-12 12.3.0-test source",
            "copyright": "Copyright (C) 2005-2022 Free Software Foundation, Inc.",
            "notice_files": json.loads(bundle.LIBGOMP_LOCK.read_text())["notice_files"]}))

    def tearDown(self):
        self.folder.cleanup()

    def archive(self, extra=(), skip=()):
        path = self.dir / "llama-b12084-bin-ubuntu-vulkan-x64.tar.gz"
        with tarfile.open(path, "w:gz") as packed:
            add(packed, "./", kind=tarfile.DIRTYPE)
            for name in ["llama-server", "llama-cli", "LICENSE", *LIBS]:
                if name not in skip:
                    add(packed, f"llama-b12084/{name}", name.encode() * 3)
            for name, target in LINKS.items():
                add(packed, f"llama-b12084/{name}", link=target)
            for args in extra:
                add(packed, *args[:2], **args[2] if len(args) > 2 else {})
        lock = self.dir / "llama.lock"
        lock.write_text(json.dumps({"repository": "ggml-org/llama.cpp", "tag": "b12084", "assets": {
            "ubuntu-vulkan-x64": {"file": path.name, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}}}))
        return path, lock

    def build(self, archive, lock, out="out"):
        return bundle.build(archive, self.binary, COMMIT, self.dir / out, "nvidia_vulkan_2b", self.deb,
                            lock_path=lock, libgomp_lock=self.gomp_lock)

    def test_the_bundle_carries_the_pinned_libgomp_and_its_notice(self):
        # llama.cpp's CPU backends link libgomp.so.1; without it a host loads no CPU backend.
        archive, lock = self.archive()
        release = self.build(archive, lock)
        extra = self.dir / "out" / "extra"
        self.assertEqual((extra / "libgomp.so.1").read_bytes(), self.gomp)
        notice = (extra / "libgomp.LICENSE").read_text()
        self.assertIn("GCC-exception-3.1", notice)
        self.assertIn("gcc-12 12.3.0-test source", notice)
        # Redistributing the library carries its copyright notice and the full licence texts.
        self.assertIn("Copyright (C) 2005-2022 Free Software Foundation, Inc.", notice)
        self.assertIn("GNU GENERAL PUBLIC LICENSE\n                       Version 3, 29 June 2007", notice)
        self.assertIn("GCC RUNTIME LIBRARY EXCEPTION\n\nVersion 3.1, 31 March 2009", notice)
        # On-prem manifests take a 40-hex commit for every extra file: the one that pins libgomp.
        for name in ("libgomp.so.1", "libgomp.LICENSE"):
            self.assertEqual(release["worker"]["extra_files"][name]["source_commit"], COMMIT)
        bundle.verify(self.dir / "out")

    def test_a_licence_text_that_differs_from_the_lock_is_refused(self):
        archive, lock = self.archive()
        tampered = json.loads(self.gomp_lock.read_text())
        tampered["notice_files"]["COPYING3"]["sha256"] = "0" * 64
        self.gomp_lock.write_text(json.dumps(tampered))
        with self.assertRaisesRegex(ValueError, "COPYING3 differs from libgomp.lock"):
            self.build(archive, lock)

    def test_a_package_or_library_that_differs_from_the_lock_is_refused(self):
        archive, lock = self.archive()
        other = self.dir / "other.deb"
        fake_deb(other, b"\x7fELF other libgomp")
        with self.assertRaisesRegex(ValueError, "not the libgomp.lock-pinned"):
            bundle.build(archive, self.binary, COMMIT, self.dir / "a", "nvidia_vulkan_2b", other,
                         lock_path=lock, libgomp_lock=self.gomp_lock)
        # The package's digest matches, but the library inside doesn't.
        tampered = json.loads(self.gomp_lock.read_text())
        tampered["sha256"] = hashlib.sha256(other.read_bytes()).hexdigest()
        self.gomp_lock.write_text(json.dumps(tampered))
        with self.assertRaisesRegex(ValueError, "differs from libgomp.lock"):
            bundle.build(archive, self.binary, COMMIT, self.dir / "b", "nvidia_vulkan_2b", other,
                         lock_path=lock, libgomp_lock=self.gomp_lock)
        self.assertFalse((self.dir / "a").exists() or (self.dir / "b").exists())

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
                         {"llama-server", "llama.LICENSE", "llama.lock", "weights.lock", "libgomp.so.1", "libgomp.LICENSE",
                      *LIBS, *LINKS})
        self.assertFalse(any(p.is_symlink() for p in extra.iterdir()))
        self.assertEqual((extra / "libggml.so").read_bytes(), b"libggml.so.0.28.0" * 3)
        self.assertEqual((extra / "llama.LICENSE").read_bytes(), b"LICENSE" * 3)
        self.assertEqual(release["release"]["plan_class"], "nvidia_vulkan_2b")
        worker = release["worker"]
        self.assertEqual(worker["sha256"], hashlib.sha256(b"\x7fELF qwen").hexdigest())
        self.assertEqual(worker["extra_files"]["llama.lock"]["source_commit"], COMMIT)
        self.assertEqual(worker["extra_files"]["libggml.so"]["source_commit"], bundle.LLAMA_SOURCE["b12084"])
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
            ([("llama-b12084/libevil.so", None, {"link": "../../etc/passwd"})], "outside the release"),
            ([("../escape", b"x")], "unexpected archive member"),
            ([("llama-b12084/sub/libx.so", b"x")], "nested"),
            ([("llama-b12084/libdev.so", None, {"kind": tarfile.FIFOTYPE})], "not a file or link"),
        ]:
            with self.subTest(error=error):
                archive, lock = self.archive(extra)
                with self.assertRaisesRegex(ValueError, error):
                    self.build(archive, lock, error.replace(" ", "-"))
        archive, lock = self.archive(skip={"libggml-vulkan.so"})
        with self.assertRaisesRegex(ValueError, "lacks libggml-vulkan.so"):
            self.build(archive, lock, "missing")



class WindowsBundle(unittest.TestCase):
    """build-windows (#1672): the pinned win-vulkan-x64 zip plus the Windows native CI artifact."""
    RUNTIME = ["llama-server.exe", "llama-server-impl.dll", "llama-common.dll", "ggml-vulkan.dll",
               "ggml-base.dll", "ggml.dll", "ggml-cpu-x64.dll", "ggml-cpu-haswell.dll", "llama.dll",
               "mtmd.dll", "libomp.dll"]
    CRT = ["msvcp140.dll", "vcruntime140.dll", "vcruntime140_1.dll", "concrt140.dll"]

    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(prefix=".qwen-win-bundle-", dir=ROOT)
        self.dir = Path(self.folder.name)
        self.artifact = self.dir / "artifact"
        self.artifact.mkdir()
        files = {bundle.WIN_BINARY: b"MZ qwen", **{n: n.encode() * 2 for n in self.CRT},
                 "usnm-amd-device-map.exe": b"MZ amd", "usnm_quality_windows_x86_64.exe": b"MZ quality"}
        for name, data in files.items():
            (self.artifact / name).write_bytes(data)
        (self.artifact / "GIT_COMMIT").write_text(COMMIT + "\n")
        (self.artifact / "SHA256SUMS").write_text("".join(
            f"{hashlib.sha256(data).hexdigest()}  {name}\n" for name, data in files.items()))

    def tearDown(self):
        self.folder.cleanup()

    def archive(self, skip=(), extra=()):
        import zipfile
        path = self.dir / "llama-b12084-bin-win-vulkan-x64.zip"
        with zipfile.ZipFile(path, "w") as packed:
            for name in [*self.RUNTIME, "llama-cli.exe", "rpc-server.exe"]:
                if name not in skip:
                    packed.writestr(name, name.encode() * 3)
            for name in extra:
                packed.writestr(name, b"x")
        lock = self.dir / "llama.lock"
        lock.write_text(json.dumps({"repository": "ggml-org/llama.cpp", "tag": "b12084", "assets": {
            "win-vulkan-x64": {"file": path.name, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}}}))
        return path, lock

    def build(self, archive, lock, plan_class="nvidia_vulkan_8b", out="out"):
        return bundle.build_windows(archive, self.artifact, COMMIT, self.dir / out, plan_class, lock)

    def test_build_takes_the_runtime_unchanged_and_the_ci_runtime_dlls(self):
        archive, lock = self.archive()
        release = self.build(archive, lock)
        out = self.dir / "out"
        names = set(release["worker"]["extra_files"])
        self.assertEqual(names, {*self.RUNTIME, *self.CRT, "llama.lock", "weights.lock"})
        self.assertNotIn("llama-cli.exe", names)
        self.assertNotIn("usnm-amd-device-map.exe", names)
        self.assertEqual((out / "extra/ggml-vulkan.dll").read_bytes(), b"ggml-vulkan.dll" * 3)
        self.assertEqual(release["release"]["platform"], "windows_x86_64")
        self.assertEqual(release["worker"]["extra_files"]["msvcp140.dll"]["source_commit"], COMMIT)
        self.assertEqual(release["worker"]["extra_files"]["ggml.dll"]["source_commit"], bundle.LLAMA_SOURCE["b12084"])
        bundle.verify(out)
        args = bundle.release_args(out)
        self.assertEqual(args[:2], ["--binary", str(out.resolve() / bundle.WIN_BINARY)])
        # Same inputs, same bytes.
        self.build(archive, lock, out="again")
        self.assertEqual((out / "SHA256SUMS").read_text(), (self.dir / "again/SHA256SUMS").read_text())

    def test_build_serves_the_6gb_partial_offload_class_with_the_same_runtime(self):
        # nvidia_vulkan_6gb_8b takes the same pinned runtime as nvidia_vulkan_8b; only the manifest's class differs.
        archive, lock = self.archive()
        full = self.build(archive, lock, out="full")
        partial = self.build(archive, lock, plan_class="nvidia_vulkan_6gb_8b", out="partial")
        self.assertEqual(partial["release"]["plan_class"], "nvidia_vulkan_6gb_8b")
        self.assertEqual(partial["worker"], full["worker"])
        bundle.verify(self.dir / "partial")
        self.assertIn("nvidia_vulkan_6gb_8b", bundle.WIN_PLAN_CLASSES)

    def test_build_refuses_wrong_inputs(self):
        archive, lock = self.archive()
        with self.assertRaisesRegex(ValueError, "not a Windows Vulkan"):
            self.build(archive, lock, plan_class="amd_vulkan_8b", out="amd")
        (self.artifact / "msvcp140.dll").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "msvcp140.dll does not match"):
            self.build(archive, lock, out="crt")
        (self.artifact / "msvcp140.dll").write_bytes(b"msvcp140.dll" * 2)
        (self.artifact / "GIT_COMMIT").write_text("b" * 40)
        with self.assertRaisesRegex(ValueError, "GIT_COMMIT"):
            self.build(archive, lock, out="commit")
        (self.artifact / "GIT_COMMIT").write_text(COMMIT)
        archive, lock = self.archive(skip=("mtmd.dll",))
        with self.assertRaisesRegex(ValueError, "lacks mtmd.dll"):
            self.build(archive, lock, out="missing")
        archive, lock = self.archive()
        lock.write_text(lock.read_text().replace(hashlib.sha256(archive.read_bytes()).hexdigest(), "0" * 64))
        with self.assertRaisesRegex(ValueError, "not the llama.lock-pinned"):
            self.build(archive, lock, out="unpinned")
        archive, lock = self.archive(extra=("../ggml-evil.dll",))
        with self.assertRaisesRegex(ValueError, "unexpected archive member"):
            self.build(archive, lock, out="unsafe")


if __name__ == "__main__":
    unittest.main()
