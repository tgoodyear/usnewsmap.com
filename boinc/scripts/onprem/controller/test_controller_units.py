"""Per-plan controller ticks (#1690): python3 -m unittest -v boinc/scripts/onprem/controller/test_controller_units.py

The unit files are parsed and checked (and run through `systemd-analyze verify` where it is
installed), controller-tick.sh runs against a fake batch_scheduler.py and a fake systemctl, and
install.sh installs into a temporary root. The tick tests need flock (Linux).
"""

import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
ONPREM = HERE.parent
REPO = HERE.parents[3]
TICK = HERE / "controller-tick.sh"
INSTALL = HERE / "install.sh"
SERVICE = HERE / "usnm-boinc-priority-controller@.service"
TIMER = HERE / "usnm-boinc-priority-controller@.timer"
LOCK = "/run/usnm-boinc-controller-tick.lock"
sys.path.insert(0, str(ONPREM.parent))
import feed_collect  # noqa: E402

# The fake batch_scheduler.py: logs its arguments and whether the tick lock is held, then exits
# with $FAKE_EXIT.
FAKE_SCHEDULER = r'''
import json, os, subprocess, sys
lock = os.environ.get("FAKE_TICK_LOCK")
held = None
if lock:
    held = subprocess.run(["flock", "-n", lock, "true"]).returncode != 0
with open(os.environ["FAKE_LOG"], "a") as log:
    log.write(json.dumps({"argv": sys.argv[1:], "lock_held": held}) + "\n")
sys.exit(int(os.environ.get("FAKE_EXIT", "0")))
'''

# The fake systemctl: show (the scheduled archive's state, from $FAKE_ARCHIVE_FILE or
# $FAKE_ARCHIVE_STATE), list-units (a hand-run archive unit, $FAKE_ARCHIVE_UNITS) and is-active
# (main's timer, $FAKE_MAIN_TIMER).
FAKE_SYSTEMCTL = r'''#!/bin/sh
echo "systemctl $*" >> "$FAKE_SYSTEMCTL_LOG"
case "$1" in
  show)
    if [ -n "$FAKE_ARCHIVE_FILE" ] && [ -f "$FAKE_ARCHIVE_FILE" ]; then cat "$FAKE_ARCHIVE_FILE"
    else echo "${FAKE_ARCHIVE_STATE:-inactive}"; fi ;;
  list-units) [ -z "$FAKE_ARCHIVE_UNITS" ] || echo "$FAKE_ARCHIVE_UNITS loaded activating start archive" ;;
  is-active) echo "${FAKE_MAIN_TIMER:-active}" ;;
esac
'''


def parse_unit(path):
    """A unit file as {section: {key: [values]}}; fails on anything systemd would not read."""
    sections, current = {}, None
    for number, raw in enumerate(path.read_text().splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith(("#", ";")):
            continue
        assert not raw.endswith("\\"), f"{path.name}: line {number} is continued"
        if line.startswith("["):
            assert line.endswith("]"), f"{path.name}: bad section {line}"
            current = sections.setdefault(line[1:-1], {})
            continue
        assert current is not None, f"{path.name}: a setting before any section"
        key, sep, value = line.partition("=")
        assert sep and key.strip() and " " not in key.strip(), f"{path.name}: bad line {line}"
        current.setdefault(key.strip(), []).append(value.strip())
    return sections


class UnitFiles(unittest.TestCase):
    def test_the_service_runs_one_tick_of_its_instances_plan(self):
        unit = parse_unit(SERVICE)
        self.assertEqual(set(unit), {"Unit", "Service"})
        service = unit["Service"]
        self.assertEqual(service["Type"], ["oneshot"])
        self.assertEqual(service["WorkingDirectory"], ["/opt/usnm"])
        self.assertEqual(service["Environment"], ["USNM_SETTINGS_HOME=/root/.config/usnewsmap"])
        # A tick killed by a start timeout would leave the controller lease held (#1698).
        self.assertEqual(service["TimeoutStartSec"], ["infinity"])
        (start,) = service["ExecStart"]
        argv = start.split()
        self.assertEqual(argv[0], "/usr/bin/bash")
        self.assertEqual(argv[2:], ["--plan", "%i"])
        # The script it runs is this one, at its place in the pinned checkout.
        self.assertTrue(argv[1].startswith("/opt/usnm/"))
        self.assertEqual(REPO / argv[1].removeprefix("/opt/usnm/"), TICK)
        self.assertNotIn("Restart", service)
        # As the shared and drain services ran.
        self.assertEqual(service["Nice"], ["10"])

    def test_the_timer_retries_five_minutes_after_any_end(self):
        unit = parse_unit(TIMER)
        self.assertEqual(set(unit), {"Unit", "Timer", "Install"})
        self.assertEqual(unit["Timer"], {"OnBootSec": ["5min"], "OnUnitInactiveSec": ["5min"]})
        self.assertEqual(unit["Install"], {"WantedBy": ["timers.target"]})

    def test_the_held_archive_holds_the_same_tick_lock(self):
        service = parse_unit(ONPREM / "usnm-boinc-held-archive.service")["Service"]
        self.assertIn(f"USNM_HELD_ARCHIVE_TICK_LOCK={LOCK}", service["Environment"])
        self.assertIn(f"TICK_LOCK={LOCK}\n", TICK.read_text())

    @unittest.skipUnless(shutil.which("systemd-analyze"), "needs systemd-analyze")
    def test_systemd_analyze_verifies_an_instance(self):
        with tempfile.TemporaryDirectory() as folder:
            units = Path(folder)
            for path in (SERVICE, TIMER, ONPREM / "usnm-boinc-held-archive.service",
                         ONPREM / "usnm-boinc-held-archive.timer"):
                shutil.copy(path, units / path.name)
            for name in ("usnm-boinc-priority-controller@main.service",
                         "usnm-boinc-priority-controller@native-feed-3.timer"):
                (units / name).symlink_to(units / name.replace("@main", "@").replace("@native-feed-3", "@"))
            done = subprocess.run(
                ["systemd-analyze", "verify", "--man=no",
                 str(units / "usnm-boinc-priority-controller@main.service"),
                 str(units / "usnm-boinc-priority-controller@native-feed-3.timer"),
                 str(units / "usnm-boinc-held-archive.service"), str(units / "usnm-boinc-held-archive.timer")],
                capture_output=True, text=True, env={**os.environ, "SYSTEMD_LOG_LEVEL": "warning"})
            # The checkout and its scripts aren't here; anything else is a fault in the files.
            problems = [line for line in done.stderr.splitlines()
                        if line.strip() and "/opt/usnm" not in line]
            self.assertEqual(problems, [], done.stderr)


HANDOFF = "--onprem-handoff claim\n--onprem-handoff collect\n--onprem-handoff export\n"
STATUS_URL = "https://boinc.usnewsmap.com/server_status.php?xml=1"

# Real-shaped flags files, as the migration writes them (docs/boinc/server/batch-scheduler.md,
# "Flags files by kind"). main: the shared service's batch.conf, one flag and its value a line.
MAIN_FLAGS = f"""# From usnm-boinc-priority-controller.service.d/batch.conf (#1690)
@kind main
--server-status-url {STATUS_URL}
--max-global-pending 3000
--other-app-budget 2500
--rest-journals
--batch-submit
--batch-spool-out
--batch-collect
--collect-parallel 4
--batch-export
{HANDOFF}--dead-letter-failed
--skip-truncated-scans
--claim-language srp
--refill-cap 175
--check-state-dir /var/lib/usnm-priority-controller.queue-feed-10
--check-state-dir /var/lib/usnm-priority-controller.queue-feed-11
--check-state-dir /var/lib/usnm-priority-controller.queue-feed-12
"""
# A native plan: native-feed-tick.sh's fixed flags, then its old extras file pasted as it is (one
# space-separated line).
NATIVE_FLAGS = f"""@kind native
--server-status-url {STATUS_URL} --max-global-pending 3000 --other-app-budget 2500
--rest-journals --batch-spool-out --batch-collect --batch-export
--onprem-handoff claim --onprem-handoff collect --onprem-handoff export --dead-letter-failed
--check-state-dir /var/lib/usnm-priority-controller --check-state-dir /var/lib/usnm-priority-controller.queue-feed-12 --check-state-dir /var/lib/usnm-priority-controller.native-feed-3 --refill-cap 96
"""
# A drain plan: the drain service's line for it.
DRAIN_FLAGS = f"""@kind drain
--drain --server-status-url {STATUS_URL} --rest-journals --batch-spool-out --batch-collect
--onprem-handoff claim --onprem-handoff collect --onprem-handoff export --dead-letter-failed
--check-state-dir /var/lib/usnm-priority-controller
"""
FIXED_NATIVE = ["--server-status-url", STATUS_URL, "--max-global-pending", "3000", "--other-app-budget", "2500",
                "--rest-journals", "--batch-spool-out", "--batch-collect", "--batch-export",
                "--onprem-handoff", "claim", "--onprem-handoff", "collect", "--onprem-handoff", "export",
                "--dead-letter-failed"]


def timer_drop_in(path, kind, boot, every):
    return (f"# From {path} (@kind {kind}), written by install.sh (#1690).\n[Timer]\nOnBootSec=\n"
            f"OnBootSec={boot}\nOnUnitInactiveSec={every}\n")


class Fixture(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = self.root = Path(self.tmp.name)
        for name in ("etc", "lib", "bin"):
            (root / name).mkdir()
        (root / "scheduler.py").write_text(FAKE_SCHEDULER)
        (root / "bin/systemctl").write_text(FAKE_SYSTEMCTL)
        (root / "bin/systemctl").chmod(0o755)
        self.lock = root / "tick.lock"
        for plan in ("main", "native-feed-4", "queue-feed-10"):
            (root / f"etc/{self.name(plan)}.json").write_text("{}")

    def tearDown(self):
        self.tmp.cleanup()

    @staticmethod
    def name(plan):
        return "boinc-priority-controller" + ("" if plan == "main" else f".{plan}")

    def flags(self, plan, text, kind=None):
        """The plan's flags file; with `kind`, the @kind line and the hand-off flags go first."""
        if kind is not None:
            text = f"@kind {kind}\n{HANDOFF}" + ("--drain\n" if kind == "drain" else "") + text
        path = self.root / f"etc/{self.name(plan)}.flags"
        path.write_text(text)
        return path

    def tick(self, plan, *extra, **env):
        root = self.root
        environment = {**os.environ, "PATH": f"{root / 'bin'}{os.pathsep}{os.environ['PATH']}",
                       "FAKE_LOG": str(root / "log"), "FAKE_SYSTEMCTL_LOG": str(root / "systemctl.log"),
                       "FAKE_TICK_LOCK": str(self.lock), **env}
        return subprocess.run(
            ["bash", str(TICK), "--plan", plan, "--config-dir", str(root / "etc"),
             "--state-parent", str(root / "lib"), "--scheduler", str(root / "scheduler.py"),
             "--tick-lock", str(self.lock), "--held-archive-lock", str(root / "archive.lock"),
             "--lock-wait-secs", "2", *extra],
            env=environment, capture_output=True, text=True, timeout=60)

    def command(self, plan):
        done = self.tick(plan, "--print-command")
        self.assertEqual(done.returncode, 0, done.stderr)
        return done.stdout.splitlines()

    def runs(self):
        path = self.root / "log"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def failure(self, done):
        lines = [json.loads(line) for line in done.stderr.splitlines() if line.startswith("{")]
        self.assertEqual(len(lines), 1, done.stderr)
        self.assertEqual(lines[0]["event"], "boinc_batch_failure")
        return lines[0]["error"]


class FlagsFiles(Fixture):
    """The resolved command and cadence of real-shaped flags files (no lock, any platform)."""

    def head(self, plan):
        root = self.root
        return ["python3", str(root / "scheduler.py"), "--config", str(root / f"etc/{self.name(plan)}.json"),
                "--state-dir", str(root / ("lib/usnm-priority-controller" + ("" if plan == "main" else f".{plan}"))),
                "--apply"]

    def test_main(self):
        path = self.flags("main", MAIN_FLAGS)
        self.assertEqual(self.command("main"), self.head("main") + [
            "--server-status-url", STATUS_URL, "--max-global-pending", "3000", "--other-app-budget", "2500",
            "--rest-journals", "--batch-submit", "--batch-spool-out", "--batch-collect", "--collect-parallel", "4",
            "--batch-export", "--onprem-handoff", "claim", "--onprem-handoff", "collect", "--onprem-handoff",
            "export", "--dead-letter-failed", "--skip-truncated-scans", "--claim-language", "srp",
            "--refill-cap", "175",
            "--check-state-dir", "/var/lib/usnm-priority-controller.queue-feed-10",
            "--check-state-dir", "/var/lib/usnm-priority-controller.queue-feed-11",
            "--check-state-dir", "/var/lib/usnm-priority-controller.queue-feed-12"])
        done = self.tick("main", "--print-timer")
        self.assertEqual(done.stdout, timer_drop_in(path, "main", "5min", "2min"))

    def test_native_with_its_old_one_line_extras(self):
        path = self.flags("native-feed-4", NATIVE_FLAGS)
        self.assertEqual(self.command("native-feed-4"), self.head("native-feed-4") + FIXED_NATIVE + [
            "--check-state-dir", "/var/lib/usnm-priority-controller",
            "--check-state-dir", "/var/lib/usnm-priority-controller.queue-feed-12",
            "--check-state-dir", "/var/lib/usnm-priority-controller.native-feed-3", "--refill-cap", "96"])
        done = self.tick("native-feed-4", "--print-timer")
        self.assertEqual(done.stdout, timer_drop_in(path, "native", "5min", "2min"))

    def test_drain(self):
        path = self.flags("queue-feed-10", DRAIN_FLAGS)
        self.assertEqual(self.command("queue-feed-10"), self.head("queue-feed-10") + [
            "--drain", "--server-status-url", STATUS_URL, "--rest-journals", "--batch-spool-out",
            "--batch-collect", "--onprem-handoff", "claim", "--onprem-handoff", "collect", "--onprem-handoff",
            "export", "--dead-letter-failed", "--check-state-dir", "/var/lib/usnm-priority-controller"])
        done = self.tick("queue-feed-10", "--print-timer")
        self.assertEqual(done.stdout, timer_drop_in(path, "drain", "17min", "30min"))

    def test_a_plan_sets_its_own_cadence(self):
        path = self.flags("native-feed-4", "@every 90s\n@boot 3min\n" + NATIVE_FLAGS)
        done = self.tick("native-feed-4", "--print-timer")
        self.assertEqual(done.stdout, timer_drop_in(path, "native", "3min", "90s"))
        # The settings aren't arguments.
        self.assertNotIn("@every", "".join(self.command("native-feed-4")))
        for bad in ("@every 2", "@every 0min", "@every 2 min", "@boot -1s", "@every 2min\n@every 3min", "@later 2min"):
            with self.subTest(bad=bad):
                self.flags("native-feed-4", bad + "\n" + NATIVE_FLAGS)
                done = self.tick("native-feed-4", "--print-timer")
                self.assertEqual(done.returncode, 1)

    def test_every_kind_needs_all_three_on_prem_hand_off_steps(self):
        for kind, text in (("main", MAIN_FLAGS), ("native", NATIVE_FLAGS), ("drain", DRAIN_FLAGS)):
            for step in ("claim", "collect", "export"):
                with self.subTest(kind=kind, step=step):
                    self.flags("native-feed-4", text.replace(f"--onprem-handoff {step}", ""))
                    for mode in ("--print-command", "--print-timer"):
                        done = self.tick("native-feed-4", mode)
                        self.assertEqual(done.returncode, 1)
                        self.assertIn(f"lacks --onprem-handoff {step}", self.failure(done))
        # --onprem-handoff=STEP is the same flag.
        self.flags("native-feed-4", NATIVE_FLAGS.replace("--onprem-handoff export", "--onprem-handoff=export"))
        self.assertIn("--onprem-handoff=export", self.command("native-feed-4"))
        # An --add-flag can't stand in for one.
        self.flags("native-feed-4", NATIVE_FLAGS.replace("--onprem-handoff export", ""))
        done = self.tick("native-feed-4", "--print-command", "--add-flag", "--onprem-handoff",
                         "--add-flag", "export")
        self.assertEqual(done.returncode, 1)

    def test_kind_and_drain_must_agree(self):
        self.flags("queue-feed-10", DRAIN_FLAGS.replace("--drain ", ""))
        self.assertIn("has no --drain", self.failure(self.tick("queue-feed-10", "--print-command")))
        self.flags("queue-feed-10", NATIVE_FLAGS + "--drain\n")
        self.assertIn("make it @kind drain", self.failure(self.tick("queue-feed-10", "--print-command")))
        for drain in ("--dr", "--dra", "--drai", "--drain"):
            with self.subTest(drain=drain):
                self.flags("queue-feed-10", NATIVE_FLAGS + drain + "\n")
                self.assertIn("make it @kind drain", self.failure(self.tick("queue-feed-10", "--print-command")))
                self.flags("queue-feed-10", DRAIN_FLAGS.replace("--drain ", drain + " "))
                self.assertEqual(self.tick("queue-feed-10", "--print-command").returncode, 0)
        self.flags("native-feed-4", NATIVE_FLAGS)
        for drain in ("--dr", "--dra", "--drai", "--drain"):
            with self.subTest(added=drain):
                done = self.tick("native-feed-4", "--print-command", "--add-flag", drain)
                self.assertIn("is @kind native", self.failure(done))
        self.flags("queue-feed-10", DRAIN_FLAGS)
        self.assertEqual(self.tick("queue-feed-10", "--print-command", "--add-flag", "--drain").returncode, 0)
        self.flags("main", MAIN_FLAGS.replace("@kind main\n", ""))
        self.assertIn("has no @kind", self.failure(self.tick("main", "--print-command")))
        self.flags("main", MAIN_FLAGS.replace("@kind main", "@kind live"))
        self.assertIn("@kind is main, native or drain", self.failure(self.tick("main", "--print-command")))

    def test_words_are_never_expanded_and_quotes_are_refused(self):
        self.flags("main", "--claim-language $LANG\n--check-state-dir /var/lib/*\n", kind="main")
        self.assertEqual(self.command("main")[-4:], ["--claim-language", "$LANG", "--check-state-dir", "/var/lib/*"])
        for line in ('--check-state-dir "/var/lib/a b"', "--check-state-dir '/x'", "--check-state-dir /a\\ b"):
            with self.subTest(line=line):
                self.flags("main", line + "\n", kind="main")
                self.assertIn("quotes and backslashes", self.failure(self.tick("main", "--print-command")))

    def test_the_wrappers_own_flags_and_resume_are_refused_in_the_file(self):
        # argparse also takes a unique prefix of an option as the option itself.
        for line in ("--apply", "--config /etc/other.json", "--config=/etc/other.json", "--state-dir /tmp",
                     "--appl", "--ap", "--conf", "--state-d", "--st=/tmp", "--refill-cap 9 --apply"):
            with self.subTest(line=line):
                self.flags("main", line + "\n", kind="main")
                self.assertIn("controller-tick.sh sets itself", self.failure(self.tick("main", "--print-command")))
        for line in ("--resume-native", "--resume", "--resume-native=1"):
            with self.subTest(line=line):
                self.flags("native-feed-4", line + "\n", kind="native")
                self.assertIn("--add-flag", self.failure(self.tick("native-feed-4", "--print-command")))
        # Flags that only start like them are the plan's own.
        self.flags("native-feed-4", "--check-state-dir /x --claim-language srp --rest-journals\n", kind="native")
        self.assertEqual(self.command("native-feed-4")[-5:],
                         ["--check-state-dir", "/x", "--claim-language", "srp", "--rest-journals"])

    def test_an_added_flag_goes_after_the_flags_file_for_one_run(self):
        self.flags("native-feed-4", NATIVE_FLAGS)
        done = self.tick("native-feed-4", "--print-command", "--add-flag", "--resume-native")
        self.assertEqual(done.stdout.splitlines()[-2:], ["96", "--resume-native"])
        self.assertNotIn("--resume-native", self.command("native-feed-4"))
        for flag in ("--apply", "--appl", "--state-d=/tmp"):
            with self.subTest(flag=flag):
                done = self.tick("native-feed-4", "--dry-run", "--add-flag", flag)
                self.assertEqual(done.returncode, 1)
                self.assertIn("sets it itself", self.failure(done))
        self.assertEqual(self.runs(), [])

    def test_a_missing_flags_file_config_or_bad_plan_name_fails(self):
        self.assertIn("has no flags file", self.failure(self.tick("main")))
        self.flags("queue-feed-12", "", kind="drain")
        self.assertIn("has no configuration", self.failure(self.tick("queue-feed-12")))
        for plan in ("", "Main", "../main", "a b", "-x", "x" * 65):
            with self.subTest(plan=plan):
                self.assertIn("--plan is a plan name", self.failure(self.tick(plan)))
        self.assertEqual(self.runs(), [])


@unittest.skipUnless(shutil.which("flock"), "needs flock (Linux)")
class Tick(Fixture):
    def test_a_tick_runs_under_the_tick_lock_and_keeps_its_exit_status(self):
        self.flags("main", MAIN_FLAGS)
        for status in (0, 1):
            with self.subTest(status=status):
                done = self.tick("main", FAKE_EXIT=str(status))
                self.assertEqual(done.returncode, status, done.stderr)
        self.assertEqual([run["lock_held"] for run in self.runs()], [True, True])
        self.assertEqual(self.runs()[0]["argv"][4:], self.command("main")[6:])
        # Released when the tick ends.
        self.assertEqual(subprocess.run(["flock", "-n", str(self.lock), "true"]).returncode, 0)

    def hold(self, path, seconds, then=""):
        ready = self.root / "held"
        ready.unlink(missing_ok=True)
        holder = subprocess.Popen(["flock", str(path), "sh", "-c", f'touch "{ready}"; sleep {seconds}; {then}'])
        for _ in range(100):
            if ready.exists():
                break
            time.sleep(0.05)
        self.assertTrue(ready.exists(), "the holder never took the lock")
        self.addCleanup(holder.wait)
        self.addCleanup(holder.kill)
        return holder

    def test_a_busy_lock_is_waited_for(self):
        self.flags("native-feed-4", NATIVE_FLAGS)
        self.hold(self.lock, 1)
        done = self.tick("native-feed-4")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual([run["lock_held"] for run in self.runs()], [True])

    def test_a_lock_still_busy_fails_the_tick_as_lock_busy(self):
        self.flags("native-feed-4", NATIVE_FLAGS)
        self.hold(self.lock, 30)
        error = self.failure(self.tick("native-feed-4"))
        self.assertEqual(feed_collect.failure_reason(error), "lock_busy")
        self.assertEqual(self.runs(), [])

    def test_a_dry_run_takes_no_lock_and_never_applies(self):
        self.flags("main", MAIN_FLAGS)
        self.hold(self.lock, 30)
        done = self.tick("main", "--dry-run")
        self.assertEqual(done.returncode, 0, done.stderr)
        (run,) = self.runs()
        self.assertNotIn("--apply", run["argv"])

    def skipped(self, done, reason):
        self.assertEqual(done.returncode, 0, done.stderr)
        line = json.loads(done.stdout)
        self.assertEqual((line["event"], line["reason"]), ("boinc_batch_skipped", reason))

    def test_other_plans_skip_while_any_held_archive_runs_and_main_does_not(self):
        self.flags("main", MAIN_FLAGS)
        self.flags("native-feed-4", NATIVE_FLAGS)
        self.flags("queue-feed-10", DRAIN_FLAGS)
        for plan in ("native-feed-4", "queue-feed-10"):
            for state in ("activating", "active", "deactivating"):
                with self.subTest(plan=plan, state=state):
                    self.skipped(self.tick(plan, FAKE_ARCHIVE_STATE=state), "held_archive_running")
            with self.subTest(plan=plan, hand_run=True):
                # A hand-run archive, e.g. usnm-held-archive-queue-feed-11.service.
                self.skipped(self.tick(plan, FAKE_ARCHIVE_UNITS="usnm-held-archive-queue-feed-11.service"),
                             "held_archive_running")
        self.assertIn("systemctl list-units --plain --no-legend --state=active,activating,deactivating "
                      "usnm-held-archive-*", (self.root / "systemctl.log").read_text())
        # Any run of held-archive.sh, under whatever unit, holds its lock.
        holder = self.hold(self.root / "archive.lock", 2)
        self.skipped(self.tick("native-feed-4"), "held_archive_running")
        holder.wait(timeout=30)  # the lock goes with the holder's whole process tree
        self.assertEqual(self.runs(), [])
        self.assertEqual(self.tick("native-feed-4", FAKE_ARCHIVE_STATE="failed").returncode, 0)
        # main waits on the lock instead: the archive starts main's tick itself as it ends.
        done = self.tick("main", FAKE_ARCHIVE_STATE="activating",
                         FAKE_ARCHIVE_UNITS="usnm-held-archive-queue-feed-11.service")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(len(self.runs()), 2)

    def test_a_drain_plan_skips_while_mains_timer_is_stopped(self):
        # As the shared drain service's ExecCondition: an operator's pause of main pauses the drains.
        self.flags("queue-feed-10", DRAIN_FLAGS)
        self.flags("native-feed-4", NATIVE_FLAGS)
        self.skipped(self.tick("queue-feed-10", FAKE_MAIN_TIMER="inactive"), "controller_paused")
        self.assertEqual(self.runs(), [])
        self.assertEqual(self.tick("native-feed-4", FAKE_MAIN_TIMER="inactive").returncode, 0)
        self.assertEqual(self.tick("queue-feed-10").returncode, 0)
        self.assertEqual(len(self.runs()), 2)
        self.assertIn("systemctl is-active usnm-boinc-priority-controller@main.timer",
                      (self.root / "systemctl.log").read_text())

    def test_a_tick_waiting_for_the_lock_skips_once_the_archive_starts(self):
        self.flags("native-feed-4", NATIVE_FLAGS)
        self.hold(self.lock, 30)
        state = self.root / "archive-state"
        environment = {**os.environ, "PATH": f"{self.root / 'bin'}{os.pathsep}{os.environ['PATH']}",
                       "FAKE_LOG": str(self.root / "log"), "FAKE_SYSTEMCTL_LOG": str(self.root / "systemctl.log"),
                       "FAKE_ARCHIVE_FILE": str(state)}
        started = time.monotonic()
        waiting = subprocess.Popen(
            ["bash", str(TICK), "--plan", "native-feed-4", "--config-dir", str(self.root / "etc"),
             "--state-parent", str(self.root / "lib"), "--scheduler", str(self.root / "scheduler.py"),
             "--tick-lock", str(self.lock), "--held-archive-lock", str(self.root / "archive.lock"),
             "--lock-wait-secs", "20", "--lock-recheck-secs", "1"],
            env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        time.sleep(1.5)
        state.write_text("activating\n")
        out, err = waiting.communicate(timeout=30)
        self.assertEqual(waiting.returncode, 0, err)
        self.assertLess(time.monotonic() - started, 10)
        self.assertEqual(json.loads(out)["reason"], "held_archive_running")
        self.assertEqual(self.runs(), [])

    def test_a_tick_that_gets_the_lock_while_the_archive_runs_skips(self):
        # The archive may start while this tick waits behind another and take the lock only after
        # main's tick: the tick that got the lock first lets it go.
        self.flags("native-feed-4", NATIVE_FLAGS)
        state = self.root / "archive-state"
        self.hold(self.lock, 1, f'echo activating > "{state}"')
        done = self.tick("native-feed-4", "--lock-wait-secs", "20", "--lock-recheck-secs", "10",
                         FAKE_ARCHIVE_FILE=str(state))
        self.skipped(done, "held_archive_running")
        self.assertEqual(self.runs(), [])


class Install(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        (self.root / "bin").mkdir()
        (self.root / "target").mkdir()
        # systemctl must not be run for a root other than /.
        (self.root / "bin/systemctl").write_text(f'#!/bin/sh\necho "$*" >> "{self.root}/systemctl.log"\n')
        (self.root / "bin/systemctl").chmod(0o755)
        self.units = self.root / "target/etc/systemd/system"

    def tearDown(self):
        self.tmp.cleanup()

    def install(self, *extra):
        env = {**os.environ, "PATH": f"{self.root / 'bin'}{os.pathsep}{os.environ['PATH']}"}
        return subprocess.run(["bash", str(INSTALL), "--root", str(self.root / "target"), *extra],
                              env=env, capture_output=True, text=True, timeout=60)

    def checkout(self):
        tick = self.root / "target/opt/usnm/boinc/scripts/onprem/controller/controller-tick.sh"
        tick.parent.mkdir(parents=True)
        tick.write_text("#!/bin/sh\n")

    def plans(self):
        etc = self.root / "target/etc/usnm"
        etc.mkdir(parents=True)
        (etc / "boinc-priority-controller.flags").write_text(MAIN_FLAGS)
        (etc / "boinc-priority-controller.native-feed-4.flags").write_text(NATIVE_FLAGS)
        (etc / "boinc-priority-controller.queue-feed-10.flags").write_text(DRAIN_FLAGS)
        # The old native tick script's extras file is no plan's flags file.
        (etc / "native-feed-native-feed-4.flags").write_text("--refill-cap 96\n")
        return etc

    def test_installs_the_template_and_the_archive_units_and_nothing_else(self):
        self.checkout()
        for _ in range(2):  # safe to run again
            done = self.install()
            self.assertEqual(done.returncode, 0, done.stderr)
        expected = {SERVICE: SERVICE.name, TIMER: TIMER.name,
                    ONPREM / "usnm-boinc-held-archive.service": "usnm-boinc-held-archive.service",
                    ONPREM / "usnm-boinc-held-archive.timer": "usnm-boinc-held-archive.timer"}
        self.assertEqual(sorted(p.name for p in self.units.iterdir()), sorted(expected.values()))
        for source, name in expected.items():
            self.assertEqual((self.units / name).read_bytes(), source.read_bytes())
            self.assertEqual(stat.S_IMODE((self.units / name).stat().st_mode), 0o644)
        self.assertFalse((self.root / "target/etc/usnm").exists())
        self.assertFalse((self.root / "systemctl.log").exists())

    def test_each_plans_cadence_becomes_its_timer_drop_in(self):
        self.checkout()
        etc = self.plans()
        before = {p.name: p.read_bytes() for p in etc.iterdir()}
        done = self.install()
        self.assertEqual(done.returncode, 0, done.stderr)
        for plan, flags, kind, boot, every in (
                ("main", "boinc-priority-controller.flags", "main", "5min", "2min"),
                ("native-feed-4", "boinc-priority-controller.native-feed-4.flags", "native", "5min", "2min"),
                ("queue-feed-10", "boinc-priority-controller.queue-feed-10.flags", "drain", "17min", "30min")):
            with self.subTest(plan=plan):
                drop_in = self.units / f"usnm-boinc-priority-controller@{plan}.timer.d/cadence.conf"
                self.assertEqual(drop_in.read_text(), timer_drop_in(etc / flags, kind, boot, every))
                timer = parse_unit(drop_in)["Timer"]
                # One empty assignment clears all the template's triggers, before both are set: an
                # empty assignment after OnBootSec would clear it again.
                self.assertEqual(timer, {"OnBootSec": ["", boot], "OnUnitInactiveSec": [every]})
                lines = drop_in.read_text().splitlines()
                self.assertEqual(lines.index("OnBootSec="), 2)
                self.assertEqual([line for line in lines if line.endswith("=")], ["OnBootSec="])
        self.assertEqual(sorted(p.name for p in self.units.glob("*.timer.d")),
                         ["usnm-boinc-priority-controller@main.timer.d",
                          "usnm-boinc-priority-controller@native-feed-4.timer.d",
                          "usnm-boinc-priority-controller@queue-feed-10.timer.d"])
        self.assertEqual({p.name: p.read_bytes() for p in etc.iterdir()}, before)

    def test_a_refused_flags_file_installs_nothing(self):
        self.checkout()
        etc = self.plans()
        (etc / "boinc-priority-controller.native-feed-4.flags").write_text(
            NATIVE_FLAGS.replace("--onprem-handoff collect", ""))
        done = self.install()
        self.assertEqual(done.returncode, 1)
        self.assertIn("native-feed-4", done.stderr)
        self.assertFalse(self.units.exists())

    def test_a_flags_file_name_read_as_another_plans_installs_nothing(self):
        self.checkout()
        etc = self.plans()
        for name in ("boinc-priority-controller.main.flags", "boinc-priority-controller..flags"):
            with self.subTest(name=name):
                (etc / name).write_text("@kind main\n")
                done = self.install()
                self.assertEqual(done.returncode, 1)
                self.assertIn(name, done.stderr)
                self.assertFalse(self.units.exists())
                (etc / name).unlink()

    def test_refuses_a_checkout_without_the_tick_script(self):
        done = self.install()
        self.assertEqual(done.returncode, 1)
        self.assertIn("move the checkout", done.stderr)
        self.assertFalse((self.root / "target/etc").exists())

    def test_refuses_unknown_arguments(self):
        self.checkout()
        done = self.install("--enable")
        self.assertEqual(done.returncode, 1)
        self.assertIn("unknown argument", done.stderr)


if __name__ == "__main__":
    unittest.main()
