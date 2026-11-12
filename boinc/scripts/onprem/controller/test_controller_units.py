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

# systemctl show UNIT -p ActiveState --value: $FAKE_ARCHIVE_STATE, or inactive.
FAKE_SYSTEMCTL = r'''#!/bin/sh
echo "systemctl $*" >> "$FAKE_SYSTEMCTL_LOG"
if [ -n "$FAKE_ARCHIVE_FILE" ] && [ -f "$FAKE_ARCHIVE_FILE" ]; then cat "$FAKE_ARCHIVE_FILE"; else echo "${FAKE_ARCHIVE_STATE:-inactive}"; fi
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


@unittest.skipUnless(shutil.which("flock"), "needs flock (Linux)")
class Tick(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = self.root = Path(self.tmp.name)
        (root / "etc").mkdir()
        (root / "lib").mkdir()
        (root / "bin").mkdir()
        (root / "scheduler.py").write_text(FAKE_SCHEDULER)
        (root / "bin/systemctl").write_text(FAKE_SYSTEMCTL)
        (root / "bin/systemctl").chmod(0o755)
        self.lock = root / "tick.lock"
        for suffix in ("", ".native-feed-3"):
            (root / f"etc/boinc-priority-controller{suffix}.json").write_text("{}")

    def tearDown(self):
        self.tmp.cleanup()

    def flags(self, plan, text):
        suffix = "" if plan == "main" else f".{plan}"
        (self.root / f"etc/boinc-priority-controller{suffix}.flags").write_text(text)

    def tick(self, plan, *extra, **env):
        root = self.root
        environment = {**os.environ, "PATH": f"{root / 'bin'}{os.pathsep}{os.environ['PATH']}",
                       "FAKE_LOG": str(root / "log"), "FAKE_SYSTEMCTL_LOG": str(root / "systemctl.log"),
                       "FAKE_TICK_LOCK": str(self.lock), **env}
        return subprocess.run(
            ["bash", str(TICK), "--plan", plan, "--config-dir", str(root / "etc"),
             "--state-parent", str(root / "lib"), "--scheduler", str(root / "scheduler.py"),
             "--tick-lock", str(self.lock), "--lock-wait-secs", "2", *extra],
            env=environment, capture_output=True, text=True, timeout=60)

    def runs(self):
        path = self.root / "log"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def failure(self, done):
        lines = [json.loads(line) for line in done.stderr.splitlines() if line.startswith("{")]
        self.assertEqual(len(lines), 1, done.stderr)
        self.assertEqual(lines[0]["event"], "boinc_batch_failure")
        return lines[0]["error"]

    def test_main_uses_the_shared_services_paths(self):
        self.flags("main", "--max-global-pending\n3000\n")
        done = self.tick("main", "--print-command")
        self.assertEqual(done.returncode, 0, done.stderr)
        root = self.root
        self.assertEqual(done.stdout.splitlines(), [
            "python3", str(root / "scheduler.py"), "--config", str(root / "etc/boinc-priority-controller.json"),
            "--state-dir", str(root / "lib/usnm-priority-controller"), "--apply", "--max-global-pending", "3000"])
        self.assertEqual(self.runs(), [])

    def test_another_plan_uses_its_suffixed_paths(self):
        self.flags("native-feed-3", "--resume-native\n")
        done = self.tick("native-feed-3", "--print-command")
        self.assertEqual(done.returncode, 0, done.stderr)
        root = self.root
        self.assertEqual(done.stdout.splitlines()[2:], [
            "--config", str(root / "etc/boinc-priority-controller.native-feed-3.json"),
            "--state-dir", str(root / "lib/usnm-priority-controller.native-feed-3"), "--apply", "--resume-native"])

    def test_the_flags_file_is_one_argument_per_line_and_never_expanded(self):
        self.flags("main", "# the live plan\n\n  --onprem-handoff  \nclaim\n--check-state-dir\n"
                           "/var/lib/a dir with spaces\n$HOME\n--server-status-url\n"
                           "https://boinc.example/server_status.php?xml=1")
        done = self.tick("main")
        self.assertEqual(done.returncode, 0, done.stderr)
        (run,) = self.runs()
        self.assertEqual(run["argv"][4:], ["--apply", "--onprem-handoff", "claim", "--check-state-dir",
                                           "/var/lib/a dir with spaces", "$HOME", "--server-status-url",
                                           "https://boinc.example/server_status.php?xml=1"])

    def test_an_added_flag_goes_after_the_flags_file_for_one_run(self):
        self.flags("native-feed-3", "--batch-collect\n")
        done = self.tick("native-feed-3", "--add-flag", "--resume-native")
        self.assertEqual(done.returncode, 0, done.stderr)
        done = self.tick("native-feed-3")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual([run["argv"][4:] for run in self.runs()],
                         [["--apply", "--batch-collect", "--resume-native"], ["--apply", "--batch-collect"]])
        for flag in ("--apply", "--appl", "--state-d=/tmp"):
            with self.subTest(flag=flag):
                done = self.tick("native-feed-3", "--dry-run", "--add-flag", flag)
                self.assertEqual(done.returncode, 1)
                self.assertIn("sets it itself", self.failure(done))
        self.assertEqual(len(self.runs()), 2)
        # Flags that only start like them are the plan's own.
        self.flags("native-feed-3", "--check-state-dir\n/var/lib/x\n--claim-language\nsrp\n")
        done = self.tick("native-feed-3", "--print-command")
        self.assertEqual(done.returncode, 0, done.stderr)

    def test_the_wrappers_own_flags_are_refused_in_the_flags_file(self):
        # argparse also takes a unique prefix of an option as the option itself.
        for line in ("--apply", "--config", "--config=/etc/other.json", "--state-dir", "--state-dir=/tmp",
                     "--appl", "--ap", "--conf", "--conf=/etc/other.json", "--state-d", "--st=/tmp"):
            with self.subTest(line=line):
                self.flags("main", f"--drain\n{line}\n")
                done = self.tick("main")
                self.assertEqual(done.returncode, 1)
                self.assertIn("controller-tick.sh sets itself", self.failure(done))
        self.assertEqual(self.runs(), [])

    def test_a_missing_flags_file_config_or_bad_plan_name_fails(self):
        done = self.tick("main")
        self.assertEqual(done.returncode, 1)
        self.assertIn("has no flags file", self.failure(done))
        self.flags("queue-feed-12", "--drain\n")
        done = self.tick("queue-feed-12")
        self.assertEqual(done.returncode, 1)
        self.assertIn("has no configuration", self.failure(done))
        for plan in ("", "Main", "../main", "a b", "-x", "x" * 65):
            with self.subTest(plan=plan):
                done = self.tick(plan)
                self.assertEqual(done.returncode, 1)
                self.assertIn("--plan is a plan name", self.failure(done))
        self.assertEqual(self.runs(), [])

    def test_a_tick_runs_under_the_tick_lock_and_keeps_its_exit_status(self):
        self.flags("main", "")
        for status in (0, 1):
            with self.subTest(status=status):
                done = self.tick("main", FAKE_EXIT=str(status))
                self.assertEqual(done.returncode, status, done.stderr)
        self.assertEqual([run["lock_held"] for run in self.runs()], [True, True])
        # Released when the tick ends.
        self.assertEqual(subprocess.run(["flock", "-n", str(self.lock), "true"]).returncode, 0)

    def hold_lock(self, seconds):
        ready = self.root / "held"
        holder = subprocess.Popen(["flock", str(self.lock), "sh", "-c", f'touch "{ready}"; sleep {seconds}'])
        for _ in range(100):
            if ready.exists():
                break
            time.sleep(0.05)
        self.assertTrue(ready.exists(), "the holder never took the lock")
        self.addCleanup(holder.wait)
        self.addCleanup(holder.kill)
        return holder

    def test_a_busy_lock_is_waited_for(self):
        self.flags("native-feed-3", "")
        self.hold_lock(1)
        done = self.tick("native-feed-3")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual([run["lock_held"] for run in self.runs()], [True])

    def test_a_lock_still_busy_fails_the_tick_as_lock_busy(self):
        self.flags("native-feed-3", "")
        self.hold_lock(30)
        done = self.tick("native-feed-3")
        self.assertEqual(done.returncode, 1)
        error = self.failure(done)
        self.assertEqual(feed_collect.failure_reason(error), "lock_busy")
        self.assertEqual(self.runs(), [])

    def test_a_tick_waiting_for_the_lock_skips_once_the_archive_starts(self):
        self.flags("native-feed-3", "")
        self.hold_lock(30)
        state = self.root / "archive-state"
        environment = {**os.environ, "PATH": f"{self.root / 'bin'}{os.pathsep}{os.environ['PATH']}",
                       "FAKE_LOG": str(self.root / "log"), "FAKE_SYSTEMCTL_LOG": str(self.root / "systemctl.log"),
                       "FAKE_ARCHIVE_FILE": str(state)}
        started = time.monotonic()
        waiting = subprocess.Popen(
            ["bash", str(TICK), "--plan", "native-feed-3", "--config-dir", str(self.root / "etc"),
             "--state-parent", str(self.root / "lib"), "--scheduler", str(self.root / "scheduler.py"),
             "--tick-lock", str(self.lock), "--lock-wait-secs", "20", "--lock-recheck-secs", "1"],
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
        self.flags("native-feed-3", "")
        state, ready = self.root / "archive-state", self.root / "held"
        # The holder marks the archive as started just before it lets the lock go.
        holder = subprocess.Popen(["flock", str(self.lock), "sh", "-c",
                                   f'touch "{ready}"; sleep 1; echo activating > "{state}"'])
        self.addCleanup(holder.wait)
        for _ in range(100):
            if ready.exists():
                break
            time.sleep(0.05)
        done = self.tick("native-feed-3", "--lock-wait-secs", "20", "--lock-recheck-secs", "10",
                         FAKE_ARCHIVE_FILE=str(state))
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(json.loads(done.stdout)["reason"], "held_archive_running")
        self.assertEqual(self.runs(), [])

    def test_a_dry_run_takes_no_lock_and_never_applies(self):
        self.flags("main", "--refill-cap\n150\n")
        self.hold_lock(30)
        done = self.tick("main", "--dry-run")
        self.assertEqual(done.returncode, 0, done.stderr)
        (run,) = self.runs()
        self.assertNotIn("--apply", run["argv"])
        self.assertEqual(run["argv"][-2:], ["--refill-cap", "150"])

    def test_other_plans_skip_while_the_held_archive_runs_and_main_does_not(self):
        self.flags("main", "")
        self.flags("native-feed-3", "")
        for state in ("activating", "active", "deactivating"):
            with self.subTest(state=state):
                done = self.tick("native-feed-3", FAKE_ARCHIVE_STATE=state)
                self.assertEqual(done.returncode, 0, done.stderr)
                line = json.loads(done.stdout)
                self.assertEqual((line["event"], line["plan"], line["reason"]),
                                 ("boinc_batch_skipped", "native-feed-3", "held_archive_running"))
        self.assertEqual(self.runs(), [])
        self.assertIn("systemctl show usnm-boinc-held-archive.service -p ActiveState --value",
                      (self.root / "systemctl.log").read_text())
        done = self.tick("native-feed-3", FAKE_ARCHIVE_STATE="failed")
        self.assertEqual(done.returncode, 0, done.stderr)
        # main waits on the lock instead: the archive starts main's tick itself as it ends.
        done = self.tick("main", FAKE_ARCHIVE_STATE="activating")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(len(self.runs()), 2)


class Install(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        (self.root / "bin").mkdir()
        (self.root / "target").mkdir()
        # systemctl must not be run for a root other than /.
        (self.root / "bin/systemctl").write_text(f'#!/bin/sh\necho "$*" >> "{self.root}/systemctl.log"\n')
        (self.root / "bin/systemctl").chmod(0o755)

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

    def test_installs_the_template_and_the_archive_units_and_nothing_else(self):
        self.checkout()
        for _ in range(2):  # safe to run again
            done = self.install()
            self.assertEqual(done.returncode, 0, done.stderr)
        units = self.root / "target/etc/systemd/system"
        expected = {SERVICE: SERVICE.name, TIMER: TIMER.name,
                    ONPREM / "usnm-boinc-held-archive.service": "usnm-boinc-held-archive.service",
                    ONPREM / "usnm-boinc-held-archive.timer": "usnm-boinc-held-archive.timer"}
        self.assertEqual(sorted(p.name for p in units.iterdir()), sorted(expected.values()))
        for source, name in expected.items():
            self.assertEqual((units / name).read_bytes(), source.read_bytes())
            self.assertEqual(stat.S_IMODE((units / name).stat().st_mode), 0o644)
        self.assertFalse((self.root / "target/etc/usnm").exists())
        self.assertFalse((self.root / "systemctl.log").exists())

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
