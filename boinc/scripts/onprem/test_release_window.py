import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("release_window", HERE / "release-window.py")
rw = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rw)

spec = importlib.util.spec_from_file_location(
    "app_version_outcomes", HERE.parents[1] / "images/server/app-version-outcomes.py")
avo = importlib.util.module_from_spec(spec)
spec.loader.exec_module(avo)

LOG = """2026-11-27 18:56:17 UTC  [RESULT#121081 usnm_qwen_aa_0] Valid; granted 1966 credit [HOST#10]
2026-11-27 18:56:17 UTC  [HOST#10 AV#138] [outlier=0] Updating HAV in DB.
2026-11-27 18:56:17 UTC  [RESULT#121082 usnm_qwen_aa_1] Valid; granted 1966 credit [HOST#16]
2026-11-27 18:56:17 UTC  [HOST#16 AV#112] [outlier=0] Updating HAV in DB.
2026-11-27 19:00:00 UTC  [RESULT#121090 usnm_qwen_bb_0] Invalid [HOST#5]
"""


def outcomes(valid=0, invalid=0, client_errors=0, validate_errors=0):
    return {"event": "boinc_app_version_outcomes", "valid": valid, "invalid": invalid,
            "client_errors": client_errors, "validate_errors": validate_errors}


class Runner:
    """Answers app-version-hold.php and the outcomes step in order; records every command."""
    def __init__(self, reports, hold_rc=0):
        self.reports, self.hold_rc, self.calls = list(reports), hold_rc, []

    def __call__(self, cmd, **_):
        self.calls.append(cmd)
        if "app-version-outcomes" in cmd:
            return subprocess.CompletedProcess(cmd, 0, json.dumps(self.reports.pop(0)) + "\n", "")
        return subprocess.CompletedProcess(cmd, self.hold_rc, "", "")

    def actions(self):
        return [c[c.index("--action") + 1] for c in self.calls if "--action" in c]


class WindowTests(unittest.TestCase):
    def window(self, reports, apply=True, validated=2, max_hours=1, hold_rc=0):
        self.dir = tempfile.TemporaryDirectory()
        log = Path(self.dir.name) / "validator.log"
        log.write_text("")
        args = rw.arguments(["--app-version", "138", "--validated", str(validated), "--max-hours", str(max_hours),
                             "--poll-secs", "60", "--journal-dir", self.dir.name, *(["--apply"] if apply else [])])
        self.now = [1_000_000]
        runner = Runner(reports, hold_rc)

        def sleep(secs):
            self.now[0] += secs
        return rw.Window(args, run=runner, clock=lambda: self.now[0], sleep=sleep, log=log), runner

    def tearDown(self):
        if hasattr(self, "dir"):
            self.dir.cleanup()

    def journal(self):
        (path,) = Path(self.dir.name).glob("release-window-av138-*.jsonl")
        return [json.loads(line)["event"] for line in path.read_text().splitlines()]

    def test_qualifies_after_k_valid_results_and_stays_released(self):
        w, r = self.window([outcomes(valid=1), outcomes(valid=2)])
        self.assertEqual(w.run_window(), 0)
        self.assertEqual(r.actions(), ["release"])
        self.assertEqual(self.journal(), ["window_open", "release", "poll", "poll", "window_qualified"])

    def test_first_client_error_holds_at_once(self):
        w, r = self.window([outcomes(valid=1), outcomes(valid=1, client_errors=1)])
        self.assertEqual(w.run_window(), 2)
        self.assertEqual(r.actions(), ["release", "hold"])

    def test_invalid_or_validate_error_holds(self):
        for bad in (outcomes(invalid=1), outcomes(validate_errors=1)):
            w, r = self.window([bad])
            self.assertEqual(w.run_window(), 2)
            self.assertEqual(r.actions(), ["release", "hold"])
            self.dir.cleanup()

    def test_expiry_holds(self):
        w, r = self.window([outcomes(valid=0)] * 61, max_hours=1)
        self.assertEqual(w.run_window(), 3)
        self.assertEqual(r.actions(), ["release", "hold"])

    def test_dry_run_changes_nothing(self):
        w, r = self.window([], apply=False)
        self.assertEqual(w.run_window(), 0)
        self.assertEqual(r.calls, [])

    def test_failed_release_is_a_tool_error(self):
        w, r = self.window([], hold_rc=1)
        with self.assertRaises(RuntimeError):
            w.run_window()

    def test_argument_bounds(self):
        for bad in (["--validated", "0"], ["--max-hours", "200"], ["--poll-secs", "5"]):
            with self.assertRaises(SystemExit):
                rw.arguments(["--app-version", "1", *bad])

    def test_validator_log_attributes_valid_results_to_their_app_version(self):
        self.assertEqual(rw.valid_in_log(LOG.splitlines(), 138), {"121081"})
        self.assertEqual(rw.valid_in_log(LOG.splitlines(), 112), {"121082"})


class OutcomesTests(unittest.TestCase):
    def test_summary(self):
        doc = avo.summary([(1, 1, 3), (3, 0, 1), (1, 2, 1), (6, 0, 2)], 138, 100)
        self.assertEqual((doc["valid"], doc["invalid"], doc["client_errors"], doc["validate_errors"]), (3, 1, 1, 2))

    def test_argument_checks(self):
        self.assertEqual(avo.main(["--mysql-defaults-file", "x", "--database", "a;b", "--app-version-id", "1",
                                   "--since", "0"]), 2)


if __name__ == "__main__":
    unittest.main()
