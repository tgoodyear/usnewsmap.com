import contextlib
import copy
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("handoff_window", Path(__file__).with_name("handoff-window.py"))
window = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(window)


def fixture():
    counts = {"count": 1, "failed": 0}
    event = {
        "event": "boinc_batch_tick", "schema_version": 1, "plan": "priority-1",
        "draining": False, "collection_backlog": 0, "collection_held": 0, "alerts": [],
        "newly_published": 1,
        "timings": {
            "commands": {label: dict(counts) for label in (
                "handoff/claim", "handoff/export", "handoff/collect", "cached scan push",
            )},
            "execution_routes": {route: dict(counts) for route in window.REQUIRED_ROUTES},
        },
    }
    return [{
        "__REALTIME_TIMESTAMP": str(at * 1_000_000),
        "_SYSTEMD_UNIT": window.UNIT,
        "MESSAGE": json.dumps(event),
    } for at in range(0, window.DAY + 1, 600)]


def change(records, function):
    event = json.loads(records[len(records) // 2]["MESSAGE"])
    function(event)
    records[len(records) // 2]["MESSAGE"] = json.dumps(event)


class HandoffWindowTests(unittest.TestCase):
    def test_complete_real_work_window_is_only_telemetry_not_retirement_permission(self):
        report = window.audit(fixture(), 0, window.DAY)
        self.assertTrue(report["telemetry_window_passed"], report)
        self.assertFalse(report["retirement_authorized"])
        self.assertEqual(len(report["remaining_evidence"]), 4)
        self.assertEqual(report["ticks"], 145)

    def test_legacy_missing_or_miscounted_routes_fail_closed(self):
        for mutate in (
            lambda e: e["timings"].pop("execution_routes"),
            lambda e: e["timings"]["execution_routes"].pop("onprem/scans"),
            lambda e: e["timings"]["execution_routes"]["onprem/scans"].update(count=2),
            lambda e: e["timings"]["commands"]["cached scan push"].update(count=True),
            lambda e: e["timings"]["execution_routes"]["onprem/scans"].update(count=True),
            lambda e: e["timings"]["execution_routes"]["onprem/scans"].update(failed=False),
        ):
            with self.subTest(mutate=mutate):
                records = fixture()
                change(records, mutate)
                self.assertFalse(window.audit(records, 0, window.DAY)["telemetry_window_passed"])

    def test_single_azure_fallback_failure_or_held_result_invalidates_window(self):
        for mutate in (
            lambda e: e["timings"]["execution_routes"].update({
                "azure/scans": e["timings"]["execution_routes"].pop("onprem/scans")}),
            lambda e: e["timings"]["execution_routes"]["onprem/scans"].update(failed=1),
            lambda e: e["timings"]["commands"]["cached scan push"].update(failed=1),
            lambda e: e.update(collection_held=1),
            lambda e: e.update(alerts=[{"code": "gold_onboarding_failed"}]),
            lambda e: e.update(draining=True),
            lambda e: e.update(plan="different-plan"),
            lambda e: e.update(event="boinc_batch_failure"),
            lambda e: e.update(event="boinc_batch_phase", ok=False),
        ):
            with self.subTest(mutate=mutate):
                records = fixture()
                change(records, mutate)
                self.assertFalse(window.audit(records, 0, window.DAY)["telemetry_window_passed"])

    def test_short_gapped_duplicate_and_idle_only_windows_are_not_acceptance(self):
        with self.assertRaises(ValueError):
            window.audit(fixture(), 0, window.DAY - 1)
        records = fixture()
        del records[10:20]
        self.assertFalse(window.audit(records, 0, window.DAY)["telemetry_window_passed"])
        records = fixture()
        records.append(copy.deepcopy(records[10]))
        self.assertFalse(window.audit(records, 0, window.DAY)["telemetry_window_passed"])
        records = fixture()
        for record in records:
            event = json.loads(record["MESSAGE"])
            event["newly_published"] = 0
            record["MESSAGE"] = json.dumps(event)
        self.assertFalse(window.audit(records, 0, window.DAY)["telemetry_window_passed"])
        self.assertFalse(window.audit([], 0, window.DAY)["telemetry_window_passed"])
        records = fixture()
        records[10]["_SYSTEMD_UNIT"] = "unrelated.service"
        self.assertFalse(window.audit(records, 0, window.DAY)["telemetry_window_passed"])

    def test_cli_retains_input_hash_and_rejects_malformed_input(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "journal.jsonl"
            args = ["--journal", str(path), "--since", "0", "--until", str(window.DAY)]
            raw = b"\n".join(json.dumps(row).encode() for row in fixture()) + b"\n"
            path.write_bytes(raw)
            with contextlib.redirect_stdout(io.StringIO()) as out:
                self.assertEqual(window.main(args), 0)
            self.assertEqual(json.loads(out.getvalue())["journal_sha256"],
                             window.hashlib.sha256(raw).hexdigest())
            path.write_text('{"MESSAGE": "truncated"\n')
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                window.main(args)
            self.assertEqual(error.exception.code, 2)


if __name__ == "__main__":
    unittest.main()
