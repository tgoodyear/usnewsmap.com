"""bin/start's pid directory is on the container's disk, linked into the project tree."""

import getpass
import grp
import os
import socket
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("boinc-daemons.sh")
HOST = socket.gethostname().split(".")[0]


def local_pid_dir(proj: Path, root: Path) -> subprocess.CompletedProcess:
    env = dict(os.environ, USNM_PID_ROOT=str(root), USNM_BOINC_USER=getpass.getuser(),
               USNM_BOINC_GROUP=grp.getgrgid(os.getgid()).gr_name)
    return subprocess.run(
        ["bash", "-c", 'set -euo pipefail; source "$1"; proj="$2"; local_pid_dir', "-", SCRIPT, proj],
        env=env,
        capture_output=True,
        text=True,
        check=True,
    )


class LocalPidDir(unittest.TestCase):
    def setUp(self):
        t = tempfile.TemporaryDirectory()
        self.addCleanup(t.cleanup)
        self.proj = Path(t.name, "share", "usnmocr")
        self.root = Path(t.name, "local")
        self.proj.mkdir(parents=True)

    def test_links_the_hosts_pid_directory_to_local_disk(self):
        local_pid_dir(self.proj, self.root)
        link = self.proj / f"pid_{HOST}"
        self.assertTrue(link.is_symlink())
        self.assertEqual(Path(os.readlink(link)), self.root / f"pid_{HOST}")
        self.assertTrue(link.is_dir())
        self.assertEqual((self.root / f"pid_{HOST}").stat().st_mode & 0o777, 0o775)

    def test_start_lock_reopens_after_unlink_with_a_handle_open(self):
        # What bin/start does: open the lock, leave it open in a daemon, unlink it, open it again.
        local_pid_dir(self.proj, self.root)
        lock = self.proj / f"pid_{HOST}" / f"start.lock.{HOST}"
        held = open(lock, "w")
        self.addCleanup(held.close)
        os.unlink(lock)
        open(lock, "w").close()

    def test_is_idempotent_and_repairs_a_wrong_link(self):
        link = self.proj / f"pid_{HOST}"
        link.symlink_to(self.root / "elsewhere")
        local_pid_dir(self.proj, self.root)
        local_pid_dir(self.proj, self.root)
        self.assertEqual(Path(os.readlink(link)), self.root / f"pid_{HOST}")

    def test_restart_hold_is_on_the_local_disk(self):
        env = dict(os.environ, USNM_PID_ROOT=str(self.root), USNM_BOINC_USER=getpass.getuser(),
                   USNM_BOINC_GROUP=grp.getgrgid(os.getgid()).gr_name)
        r = subprocess.run(
            ["bash", "-c", 'set -euo pipefail; source "$1"; proj="$2"; local_pid_dir; echo "$hold"',
             "-", SCRIPT, self.proj],
            env=env, capture_output=True, text=True, check=True)
        self.assertEqual(r.stdout.strip(), str(self.root / f"pid_{HOST}" / "usnm-feeder-restart-hold"))

    def test_keeps_an_existing_directory(self):
        existing = self.proj / f"pid_{HOST}"
        existing.mkdir()
        (existing / "feeder.pid").write_text("123\n")
        r = local_pid_dir(self.proj, self.root)
        self.assertFalse(existing.is_symlink())
        self.assertEqual((existing / "feeder.pid").read_text(), "123\n")
        self.assertIn("not a link", r.stderr)


class Maintenance(unittest.TestCase):
    """stop_maintenance holds the project stopped across container starts (the share cutover)."""

    def check(self, proj: Path) -> subprocess.CompletedProcess:
        return subprocess.run(
            ["bash", "-c", 'set -euo pipefail; source "$1"; proj="$2"; maintenance', "-", SCRIPT, proj],
            capture_output=True, text=True, check=False)

    def test_marker_holds_the_start(self):
        with tempfile.TemporaryDirectory() as t:
            proj = Path(t)
            self.assertEqual(self.check(proj).returncode, 1)
            (proj / "stop_maintenance").touch()
            r = self.check(proj)
            self.assertEqual(r.returncode, 0)
            self.assertIn("not starting the project", r.stderr)

    def test_both_starts_check_the_marker_and_the_restart_hold(self):
        main = SCRIPT.read_text().split("main() {")[1].split("\n}")[0]
        self.assertIn("while maintenance || on_hold; do sleep 60; done\n  run 'bin/start'", main)
        self.assertIn("maintenance || held || run 'bin/start --cron'", main)


class RestartBound(unittest.TestCase):
    """The loop restarts a stopped feeder a bounded number of times, then holds (#1462)."""

    def setUp(self):
        t = tempfile.TemporaryDirectory()
        self.addCleanup(t.cleanup)
        self.proj = Path(t.name, "project")
        self.pids = self.proj / "pid_h"
        self.proc = Path(t.name, "proc")
        self.hold = Path(t.name, "local", "usnm-feeder-restart-hold")
        for d in (self.pids, self.proc, self.hold.parent):
            d.mkdir(parents=True)

    def feeder(self, comm="feeder", state="S"):
        (self.proc / "77").mkdir(exist_ok=True)
        (self.proc / "77" / "comm").write_text(comm + "\n")
        (self.proc / "77" / "stat").write_text(f"77 ({comm}) {state} 1 77 77 0 -1\n")
        (self.pids / "feeder.pid").write_text("77\n")

    def cycles(self, times, after="", **settings):
        """One loop per time: "start" runs bin/start --cron, "hold" doesn't."""
        steps = " ".join(str(t) for t in times)
        script = ('epoch() { echo "$T"; }; proj="$2"; host=h; proc="$3"; hold="$4"; restart_bound; '
                  f'for T in {steps}; do if held; then echo "$T hold"; else echo "$T start"; fi; done; '
                  + after)
        env = {k: v for k, v in os.environ.items() if not k.startswith("USNM_FEEDER_")} | settings
        r = subprocess.run(
            ["bash", "-c", 'set -euo pipefail; source "$1"; ' + script, "-", SCRIPT,
             self.proj, self.proc, self.hold],
            env=env, capture_output=True, text=True, check=True)
        return [line.split()[1] for line in r.stdout.splitlines()], r.stderr

    def test_holds_after_the_limit_until_the_marker_is_removed(self):
        runs, log = self.cycles([0, 300, 600, 900, 1200],
                                after='rm "$hold"; T=1500; if held; then echo "$T hold"; else echo "$T start"; fi')
        self.assertEqual(runs, ["start", "start", "start", "hold", "hold", "start"])
        self.assertIn("restart 3 of 3 in 3600s", log)
        self.assertIn("holding the project until", log)
        self.assertIn("usnm-feeder-restart-hold is present", log)
        self.assertIn("restart 1 of 3 in 3600s", log.split("is present")[-1])

    def test_window_forgets_old_restarts_but_catches_a_slow_loop(self):
        runs, _ = self.cycles([0, 1800, 3500, 3700, 4000])
        self.assertEqual(runs, ["start", "start", "start", "start", "hold"])
        self.assertEqual(self.hold.read_text(), "held_at=4000 restarts=3 window_s=3600\n")

    def test_never_writes_through_a_planted_link_and_holds_without_a_marker(self):
        target = self.hold.parent.parent / "outside"
        self.hold.symlink_to(target)
        runs, log = self.cycles([0, 300, 600, 900],
                                after='rm "$hold"; T=1200; if held; then echo "$T hold"; else echo "$T start"; fi')
        self.assertEqual(runs, ["start", "start", "start", "hold", "hold"])
        self.assertFalse(target.exists())
        self.assertFalse(self.hold.is_symlink())
        self.assertIn("could not write", log)
        self.assertIn("holding the project until the container restarts", log.split("could not write")[-1])

    def test_running_feeder_or_stopped_project_does_not_count(self):
        self.feeder()
        runs, log = self.cycles(range(0, 3600, 300))
        self.assertEqual(set(runs), {"start"})
        self.assertNotIn("restart", log)
        (self.pids / "feeder.pid").unlink()
        (self.proj / "stop_daemons").touch()
        runs, log = self.cycles(range(0, 3600, 300))
        self.assertEqual(set(runs), {"start"})
        self.assertFalse(self.hold.exists())

    def test_zombie_reused_or_garbled_pid_is_a_stopped_feeder(self):
        for name, prepare in (
            ("zombie", lambda: self.feeder(state="Z")),
            ("reused", lambda: self.feeder(comm="apache2")),
            ("garbled", lambda: (self.pids / "feeder.pid").write_text("77; rm -rf /\n")),
        ):
            with self.subTest(name):
                prepare()
                self.hold.unlink(missing_ok=True)
                runs, _ = self.cycles([0, 300, 600, 900])
                self.assertEqual(runs, ["start", "start", "start", "hold"])

    def test_settings_zero_never_restarts_and_bad_values_keep_the_default(self):
        runs, _ = self.cycles([0], USNM_FEEDER_RESTART_LIMIT="0")
        self.assertEqual(runs, ["hold"])
        self.hold.unlink()
        runs, _ = self.cycles([0, 30, 60, 130, 140], USNM_FEEDER_RESTART_LIMIT="2", USNM_FEEDER_RESTART_WINDOW="100")
        self.assertEqual(runs, ["start", "start", "hold", "hold", "hold"])
        self.hold.unlink()
        runs, _ = self.cycles([0, 60, 120, 180], USNM_FEEDER_RESTART_LIMIT="2", USNM_FEEDER_RESTART_WINDOW="100")
        self.assertEqual(runs, ["start", "start", "start", "start"])
        for limit, window in (("-1", "0"), ("3x", "1e3"), ("12345", "")):
            with self.subTest(limit=limit, window=window):
                self.hold.unlink(missing_ok=True)
                runs, log = self.cycles([0, 300, 600, 900], USNM_FEEDER_RESTART_LIMIT=limit,
                                        USNM_FEEDER_RESTART_WINDOW=window)
                self.assertEqual(runs, ["start", "start", "start", "hold"])
                self.assertIn(f"ignoring USNM_FEEDER_RESTART_LIMIT={limit}; using 3", log)
                if window:
                    self.assertIn(f"ignoring USNM_FEEDER_RESTART_WINDOW={window}; using 3600", log)


if __name__ == "__main__":
    unittest.main()
