import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import urllib.error


spec = importlib.util.spec_from_file_location("sset_status", Path(__file__).with_name("sset-status.py"))
status_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(status_module)


class StatusPushTests(unittest.TestCase):
    def test_failed_push_is_a_failed_update_but_retains_local_status(self):
        url = "http://dashboard.invalid/api/sset/usnm-sset"
        failures = [
            urllib.error.HTTPError(url, 403, "Forbidden", {}, None),
            urllib.error.URLError("connection refused"),
        ]
        for error in failures:
            with self.subTest(error=type(error).__name__), tempfile.TemporaryDirectory() as directory:
                output = Path(directory) / "status.json"
                stderr = io.StringIO()
                with patch("sys.argv", ["sset-status", "--out", str(output), "--push-url", url]), \
                        patch.object(status_module, "status", return_value={"curated": 3544}), \
                        patch.object(status_module.urllib.request, "urlopen", side_effect=error), \
                        contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(stderr):
                    self.assertEqual(status_module.main(), 1)
                self.assertEqual(json.loads(output.read_text()), {"curated": 3544})
                self.assertIn("error: can't push", stderr.getvalue())

    def test_successful_push_still_reports_success(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "status.json"
            with patch("sys.argv", [
                "sset-status", "--out", str(output), "--push-url",
                "http://dashboard.invalid/api/sset/usnm-sset",
            ]), patch.object(status_module, "status", return_value={"curated": 3544}), \
                    patch.object(status_module.urllib.request, "urlopen") as send, \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(status_module.main(), 0)
            send.return_value.close.assert_called_once()
            self.assertEqual(json.loads(output.read_text()), {"curated": 3544})


class StatusJournalTests(unittest.TestCase):
    def test_refused_ids_and_package_only_volumes_are_counted(self):
        listed = {"fields": {
            "message": "Serial Set volumes", "volumes": 2, "documents": 4, "to_fetch": 2,
            "rejected": 3, "package_only": 1,
        }}
        journal = json.dumps(listed) + "\n"
        systemctl_show = "ExecMainStartTimestamp=@1700000000\n"

        def run(argv, **_):
            stdout = {"show": systemctl_show, "is-active": "active\n"}.get(argv[1], journal)
            return type("Done", (), {"stdout": stdout})()

        with tempfile.TemporaryDirectory() as directory, \
                patch.object(status_module.subprocess, "run", side_effect=run):
            out = status_module.status("usnm-sset.service", str(Path(directory) / "done"),
                                       str(Path(directory) / "status.json"))
        self.assertEqual((out["rejected"], out["package_only"], out["finished"]), (3, 1, False))

    def test_a_unit_not_started_since_boot_still_has_the_counts(self):
        def run(argv, **_):
            stdout = {"show": "ExecMainStartTimestamp=\n", "is-active": "inactive\n"}[argv[1]]
            return type("Done", (), {"stdout": stdout})()

        with tempfile.TemporaryDirectory() as directory, \
                patch.object(status_module.subprocess, "run", side_effect=run):
            out_path = Path(directory) / "status.json"
            out_path.write_text(json.dumps({"curated": 7, "started": None}))
            out = status_module.status("usnm-sset.service", str(Path(directory) / "done"), str(out_path))
        self.assertEqual((out["curated"], out["rejected"], out["package_only"]), (7, 0, 0))


if __name__ == "__main__":
    unittest.main()
