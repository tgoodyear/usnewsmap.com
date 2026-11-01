"""Pinned data-flow archive verification and failure-safe host installation."""
import hashlib
import errno
import importlib.util
import io
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "install_data_flow", Path(__file__).with_name("install-data-flow.py"))
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


class InstallerTests(unittest.TestCase):
    def archive(self, root, *, commit="a" * 40, extra=None):
        path = root / "source.tar"
        with tarfile.open(path, "w", format=tarfile.PAX_FORMAT,
                          pax_headers={"comment": commit}) as archive:
            for name in [*installer.FILES, *([] if extra is None else [extra])]:
                data = b"# fixture\n" if name.endswith(".py") else b"[Unit]\n"
                entry = tarfile.TarInfo(name)
                entry.size = len(data)
                archive.addfile(entry, io.BytesIO(data))
        return path, hashlib.sha256(path.read_bytes()).hexdigest()

    def test_pinned_archive_rejects_mismatch_and_unexpected_members(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            path, digest = self.archive(root)
            self.assertEqual(len(installer.read_archive(path, "a" * 40, digest)), 8)
            for commit, checksum in (("b" * 40, digest), ("a" * 40, "0" * 64)):
                with self.assertRaisesRegex(ValueError, "differs"):
                    installer.read_archive(path, commit, checksum)
            path, digest = self.archive(root, extra="../outside.py")
            with self.assertRaisesRegex(ValueError, "unsafe"):
                installer.read_archive(path, "a" * 40, digest)

    def test_install_preserves_catalog_and_environment_and_rolls_back_failure(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            path, digest = self.archive(root)
            payload = installer.read_archive(path, "a" * 40, digest)
            database = root / "var/lib/usnm-data-flow/catalog.sqlite"
            database.parent.mkdir(parents=True)
            with sqlite3.connect(database) as db:
                db.execute("CREATE TABLE retained (value TEXT)")
                db.execute("INSERT INTO retained VALUES ('fixture')")
            db.close()
            env = root / installer.ENV
            env.parent.mkdir(parents=True)
            env.write_text("USNM_DATA_FLOW_OCR_URL=http://127.0.0.1:8090\n")
            original = {database: database.read_bytes(), env: env.read_bytes()}
            for relative in payload:
                target = root / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(b"old reviewed bytes\n")
            before = {relative: (root / relative).read_bytes() for relative in payload}
            for failure_call in (1, 2):
                with self.subTest(failure_call=failure_call), \
                        patch.object(installer.subprocess, "run") as run:
                    run.side_effect = ([None] * failure_call
                                       + [subprocess.CalledProcessError(1, ["fixture-verification"])]
                                       + [None])
                    with self.assertRaises(subprocess.CalledProcessError):
                        installer.install(payload, "a" * 40, root=root)
                    self.assertEqual(run.call_args.args[0], ["systemctl", "daemon-reload"])
                for relative, data in before.items():
                    self.assertEqual((root / relative).read_bytes(), data)
                self.assertFalse((root / installer.STAMP).exists())
                for target, data in original.items():
                    self.assertEqual(target.read_bytes(), data)
            with patch.object(installer.subprocess, "run") as run:
                backup = installer.install(payload, "a" * 40, root=root)
                self.assertTrue(backup.is_dir())
                self.assertEqual([call.args[0][:2] for call in run.call_args_list],
                                 [["systemd-analyze", "verify"], ["systemd-analyze", "verify"],
                                  ["systemctl", "daemon-reload"]])
            self.assertEqual((root / installer.STAMP).read_text(), "a" * 40 + "\n")
            for target, data in original.items():
                self.assertEqual(target.read_bytes(), data)

    def test_archive_mutation_after_hash_cannot_change_verified_payload(self):
        with tempfile.TemporaryDirectory() as temp:
            path, digest = self.archive(Path(temp))
            sha256 = hashlib.sha256

            def mutate_after_hash(data):
                result = sha256(data)
                path.write_bytes(path.read_bytes().replace(b"# fixture\n", b"# changed\n"))
                return result

            with patch.object(installer.hashlib, "sha256", side_effect=mutate_after_hash):
                payload = installer.read_archive(path, "a" * 40, digest)
            self.assertEqual(payload[installer.FILES["scripts/corpus/data_catalog.py"]],
                             b"# fixture\n")
            self.assertNotEqual(sha256(path.read_bytes()).hexdigest(), digest)

    def test_first_install_requires_safe_explicit_url(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            path, digest = self.archive(root)
            payload = installer.read_archive(path, "a" * 40, digest)
            for url in (None, "http://name:password@example.org", "http://example.org\nBAD=yes",
                        "http://127.0.0.1:8090"):
                with self.assertRaises(ValueError):
                    installer.install(payload, "a" * 40, ocr_url=url, root=root)
            with patch.object(installer.subprocess, "run"):
                installer.install(payload, "a" * 40,
                                  "http://127.0.0.1:8090/api/view.json", root=root)
            with self.assertRaisesRegex(ValueError, "preserve"):
                installer.install(payload, "a" * 40,
                                  "http://127.0.0.1:8090/api/view.json", root=root)

    def test_disk_full_rollback_preserves_inodes_and_attempts_all_recovery(self):
        for fail_restore in (False, True):
            with self.subTest(fail_restore=fail_restore), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                path, digest = self.archive(root)
                payload = installer.read_archive(path, "a" * 40, digest)
                env = root / installer.ENV
                env.parent.mkdir(parents=True)
                env.write_text("USNM_DATA_FLOW_OCR_URL=http://127.0.0.1/api/view.json\n")
                for relative in payload:
                    target = root / relative
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_bytes(b"old reviewed bytes\n")
                first = root / next(iter(payload))
                first.chmod(0o640)
                other_group = next((gid for gid in os.getgroups() if gid != os.getgid()), os.getgid())
                os.chown(first, -1, other_group)
                original = first.stat()
                write, replace = installer.atomic_write, os.replace
                writes = 0

                def disk_full(path, data, mode):
                    nonlocal writes
                    writes += 1
                    if writes == 2:
                        raise OSError(errno.ENOSPC, "fixture disk full")
                    write(path, data, mode)

                def restore(source, target):
                    if fail_restore and str(source).endswith(".rollback") and target == first:
                        raise OSError(errno.EIO, "fixture recovery failure")
                    return replace(source, target)

                with patch.object(installer.subprocess, "run") as run, \
                        patch.object(installer, "atomic_write", side_effect=disk_full), \
                        patch.object(installer.os, "replace", side_effect=restore):
                    with self.assertRaises(ExceptionGroup if fail_restore else OSError):
                        installer.install(payload, "a" * 40, root=root)
                    self.assertEqual(run.call_args.args[0], ["systemctl", "daemon-reload"])
                if fail_restore:
                    recovery = list(first.parent.glob("." + first.name + ".*.rollback"))
                    self.assertEqual(len(recovery), 1)
                    self.assertEqual(recovery[0].read_bytes(), b"old reviewed bytes\n")
                else:
                    self.assertEqual(first.read_bytes(), b"old reviewed bytes\n")
                    self.assertEqual((first.stat().st_ino, first.stat().st_gid, first.stat().st_mode),
                                     (original.st_ino, original.st_gid, original.st_mode))
                for relative in list(payload)[1:]:
                    self.assertEqual((root / relative).read_bytes(), b"old reviewed bytes\n")

    def test_main_requires_every_unit_inactive_and_validates_dry_run_units(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            path, digest = self.archive(root)
            payload = installer.read_archive(path, "a" * 40, digest)
            argv = ["install-data-flow.py", "--archive", str(path),
                    "--commit", "a" * 40, "--sha256", digest]
            with patch.object(sys, "argv", argv), \
                    patch.object(installer.subprocess, "run",
                                 side_effect=subprocess.CalledProcessError(1, ["systemd-analyze"])), \
                    patch.object(installer, "install") as install:
                with self.assertRaises(subprocess.CalledProcessError):
                    installer.main()
                install.assert_not_called()
            real_open = os.open

            def open_lock(path, flags, mode=0o777, **kwargs):
                if path == "/run/usnm-data-flow-install.lock":
                    path = root / "install.lock"
                return real_open(path, flags, mode, **kwargs)

            for active in (*installer.UNITS, "query-failure", None):
                with self.subTest(active=active), patch.object(sys, "argv", [*argv, "--apply"]), \
                        patch.object(installer.os, "geteuid", return_value=0), \
                        patch.object(installer.os, "open", side_effect=open_lock), \
                        patch.object(installer.pwd, "getpwnam"), \
                        patch.object(installer, "install", return_value=root / "backup") as install, \
                        patch.object(installer.subprocess, "run") as run:
                    def state(command, **_kwargs):
                        if active == "query-failure":
                            raise subprocess.CalledProcessError(1, command)
                        return subprocess.CompletedProcess(
                            command, 0, stdout="active\n" if command[-1] == active else "inactive\n")
                    run.side_effect = state
                    if active is not None:
                        with self.assertRaises((RuntimeError, subprocess.CalledProcessError)):
                            installer.main()
                        install.assert_not_called()
                    else:
                        installer.main()
                        install.assert_called_once()
