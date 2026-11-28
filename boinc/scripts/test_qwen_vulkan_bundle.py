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
# The amd-vulkan-runtime artifact may come from a later commit with the same patch and builder.
RUNTIME_COMMIT = "d" * 40
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

    def amd_artifact(self, helper=b"\x7fELF amd", lock=None):
        artifact = self.dir / "linux-artifact"
        artifact.mkdir(exist_ok=True)
        # The build-time locks; weights.lock deliberately unlike the repo's, so the test sees which is used.
        files = {bundle.BINARY: self.binary.read_bytes(), bundle.AMD_HELPER: helper,
                 "llama.lock": (lock or self.dir / "llama.lock").read_bytes(), "weights.lock": b'{"build": "time"}\n'}
        for name, data in files.items():
            (artifact / name).write_bytes(data)
        (artifact / "GIT_COMMIT").write_text(COMMIT + "\n")
        (artifact / "SHA256SUMS").write_text("".join(
            f"{hashlib.sha256(data).hexdigest()}  {name}\n" for name, data in files.items()))
        return artifact

    def build_amd(self, archive, lock, artifact, out="amd"):
        return bundle.build(archive, self.binary, COMMIT, self.dir / out, "amd_vulkan_8b", self.deb,
                            lock_path=lock, libgomp_lock=self.gomp_lock, artifact=artifact)

    def test_amd_adds_only_the_artifacts_helper_to_the_same_runtime(self):
        archive, lock = self.archive()
        nvidia = self.build(archive, lock, out="nvidia")
        amd = self.build_amd(archive, lock, self.amd_artifact())
        extras = amd["worker"]["extra_files"]
        self.assertEqual(set(extras) - set(nvidia["worker"]["extra_files"]), {"usnm-amd-device-map"})
        self.assertEqual(extras["usnm-amd-device-map"]["source_commit"], COMMIT)
        self.assertEqual((self.dir / "amd/extra/usnm-amd-device-map").stat().st_mode & 0o777, 0o755)
        bundle.verify(self.dir / "amd")
        # Against a reviewed AMD manifest whose helper was built at another commit: same runtime.
        reviewed = json.loads((self.dir / "amd/release.json").read_text())
        reviewed["worker"]["extra_files"]["usnm-amd-device-map"]["sha256"] = "0" * 64
        (self.dir / "reviewed.json").write_text(json.dumps(reviewed))
        bundle.verify(self.dir / "amd", self.dir / "reviewed.json")
        # A reviewed manifest without the helper (another class) is not the same runtime.
        with self.assertRaisesRegex(ValueError, "runtime differs"):
            bundle.verify(self.dir / "amd", self.dir / "nvidia/release.json")
        # An AMD bundle without its helper does not verify.
        (self.dir / "amd/extra/usnm-amd-device-map").unlink()
        sums = self.dir / "amd/SHA256SUMS"
        sums.write_text("".join(l + "\n" for l in sums.read_text().splitlines() if "usnm-amd-device-map" not in l))
        with self.assertRaises(ValueError):
            bundle.verify(self.dir / "amd")

    def test_amd_bundles_the_artifacts_build_time_locks(self):
        # Copilot on #2210: the checkout's locks could differ from the ones the binary checks.
        archive, lock = self.archive()
        artifact = self.amd_artifact()
        release = bundle.build(archive, self.binary, COMMIT, self.dir / "amd", "amd_vulkan_8b", self.deb,
                               libgomp_lock=self.gomp_lock, artifact=artifact)
        out = self.dir / "amd"
        self.assertEqual((out / "extra/weights.lock").read_bytes(), b'{"build": "time"}\n')
        self.assertNotEqual((out / "extra/weights.lock").read_bytes(), bundle.WEIGHTS.read_bytes())
        self.assertEqual((out / "extra/llama.lock").read_bytes(), (artifact / "llama.lock").read_bytes())
        self.assertEqual(release["worker"]["extra_files"]["weights.lock"]["source_commit"], COMMIT)
        bundle.verify(out)
        for name in ("weights.lock", "llama.lock"):
            with self.subTest(name):
                artifact = self.amd_artifact()
                (artifact / name).write_text('{"changed": true}\n')
                with self.assertRaisesRegex(ValueError, f"{name} is missing .* or does not match"):
                    bundle.build(archive, self.binary, COMMIT, self.dir / f"changed-{name}", "amd_vulkan_8b",
                                 self.deb, libgomp_lock=self.gomp_lock, artifact=artifact)
        # The artifact's llama.lock must also pin the archive.
        other = self.dir / "other.lock"
        other.write_text(lock.read_text().replace(hashlib.sha256(archive.read_bytes()).hexdigest(), "0" * 64))
        with self.assertRaisesRegex(ValueError, "not the llama.lock-pinned"):
            bundle.build(archive, self.binary, COMMIT, self.dir / "unpinned", "amd_vulkan_8b", self.deb,
                         libgomp_lock=self.gomp_lock, artifact=self.amd_artifact(lock=other))

    def test_amd_refuses_a_missing_or_mismatched_artifact(self):
        archive, lock = self.archive()
        with self.assertRaisesRegex(ValueError, "--artifact .* required for amd_vulkan_8b"):
            bundle.build(archive, self.binary, COMMIT, self.dir / "none", "amd_vulkan_8b", self.deb,
                         lock_path=lock, libgomp_lock=self.gomp_lock)
        artifact = self.amd_artifact()
        with self.assertRaisesRegex(ValueError, "only for it"):
            bundle.build(archive, self.binary, COMMIT, self.dir / "nv", "nvidia_vulkan_8b", self.deb,
                         lock_path=lock, libgomp_lock=self.gomp_lock, artifact=artifact)
        (artifact / "usnm-amd-device-map").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "usnm-amd-device-map is missing .* or does not match"):
            self.build_amd(archive, lock, artifact, out="helper")
        artifact = self.amd_artifact()
        (artifact / "GIT_COMMIT").write_text("b" * 40)
        with self.assertRaisesRegex(ValueError, "GIT_COMMIT"):
            self.build_amd(archive, lock, artifact, out="commit")
        artifact = self.amd_artifact()
        self.binary.write_bytes(b"\x7fELF another qwen")
        with self.assertRaisesRegex(ValueError, "--binary is not the artifact's"):
            self.build_amd(archive, lock, artifact, out="binary")

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
            self.build(archive, lock, plan_class="apple_gpu_8b", out="apple")
        with self.assertRaisesRegex(ValueError, "--amd-runtime .* is required for amd_vulkan_8b"):
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


    def amd_runtime(self, dll=b"MZ patched vulkan", patch=None, **receipt):
        runtime = self.dir / "amd-runtime"
        runtime.mkdir(exist_ok=True)
        (runtime / "ggml-vulkan.dll").write_bytes(dll)
        digest = hashlib.sha256(dll).hexdigest()
        (runtime / "SHA256SUMS").write_text(f"{digest}  ggml-vulkan.dll\n")
        (runtime / "GIT_COMMIT").write_text(RUNTIME_COMMIT + "\r\n")
        fields = {"schema_version": 1, "source_commit": bundle.LLAMA_SOURCE["b12084"],
                  "patch_sha256": hashlib.sha256((patch or bundle.AMD_PATCH).read_bytes()).hexdigest(),
                  "runtime_sha256": digest, "vulkan_sdk": "C:\\VulkanSDK\\1.4.357.0", **receipt}
        # PowerShell's ConvertTo-Json | Set-Content: CRLF line ends.
        (runtime / "AMD_RUNTIME_BUILD.json").write_bytes(json.dumps(fields, indent=4).replace("\n", "\r\n").encode())
        return runtime

    def build_amd(self, archive, lock, runtime, out="amd"):
        return bundle.build_windows(archive, self.artifact, COMMIT, self.dir / out, "amd_vulkan_8b", lock,
                                    amd_runtime=runtime)

    def test_amd_replaces_only_ggml_vulkan_and_adds_the_helper_and_receipt(self):
        archive, lock = self.archive()
        nvidia = self.build(archive, lock, out="nvidia")
        amd = self.build_amd(archive, lock, self.amd_runtime())
        extras, base = amd["worker"]["extra_files"], nvidia["worker"]["extra_files"]
        self.assertEqual(set(extras) - set(base), {"usnm-amd-device-map.exe", "AMD_RUNTIME_BUILD.json"})
        changed = {n for n in base if extras[n]["sha256"] != base[n]["sha256"]}
        self.assertEqual(changed, {"ggml-vulkan.dll"})
        self.assertEqual((self.dir / "amd/extra/ggml-vulkan.dll").read_bytes(), b"MZ patched vulkan")
        self.assertEqual(extras["usnm-amd-device-map.exe"]["source_commit"], COMMIT)
        for name in ("ggml-vulkan.dll", "AMD_RUNTIME_BUILD.json"):
            self.assertEqual(extras[name]["source_commit"], RUNTIME_COMMIT)
        self.assertEqual(extras["ggml.dll"]["source_commit"], bundle.LLAMA_SOURCE["b12084"])
        bundle.verify(self.dir / "amd")
        self.assertIn("--extra-file", bundle.release_args(self.dir / "amd"))
        self.assertIn(f"AMD_RUNTIME_BUILD.json={(self.dir / 'amd/extra/AMD_RUNTIME_BUILD.json').resolve()}",
                      bundle.release_args(self.dir / "amd"))
        # A reviewed release whose patched DLL, receipt and helper were built at another commit has the
        # same pinned runtime; one that also differs in another DLL, or lacks AMD's files, does not.
        reviewed = json.loads((self.dir / "amd/release.json").read_text())
        for name in ("ggml-vulkan.dll", "AMD_RUNTIME_BUILD.json", "usnm-amd-device-map.exe"):
            reviewed["worker"]["extra_files"][name]["sha256"] = "0" * 64
        (self.dir / "reviewed.json").write_text(json.dumps(reviewed))
        bundle.verify(self.dir / "amd", self.dir / "reviewed.json")
        reviewed["worker"]["extra_files"]["ggml.dll"]["sha256"] = "0" * 64
        (self.dir / "reviewed.json").write_text(json.dumps(reviewed))
        with self.assertRaisesRegex(ValueError, "runtime differs"):
            bundle.verify(self.dir / "amd", self.dir / "reviewed.json")
        with self.assertRaisesRegex(ValueError, "runtime differs"):
            bundle.verify(self.dir / "amd", self.dir / "nvidia/release.json")
        # A reviewed AMD manifest with the receipt and helper but without the patched DLL is incomplete.
        del reviewed["worker"]["extra_files"]["ggml-vulkan.dll"]
        reviewed["worker"]["extra_files"]["ggml.dll"]["sha256"] = extras["ggml.dll"]["sha256"]
        (self.dir / "reviewed.json").write_text(json.dumps(reviewed))
        with self.assertRaisesRegex(ValueError, "runtime differs"):
            bundle.verify(self.dir / "amd", self.dir / "reviewed.json")

    @unittest.skipUnless(shutil.which("git") and (ROOT / ".git").exists(), "needs the git checkout")
    def test_the_patch_whose_digest_the_receipt_records_is_checked_out_with_lf(self):
        attrs = subprocess.run(["git", "-C", str(ROOT), "check-attr", "text", "eol", "--",
                                "boinc/scripts/patch_amd_vulkan_identity.py"],
                               capture_output=True, text=True, check=True).stdout
        self.assertIn("text: set", attrs)
        self.assertIn("eol: lf", attrs)
        self.assertNotIn(b"\r", bundle.AMD_PATCH.read_bytes())

    def test_amd_verify_refuses_a_dll_the_receipt_does_not_record(self):
        archive, lock = self.archive()
        self.build_amd(archive, lock, self.amd_runtime())
        out = self.dir / "amd"
        (out / "extra/ggml-vulkan.dll").write_bytes(b"MZ other")
        release = json.loads((out / "release.json").read_text())
        release["worker"]["extra_files"]["ggml-vulkan.dll"]["sha256"] = hashlib.sha256(b"MZ other").hexdigest()
        (out / "release.json").write_text(json.dumps(release))
        bundle.write_sums(out, [p.relative_to(out).as_posix() for p in out.rglob("*")
                                if p.is_file() and p.name != "SHA256SUMS"])
        with self.assertRaisesRegex(ValueError, "not the one AMD_RUNTIME_BUILD.json records"):
            bundle.verify(out)

    def test_amd_refuses_wrong_runtime_inputs(self):
        archive, lock = self.archive()
        with self.assertRaisesRegex(ValueError, "only for it"):
            bundle.build_windows(archive, self.artifact, COMMIT, self.dir / "nv", "nvidia_vulkan_8b", lock,
                                 amd_runtime=self.amd_runtime())
        cases = {"source_commit": ("source_commit", "c" * 40), "runtime_sha256": ("runtime_sha256", "0" * 64),
                 "patch_sha256": ("patch_sha256", "0" * 64), "schema_version": ("schema_version", 2)}
        for label, (key, value) in cases.items():
            with self.subTest(label):
                with self.assertRaisesRegex(ValueError, f"AMD_RUNTIME_BUILD.json {key}"):
                    self.build_amd(archive, lock, self.amd_runtime(**{key: value}), out=f"r-{label}")
        runtime = self.amd_runtime()
        (runtime / "ggml-vulkan.dll").write_bytes(b"MZ swapped")
        with self.assertRaisesRegex(ValueError, "ggml-vulkan.dll is missing .* or does not match"):
            self.build_amd(archive, lock, runtime, out="swapped")
        runtime = self.amd_runtime()
        (runtime / "GIT_COMMIT").write_text("not a commit")
        with self.assertRaisesRegex(ValueError, "GIT_COMMIT"):
            self.build_amd(archive, lock, runtime, out="commit")
        runtime = self.amd_runtime()
        (self.artifact / "usnm-amd-device-map.exe").write_bytes(b"MZ changed")
        with self.assertRaisesRegex(ValueError, "usnm-amd-device-map.exe does not match"):
            self.build_amd(archive, lock, runtime, out="helper")
        (self.artifact / "usnm-amd-device-map.exe").write_bytes(b"MZ amd")
        archive, lock = self.archive(skip=("ggml-vulkan.dll",))
        with self.assertRaisesRegex(ValueError, "lacks ggml-vulkan.dll"):
            self.build_amd(archive, lock, runtime, out="nodll")


class MacBundle(unittest.TestCase):
    """build-macos: the pinned macos-arm64 tarball plus the macOS native CI artifact."""
    FILES = ["llama-server", "libggml-base.0.0.1.dylib", "libggml-blas.0.0.1.dylib", "libggml-cpu.0.0.1.dylib",
             "libggml-metal.0.0.1.dylib", "libggml-rpc.0.0.1.dylib", "libggml.0.0.1.dylib",
             "libllama-common.0.0.1.dylib", "libllama-server-impl.dylib", "libllama.0.0.1.dylib",
             "libmtmd.0.0.1.dylib", "llama-cli"]

    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(prefix=".qwen-mac-bundle-", dir=ROOT)
        self.dir = Path(self.folder.name)
        self.artifact = self.dir / "artifact"
        self.artifact.mkdir()
        (self.artifact / bundle.MAC_BINARY).write_bytes(b"\xcf\xfa\xed\xfe qwen")
        (self.artifact / "GIT_COMMIT").write_text(COMMIT + "\n")
        # The build-time weights.lock, deliberately unlike the repo's, so the test sees which is used.
        (self.artifact / "weights.lock").write_text('{"build": "time"}\n')
        self.sums()

    def sums(self):
        names = [n for n in (bundle.MAC_BINARY, "llama.lock", "weights.lock") if (self.artifact / n).exists()]
        (self.artifact / "SHA256SUMS").write_text("".join(
            f"{hashlib.sha256((self.artifact / n).read_bytes()).hexdigest()}  {n}\n" for n in names))

    def tearDown(self):
        self.folder.cleanup()

    def archive(self, skip=()):
        path = self.dir / "llama-b12084-bin-macos-arm64.tar.gz"
        with tarfile.open(path, "w:gz") as packed:
            for name in self.FILES:
                if name not in skip:
                    add(packed, f"llama-b12084/{name}", name.encode() * 3)
            for name in sorted(bundle.MAC_RUNTIME - {"llama-server", "libllama-server-impl.dylib"}):
                versioned = name.replace(".0.dylib", ".0.0.1.dylib")
                if versioned not in skip:
                    add(packed, f"llama-b12084/{name}", link=versioned)
        lock = self.artifact / "llama.lock"
        lock.write_text(json.dumps({"repository": "ggml-org/llama.cpp", "tag": "b12084", "assets": {
            "macos-arm64": {"file": path.name, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}}}))
        self.sums()
        return path, lock

    def build(self, archive, lock, out="out", plan_class="apple_gpu_8b"):
        # No lock override: the bundle takes the artifact's build-time locks.
        return bundle.build_macos(archive, self.artifact, COMMIT, self.dir / out, plan_class)

    def test_build_takes_exactly_the_reviewed_runtime_links_as_files(self):
        archive, lock = self.archive()
        release = self.build(archive, lock)
        out = self.dir / "out"
        self.assertEqual(set(release["worker"]["extra_files"]), bundle.MAC_REQUIRED)
        self.assertEqual((out / "extra/libggml.0.dylib").read_bytes(), b"libggml.0.0.1.dylib" * 3)
        self.assertFalse((out / "extra/libggml.0.dylib").is_symlink())
        self.assertNotIn("llama-cli", release["worker"]["extra_files"])
        self.assertEqual(release["release"]["platform"], "arm64-apple-darwin")
        bundle.verify(out)
        self.assertEqual(bundle.release_args(out)[:2], ["--binary", str(out.resolve() / bundle.MAC_BINARY)])
        self.build(archive, lock, out="again")
        self.assertEqual((out / "SHA256SUMS").read_text(), (self.dir / "again/SHA256SUMS").read_text())
        # Same runtime as an earlier release: only the locks may differ.
        reviewed = self.dir / "reviewed.json"
        reviewed.write_text(json.dumps({"worker": release["worker"]}))
        bundle.verify(out, same_runtime_as=reviewed)

    def test_the_bundle_carries_the_artifacts_build_time_locks(self):
        # Copilot on #2179: the repo's locks could differ from the ones the binary checks.
        archive, lock = self.archive()
        self.build(archive, lock)
        out = self.dir / "out"
        self.assertEqual((out / "extra/weights.lock").read_bytes(), (self.artifact / "weights.lock").read_bytes())
        self.assertEqual((out / "extra/llama.lock").read_bytes(), lock.read_bytes())
        (self.artifact / "weights.lock").write_text('{"changed": true}\n')
        with self.assertRaisesRegex(ValueError, "weights.lock"):
            self.build(archive, lock, out="again")

    def test_refuses_a_missing_runtime_file_a_wrong_class_and_a_wrong_artifact(self):
        archive, lock = self.archive(skip=("libggml-metal.0.0.1.dylib",))
        with self.assertRaisesRegex(ValueError, "lacks|links outside"):
            self.build(archive, lock)
        archive, lock = self.archive()
        with self.assertRaises(ValueError):
            self.build(archive, lock, out="x", plan_class="nvidia_vulkan_8b")
        (self.artifact / "GIT_COMMIT").write_text("b" * 40)
        with self.assertRaisesRegex(ValueError, "GIT_COMMIT"):
            self.build(archive, lock, out="y")


if __name__ == "__main__":
    unittest.main()
