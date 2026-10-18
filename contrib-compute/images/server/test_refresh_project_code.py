import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("refresh_code", Path(__file__).with_name("refresh-project-code.py"))
code = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(code)


class RefreshProjectCode(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir=Path.cwd())
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.project, self.source, self.templates = [self.root / n for n in ("project", "image", "templates")]
        self.templates.mkdir()
        for rel in ("bin/create_work", "cgi-bin/cgi", "py/Boinc/db_base.py", "html/inc/boinc_db.inc"):
            dest = self.project / rel
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_text("old " + rel)
            os.chmod(dest, 0o755)
            src = self.source / ("tools/create_work" if rel == "bin/create_work" else "sched/cgi" if rel == "cgi-bin/cgi" else rel)
            src.parent.mkdir(parents=True, exist_ok=True)
            src.write_text("new " + rel)
        (self.project / "config.xml").write_text("private unchanged config")
        (self.project / "upload").mkdir()
        (self.project / "upload/result").write_text("ongoing work")
        (self.project / "bin/boinc_path_config.py").write_text("project local path")
        self.backup = self.root / "backup"

    def test_copy_migrate_rollback_preserves_state_and_modes(self):
        result = code.apply(self.project, self.source, self.templates, self.backup)
        self.assertEqual(result["replaced"], 4)
        self.assertEqual((self.project / "bin/create_work").read_text(), "new bin/create_work")
        self.assertEqual((self.project / "bin/create_work").stat().st_mode & 0o777, 0o755)
        self.assertEqual((self.project / "upload/result").read_text(), "ongoing work")
        code.rollback(self.project, self.backup)
        self.assertEqual((self.project / "bin/create_work").read_text(), "old bin/create_work")
        self.assertEqual((self.project / "config.xml").read_text(), "private unchanged config")
        self.assertEqual((self.project / "bin/boinc_path_config.py").read_text(), "project local path")
        manifest = (self.backup / "manifest.json").read_text()
        self.assertNotIn("private unchanged", manifest)
        self.assertNotIn("upload/result", manifest)

    def test_unknown_file_fails_before_any_backup_or_write(self):
        (self.project / "bin/unknown").write_text("unknown")
        with self.assertRaises(ValueError):
            code.apply(self.project, self.source, self.templates, self.backup)
        self.assertFalse(self.backup.exists())

    def test_runtime_symlink_escape_rejected(self):
        (self.project / "py/escape").symlink_to(self.source)
        with self.assertRaises(ValueError):
            code.apply(self.project, self.source, self.templates, self.backup)

    def test_partial_failure_restores_already_replaced_files(self):
        original = code.replace
        calls = 0
        def fail_once(source, target, mode, *owner):
            nonlocal calls
            calls += 1
            if calls == 2:
                raise OSError("fixture SMB busy file")
            return original(source, target, mode, *owner)
        with patch.object(code, "replace", side_effect=fail_once):
            with self.assertRaises(OSError):
                code.apply(self.project, self.source, self.templates, self.backup)
        self.assertEqual((self.project / "bin/create_work").read_text(), "old bin/create_work")

    def test_rollback_refuses_new_concurrent_code_change(self):
        code.apply(self.project, self.source, self.templates, self.backup)
        (self.project / "bin/create_work").write_text("concurrent deployment")
        with self.assertRaises(ValueError):
            code.rollback(self.project, self.backup)

    def test_rollback_refuses_corrupted_backup(self):
        code.apply(self.project, self.source, self.templates, self.backup)
        (self.backup / "files/bin/create_work").write_text("bad")
        with self.assertRaises(ValueError):
            code.rollback(self.project, self.backup)

    def test_rollback_refuses_manifest_path_escape(self):
        code.apply(self.project, self.source, self.templates, self.backup)
        p = self.backup / "manifest.json"
        m = json.loads(p.read_text())
        m["files"][0]["path"] = "../outside"
        p.write_text(json.dumps(m))
        with self.assertRaises(ValueError):
            code.rollback(self.project, self.backup)
