"""Offline OCI layer and rootfs-wrapper regressions, using synthetic archives."""

import hashlib
import io
import json
import os
from pathlib import Path
import pwd
import shlex
import shutil
import subprocess
import sys
import tarfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
JOBS = ROOT / "docs/reports/2026-11-gpu-studies/scripts/jobs"
HELPER = JOBS / "rootfs-layer.py"
WRAPPER = JOBS / "rootfs.sh"
WORK = Path.cwd() / f".gpu-study-rootfs-test-{os.getpid()}"


def archive(entries, compressed=False, *, format=tarfile.PAX_FORMAT):
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w:gz" if compressed else "w", format=format) as layer:
        for name, kind, value in entries:
            member = tarfile.TarInfo(name)
            member.uid, member.gid = os.getuid(), os.getgid()
            member.mode = 0o755 if kind == "dir" else 0o644
            if kind == "file":
                member.size = len(value)
                layer.addfile(member, io.BytesIO(value))
            else:
                member.type = {
                    "dir": tarfile.DIRTYPE,
                    "symlink": tarfile.SYMTYPE,
                    "hardlink": tarfile.LNKTYPE,
                    "unknown": b"Z",
                }[kind]
                member.linkname = value
                layer.addfile(member)
    return output.getvalue()


def digest(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def executable(path, text):
    path.write_text(text, encoding="utf-8")
    path.chmod(0o755)


class GPUStudyRootfsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        WORK.mkdir(mode=0o700)
        cls.addClassCleanup(shutil.rmtree, WORK)

    def setUp(self):
        self.work = WORK / self._testMethodName
        self.work.mkdir()
        self.root = self.work / "root"
        self.root.mkdir()
        self.outside = self.work / "outside"
        self.outside.mkdir()
        (self.outside / "victim").write_text("outside", encoding="utf-8")

    def apply(self, entries, *, root=None, compressed=False, expected=None, env=None, umask=-1):
        return self.apply_bytes(
            archive(entries, compressed), root=root, expected=expected, env=env, umask=umask
        )

    def apply_bytes(self, data, *, root=None, expected=None, env=None, umask=-1):
        root = root or self.root
        return subprocess.run(
            [sys.executable, "-B", "-I", str(HELPER), "--action", "apply",
             "--root", str(root.resolve()), "--digest", expected or digest(data)],
            input=data,
            cwd=ROOT,
            env=env,
            umask=umask,
            capture_output=True,
            timeout=60,
            check=False,
        )

    def assert_ok(self, result):
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors="replace"))

    def assert_failed(self, result, message=None):
        stderr = result.stderr.decode(errors="replace")
        self.assertNotEqual(result.returncode, 0, stderr)
        self.assertTrue(stderr, "failure must be visible")
        if message:
            self.assertIn(message, stderr)

    def assert_outside_unchanged(self):
        self.assertEqual((self.outside / "victim").read_text(encoding="utf-8"), "outside")

    def test_normal_whiteouts_remove_lower_entries_but_not_same_layer_files(self):
        self.assert_ok(self.apply([
            ("d", "dir", ""),
            ("d/old", "file", b"old"),
            ("d/replaced", "file", b"old replacement"),
            ("d/tree", "dir", ""),
            ("d/tree/nested", "dir", ""),
            ("d/tree/nested/file", "file", b"old nested"),
            ("d/tree/nested/link", "symlink", str(self.outside)),
            ("d/link", "symlink", str(self.outside)),
            ("d/kept", "file", b"kept"),
        ], compressed=True))
        self.assert_ok(self.apply([
            ("d/replaced", "file", b"new replacement"),
            ("d/added", "file", b"new"),
            ("d/.wh.old", "file", b""),
            ("d/.wh.replaced", "file", b""),
            ("d/.wh.tree", "file", b""),
            ("d/.wh.link", "file", b""),
            ("d/.wh.missing", "file", b""),
        ]))
        self.assertEqual((self.root / "d/replaced").read_bytes(), b"new replacement")
        self.assertEqual((self.root / "d/added").read_bytes(), b"new")
        self.assertEqual((self.root / "d/kept").read_bytes(), b"kept")
        self.assertFalse((self.root / "d/old").exists())
        self.assertFalse((self.root / "d/tree").exists())
        self.assertFalse((self.root / "d/link").is_symlink())
        self.assertFalse(any(path.name.startswith(".wh.") for path in self.root.rglob("*")))
        self.assert_outside_unchanged()

    def test_opaque_whiteouts_hide_only_lower_children_including_at_root(self):
        self.assert_ok(self.apply([
            ("group/old", "file", b"old"),
            ("group/replace", "file", b"old"),
            ("group/sub/old", "file", b"old"),
            ("sibling", "file", b"kept"),
        ]))
        self.assert_ok(self.apply([
            ("group/replace", "file", b"new"),
            ("group/new/child", "file", b"new child"),
            ("group/.wh..wh..opq", "file", b""),
        ], compressed=True))
        self.assertEqual((self.root / "group/replace").read_bytes(), b"new")
        self.assertEqual((self.root / "group/new/child").read_bytes(), b"new child")
        self.assertFalse((self.root / "group/old").exists())
        self.assertFalse((self.root / "group/sub").exists())
        self.assertEqual((self.root / "sibling").read_bytes(), b"kept")
        self.assertFalse((self.root / "group/.wh..wh..opq").exists())

        self.assert_ok(self.apply([
            ("root-added", "file", b"new root"),
            (".wh..wh..opq", "file", b""),
        ]))
        self.assertEqual(sorted(path.name for path in self.root.iterdir()), ["root-added"])
        self.assertEqual((self.root / "root-added").read_bytes(), b"new root")

    def test_whiteout_and_recreated_directory_are_order_independent(self):
        self.assert_ok(self.apply([("d/old", "file", b"old")]))
        self.assert_ok(self.apply([
            ("d", "dir", ""),
            ("d/new", "file", b"new"),
            (".wh.d", "file", b""),
        ]))
        self.assertEqual((self.root / "d/new").read_bytes(), b"new")
        self.assertFalse((self.root / "d/old").exists())
        self.assertFalse((self.root / ".wh.d").exists())

    def test_long_pax_and_gnu_paths_apply_whiteouts(self):
        for kind in (tarfile.PAX_FORMAT, tarfile.GNU_FORMAT):
            with self.subTest(format=kind):
                root = self.work / f"format-{kind}"
                root.mkdir()
                directory = "d" * 110
                first = archive([(f"{directory}/old", "file", b"old")], format=kind)
                second = archive([
                    (f"{directory}/new", "file", b"new"),
                    (f"{directory}/.wh.old", "file", b""),
                ], format=kind)
                self.assert_ok(self.apply_bytes(first, root=root))
                self.assert_ok(self.apply_bytes(second, root=root))
                self.assertFalse((root / directory / "old").exists())
                self.assertEqual((root / directory / "new").read_bytes(), b"new")
                self.assertFalse((root / directory / ".wh.old").exists())

    def test_gnu_long_symlink_name_and_target_can_be_stored(self):
        name, target = "n" * 110, "t" * 110
        data = archive([(name, "symlink", target)], format=tarfile.GNU_FORMAT)
        self.assert_ok(self.apply_bytes(data))
        self.assertEqual(os.readlink(self.root / name), target)

    def test_type_replacement_and_lower_hardlinks_preserve_other_entries(self):
        self.assert_ok(self.apply([
            ("d/old", "file", b"old directory"),
            ("a", "file", b"old linked file"),
            ("b", "hardlink", "a"),
            ("old-file", "file", b"old file"),
        ]))
        self.assertEqual(os.stat(self.root / "a").st_ino, os.stat(self.root / "b").st_ino)
        self.assert_ok(self.apply([
            ("d", "file", b"new file"),
            ("a", "file", b"new file"),
            ("old-file/child", "file", b"new child"),
            ("old-file", "dir", ""),
        ]))
        self.assertEqual((self.root / "d").read_bytes(), b"new file")
        self.assertEqual((self.root / "a").read_bytes(), b"new file")
        self.assertEqual((self.root / "b").read_bytes(), b"old linked file")
        self.assertEqual((self.root / "old-file/child").read_bytes(), b"new child")
        self.assertNotEqual(os.stat(self.root / "a").st_ino, os.stat(self.root / "b").st_ino)

    def test_hardlink_to_lower_file_survives_later_target_replacement(self):
        self.assert_ok(self.apply([("target", "file", b"old")]))
        self.assert_ok(self.apply([
            ("copy", "hardlink", "target"),
            ("target", "file", b"new"),
        ]))
        self.assertEqual((self.root / "copy").read_bytes(), b"old")
        self.assertEqual((self.root / "target").read_bytes(), b"new")

    def test_file_mode_is_not_changed_by_the_callers_umask(self):
        self.assert_ok(self.apply([("file", "file", b"data")], umask=0o077))
        self.assertEqual(os.stat(self.root / "file").st_mode & 0o777, 0o644)

    def test_numeric_image_owner_is_not_remapped_to_a_host_username(self):
        names = [entry for entry in pwd.getpwall() if entry.pw_uid != os.getuid()]
        if not names:
            self.skipTest("no alternate local username")
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode="w") as layer:
            member = tarfile.TarInfo("file")
            member.uid, member.gid = os.getuid(), os.getgid()
            member.uname = names[0].pw_name
            member.size = 4
            layer.addfile(member, io.BytesIO(b"data"))
        self.assert_ok(self.apply_bytes(output.getvalue()))
        self.assertEqual(os.stat(self.root / "file").st_uid, os.getuid())
        self.assertEqual(os.stat(self.root / "file").st_gid, os.getgid())

    def test_layer_staging_does_not_change_root_directory_mtime(self):
        self.assert_ok(self.apply([("group/old", "file", b"old")]))
        os.utime(self.root, ns=(1_600_000_000_000_000_000, 1_600_000_000_000_000_000))
        before = os.stat(self.root).st_mtime_ns
        self.assert_ok(self.apply([("group/new", "file", b"new")]))
        self.assertEqual(os.stat(self.root).st_mtime_ns, before)

    def test_hardlink_cannot_depend_on_a_replaced_lower_directory(self):
        self.assert_ok(self.apply([("d/target", "file", b"old")]))
        result = self.apply([
            ("copy", "hardlink", "d/target"),
            ("d", "file", b"replacement"),
        ])
        self.assert_failed(result, "hardlink target below a replacement")
        self.assertEqual((self.root / "d/target").read_bytes(), b"old")
        self.assertFalse((self.root / "copy").exists())

    def test_whiteouts_refuse_symlink_ancestors_and_never_touch_the_target(self):
        for name in (".wh.victim", ".wh..wh..opq"):
            with self.subTest(whiteout=name):
                root = self.work / ("case-" + name)
                root.mkdir()
                (root / "link").symlink_to(self.outside, target_is_directory=True)
                result = self.apply([("link/" + name, "file", b"")], root=root)
                self.assert_failed(result, "symlink in layer path")
                self.assertTrue((root / "link").is_symlink())
                self.assert_outside_unchanged()

    def test_whiteout_does_not_delete_a_case_aliased_lower_entry(self):
        (self.root / "victim").write_text("inside", encoding="utf-8")
        if not (self.root / "VICTIM").exists():
            self.skipTest("filesystem does not alias case")
        result = self.apply([(".wh.VICTIM", "file", b"")])
        self.assert_failed(result, "filesystem")
        self.assertEqual((self.root / "victim").read_text(encoding="utf-8"), "inside")
        result = self.apply([("VICTIM", "file", b"replacement")])
        self.assert_failed(result, "filesystem")
        self.assertEqual((self.root / "victim").read_text(encoding="utf-8"), "inside")

    def test_whiteout_does_not_delete_a_normalization_aliased_entry(self):
        composed, decomposed = "caf\u00e9", "cafe\u0301"
        (self.root / composed).write_text("inside", encoding="utf-8")
        if not (self.root / decomposed).exists():
            self.skipTest("filesystem does not alias Unicode normalization")
        result = self.apply([(f".wh.{decomposed}", "file", b"")])
        self.assert_failed(result, "filesystem")
        self.assertEqual((self.root / composed).read_text(encoding="utf-8"), "inside")

    def test_case_aliased_same_layer_symlink_is_rejected(self):
        (self.root / "probe").touch()
        aliased = (self.root / "PROBE").exists()
        (self.root / "probe").unlink()
        if not aliased:
            self.skipTest("filesystem does not alias case")
        result = self.apply([
            ("link", "symlink", str(self.outside)),
            ("LINK/victim", "file", b"bad"),
        ])
        self.assert_failed(result, "filesystem")
        self.assertFalse((self.root / "link").is_symlink())
        self.assert_outside_unchanged()
        result = self.apply([
            ("link", "symlink", str(self.outside / "victim")),
            ("copy", "hardlink", "LINK"),
        ])
        self.assert_failed(result, "filesystem")
        self.assertFalse((self.root / "link").is_symlink())
        self.assert_outside_unchanged()

    def test_whiteouts_under_replaced_symlinks_do_not_follow_them(self):
        (self.root / "link").symlink_to(self.outside, target_is_directory=True)
        self.assert_ok(self.apply([
            ("link", "dir", ""),
            ("link/new", "file", b"new"),
            ("link/.wh.victim", "file", b""),
            ("link/.wh..wh..opq", "file", b""),
        ]))
        self.assertFalse((self.root / "link").is_symlink())
        self.assertEqual((self.root / "link/new").read_bytes(), b"new")
        self.assert_outside_unchanged()

        other = self.work / "removed-link"
        other.mkdir()
        (other / "link").symlink_to(self.outside, target_is_directory=True)
        self.assert_ok(self.apply([
            ("link/.wh.victim", "file", b""),
            (".wh.link", "file", b""),
        ], root=other))
        self.assertFalse((other / "link").is_symlink())
        self.assert_outside_unchanged()

    def test_nonwhiteout_symlink_traversal_is_rejected_and_leaf_replacement_is_safe(self):
        (self.root / "link").symlink_to(self.outside, target_is_directory=True)
        result = self.apply([("link/victim", "file", b"bad")])
        self.assert_failed(result, "symlink in layer path")
        self.assert_outside_unchanged()

        other = self.work / "archive-link"
        other.mkdir()
        result = self.apply([
            ("link", "symlink", str(self.outside)),
            ("link/victim", "file", b"bad"),
        ], root=other)
        self.assert_failed(result, "non-directory archive ancestor")
        self.assertFalse((other / "link").is_symlink())
        self.assert_outside_unchanged()

        (self.root / "leaf").symlink_to(self.outside / "victim")
        self.assert_ok(self.apply([("leaf", "file", b"inside")]))
        self.assertFalse((self.root / "leaf").is_symlink())
        self.assertEqual((self.root / "leaf").read_bytes(), b"inside")
        self.assert_outside_unchanged()

    def test_invalid_archive_paths_and_whiteouts_fail_before_deleting(self):
        cases = [
            [("../victim", "file", b"bad")],
            [("/absolute", "file", b"bad")],
            [("a/../.wh.victim", "file", b"")],
            [(".wh.", "file", b"")],
            [(".wh..", "file", b"")],
            [(".wh...", "file", b"")],
            [(".wh..wh.other", "file", b"")],
            [(".wh.nonempty", "file", b"bad")],
            [(".wh.directory", "dir", "")],
            [(".wh.link", "symlink", str(self.outside))],
            [(".wh.hidden/child", "file", b"bad")],
            [("duplicate", "file", b"one"), ("./duplicate", "file", b"two")],
            [("hardlink", "hardlink", "../outside/victim")],
            [("unknown", "unknown", "")],
        ]
        for index, invalid in enumerate(cases):
            with self.subTest(invalid=invalid):
                root = self.work / f"invalid-{index}"
                root.mkdir()
                (root / "keep").write_text("keep", encoding="utf-8")
                result = self.apply([(".wh.keep", "file", b""), *invalid], root=root)
                self.assert_failed(result, "rootfs layer:")
                self.assertEqual((root / "keep").read_text(encoding="utf-8"), "keep")
                self.assert_outside_unchanged()

    def test_digest_mismatch_and_corrupt_archive_leave_lower_layer_intact(self):
        (self.root / "keep").write_text("keep", encoding="utf-8")
        data = archive([(".wh.keep", "file", b"")], compressed=True)
        result = self.apply_bytes(data, expected="sha256:" + "0" * 64)
        self.assert_failed(result, "sha256 mismatch")
        result = self.apply_bytes(b"not a tar archive")
        self.assert_failed(result, "rootfs layer:")
        self.assertEqual((self.root / "keep").read_text(encoding="utf-8"), "keep")
        self.assertEqual([path.name for path in self.root.iterdir()], ["keep"])

    def test_later_corrupt_or_concatenated_archive_fails_before_whiteouts(self):
        (self.root / "keep").write_text("keep", encoding="utf-8")
        corrupt = bytearray(archive([(".wh.keep", "file", b""), ("later", "file", b"x")]))
        corrupt[512] ^= 0x01
        concatenated = archive([(".wh.keep", "file", b"")]) + archive([("later", "file", b"x")])
        incomplete = archive([(".wh.keep", "file", b"")])[:1024]
        bad_crc = bytearray(archive([(".wh.keep", "file", b"")], compressed=True))
        bad_crc[-8] ^= 0x01
        for data in (bytes(corrupt), concatenated, incomplete, bytes(bad_crc)):
            with self.subTest(size=len(data)):
                self.assert_failed(self.apply_bytes(data), "rootfs layer:")
                self.assertEqual((self.root / "keep").read_text(encoding="utf-8"), "keep")
                self.assertFalse((self.root / "later").exists())

    def test_unsupported_extended_metadata_fails_before_deletion(self):
        (self.root / "keep").write_text("keep", encoding="utf-8")
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode="w", format=tarfile.PAX_FORMAT) as layer:
            marker = tarfile.TarInfo(".wh.keep")
            layer.addfile(marker)
            member = tarfile.TarInfo("file")
            member.pax_headers = {"SCHILY.xattr.user.example": "value"}
            layer.addfile(member)
        result = self.apply_bytes(output.getvalue())
        self.assert_failed(result, "unsupported layer metadata")
        self.assertEqual((self.root / "keep").read_text(encoding="utf-8"), "keep")
        self.assertFalse((self.root / "file").exists())

    def test_malformed_pax_time_fails_before_deletion(self):
        (self.root / "keep").write_text("keep", encoding="utf-8")
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode="w", format=tarfile.PAX_FORMAT) as layer:
            layer.addfile(tarfile.TarInfo(".wh.keep"))
            member = tarfile.TarInfo("file")
            member.pax_headers = {"mtime": "not-a-time"}
            layer.addfile(member)
        result = self.apply_bytes(output.getvalue())
        self.assert_failed(result, "invalid PAX mtime")
        self.assertEqual((self.root / "keep").read_text(encoding="utf-8"), "keep")

    def test_malformed_pax_record_fails_before_deletion(self):
        (self.root / "keep").write_text("keep", encoding="utf-8")
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode="w", format=tarfile.PAX_FORMAT) as layer:
            layer.addfile(tarfile.TarInfo(".wh.keep"))
            member = tarfile.TarInfo("later")
            member.pax_headers = {"comment": "example"}
            layer.addfile(member)
        data = bytearray(output.getvalue())
        self.assertEqual(data[512 + 156], ord("x"))
        data[1024] = ord("x")
        result = self.apply_bytes(bytes(data))
        self.assert_failed(result, "PAX")
        self.assertEqual((self.root / "keep").read_text(encoding="utf-8"), "keep")
        self.assertFalse((self.root / "later").exists())

    def test_tar_options_are_not_inherited(self):
        real_tar = shutil.which("tar")
        self.assertIsNotNone(real_tar)
        bin_dir = self.work / "bin"
        bin_dir.mkdir()
        executable(
            bin_dir / "tar",
            """#!/usr/bin/env python3
import os, sys
if "TAR_OPTIONS" in os.environ:
    print("unsafe TAR_OPTIONS leaked", file=sys.stderr)
    sys.exit(91)
os.execv(os.environ["REAL_TAR"], [os.environ["REAL_TAR"], *sys.argv[1:]])
""",
        )
        env = os.environ.copy()
        env.update(PATH=str(bin_dir) + os.pathsep + env["PATH"], REAL_TAR=real_tar,
                   TAR_OPTIONS="--absolute-names --dereference")
        self.assert_ok(self.apply([("safe", "file", b"ok")], env=env))
        self.assertEqual((self.root / "safe").read_bytes(), b"ok")

    def fake_registry(self):
        bin_dir = self.work / "bin"
        manifest_dir = self.work / "manifests"
        blob_dir = self.work / "blobs"
        for directory in (bin_dir, manifest_dir, blob_dir):
            directory.mkdir()
        executable(bin_dir / "python3", f'#!/bin/sh\nexec {shlex.quote(sys.executable)} "$@"\n')
        executable(
            bin_dir / "curl",
            """#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
url = args[-1]
with open(os.environ["CURL_TRACE"], "a", encoding="utf-8") as trace:
    trace.write(url + "\\n")
if "/token?" in url:
    print(json.dumps({"token": "synthetic-token"}))
elif "-K" not in args or "synthetic-token" not in sys.stdin.read():
    print("missing synthetic auth config", file=sys.stderr)
    sys.exit(97)
elif "/manifests/sha256:" in url:
    sys.stdout.buffer.write((pathlib.Path(os.environ["MANIFESTS"]) / url.rsplit(":", 1)[1]).read_bytes())
elif "/blobs/sha256:" in url:
    sys.stdout.buffer.write((pathlib.Path(os.environ["BLOBS"]) / url.rsplit(":", 1)[1]).read_bytes())
    if os.environ.get("FAIL_BLOB") and url.endswith(os.environ["FAIL_BLOB"]):
        sys.exit(22)
else:
    print("unexpected URL", file=sys.stderr)
    sys.exit(98)
""",
        )
        env = os.environ.copy()
        env.update(
            PATH=str(bin_dir) + os.pathsep + env["PATH"],
            MANIFESTS=str(manifest_dir), BLOBS=str(blob_dir),
            CURL_TRACE=str(self.work / "curl-trace"), LC_ALL="C",
            PYTHONDONTWRITEBYTECODE="1",
        )
        return manifest_dir, blob_dir, env

    def run_wrapper(self, root, env, image_digest):
        env = env.copy()
        env["ROOTFS"] = str(root)
        return subprocess.run(
            ["bash", str(WRAPPER), "--repo", "fixture/image", "--digest", image_digest],
            cwd=ROOT, env=env, capture_output=True, timeout=60, check=False,
        )

    def test_wrapper_applies_offline_index_layers_with_resolved_root(self):
        manifests, blobs, env = self.fake_registry()
        first = archive([("obsolete", "file", b"old"), ("keep", "file", b"keep")], True)
        second = archive([("new", "file", b"new"), (".wh.obsolete", "file", b"")])
        layers = [digest(first), digest(second)]
        for item, blob in zip(layers, (first, second)):
            (blobs / item[7:]).write_bytes(blob)
        manifest = json.dumps({"layers": [{"digest": item} for item in layers]}).encode() + b"\n"
        selected = digest(manifest)
        (manifests / selected[7:]).write_bytes(manifest)
        index = {"manifests": [
            {"platform": {"os": "windows", "architecture": "amd64"},
             "digest": "sha256:" + "c" * 64},
            {"platform": {"os": "linux", "architecture": "amd64"}, "digest": selected},
        ]}
        index_data = json.dumps(index).encode() + b"\n\n"
        image = digest(index_data)
        (manifests / image[7:]).write_bytes(index_data)
        alias = self.work / "root-alias"
        alias.symlink_to(self.root, target_is_directory=True)
        result = self.run_wrapper(alias, env, image)
        self.assert_ok(result)
        self.assertEqual((self.root / "keep").read_bytes(), b"keep")
        self.assertEqual((self.root / "new").read_bytes(), b"new")
        self.assertFalse((self.root / "obsolete").exists())
        trace = (self.work / "curl-trace").read_text(encoding="utf-8").splitlines()
        self.assertEqual(len(trace), 4 + 1)
        self.assertTrue(trace[1].endswith("/manifests/" + image))
        self.assertTrue(trace[2].endswith("/manifests/" + selected))
        self.assertTrue(trace[3].endswith("/blobs/" + layers[0]))
        self.assertTrue(trace[4].endswith("/blobs/" + layers[1]))
        self.assertNotIn("synthetic-token", "\n".join(trace))

    def test_wrapper_rejects_bad_layer_digest_before_whiteouts(self):
        manifests, blobs, env = self.fake_registry()
        first = archive([("keep", "file", b"keep")])
        wrong = "sha256:" + "0" * 64
        bad = archive([(".wh.keep", "file", b"")])
        (blobs / digest(first)[7:]).write_bytes(first)
        (blobs / wrong[7:]).write_bytes(bad)
        manifest = json.dumps({"layers": [{"digest": digest(first)}, {"digest": wrong}]}).encode()
        image = digest(manifest)
        (manifests / image[7:]).write_bytes(manifest)
        result = self.run_wrapper(self.root, env, image)
        self.assert_failed(result, "sha256 mismatch")
        self.assertEqual((self.root / "keep").read_bytes(), b"keep")
        self.assertEqual([path.name for path in self.root.iterdir()], ["keep"])

    def test_wrapper_checks_curl_status_before_applying_a_complete_blob(self):
        manifests, blobs, env = self.fake_registry()
        first = archive([("keep", "file", b"keep")])
        second = archive([(".wh.keep", "file", b"")])
        layers = [digest(first), digest(second)]
        for item, blob in zip(layers, (first, second)):
            (blobs / item[7:]).write_bytes(blob)
        manifest = json.dumps({"layers": [{"digest": item} for item in layers]}).encode()
        image = digest(manifest)
        (manifests / image[7:]).write_bytes(manifest)
        env["FAIL_BLOB"] = layers[1]
        result = self.run_wrapper(self.root, env, image)
        self.assert_failed(result, "layer download failed")
        self.assertEqual((self.root / "keep").read_bytes(), b"keep")
        self.assertEqual([path.name for path in self.root.iterdir()], ["keep"])

    def test_wrapper_rejects_a_manifest_not_matching_the_pinned_digest(self):
        manifests, _, env = self.fake_registry()
        image = "sha256:" + "a" * 64
        (manifests / image[7:]).write_text('{"layers":[]}\n', encoding="utf-8")
        result = self.run_wrapper(self.root, env, image)
        self.assert_failed(result, "manifest sha256 mismatch")
        self.assertEqual(list(self.root.iterdir()), [])
        trace = (self.work / "curl-trace").read_text(encoding="utf-8").splitlines()
        self.assertEqual(len(trace), 2)

    def test_wrapper_accepts_an_empty_image_without_fetching_layers(self):
        manifests, _, env = self.fake_registry()
        manifest = b'{"layers":[]}\n'
        image = digest(manifest)
        (manifests / image[7:]).write_bytes(manifest)
        self.assert_ok(self.run_wrapper(self.root, env, image))
        self.assertEqual(list(self.root.iterdir()), [])
        trace = (self.work / "curl-trace").read_text(encoding="utf-8").splitlines()
        self.assertEqual(len(trace), 2)

    def test_root_must_be_resolved_non_root_and_empty_before_any_registry_call(self):
        for root in ("/", "relative-root", str(self.work / "root-alias")):
            if root.endswith("root-alias"):
                Path(root).symlink_to(self.root, target_is_directory=True)
            with self.subTest(root=root):
                result = subprocess.run(
                    [sys.executable, "-B", "-I", str(HELPER), "--action", "check", "--root", root],
                    cwd=ROOT, capture_output=True, timeout=10, check=False,
                )
                self.assert_failed(result, "rootfs layer:")
        _, _, env = self.fake_registry()
        (self.root / "keep").write_text("keep", encoding="utf-8")
        result = self.run_wrapper(self.root, env, "sha256:" + "a" * 64)
        self.assert_failed(result, "ROOTFS must be empty")
        self.assertFalse((self.work / "curl-trace").exists())
        self.assertEqual((self.root / "keep").read_text(encoding="utf-8"), "keep")


if __name__ == "__main__":
    unittest.main()
