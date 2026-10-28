"""Pinned data-flow archive verification and failure-safe host installation."""
import hashlib
import importlib.util
import io
from pathlib import Path
import sqlite3
import subprocess
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
            self.assertEqual(len(installer.read_archive(path, "a" * 40, digest)), 6)
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
            env = root / installer.ENV
            env.parent.mkdir(parents=True)
            env.write_text("USNM_DATA_FLOW_OCR_URL=http://127.0.0.1:8090\n")
            original = {database: database.read_bytes(), env: env.read_bytes()}
            for relative in payload:
                target = root / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(b"old reviewed bytes\n")
            before = {relative: (root / relative).read_bytes() for relative in payload}
            with patch.object(installer.subprocess, "run") as run:
                run.side_effect = [subprocess.CalledProcessError(1, ["systemd-analyze"]), None]
                with self.assertRaises(subprocess.CalledProcessError):
                    installer.install(payload, "a" * 40, root=root)
            for relative, data in before.items():
                self.assertEqual((root / relative).read_bytes(), data)
            self.assertFalse((root / installer.STAMP).exists())
            with patch.object(installer.subprocess, "run") as run:
                backup = installer.install(payload, "a" * 40, root=root)
                self.assertTrue(backup.is_dir())
                self.assertEqual([call.args[0][:2] for call in run.call_args_list],
                                 [["systemd-analyze", "verify"], ["systemctl", "daemon-reload"]])
            self.assertEqual((root / installer.STAMP).read_text(), "a" * 40 + "\n")
            for target, data in original.items():
                self.assertEqual(target.read_bytes(), data)

    def test_first_install_requires_safe_explicit_url(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            for url in (None, "http://name:password@example.org", "http://example.org\nBAD=yes"):
                with self.assertRaises(ValueError):
                    installer.install({}, "a" * 40, ocr_url=url, root=root)
            with patch.object(installer.subprocess, "run"):
                installer.install({}, "a" * 40, "http://127.0.0.1:8090", root=root)
            with self.assertRaisesRegex(ValueError, "preserve"):
                installer.install({}, "a" * 40, "http://127.0.0.1:8090", root=root)
