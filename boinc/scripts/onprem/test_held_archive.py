"""The held-page archive's run (#835): python3 -m unittest -v boinc/scripts/onprem/test_held_archive.py

held-archive.sh runs against fakes: systemctl (the controller's timer and service), the step
command (the BOINC host's bridge step, over ssh in production) and archive_history.py. Linux
only: it needs flock and sha256sum.
"""

import json
import os
import shutil
import subprocess
import tempfile
import unittest
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
receipt = {"step": "dispatch", "batch": name, "complete": True, "pages": 5,
           "manifest_sha256": hashlib.sha256(manifest).hexdigest()}
(batch / "receipts/dispatch.json").write_text(json.dumps(receipt))
print(json.dumps({"event": "boinc_archive_dispatch", "apply": True, **receipt}))
'''

# The fake step command: checks the tar and the arguments, answers with a bridge receipt.
FAKE_STEP = r'''
import hashlib, io, json, os, sys, tarfile
args = sys.argv[1:]
open(os.environ["FAKE_LOG"], "a").write("step " + " ".join(args) + "\n")
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
      *.timer) cat "$FAKE_TIMER" ;;
      *) echo inactive ;;
    esac ;;
  stop) echo inactive > "$FAKE_TIMER" ;;
  start)
    [ -z "$FAKE_START_FAILS" ] || exit 1
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
        self.assertIn("systemctl stop usnm-boinc-priority-controller.timer", log)
        stop = log.index("systemctl stop usnm-boinc-priority-controller.timer")
        first_apply = next(i for i, line in enumerate(log) if line.startswith("tool dispatch") and "--apply" in line)
        self.assertLess(stop, first_apply)
        self.assertEqual(log[-2:], ["systemctl start usnm-boinc-priority-controller.timer",
                                    "systemctl start --no-block usnm-boinc-priority-controller.service"])
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
        self.assertLess(released, log.index("systemctl start usnm-boinc-priority-controller.timer"))
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


if __name__ == "__main__":
    unittest.main()
