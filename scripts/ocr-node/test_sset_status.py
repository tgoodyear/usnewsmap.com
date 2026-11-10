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

    def test_package_only_volumes_left_out_by_this_run_are_counted_too(self):
        events = [
            {"fields": {"message": "Serial Set volumes", "volumes": 3, "documents": 5, "to_fetch": 2,
                        "rejected": 0, "package_only": 1}},
            {"fields": {"message": "fetching a Serial Set volume", "volume": "13163_04_00", "documents": 1}},
            {"fields": {"message": "every document of the volume is left out; an empty attempt written",
                        "volume": "13163_04_00", "package_only": True}},
            {"fields": {"message": "fetching a Serial Set volume", "volume": "01088_00_00", "documents": 4}},
            {"fields": {"message": "every document of the volume is left out; an empty attempt written",
                        "volume": "01088_00_00", "package_only": False}},
        ]
        journal = "".join(json.dumps(e) + "\n" for e in events)

        def run(argv, **_):
            stdout = {"show": "ExecMainStartTimestamp=@1700000000\n", "is-active": "active\n"}.get(argv[1], journal)
            return type("Done", (), {"stdout": stdout})()

        with tempfile.TemporaryDirectory() as directory, \
                patch.object(status_module.subprocess, "run", side_effect=run):
            out = status_module.status("usnm-sset.service", str(Path(directory) / "done"),
                                       str(Path(directory) / "status.json"))
        self.assertEqual((out["empty"], out["package_only"], out["documents_read"]), (2, 2, 5))

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

    def test_a_finished_fetch_reports_the_done_markers_totals(self):
        # The last journal sample was taken with two volumes to go; the
        # marker, written when the run finished, has them all.
        def run(argv, **_):
            stdout = {"show": "ExecMainStartTimestamp=\n", "is-active": "inactive\n"}[argv[1]]
            return type("Done", (), {"stdout": stdout})()

        with tempfile.TemporaryDirectory() as directory, \
                patch.object(status_module.subprocess, "run", side_effect=run):
            out_path = Path(directory) / "status.json"
            out_path.write_text(json.dumps({
                "to_fetch": 512, "curated": 509, "empty": 1, "pages": 607977, "left": 2,
                "current_volume": "14276_00_00", "eta_hours": 0.1, "started": "2026-11-05T03:18:58+00:00",
                "at": "2026-11-06T17:35:14+00:00",
            }))
            done = Path(directory) / "done"
            done.write_text(json.dumps({
                "already": 11407, "empty_volumes": 1, "finished_at": "2026-11-06T17:42:05Z",
                "pages": 611185, "volumes": 11919, "written": 511,
            }))
            out = status_module.status("usnm-sset.service", str(done), str(out_path))
        self.assertTrue(out["finished"])
        self.assertEqual(
            (out["to_fetch"], out["curated"], out["empty"], out["pages"], out["left"]),
            (512, 511, 1, 611185, 0),
        )
        self.assertEqual((out["series_volumes"], out["already"]), (11919, 11407))
        self.assertIsNone(out["current_volume"])
        self.assertIsNone(out["eta_hours"])
        self.assertNotEqual(out["at"], "2026-11-06T17:35:14+00:00")

    def test_an_unreadable_done_marker_keeps_the_journals_totals(self):
        def run(argv, **_):
            stdout = {"show": "ExecMainStartTimestamp=\n", "is-active": "inactive\n"}[argv[1]]
            return type("Done", (), {"stdout": stdout})()

        with tempfile.TemporaryDirectory() as directory, \
                patch.object(status_module.subprocess, "run", side_effect=run):
            out_path = Path(directory) / "status.json"
            out_path.write_text(json.dumps({"curated": 7, "left": 2}))
            done = Path(directory) / "done"
            done.write_text("")
            out = status_module.status("usnm-sset.service", str(done), str(out_path))
        self.assertTrue(out["finished"])
        self.assertEqual((out["curated"], out["left"]), (7, 2))

if __name__ == "__main__":
    unittest.main()
