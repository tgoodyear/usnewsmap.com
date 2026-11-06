"""Feeder allocation detection is independent of queued work and never repairs it."""

import configparser
import importlib.util
import io
import json
import math
import os
from pathlib import Path
import shlex
import struct
import sys
import tempfile
import types
import unittest
from unittest.mock import Mock, call, patch
import xml.etree.ElementTree as ET


HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("feeder_slot_guard", HERE / "feeder-slot-guard.py")
GUARD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GUARD)

PROJECT = Path("fixture-project")
DIRECTORY = PROJECT / "pid_test"
BOOT = "01234567-89ab-cdef-0123-456789abcdef"
IDENTITY = (42, "300", BOOT)
NOW_MS = 1_000_000


def bits(value):
    return struct.pack(">d", value).hex()


def snapshot(**overrides):
    record = {
        "version": 1, "pid": 42, "start_ticks": "300", "boot_id": BOOT,
        "sequence": 1, "monotonic_ms": NOW_MS - 1000, "drift": False,
        "uncertain": False, "reloading": False,
        "apps": [[1, bits(1.0)], [6, bits(16.0)], [9, bits(1.0)]],
    }
    return json.dumps(record | overrides).encode()


class Observation(unittest.TestCase):
    def observe(self, raw=None, current=None, identities=None, reads=None, times=None):
        raw = snapshot() if raw is None else raw
        identities = identities or [(DIRECTORY, IDENTITY)] * 2
        reads = reads or [raw, raw]
        with patch.object(GUARD, "feeder_identity", side_effect=identities), \
             patch.object(GUARD, "status_path", return_value=DIRECTORY / "usnm-feeder-slots.json"), \
             patch.object(GUARD, "read_bounded", side_effect=reads), \
             patch.object(GUARD, "database_apps",
                          return_value={1: 1.0, 6: 16.0, 9: 1.0} if current is None else current) as db, \
             patch.object(GUARD.time, "monotonic", side_effect=times, return_value=NOW_MS / 1000):
            result = GUARD.check(PROJECT)
        return result, db

    def test_match_and_subcent_weight_change_use_the_full_double(self):
        match, _ = self.observe()
        self.assertEqual(match, {"status": "match", "reason": "allocation_matches_registry"})
        changed, _ = self.observe(current={1: 1.0, 6: 16.000001, 9: 1.0})
        self.assertEqual(changed["status"], "drift")
        self.assertEqual(changed["weight_changed_app_ids"], [6])

    def test_app_addition_removal_and_same_count_replacement(self):
        for current, added, removed in (
            ({1: 1.0, 6: 16.0, 9: 1.0, 10: 1.0}, [10], []),
            ({1: 1.0, 6: 16.0}, [], [9]),
            ({1: 1.0, 6: 16.0, 10: 1.0}, [10], [9]),
            ({}, [], [1, 6, 9]),
        ):
            with self.subTest(current=current):
                result, _ = self.observe(current=current)
                self.assertEqual(result["status"], "drift")
                self.assertEqual(result["added_app_ids"], added)
                self.assertEqual(result["removed_app_ids"], removed)

    def test_reload_drift_stays_actionable_even_if_registry_reverts_or_db_is_down(self):
        result, db = self.observe(raw=snapshot(drift=True))
        self.assertEqual(result, {"status": "drift", "reason": "reload_changed_app_order_or_weights"})
        db.assert_not_called()

    def test_unknown_snapshot_is_never_accepted(self):
        for raw, reason in (
            (snapshot(uncertain=True), "feeder_snapshot_uncertain"),
            (snapshot(reloading=True), "feeder_reload_in_progress"),
            (snapshot(monotonic_ms=NOW_MS - GUARD.MAX_AGE_MS - 1), "stale_snapshot"),
            (snapshot(monotonic_ms=NOW_MS + 1), "stale_snapshot"),
            (snapshot(pid=43), "snapshot_process_mismatch"),
            (snapshot(start_ticks="301"), "snapshot_process_mismatch"),
            (snapshot(boot_id="ffffffff-89ab-cdef-0123-456789abcdef"), "snapshot_process_mismatch"),
            (snapshot(version=2), "invalid_snapshot"),
            (snapshot(apps=[]), "invalid_snapshot"),
            (snapshot(apps=[[1, bits(1.0)], [1, bits(1.0)]]), "invalid_snapshot"),
            (snapshot(apps=[[6, bits(math.nan)]]), "invalid_snapshot"),
            (snapshot(apps=[[6, bits(-1.0)]]), "invalid_snapshot"),
            (b"{", "observation_unavailable"),
        ):
            with self.subTest(reason=reason, raw=raw[:90]):
                result, db = self.observe(raw=raw)
                self.assertEqual(result, {"status": "unknown", "reason": reason})
                db.assert_not_called()

    def test_restart_pid_reuse_and_concurrent_reload_do_not_mix_observations(self):
        new_process = (DIRECTORY, (42, "301", BOOT))
        result, _ = self.observe(identities=[(DIRECTORY, IDENTITY), new_process])
        self.assertEqual(result, {"status": "unknown", "reason": "observation_changed"})
        result, _ = self.observe(reads=[snapshot(), snapshot(sequence=2)])
        self.assertEqual(result, {"status": "unknown", "reason": "observation_changed"})
        result, _ = self.observe(identities=[new_process])
        self.assertEqual(result, {"status": "unknown", "reason": "snapshot_process_mismatch"})
        result, _ = self.observe(raw=snapshot(start_ticks="301"), identities=[new_process] * 2)
        self.assertEqual(result["status"], "match")

    def test_snapshot_expiring_during_observation_is_unknown(self):
        for drift in (False, True):
            with self.subTest(drift=drift):
                result, _ = self.observe(
                    raw=snapshot(monotonic_ms=NOW_MS - GUARD.MAX_AGE_MS + 1, drift=drift),
                    times=[NOW_MS / 1000, (NOW_MS + 10) / 1000],
                )
                self.assertEqual(result, {"status": "unknown", "reason": "stale_snapshot"})

    def test_missing_feeder_or_failed_database_is_unknown_not_healthy(self):
        with patch.object(GUARD, "feeder_identity", side_effect=FileNotFoundError):
            self.assertEqual(GUARD.check(PROJECT)["status"], "unknown")
        with patch.object(GUARD, "feeder_identity", side_effect=RuntimeError("private detail")):
            self.assertEqual(GUARD.check(PROJECT), {"status": "unknown", "reason": "observation_unavailable"})
        with patch.object(GUARD, "feeder_identity", return_value=(DIRECTORY, IDENTITY)), \
             patch.object(GUARD, "status_path", return_value=DIRECTORY / "usnm-feeder-slots.json"), \
             patch.object(GUARD, "read_bounded", return_value=snapshot()), \
             patch.object(GUARD, "database_apps", side_effect=GUARD.Unknown("database_unavailable")), \
             patch.object(GUARD.time, "monotonic", return_value=NOW_MS / 1000):
            self.assertEqual(GUARD.check(PROJECT), {"status": "unknown", "reason": "database_unavailable"})

    def test_maintenance_is_not_reported_as_a_match_or_queried(self):
        with patch.object(Path, "exists", return_value=True), \
             patch.object(GUARD, "database_apps") as db:
            result = GUARD.check(PROJECT)
        self.assertEqual(result, {"status": "maintenance", "reason": "stop_maintenance"})
        db.assert_not_called()

    def test_supervisor_restart_hold_is_unknown_without_a_query(self):
        with tempfile.TemporaryDirectory() as root:
            snapshot_path = Path(root, "usnm-feeder-slots.json")
            Path(root, GUARD.HOLD).touch()
            with patch.object(GUARD, "status_path", return_value=snapshot_path), \
                 patch.object(GUARD, "feeder_identity") as identity, \
                 patch.object(GUARD, "database_apps") as db:
                result = GUARD.check(PROJECT)
        self.assertEqual(result, {"status": "unknown", "reason": "feeder_restart_hold"})
        identity.assert_not_called()
        db.assert_not_called()
        # The supervisor writes the marker the guard reads, in the same directory.
        self.assertIn(f'hold="$dir/{GUARD.HOLD}"', (HERE / "boinc-daemons.sh").read_text())

    def test_unavailable_maintenance_probe_is_unknown_not_a_monitor_crash(self):
        with patch.object(Path, "exists", side_effect=PermissionError("private detail")), \
             patch.object(GUARD, "database_apps") as db:
            result = GUARD.check(PROJECT)
        self.assertEqual(result, {"status": "unknown", "reason": "observation_unavailable"})
        db.assert_not_called()


class ProcessIdentity(unittest.TestCase):
    def stat_line(self, state="S", ticks="300"):
        # A comm containing parentheses must not shift Linux /proc/stat field 22.
        fields = [state] + ["0"] * 18 + [ticks] + ["0"]
        return ("42 (feeder with) parentheses) " + " ".join(fields)).encode()

    def identity(self, pid=b"42\n", stat_line=None,
                 command=b"/project/bin/feeder\0-d\0" b"2\0--allapps\0"):
        records = {
            DIRECTORY / "feeder.pid": pid,
            Path("/proc/42/stat"): stat_line or self.stat_line(),
            Path("/proc/42/cmdline"): command,
            Path("/proc/sys/kernel/random/boot_id"): (BOOT + "\n").encode(),
        }
        with patch.object(GUARD.socket, "gethostname", return_value="test.example"), \
             patch.object(GUARD, "read_bounded", side_effect=lambda path, *_: records[path]):
            return GUARD.feeder_identity(PROJECT)

    def test_checks_current_pid_start_tick_boot_and_allapps_command(self):
        self.assertEqual(self.identity(), (DIRECTORY, IDENTITY))
        for arguments in (
            {"pid": b"0"}, {"stat_line": self.stat_line(state="Z")},
            {"stat_line": self.stat_line(ticks="bad")},
            {"command": b"/project/bin/other\0--allapps\0"},
            {"command": b"/project/bin/feeder\0"},
        ):
            with self.subTest(arguments=arguments), self.assertRaises(GUARD.Unknown):
                self.identity(**arguments)


class SnapshotFile(unittest.TestCase):
    def test_owner_permissions_and_symlinks_are_checked(self):
        path = Path.cwd() / f".feeder-guard-owner-{os.getpid()}"
        link = Path.cwd() / f".feeder-guard-owner-link-{os.getpid()}"
        with path.open("xb") as stream:
            self.addCleanup(path.unlink, missing_ok=True)
            stream.write(b"fixture")
        path.chmod(0o640)
        self.assertEqual(GUARD.read_bounded(path, owner=os.geteuid()), b"fixture")
        with self.assertRaises(GUARD.Unknown):
            GUARD.read_bounded(path, owner=os.geteuid() + 1)
        path.chmod(0o660)
        with self.assertRaises(GUARD.Unknown):
            GUARD.read_bounded(path, owner=os.geteuid())
        link.symlink_to(path)
        self.addCleanup(link.unlink, missing_ok=True)
        with self.assertRaises(OSError):
            GUARD.read_bounded(link, owner=os.geteuid())


class Database(unittest.TestCase):
    def connect(self, rows, cipher=("Ssl_cipher", "TLS_AES_256_GCM_SHA384"),
                host="db.example:3307"):
        config = ET.ElementTree(ET.fromstring(
            "<boinc><config><db_host>" + host + "</db_host><db_user>reader</db_user>"
            "<db_passwd>synthetic</db_passwd><db_name>usnmocr</db_name></config></boinc>"
        ))
        cursor = Mock()
        cursor.fetchone.return_value = cipher
        cursor.fetchall.return_value = rows
        db = Mock()
        db.cursor.return_value = cursor
        connector = Mock(return_value=db)
        module = types.SimpleNamespace(connect=connector)
        with patch.dict(sys.modules, {"MySQLdb": module}), \
             patch.object(GUARD.ET, "parse", return_value=config), \
             patch.object(Path, "is_file", return_value=True):
            result = GUARD.database_apps(PROJECT)
        return result, connector, cursor, db

    def test_uses_bounded_active_registry_query_and_verified_tls(self):
        result, connect, cursor, db = self.connect([(1, 1.0), (6, 16.0)])
        self.assertEqual(result, {1: 1.0, 6: 16.0})
        options = connect.call_args.kwargs
        self.assertEqual((options["host"], options["port"]), ("db.example", 3307))
        self.assertEqual(options["ssl"], {"ca": GUARD.TLS_CA})
        self.assertEqual(options["read_default_file"], str(GUARD.TLS_OPTIONS))
        self.assertEqual(cursor.execute.call_args_list, [
            call("SHOW SESSION STATUS LIKE 'Ssl_cipher'"),
            call("SELECT id, weight FROM app WHERE deprecated=0 ORDER BY id LIMIT 11"),
        ])
        cursor.close.assert_called_once()
        db.close.assert_called_once()

    def test_failed_tls_bad_rows_and_invalid_endpoint_do_not_expose_details(self):
        for kwargs in (
            {"rows": [(1, 1.0)], "cipher": None},
            {"rows": [(1, -1.0)]},
            {"rows": [(1, 1.0), (1, 2.0)]},
            {"rows": [(1, 1.0)], "host": "db.example:not-a-port"},
        ):
            with self.subTest(kwargs=kwargs), self.assertRaises(GUARD.Unknown) as exc:
                self.connect(**kwargs)
            self.assertEqual(str(exc.exception), "database_unavailable")


class Reporting(unittest.TestCase):
    def test_one_shot_exit_codes_and_structured_report(self):
        for status, exit_code in (("match", 0), ("maintenance", 0), ("drift", 1), ("unknown", 2)):
            with self.subTest(status=status), patch.object(GUARD, "check", return_value={
                "status": status, "reason": "fixture",
            }), patch.object(sys, "stdout", new_callable=io.StringIO) as stdout:
                self.assertEqual(GUARD.main(["--project", str(PROJECT)]), exit_code)
                record = json.loads(stdout.getvalue())
                self.assertEqual(record["event"], "boinc_feeder_slot_guard")
                self.assertEqual(record["status"], status)

    def test_server_image_supervises_the_read_only_monitor_as_boincadm(self):
        image = (HERE / "Containerfile").read_text()
        supervisor = (HERE / "supervisord.conf").read_text().split("[program:feeder-slot-guard]", 1)[1]
        self.assertIn("images/server/feeder-slot-guard.py", image)
        self.assertIn("feeder-slot-guard.py --watch", supervisor)
        self.assertIn("user=boincadm", supervisor)

    def test_onprem_timer_makes_drift_or_unknown_a_failed_unit(self):
        onprem = HERE.parents[2] / "scripts/boinc/onprem"
        unit = configparser.ConfigParser()
        unit.read(onprem / "usnm-feeder-slot-guard.service")
        command = shlex.split(unit["Service"]["ExecStart"])
        self.assertEqual(command[:5], ["/usr/bin/podman", "exec", "--user", "boincadm", "usnm-boinc-server"])
        self.assertEqual(command[-2:], ["--project", "/project/usnmocr"])
        self.assertNotIn("--watch", command)
        self.assertNotIn("Restart", unit["Service"])
        timer = configparser.ConfigParser()
        timer.read(onprem / "usnm-feeder-slot-guard.timer")
        self.assertEqual(timer["Timer"]["OnUnitInactiveSec"], "5min")


if __name__ == "__main__":
    unittest.main()
