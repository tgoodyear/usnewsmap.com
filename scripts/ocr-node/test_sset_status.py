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


if __name__ == "__main__":
    unittest.main()
