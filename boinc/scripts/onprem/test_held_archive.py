"""The held-page archive's run (#835): python3 -m unittest -v boinc/scripts/onprem/test_held_archive.py

held-archive.sh runs against fakes: systemctl (the controller's timer and service), the step
command (the BOINC host's bridge step, over ssh in production) and archive_history.py. Linux
only: it needs flock and sha256sum.
"""

import json
import os
import shutil
import signal
import subprocess
import tempfile
import time
import unittest
import unittest.mock
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "held-archive.sh"

# The fake archive tool: each --apply dispatch plans the next batch from $FAKE_BATCHES (a count),
# writes its manifest and receipt, and prints the receipt; with none left it prints batch null.
FAKE_TOOL = r'''
import hashlib, json, os, sys
from pathlib import Path
args = sys.argv[1:]
log = Path(os.environ["FAKE_LOG"])
log.open("a").write("tool " + " ".join(args) + "\n")
if os.environ.get("FAKE_TICK_LOCK"):
    import subprocess
    free = subprocess.run(["flock", "-n", os.environ["FAKE_TICK_LOCK"], "true"]).returncode == 0
    log.open("a").write("tick lock " + ("free" if free else "held") + "\n")
archive = Path(args[args.index("--archive-dir") + 1])
left = Path(os.environ["FAKE_LEFT"])
n = int(left.read_text())
if os.environ.get("FAKE_DISPATCH_FAILS") and "--apply" in args:
    sys.exit(1)
if "--apply" not in args:
    print(json.dumps({"event": "boinc_archive_dispatch", "apply": False, "journals": 15, "journal_cap": 10000,
                      "pages": {"eligible": 1000 * n}}))
    sys.exit(0)
if n == 0:
    print(json.dumps({"event": "boinc_archive_dispatch", "apply": True, "batch": None, "journals": 15}))
    sys.exit(0)
left.write_text(str(n - 1))
existing = sorted(p.name for p in (archive / "batches").iterdir())
name = "%04d" % (int(existing[-1]) + 1 if existing else 1)
batch = archive / "batches" / name
(batch / "receipts").mkdir(parents=True)
manifest = json.dumps({"pages": [{"page": "p%d" % i, "held": i < 3} for i in range(5)]}).encode()
(batch / "manifest.json").write_bytes(manifest)
pages = 5
if os.environ.get("FAKE_SECS_PER_PAGE"):
    # Honours --pages, taking this long per page, like a real batch.
    import time
    pages = int(args[args.index("--pages") + 1])
    time.sleep(pages * float(os.environ["FAKE_SECS_PER_PAGE"]))
receipt = {"step": "dispatch", "batch": name, "complete": True, "pages": pages,
           "manifest_sha256": hashlib.sha256(manifest).hexdigest()}
(batch / "receipts/dispatch.json").write_text(json.dumps(receipt))
print(json.dumps({"event": "boinc_archive_dispatch", "apply": True, **receipt}))
'''

# The fake step command: checks the tar and the arguments, answers with a bridge receipt.
FAKE_STEP = r'''
import hashlib, io, json, os, sys, tarfile
args = sys.argv[1:]
open(os.environ["FAKE_LOG"], "a").write("step " + " ".join(args) + "\n")
import time
time.sleep(float(os.environ.get("FAKE_BRIDGE_SLEEP", "0")))
if os.environ.get("FAKE_BRIDGE_FAILS"):
    sys.exit(1)
files = {}
with tarfile.open(fileobj=io.BytesIO(sys.stdin.buffer.read()), mode="r:") as tar:
    for m in tar:
        if m.isreg():
            files[m.name] = tar.extractfile(m).read()
assert sorted(files) == ["manifest.json", "receipts/dispatch.json"], sorted(files)
batch = args[args.index("--var") + 1].split("=", 1)[1]
print(json.dumps({"event": "boinc_archive_bridge", "apply": True, "batch": batch, "complete": True,
                  "manifest_sha256": hashlib.sha256(files["manifest.json"]).hexdigest()}))
'''

FAKE_SYSTEMCTL = r'''#!/bin/sh
echo "systemctl $*" >> "$FAKE_LOG"
case "$1" in
  is-active)
    case "$2" in
      # The shared unit from before #1690, when a test sets it apart.
      usnm-boinc-priority-controller.timer) cat "${FAKE_LEGACY_TIMER:-$FAKE_TIMER}" ;;
      *.timer) cat "$FAKE_TIMER" ;;
      # A tick that is running on the first look and then ends in the state the test names.
      *.service)
        if [ -n "$FAKE_GAP_TICK" ] && [ -e "$USNM_HELD_ARCHIVE_LOCK.gap" ]; then
          n=$(cat "$FAKE_GAP_TICK_N" 2> /dev/null || echo 0); echo $((n + 1)) > "$FAKE_GAP_TICK_N"
          if [ "$n" -eq 0 ]; then echo activating; else [ -z "$FAKE_GAP_TICK_SLEEP" ] || sleep "$FAKE_GAP_TICK_SLEEP"; echo "$FAKE_GAP_TICK"; fi
        else echo inactive; fi ;;
      *) echo inactive ;;
    esac ;;
  stop) echo inactive > "$FAKE_TIMER" ;;
  start)
    [ -z "$FAKE_START_FAILS" ] || exit 1
    # The tick before a batch (--pre-batch-tick) starts the plan's service and waits for it.
    case "$2" in *.service)
      if [ -n "$FAKE_TICK_SLEEP" ]; then
        trap 'kill $p 2> /dev/null; exit 143' TERM
        sleep "$FAKE_TICK_SLEEP" & p=$!
        wait $p
        trap - TERM
      fi
      [ -z "$FAKE_TICK_FAILS" ] || exit 1 ;; esac
    if [ -n "$FAKE_PROBE" ]; then
      [ ! -e "$USNM_HELD_ARCHIVE_LOCK.gap" ] || echo "gap file present" >> "$FAKE_LOG"
      if flock -n "$USNM_HELD_ARCHIVE_LOCK" true; then echo "archive lock free" >> "$FAKE_LOG"; else echo "archive lock held" >> "$FAKE_LOG"; fi
      if flock -n "$USNM_HELD_ARCHIVE_LOCK.run" true; then echo "run lock free" >> "$FAKE_LOG"; else echo "run lock held" >> "$FAKE_LOG"; fi
      if [ -n "$FAKE_TICK_LOCK" ]; then
        if flock -n "$FAKE_TICK_LOCK" true; then echo "tick lock free" >> "$FAKE_LOG"; else echo "tick lock held" >> "$FAKE_LOG"; fi
      fi
    fi
    case "$2" in *.timer) echo active > "$FAKE_TIMER" ;; esac ;;
esac
'''


@unittest.skipUnless(shutil.which("flock") and shutil.which("sha256sum"), "needs flock and sha256sum (Linux)")
class HeldArchive(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = self.root = Path(self.tmp.name)
        (root / "bin").mkdir()
        (root / "bin/systemctl").write_text(FAKE_SYSTEMCTL)
        (root / "bin/systemctl").chmod(0o755)
        (root / "tool.py").write_text(FAKE_TOOL)
        (root / "step.py").write_text(FAKE_STEP)
        (root / "archive/batches").mkdir(parents=True)
        (root / "state").mkdir()
        (root / "state/state.json").write_text(json.dumps({"claims": {"a": 1}, "archived_held": {"b": 1, "c": 2}}))
        (root / "config.json").write_text(json.dumps(
            {"env": "prod", "step_command": ["python3", str(root / "step.py"), "--"]}))
        (root / "timer").write_text("active\n")
        (root / "left").write_text("2")

    def tearDown(self):
        self.tmp.cleanup()

    def run_archive(self, *extra, **env):
        root = self.root
        environment = {**os.environ, "PATH": f"{root / 'bin'}{os.pathsep}{os.environ['PATH']}",
                       "FAKE_LOG": str(root / "log"), "FAKE_TIMER": str(root / "timer"),
                       "FAKE_LEFT": str(root / "left"), "USNM_HELD_ARCHIVE_LOCK": str(root / "lock"), **env}
        return subprocess.run(
            ["bash", str(SCRIPT), "--config", str(root / "config.json"), "--state-dir", str(root / "state"),
             "--archive-dir", str(root / "archive"), "--tool", str(root / "tool.py"), "--wait-mins", "1", *extra],
            env=environment, capture_output=True, text=True, timeout=60)

    def log(self):
        return (self.root / "log").read_text().splitlines()

    def summary(self, done):
        return json.loads([line for line in done.stdout.splitlines() if '"boinc_held_archive"' in line][-1])

    def test_pauses_dispatches_and_bridges_each_batch_then_resumes(self):
        done = self.run_archive()
        self.assertEqual(done.returncode, 0, done.stderr)
        log = self.log()
        self.assertIn("systemctl stop usnm-boinc-priority-controller@main.timer", log)
        stop = log.index("systemctl stop usnm-boinc-priority-controller@main.timer")
        first_apply = next(i for i, line in enumerate(log) if line.startswith("tool dispatch") and "--apply" in line)
        self.assertLess(stop, first_apply)
        self.assertEqual(log[-2:], ["systemctl start usnm-boinc-priority-controller@main.timer",
                                    "systemctl start --no-block usnm-boinc-priority-controller@main.service"])
        steps = [line for line in log if line.startswith("step ")]
        self.assertEqual(steps, [f"step -- --env prod --target archive --step bridge --var ARCHIVE_BATCH={n} "
                                 "--wait-mins 1" for n in ("0001", "0002")])
        for n in ("0001", "0002"):
            receipt = json.loads((self.root / f"archive/batches/{n}/receipts/bridge.json").read_text())
            self.assertEqual((receipt["batch"], receipt["complete"]), (n, True))
        summary = self.summary(done)
        self.assertEqual((summary["batches"], summary["pages"], summary["held"]), (["0001", "0002"], 10, 6))
        self.assertEqual(summary["state"], {"claims": 1, "selected": 0, "held_since": 0, "archived_held": 2})
        self.assertEqual((self.root / "timer").read_text().strip(), "active")

    def test_nothing_to_archive_still_resumes(self):
        (self.root / "left").write_text("0")
        done = self.run_archive()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.summary(done)["batches"], [])
        self.assertEqual((self.root / "timer").read_text().strip(), "active")

    def test_an_operator_pause_is_left_alone(self):
        (self.root / "timer").write_text("inactive\n")
        done = self.run_archive()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("an operator has paused the controller", done.stdout)
        self.assertFalse(any(line.startswith(("tool", "step", "systemctl stop", "systemctl start"))
                             for line in self.log()))
        self.assertEqual((self.root / "timer").read_text().strip(), "inactive")

    def test_the_main_plans_unit_is_the_default(self):
        done = self.run_archive()
        self.assertEqual(done.returncode, 0, done.stderr)
        tool = [line for line in self.log() if line.startswith("tool dispatch")]
        self.assertTrue(tool and all("--controller-unit usnm-boinc-priority-controller@main" in line
                                     for line in tool))

    def test_a_controller_still_on_the_shared_unit_fails_instead_of_skipping(self):
        # #1690: before the host moves to the per-plan units, the main plan's timer isn't there
        # but the shared one ticks. That isn't an operator's pause: skipping would let the
        # journals reach the cap unnoticed.
        (self.root / "timer").write_text("inactive\n")
        (self.root / "legacy").write_text("active\n")
        done = self.run_archive(FAKE_LEGACY_TIMER=str(self.root / "legacy"))
        self.assertEqual(done.returncode, 1)
        self.assertIn("migrate to the per-plan units", done.stdout)
        self.assertFalse(any(line.startswith(("tool", "step", "systemctl stop", "systemctl start"))
                             for line in self.log()))
        # Named on purpose, the shared unit is still archived around.
        done = self.run_archive(FAKE_LEGACY_TIMER=str(self.root / "legacy"),
                                USNM_CONTROLLER_UNIT="usnm-boinc-priority-controller")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("systemctl stop usnm-boinc-priority-controller.timer", self.log())

    def test_a_failed_bridge_fails_the_run_resumes_and_is_retried_first_next_time(self):
        done = self.run_archive(FAKE_BRIDGE_FAILS="1")
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("boinc_held_archive_failure", done.stdout)
        self.assertEqual((self.root / "timer").read_text().strip(), "active")
        self.assertFalse((self.root / "archive/batches/0001/receipts/bridge.json").exists())
        (self.root / "log").write_text("")
        done = self.run_archive()
        self.assertEqual(done.returncode, 0, done.stderr)
        steps = [line.split("ARCHIVE_BATCH=")[1].split()[0] for line in self.log() if line.startswith("step ")]
        self.assertEqual(steps, ["0001", "0002"])
        self.assertEqual(self.summary(done)["batches"], ["0002"])

    def test_a_failed_dispatch_fails_the_run_and_resumes(self):
        done = self.run_archive(FAKE_DISPATCH_FAILS="1")
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("the dispatch step failed", done.stderr)
        self.assertEqual((self.root / "timer").read_text().strip(), "active")

    def hold_tick_lock(self, tick, seconds):
        """A tick queued on the lock: it holds it for `seconds`, logs its release, and has the
        lock before this returns."""
        ready = self.root / "holder-ready"
        holder = subprocess.Popen(["flock", str(tick), "sh", "-c",
                                   f'touch "{ready}"; sleep {seconds}; echo "holder released" >> "$FAKE_LOG"'],
                                  env={**os.environ, "FAKE_LOG": str(self.root / "log")},
                                  start_new_session=True)
        # flock, sh and sleep together: killing only the flock wrapper would leave its children
        # holding the lock and writing into a removed directory after tearDown.
        self.addCleanup(self.end_group, holder)
        for _ in range(500):
            if ready.exists():
                break
            time.sleep(0.01)
        self.assertTrue(ready.exists(), "the holder never took the tick lock")
        return holder

    @staticmethod
    def end_group(process):
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait()

    def test_a_tick_lock_is_waited_for_then_held_through_the_run(self):
        # The run must wait for a tick already holding the lock, then hold it itself for
        # every dispatch, and release it only once it has resumed the controller.
        tick = self.root / "tick.lock"
        holder = self.hold_tick_lock(tick, 2)
        done = self.run_archive("--tick-lock", str(tick), FAKE_TICK_LOCK=str(tick))
        holder.wait()
        self.assertEqual(done.returncode, 0, done.stderr)
        log = self.log()
        first_tool = next(i for i, line in enumerate(log) if line.startswith("tool "))
        self.assertLess(log.index("holder released"), first_tool, log)
        self.assertIn("tick lock held", log)
        self.assertNotIn("tick lock free", log)
        self.assertEqual(log[-2:], ["systemctl start usnm-boinc-priority-controller@main.timer",
                                    "systemctl start --no-block usnm-boinc-priority-controller@main.service"])
        self.assertEqual(subprocess.run(["flock", "-n", str(tick), "true"]).returncode, 0)

    def test_a_stop_while_waiting_for_the_tick_lock_resumes_at_once(self):
        tick = self.root / "tick.lock"
        holder = self.hold_tick_lock(tick, 30)
        root = self.root
        run = subprocess.Popen(
            ["bash", str(SCRIPT), "--config", str(root / "config.json"), "--state-dir", str(root / "state"),
             "--archive-dir", str(root / "archive"), "--tool", str(root / "tool.py"), "--wait-mins", "1",
             "--tick-lock", str(tick)],
            env={**os.environ, "PATH": f"{root / 'bin'}{os.pathsep}{os.environ['PATH']}",
                 "FAKE_LOG": str(root / "log"), "FAKE_TIMER": str(root / "timer"),
                 "FAKE_LEFT": str(root / "left"), "USNM_HELD_ARCHIVE_LOCK": str(root / "lock")},
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            time.sleep(2)
            started = time.monotonic()
            run.send_signal(signal.SIGTERM)
            run.communicate(timeout=10)
            self.assertLess(time.monotonic() - started, 5)
            self.assertEqual(run.returncode, 143)
            self.assertFalse(any(line.startswith("tool ") for line in self.log()))
            self.assertEqual((self.root / "timer").read_text().strip(), "active")
        finally:
            if run.poll() is None:
                run.kill()
                run.wait()

    def test_without_a_tick_lock_nothing_is_held(self):
        tick = self.root / "tick.lock"
        done = self.run_archive(FAKE_TICK_LOCK=str(tick))
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertNotIn("tick lock held", self.log())

    def test_a_failed_restart_fails_an_otherwise_good_run(self):
        done = self.run_archive(FAKE_START_FAILS="1")
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("the controller was not started again", done.stdout)

    def test_the_tool_checks_the_same_controller_unit(self):
        done = self.run_archive(USNM_CONTROLLER_UNIT="other-controller")
        self.assertEqual(done.returncode, 0, done.stderr)
        tool = [line for line in self.log() if line.startswith("tool dispatch")]
        self.assertTrue(tool and all("--controller-unit other-controller" in line for line in tool))
        self.assertIn("systemctl stop other-controller.timer", self.log())

    def test_other_plans_state_dirs_are_checked(self):
        # #1713: sibling <state dir>.* directories (the drain plans) by default, so a page
        # another plan claimed is never archived; an explicit --check-state-dir replaces them.
        root = self.root
        for name in ("state.queue-feed-10", "state.queue-feed-11", "state.empty"):
            (root / name).mkdir()
        for name in ("state.queue-feed-10", "state.queue-feed-11"):
            (root / name / "state.json").write_text("{}")
        done = self.run_archive()
        self.assertEqual(done.returncode, 0, done.stderr)
        tool = [line for line in self.log() if line.startswith("tool dispatch")]
        expected = (f"--check-state-dir {root / 'state.queue-feed-10'} "
                    f"--check-state-dir {root / 'state.queue-feed-11'}")
        self.assertTrue(tool and all(expected in line for line in tool), tool)
        self.assertFalse(any("state.empty" in line for line in tool))
        # A trailing slash on --state-dir still finds the siblings (Copilot on #1714).
        (root / "log").write_text("")
        (root / "left").write_text("1")
        done = self.run_archive("--state-dir", f"{root / 'state'}//")
        self.assertEqual(done.returncode, 0, done.stderr)
        tool = [line for line in self.log() if line.startswith("tool dispatch")]
        self.assertTrue(tool and all(expected in line for line in tool), tool)
        (root / "log").write_text("")
        (root / "left").write_text("1")
        done = self.run_archive("--check-state-dir", str(root / "state.queue-feed-11"))
        self.assertEqual(done.returncode, 0, done.stderr)
        tool = [line for line in self.log() if line.startswith("tool dispatch")]
        self.assertTrue(all(f"--check-state-dir {root / 'state.queue-feed-11'}" in line
                            and "queue-feed-10" not in line for line in tool), tool)

    def test_a_stop_interrupts_the_tool_waits_for_it_and_resumes(self):
        import signal
        import time
        (self.root / "tool.py").write_text(
            "import os, sys, time\n"
            "open(os.environ['FAKE_LOG'], 'a').write('tool ' + ' '.join(sys.argv[1:]) + '\\n')\n"
            "try:\n"
            "    time.sleep(30)\n"
            "except KeyboardInterrupt:\n"
            "    open(os.environ['FAKE_LOG'], 'a').write('tool released the lease\\n')\n"
            "    sys.exit(1)\n")
        root = self.root
        env = {**os.environ, "PATH": f"{root / 'bin'}{os.pathsep}{os.environ['PATH']}",
               "FAKE_LOG": str(root / "log"), "FAKE_TIMER": str(root / "timer"),
               "FAKE_LEFT": str(root / "left"), "USNM_HELD_ARCHIVE_LOCK": str(root / "lock")}
        proc = subprocess.Popen(
            ["bash", str(SCRIPT), "--config", str(root / "config.json"), "--state-dir", str(root / "state"),
             "--archive-dir", str(root / "archive"), "--tool", str(root / "tool.py"), "--wait-mins", "1"],
            env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        deadline = time.time() + 20
        def started():
            log = root / "log"
            return log.exists() and any(line.startswith("tool dispatch") for line in log.read_text().splitlines())
        while time.time() < deadline and not started():
            time.sleep(0.1)
        proc.send_signal(signal.SIGTERM)
        proc.communicate(timeout=30)
        self.assertEqual(proc.returncode, 143)
        log = self.log()
        released = log.index("tool released the lease")
        self.assertLess(released, log.index("systemctl start usnm-boinc-priority-controller@main.timer"))
        self.assertEqual((root / "timer").read_text().strip(), "active")

    def test_a_stop_during_the_bridge_ends_it_and_resumes(self):
        import signal
        import time
        (self.root / "step.py").write_text(
            "import os, sys, time\n"
            "open(os.environ['FAKE_LOG'], 'a').write('step started\\n')\n"
            "time.sleep(30)\n")
        root = self.root
        env = {**os.environ, "PATH": f"{root / 'bin'}{os.pathsep}{os.environ['PATH']}",
               "FAKE_LOG": str(root / "log"), "FAKE_TIMER": str(root / "timer"),
               "FAKE_LEFT": str(root / "left"), "USNM_HELD_ARCHIVE_LOCK": str(root / "lock")}
        start = time.time()
        proc = subprocess.Popen(
            ["bash", str(SCRIPT), "--config", str(root / "config.json"), "--state-dir", str(root / "state"),
             "--archive-dir", str(root / "archive"), "--tool", str(root / "tool.py"), "--wait-mins", "1"],
            env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        log = root / "log"
        while time.time() - start < 20 and not (log.exists() and "step started" in log.read_text()):
            time.sleep(0.1)
        proc.send_signal(signal.SIGTERM)
        proc.communicate(timeout=30)
        self.assertEqual(proc.returncode, 143)
        self.assertLess(time.time() - start, 20)
        self.assertEqual((root / "timer").read_text().strip(), "active")

    def test_max_batches(self):
        (self.root / "left").write_text("5")
        done = self.run_archive("--max-batches", "2")
        self.assertEqual(done.returncode, 0, done.stderr)
        summary = self.summary(done)
        self.assertEqual((summary["batches"], summary["eligible_left"]), (["0001", "0002"], 3000))

    def test_max_minutes_starts_no_batch_after_the_budget(self):
        (self.root / "left").write_text("5")
        # bash starts SECONDS at the value in its environment: the run is 2 minutes old already,
        # so not even the first batch starts, and the controller is left running.
        done = self.run_archive("--max-batches", "5", "--max-minutes", "1", SECONDS="120")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.summary(done)["batches"], [])
        self.assertEqual((self.root / "timer").read_text().strip(), "active")

    def test_max_minutes_spent_starts_no_batch_even_with_a_zero_estimate(self):
        done = self.run_archive("--max-batches", "5", "--max-minutes", "1", "--batch-mins", "0", SECONDS="60")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.summary(done)["batches"], [])

    def test_max_minutes_starts_no_batch_the_rest_of_it_cannot_cover(self):
        (self.root / "left").write_text("5")
        # 4 minutes of 5 are used: under --batch-mins 2 a batch does not fit, though the budget is not spent.
        done = self.run_archive("--max-batches", "5", "--max-minutes", "5", "--batch-mins", "2", SECONDS="240")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.summary(done)["batches"], [])
        # With time for the estimate, the first runs; its measured 1 s then replaces the 2-minute
        # estimate, so the rest follow.
        (self.root / "left").write_text("5")
        done = self.run_archive("--max-batches", "5", "--max-minutes", "5", "--batch-mins", "2", SECONDS="177",
                                FAKE_BRIDGE_SLEEP="1")
        self.assertEqual(self.summary(done)["batches"], ["0001", "0002", "0003", "0004", "0005"])

    def test_max_minutes_checks_the_budget_after_the_first_tick(self):
        (self.root / "left").write_text("5")
        # 50 s old, a 1-minute estimate fits in 2; the tick takes 12 s, and then it no longer does.
        done = self.run_archive("--max-batches", "5", "--max-minutes", "2", "--batch-mins", "1", "--pre-batch-tick",
                                SECONDS="50", FAKE_TICK_SLEEP="12")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.summary(done)["batches"], [])

    def test_a_pre_batch_tick_still_running_when_the_budget_ends_fails_the_run_and_resumes(self):
        (self.root / "left").write_text("5")
        # 3 s of the budget are left: the tick is waited for that long, not a minute, and no batch starts.
        done = self.run_archive("--max-batches", "5", "--max-minutes", "1", "--batch-mins", "0", "--pre-batch-tick",
                                SECONDS="57", FAKE_TICK_SLEEP="30")
        self.assertNotEqual(done.returncode, 0)
        self.assertIn('"timed_out": true', done.stdout)
        self.assertEqual([line for line in self.log() if line.startswith("tool ") and "--apply" in line], [])
        self.assertEqual((self.root / "timer").read_text().strip(), "active")
        self.assertFalse(Path(f"{self.root / 'lock'}.gap").exists())

    def test_a_failed_gap_tick_is_logged_and_not_repeated(self):
        (self.root / "left").write_text("5")
        done = self.run_archive("--max-batches", "2", "--gap-mins", "1", "--pre-batch-tick",
                                FAKE_GAP_TICK="failed", FAKE_GAP_TICK_N=str(self.root / "n"))
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.summary(done)["batches"], ["0001", "0002"])
        self.assertEqual(done.stdout.count('"ok": false, "gap_tick": true'), 1, done.stdout)
        # Only the tick before the first batch was started with a blocking start.
        self.assertEqual(self.log().count(self.TICK), 1)

    def test_max_minutes_expects_the_longest_batch_so_far(self):
        (self.root / "left").write_text("5")
        # Batch 1 takes 6 s (the bridge sleeps); at 56 s of 60 another 6 s does not fit, though 0 minutes were guessed.
        done = self.run_archive("--max-batches", "5", "--max-minutes", "1", "--batch-mins", "0",
                                SECONDS="50", FAKE_BRIDGE_SLEEP="6")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.summary(done)["batches"], ["0001"])

    def test_max_minutes_leaves_the_bridge_step_its_own_wait(self):
        done = self.run_archive("--max-batches", "1", "--max-minutes", "2", "--batch-mins", "0", "--wait-mins", "30")
        self.assertEqual(done.returncode, 0, done.stderr)
        steps = [line for line in self.log() if line.startswith("step ")]
        self.assertTrue(steps and "--wait-mins 30" in steps[0], steps)

    def test_gap_lets_the_controller_tick_between_batches(self):
        (self.root / "left").write_text("5")
        tick = self.root / "tick.lock"
        done = self.run_archive("--max-batches", "3", "--gap-mins", "1", "--tick-lock", str(tick),
                                FAKE_PROBE="1", FAKE_TICK_LOCK=str(tick), USNM_HELD_ARCHIVE_TICK_START_SECS="1")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.summary(done)["batches"], ["0001", "0002", "0003"])
        log = self.log()
        timer, service = ("systemctl stop usnm-boinc-priority-controller@main.timer",
                          "systemctl start --no-block usnm-boinc-priority-controller@main.service")
        # One pause per batch, one start between two batches and one at the end.
        self.assertEqual(log.count(timer), 3, log)
        self.assertEqual(log.count(service), 3, log)
        applies = [i for i, line in enumerate(log) if line.startswith("tool ") and "--apply" in line]
        steps = [i for i, line in enumerate(log) if line.startswith("step ")]
        stops = [i for i, line in enumerate(log) if line == timer]
        starts = [i for i, line in enumerate(log) if line == service]
        # Every batch is dispatched and bridged inside its own pause, and the controller starts
        # between them.
        for n in range(3):
            self.assertLess(stops[n], applies[n], log)
            self.assertLess(applies[n], steps[n], log)
            self.assertLess(steps[n], starts[n], log)
            if n < 2:
                self.assertLess(starts[n], stops[n + 1], log)
        # While the controller ticks between batches nothing is held but the run lock, and the
        # gap file tells the plans' ticks so; at the end the tick lock is still held while it starts.
        # The fake probes at each of a start's two commands (the timer, then the tick): the two
        # between batches see only the run lock, and the closing pair still the whole pause.
        self.assertEqual(log.count("gap file present"), 4, log)
        self.assertEqual(log.count("archive lock free"), 4, log)
        self.assertEqual(log.count("archive lock held"), 2, log)
        self.assertEqual(log.count("run lock held"), 6, log)
        self.assertEqual(log.count("tick lock free"), 4, log)
        self.assertEqual(log.count("tick lock held"), 2 + 4, log)  # + the tool's check in each dispatch and in the closing dry run
        self.assertFalse(Path(f"{self.root / 'lock'}.gap").exists())
        self.assertEqual((self.root / "timer").read_text().strip(), "active")
        self.assertEqual(subprocess.run(["flock", "-n", str(tick), "true"]).returncode, 0)

    TICK = "systemctl start usnm-boinc-priority-controller@main.service"
    STOP = "systemctl stop usnm-boinc-priority-controller@main.timer"

    def test_pre_batch_tick_is_off_by_default(self):
        done = self.run_archive("--max-batches", "2")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertNotIn(self.TICK, self.log())
        self.assertNotIn("boinc_held_archive_pre_batch_tick", done.stdout)

    def test_pre_batch_tick_runs_before_each_batch_with_the_locks_let_go(self):
        (self.root / "left").write_text("5")
        tick = self.root / "tick.lock"
        done = self.run_archive("--max-batches", "2", "--pre-batch-tick", "--tick-lock", str(tick),
                                FAKE_PROBE="1", FAKE_TICK_LOCK=str(tick))
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.summary(done)["batches"], ["0001", "0002"])
        log = self.log()
        ticks = [i for i, line in enumerate(log) if line == self.TICK]
        stops = [i for i, line in enumerate(log) if line == self.STOP]
        applies = [i for i, line in enumerate(log) if line.startswith("tool ") and "--apply" in line]
        self.assertEqual(len(ticks), 2, log)
        for n in range(2):
            # The tick comes after the first pause (the run's own) or the last batch, and the pause
            # that dispatches the batch comes after the tick.
            self.assertLess(ticks[n], stops[n + 1], log)
            self.assertLess(stops[n + 1], applies[n], log)
            # Only the run lock is held while it ticks, and the gap file says so.
            self.assertEqual(log[ticks[n] + 1:ticks[n] + 5],
                             ["gap file present", "archive lock free", "run lock held", "tick lock free"], log)
        self.assertEqual(done.stdout.count('"boinc_held_archive_pre_batch_tick", "ok": true'), 2)
        self.assertFalse(Path(f"{self.root / 'lock'}.gap").exists())
        self.assertEqual((self.root / "timer").read_text().strip(), "active")
        self.assertEqual(subprocess.run(["flock", "-n", str(tick), "true"]).returncode, 0)

    def test_pre_batch_tick_with_a_gap_ticks_before_every_batch(self):
        (self.root / "left").write_text("5")
        done = self.run_archive("--max-batches", "3", "--gap-mins", "1", "--pre-batch-tick",
                                USNM_HELD_ARCHIVE_TICK_START_SECS="1")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.summary(done)["batches"], ["0001", "0002", "0003"])
        log = self.log()
        self.assertEqual(log.count(self.TICK), 3, log)
        applies = [i for i, line in enumerate(log) if line.startswith("tool ") and "--apply" in line]
        ticks = [i for i, line in enumerate(log) if line == self.TICK]
        for n in range(3):
            self.assertLess(ticks[n], applies[n], log)

    def test_pre_batch_tick_does_not_double_the_gap_tick(self):
        (self.root / "left").write_text("5")
        # The fake service is never active, so the gap sees no tick of its own: one blocking tick
        # per batch. When it does see one end, none is added (covered by the real unit's state).
        done = self.run_archive("--max-batches", "2", "--gap-mins", "1", "--pre-batch-tick",
                                USNM_HELD_ARCHIVE_TICK_START_SECS="1")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.log().count(self.TICK), 2)

    def test_a_gap_that_uses_the_budget_up_ends_the_run_without_pausing_again(self):
        (self.root / "left").write_text("5")
        # The gap's tick is running on the first look and ends 12 s into the gap, past the minute (50 s old
        # at the start), so no batch follows and the run ends well, with the controller running.
        done = self.run_archive("--max-batches", "5", "--max-minutes", "1", "--gap-mins", "1", "--batch-mins", "0",
                                SECONDS="50", FAKE_GAP_TICK="inactive", FAKE_GAP_TICK_N=str(self.root / "n"),
                                FAKE_GAP_TICK_SLEEP="12", USNM_HELD_ARCHIVE_TICK_START_SECS="1")
        self.assertEqual(done.returncode, 0, done.stderr)
        summary = json.loads([line for line in done.stdout.splitlines() if line.startswith("{")][-1])
        self.assertEqual((summary["batches"], summary["budget_ended"]), (["0001"], True))
        self.assertEqual((self.root / "timer").read_text().strip(), "active")
        self.assertFalse(Path(f"{self.root / 'lock'}.gap").exists())
        # No second pause: the controller was stopped once, for the one batch.
        self.assertEqual(self.log().count(self.STOP), 1)

    def test_a_failed_pre_batch_tick_is_logged_and_the_batch_goes_on(self):
        (self.root / "left").write_text("5")
        done = self.run_archive("--max-batches", "2", "--pre-batch-tick", FAKE_TICK_FAILS="1")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.summary(done)["batches"], ["0001", "0002"])
        self.assertEqual(done.stdout.count('"boinc_held_archive_pre_batch_tick", "ok": false'), 2)
        self.assertIn("the batch goes on", done.stderr)
        self.assertEqual((self.root / "timer").read_text().strip(), "active")

    def test_a_pre_batch_tick_does_not_skip_the_archive_checks(self):
        # The tool still runs once per batch with the same arguments as without the tick.
        (self.root / "left").write_text("1")
        done = self.run_archive("--max-batches", "1", "--pre-batch-tick")
        self.assertEqual(done.returncode, 0, done.stderr)
        applies = [line for line in self.log() if line.startswith("tool ") and "--apply" in line]
        self.assertEqual(len(applies), 1)
        self.assertIn("--confirm-controller-paused", applies[0])
        self.assertIn("--include-held", applies[0])

    def test_a_gap_batch_failure_resumes_and_clears_the_gap(self):
        (self.root / "left").write_text("5")
        done = self.run_archive("--max-batches", "3", "--gap-mins", "1", USNM_HELD_ARCHIVE_TICK_START_SECS="1",
                                FAKE_DISPATCH_FAILS="1")
        self.assertNotEqual(done.returncode, 0)
        self.assertFalse(Path(f"{self.root / 'lock'}.gap").exists())
        self.assertEqual((self.root / "timer").read_text().strip(), "active")

    def asked_pages(self):
        return [int(line.split("--pages ")[1].split()[0]) for line in self.log()
                if line.startswith("tool dispatch") and "--apply" in line]

    def test_max_pause_mins_grows_batches_to_fill_the_pause_and_ticks_between_them(self):
        (self.root / "left").write_text("3")
        # 0.002 s a page and a 20 s pause: each batch takes a second or two, so the next may double.
        done = self.run_archive("--pages", "1000", "--max-batches", "3", "--gap-mins", "1", "--max-pause-mins", "1",
                                USNM_HELD_ARCHIVE_MAX_PAUSE_SECS="20", FAKE_SECS_PER_PAGE="0.002",
                                USNM_HELD_ARCHIVE_TICK_START_SECS="1")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.asked_pages(), [100, 200, 400])
        self.assertEqual(int((self.root / "archive/held-archive.pages").read_text()), 800)
        # The controller is started in each of the two gaps, then once more at the end.
        log = self.log()
        self.assertEqual(log.count("systemctl start --no-block usnm-boinc-priority-controller@main.service"), 3)
        sizes = [json.loads(line) for line in done.stdout.splitlines() if "boinc_held_archive_batch_size" in line]
        self.assertEqual([size["next_pages"] for size in sizes], [200, 400, 800])
        self.assertEqual((self.root / "timer").read_text().strip(), "active")

    def test_max_pause_mins_shrinks_a_batch_that_overran_the_pause(self):
        (self.root / "left").write_text("2")
        # 100 pages at 0.03 s are about 3 s against a 1 s pause: the next asks for about a third.
        done = self.run_archive("--pages", "1000", "--max-batches", "2", "--gap-mins", "1", "--max-pause-mins", "1",
                                USNM_HELD_ARCHIVE_MAX_PAUSE_SECS="1", FAKE_SECS_PER_PAGE="0.03",
                                USNM_HELD_ARCHIVE_TICK_START_SECS="1")
        self.assertEqual(done.returncode, 0, done.stderr)
        first, second = self.asked_pages()
        self.assertEqual(first, 100)
        self.assertTrue(25 <= second <= 50, second)

    def test_max_pause_mins_starts_from_the_last_runs_size_within_pages(self):
        (self.root / "archive/held-archive.pages").write_text("700\n")
        (self.root / "left").write_text("1")
        done = self.run_archive("--pages", "300", "--max-batches", "1", "--gap-mins", "1", "--max-pause-mins", "10")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.asked_pages(), [300])

    def test_without_max_pause_mins_every_batch_asks_for_pages(self):
        (self.root / "archive/held-archive.pages").write_text("40\n")
        done = self.run_archive("--pages", "300", "--max-batches", "2")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.asked_pages(), [300, 300])

    def test_max_pause_mins_needs_a_gap(self):
        done = self.run_archive("--max-pause-mins", "10")
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("--max-pause-mins needs --gap-mins", done.stderr)
        self.assertFalse((self.root / "log").exists() and any(l.startswith("systemctl stop") for l in self.log()))

    def test_two_archives_never_overlap_even_in_a_gap(self):
        lock = self.root / "lock"
        holder = subprocess.Popen(["flock", f"{lock}.run", "sleep", "30"], start_new_session=True)
        self.addCleanup(self.end_group, holder)
        time.sleep(0.3)
        done = self.run_archive("--lock-wait-mins", "0")
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("another held-page archive is running", done.stderr)
        self.assertNotIn("systemctl stop", "\n".join(self.log()) if (self.root / "log").exists() else "")

    def test_a_busy_archive_lock_is_waited_for_and_the_wait_logged(self):
        lock = self.root / "lock"
        holder = subprocess.Popen(["flock", f"{lock}.run", "sleep", "2"], start_new_session=True)
        self.addCleanup(self.end_group, holder)
        time.sleep(0.3)
        done = self.run_archive("--lock-wait-mins", "1")
        self.assertEqual(done.returncode, 0, done.stderr)
        events = [json.loads(line) for line in done.stdout.splitlines() if "boinc_held_archive_lock_wait" in line]
        self.assertEqual([e["waiting"] for e in events], [True, False])
        self.assertGreaterEqual(events[1]["waited_seconds"], 1)
        self.assertEqual(self.summary(done)["batches"], ["0001", "0002"])

    def test_a_queued_run_marks_its_invocation_while_it_waits_and_clears_it(self):
        # controller-tick.sh doesn't skip other plans for a run that is only queued (#2131).
        lock = self.root / "lock"
        queued = Path(f"{lock}.queued.d")
        invocation = "0123456789abcdef0123456789abcdef"
        holder = subprocess.Popen(["flock", f"{lock}.run", "sleep", "2"], start_new_session=True)
        self.addCleanup(self.end_group, holder)
        time.sleep(0.3)
        root = self.root
        environment = {**os.environ, "PATH": f"{root / 'bin'}{os.pathsep}{os.environ['PATH']}",
                       "FAKE_LOG": str(root / "log"), "FAKE_TIMER": str(root / "timer"),
                       "FAKE_LEFT": str(root / "left"), "USNM_HELD_ARCHIVE_LOCK": str(lock),
                       "INVOCATION_ID": invocation}
        run = subprocess.Popen(
            ["bash", str(SCRIPT), "--config", str(root / "config.json"), "--state-dir", str(root / "state"),
             "--archive-dir", str(root / "archive"), "--tool", str(root / "tool.py"), "--wait-mins", "1",
             "--lock-wait-mins", "1"],
            env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        for _ in range(100):
            if (queued / invocation).exists():
                break
            time.sleep(0.05)
        self.assertTrue((queued / invocation).exists(), "the queued run left no marker")
        out, err = run.communicate(timeout=60)
        self.assertEqual(run.returncode, 0, err)
        self.assertIn("boinc_held_archive_lock_wait", out)
        self.assertEqual(list(queued.iterdir()), [])

    def test_a_queued_run_that_times_out_clears_its_marker(self):
        lock = self.root / "lock"
        holder = subprocess.Popen(["flock", f"{lock}.run", "sleep", "30"], start_new_session=True)
        self.addCleanup(self.end_group, holder)
        time.sleep(0.3)
        done = self.run_archive("--lock-wait-mins", "1", USNM_HELD_ARCHIVE_LOCK_WAIT_SECS="1",
                                INVOCATION_ID="fedcba9876543210fedcba9876543210")
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("another held-page archive is running", done.stderr)
        self.assertEqual(list(Path(f"{lock}.queued.d").iterdir()), [])

    def test_a_run_outside_systemd_marks_nothing(self):
        lock = self.root / "lock"
        holder = subprocess.Popen(["flock", f"{lock}.run", "sleep", "2"], start_new_session=True)
        self.addCleanup(self.end_group, holder)
        time.sleep(0.3)
        environment = {k: v for k, v in os.environ.items() if k != "INVOCATION_ID"}
        with unittest.mock.patch.dict(os.environ, environment, clear=True):
            done = self.run_archive("--lock-wait-mins", "1")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertFalse(Path(f"{lock}.queued.d").exists())

    def test_skip_if_busy_skips_the_slot_when_another_archive_runs(self):
        lock = self.root / "lock"
        holder = subprocess.Popen(["flock", f"{lock}.run", "sleep", "30"], start_new_session=True)
        self.addCleanup(self.end_group, holder)
        time.sleep(0.3)
        started = time.monotonic()
        done = self.run_archive("--skip-if-busy", "--lock-wait-mins", "90")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertLess(time.monotonic() - started, 10)
        self.assertIn("busy, next slot", done.stdout)
        self.assertNotIn("boinc_held_archive_lock_wait", done.stdout)
        self.assertNotIn("systemctl stop", "\n".join(self.log()) if (self.root / "log").exists() else "")

    def test_skip_if_busy_skips_at_most_one_slot_in_a_row(self):
        lock = self.root / "lock"
        holder = subprocess.Popen(["flock", f"{lock}.run", "sleep", "30"], start_new_session=True)
        self.addCleanup(self.end_group, holder)
        time.sleep(0.3)
        first = self.run_archive("--skip-if-busy")
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertIn("busy, next slot", first.stdout)
        # The next run after a skip waits for the lock instead of skipping again.
        second = self.run_archive("--skip-if-busy", "--lock-wait-mins", "1", USNM_HELD_ARCHIVE_LOCK_WAIT_SECS="1")
        self.assertNotEqual(second.returncode, 0)
        self.assertNotIn("busy, next slot", second.stdout)
        self.assertIn("boinc_held_archive_lock_wait", second.stdout)
        self.end_group(holder)
        # Once it has the lock the marker is gone, so a later busy slot can be skipped again.
        third = self.run_archive("--skip-if-busy")
        self.assertEqual(third.returncode, 0, third.stderr)
        self.assertNotIn("busy, next slot", third.stdout)
        holder = subprocess.Popen(["flock", f"{lock}.run", "sleep", "30"], start_new_session=True)
        self.addCleanup(self.end_group, holder)
        time.sleep(0.3)
        fourth = self.run_archive("--skip-if-busy")
        self.assertEqual(fourth.returncode, 0, fourth.stderr)
        self.assertIn("busy, next slot", fourth.stdout)

    def test_skip_if_busy_still_fails_when_a_tick_holds_the_archive_lock_too_long(self):
        lock = self.root / "lock"
        holder = subprocess.Popen(["flock", f"{lock}", "sleep", "30"], start_new_session=True)
        self.addCleanup(self.end_group, holder)
        time.sleep(0.3)
        done = self.run_archive("--skip-if-busy", "--lock-wait-mins", "1", USNM_HELD_ARCHIVE_LOCK_WAIT_SECS="1")
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("another held-page archive is running", done.stderr)

    def test_skip_if_busy_runs_normally_when_nothing_else_does(self):
        done = self.run_archive("--skip-if-busy")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertNotIn("busy, next slot", done.stdout)
        self.assertEqual(self.summary(done)["batches"], ["0001", "0002"])

    def test_a_stop_while_waiting_for_the_archive_lock_ends_the_waiter_too(self):
        lock = self.root / "lock"
        holder = subprocess.Popen(["flock", f"{lock}.run", "sleep", "30"], start_new_session=True)
        self.addCleanup(self.end_group, holder)
        time.sleep(0.3)
        root = self.root
        environment = {**os.environ, "PATH": f"{root / 'bin'}{os.pathsep}{os.environ['PATH']}",
                       "FAKE_LOG": str(root / "log"), "FAKE_TIMER": str(root / "timer"),
                       "FAKE_LEFT": str(root / "left"), "USNM_HELD_ARCHIVE_LOCK": str(lock)}
        run = subprocess.Popen(
            ["bash", str(SCRIPT), "--config", str(root / "config.json"), "--state-dir", str(root / "state"),
             "--archive-dir", str(root / "archive"), "--tool", str(root / "tool.py"), "--lock-wait-mins", "5"],
            env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.addCleanup(run.kill)
        waiters = []
        for _ in range(200):
            waiters = subprocess.run(["pgrep", "-x", "-P", str(run.pid), "flock"], capture_output=True, text=True).stdout.split()
            if waiters:
                break
            time.sleep(0.05)
        self.assertTrue(waiters, "the run never started waiting")
        run.send_signal(signal.SIGTERM)
        out, _ = run.communicate(timeout=20)
        self.assertEqual(run.returncode, 143)
        self.assertIn('"waiting": true', out)
        time.sleep(0.2)
        for pid in waiters:
            self.assertFalse(Path(f"/proc/{pid}").exists(), f"the flock waiter {pid} is still alive")
        self.assertNotIn("systemctl stop", "\n".join(self.log()) if (self.root / "log").exists() else "")

    def test_the_wait_for_the_archive_lock_times_out_without_pausing_the_controller(self):
        lock = self.root / "lock"
        holder = subprocess.Popen(["flock", f"{lock}", "sleep", "30"], start_new_session=True)
        self.addCleanup(self.end_group, holder)
        time.sleep(0.3)
        started = time.monotonic()
        done = self.run_archive("--lock-wait-mins", "1", USNM_HELD_ARCHIVE_LOCK_WAIT_SECS="1")
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("another held-page archive is running", done.stderr)
        self.assertLess(time.monotonic() - started, 20)
        self.assertNotIn("systemctl stop", "\n".join(self.log()) if (self.root / "log").exists() else "")

    def test_the_budget_starts_once_the_locks_are_held(self):
        lock = self.root / "lock"
        holder = subprocess.Popen(["flock", f"{lock}.run", "sleep", "2"], start_new_session=True)
        self.addCleanup(self.end_group, holder)
        time.sleep(0.3)
        # A 1 minute budget the 2 second wait does not touch: both batches still run.
        done = self.run_archive("--lock-wait-mins", "1", "--max-minutes", "1", "--batch-mins", "0")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.summary(done)["batches"], ["0001", "0002"])

if __name__ == "__main__":
    unittest.main()
