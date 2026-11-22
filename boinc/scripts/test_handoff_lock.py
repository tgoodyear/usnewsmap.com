"""python3 -m unittest -v boinc/scripts/test_handoff_lock.py

The controller's local expiring lock (#1367): the record helper, onprem-step.sh's lock steps and
fencing (all stopped before podman), and the controller's use of them.
"""

import fcntl
import json
import os
import subprocess
import tempfile
import threading
import time
import unittest
from datetime import timedelta
from pathlib import Path
from unittest.mock import patch

import importlib.util


def load(name):
    """Load a sibling module by path: CI runs this file from the repository root."""
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(f"{name}.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


batch = load("batch_scheduler")
lock = load("handoff_lock")

SCRIPT = Path(__file__).resolve().parent / "onprem-step.sh"


class RecordTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def test_acquire_records_the_holder_and_a_second_acquire_is_refused(self):
        record = lock.acquire(self.root, "a-1", "ct211", 42, 300)
        self.assertEqual((record["holder"], record["host"], record["pid"], record["generation"]),
                         ("a-1", "ct211", 42, 1))
        for key in ("acquired_at", "renewed_at", "expires_at"):
            self.assertIn(key, record)
        with self.assertRaisesRegex(lock.LockError, "held: holder a-1 on host ct211"):
            lock.acquire(self.root, "b-2", "ct212", 7)

    def test_expired_lock_is_taken_with_the_next_generation(self):
        lock.acquire(self.root, "a-1", "h", 1, 300)
        with patch.object(lock, "now", return_value=lock.now() + timedelta(seconds=301)):
            record = lock.acquire(self.root, "b-2", "h", 2)
        self.assertEqual((record["holder"], record["generation"]), ("b-2", 2))

    def hold_step_lock(self, path, seconds=None):
        """An old holder's running step: the host's step lock, held (until `seconds` pass, if given)."""
        held = open(path, "a")
        fcntl.flock(held, fcntl.LOCK_EX)
        if seconds is not None:
            def later():
                time.sleep(seconds)
                held.close()
            threading.Thread(target=later, daemon=True).start()
        return held

    def test_takeover_waits_for_a_running_step_of_the_old_generation(self):
        lock.acquire(self.root, "a-1", "h", 1, 300, self.root, 5)
        self.assertFalse((self.root / "usnm-boinc-handoff.lock").exists(), "a first lock has nothing to drain")
        self.hold_step_lock(self.root / "usnm-boinc-handoff.lock", seconds=1)
        started = time.monotonic()
        with patch.object(lock, "now", return_value=lock.now() + timedelta(seconds=301)):
            record = lock.acquire(self.root, "b-2", "h", 2, 300, self.root, 10)
        self.assertGreaterEqual(time.monotonic() - started, 0.8)
        self.assertEqual(record["generation"], 2)
        self.assertEqual(lock.read(self.root)["holder"], "b-2")

    def test_takeover_that_cannot_drain_fails_and_frees_the_new_generation(self):
        lock.acquire(self.root, "a-1", "h", 1, 300)
        held = self.hold_step_lock(self.root / "usnm-boinc-step.lock")
        with patch.object(lock, "now", return_value=lock.now() + timedelta(seconds=301)):
            with self.assertRaisesRegex(lock.LockError, "earlier step still holds the step lock"):
                lock.acquire(self.root, "b-2", "h", 2, 300, self.root, 0)
        record = lock.read(self.root)
        self.assertEqual((record["holder"], record["generation"]), ("b-2", 2))
        self.assertEqual(record["expires_at"], record["acquired_at"], "released for the next tick")
        # The old holder stays fenced out, and once its step ends the next acquire goes through.
        with self.assertRaises(lock.LockError):
            lock.fence(self.root, "a-1", 1)
        # The next try drains again: the old step still runs, so it may not skip the wait.
        with patch.object(lock, "now", return_value=lock.now() + timedelta(seconds=302)):
            with self.assertRaisesRegex(lock.LockError, "earlier step still holds"):
                lock.acquire(self.root, "c-3", "h", 3, 300, self.root, 0)
        held.close()
        with patch.object(lock, "now", return_value=lock.now() + timedelta(seconds=303)):
            self.assertEqual(lock.acquire(self.root, "d-4", "h", 4, 300, self.root, 0)["generation"], 4)

    def test_a_released_lock_has_nothing_to_drain(self):
        lock.acquire(self.root, "a-1", "h", 1)
        lock.release(self.root, "a-1", 1)
        held = self.hold_step_lock(self.root / "usnm-boinc-handoff.lock")
        self.assertEqual(lock.acquire(self.root, "b-2", "h", 2, 300, self.root, 0)["generation"], 2)
        held.close()

    def test_plans_have_records_of_their_own_and_take_over_without_holding_each_other(self):
        lock.acquire(self.root, "a-1", "h", 1, 300, plan="plan-a")
        self.assertEqual(lock.acquire(self.root, "b-1", "h", 2, 300, plan="plan-b")["generation"], 1)
        self.assertEqual(lock.acquire(self.root, "c-1", "h", 3)["generation"], 1)
        self.assertTrue((self.root / "scheduler/plans/plan-a/controller.lock.json").is_file())
        with self.assertRaisesRegex(lock.LockError, "held: holder a-1"):
            lock.acquire(self.root, "a-2", "h", 4, 300, plan="plan-a")
        lock.fence(self.root, "a-1", 1, plan="plan-a")
        with self.assertRaises(lock.LockError):
            lock.fence(self.root, "a-1", 1)
        # Plan b's step holds its own lock: plan a's takeover does not wait for it, plan b's does.
        step_b = self.hold_step_lock(self.root / "usnm-boinc-handoff.plan-plan-b.lock")
        shared = open(self.root / "usnm-boinc-handoff.lock", "a")
        fcntl.flock(shared, fcntl.LOCK_SH)
        with patch.object(lock, "now", return_value=lock.now() + timedelta(seconds=301)):
            self.assertEqual(lock.acquire(self.root, "a-3", "h", 5, 300, self.root, 0, "plan-a")["generation"], 2)
            with self.assertRaisesRegex(lock.LockError, "earlier step"):
                lock.acquire(self.root, "b-3", "h", 6, 300, self.root, 0, "plan-b")
        step_b.close()
        shared.close()

    def test_scoped_takeover_waits_for_a_whole_hand_off_lock_holder_and_a_submit(self):
        lock.acquire(self.root, "a-1", "h", 1, 300, plan="plan-a")
        for name in ("usnm-boinc-handoff.lock", "usnm-boinc-step.lock"):
            held = self.hold_step_lock(self.root / name)
            with patch.object(lock, "now", return_value=lock.now() + timedelta(seconds=301)):
                with self.assertRaisesRegex(lock.LockError, "earlier step", msg=name):
                    lock.acquire(self.root, "n", "h", 2, 300, self.root, 0, "plan-a")
            held.close()
            # The failed takeover let its own generation go, still needing a drain.
            self.assertNotIn("released", lock.read(self.root, "plan-a"))

    def test_bad_plan_is_refused(self):
        done = lock.main(["--root", str(self.root), "--plan", "../x", "status"])
        self.assertEqual(done, 1)

    def test_acquire_without_a_drain_lock_is_unchanged(self):
        lock.acquire(self.root, "a-1", "h", 1)
        lock.release(self.root, "a-1", 1)
        self.assertEqual(lock.acquire(self.root, "b-2", "h", 2)["generation"], 2)

    def test_renew_extends_and_fences_the_old_holder(self):
        first = lock.acquire(self.root, "a-1", "h", 1, 60)
        later = lock.now() + timedelta(seconds=50)
        with patch.object(lock, "now", return_value=later):
            renewed = lock.renew(self.root, "a-1", first["generation"], 300)
        self.assertGreater(renewed["expires_at"], first["expires_at"])
        with patch.object(lock, "now", return_value=later + timedelta(seconds=301)):
            lock.acquire(self.root, "b-2", "h", 2)
        with self.assertRaisesRegex(lock.LockError, "moved on"):
            lock.renew(self.root, "a-1", 1)
        with self.assertRaisesRegex(lock.LockError, "moved on"):
            lock.fence(self.root, "a-1", 1)
        lock.fence(self.root, "b-2", 2)

    def test_expired_record_fails_renew_and_fence_of_its_own_holder(self):
        lock.acquire(self.root, "a-1", "h", 1, 60)
        with patch.object(lock, "now", return_value=lock.now() + timedelta(seconds=61)):
            with self.assertRaisesRegex(lock.LockError, "expired"):
                lock.renew(self.root, "a-1", 1)
            with self.assertRaisesRegex(lock.LockError, "expired .* min ago|expired 0 min ago"):
                lock.fence(self.root, "a-1", 1)

    def test_release_frees_the_lock_and_keeps_counting_generations(self):
        lock.acquire(self.root, "a-1", "h", 1)
        self.assertIsNone(lock.release(self.root, "other", 1))
        lock.release(self.root, "a-1", 1)
        self.assertEqual(lock.acquire(self.root, "b-2", "h", 2)["generation"], 2)

    def test_status_cli_prints_holder_and_expiry(self):
        lock.acquire(self.root, "a-1", "ct211", 1)
        with patch("sys.stdout") as out:
            self.assertEqual(lock.main(["--root", str(self.root), "status"]), 0)
        printed = "".join(call.args[0] for call in out.write.call_args_list)
        self.assertIn("holder a-1 on host ct211", printed)
        self.assertIn("expires_at", printed)

    def test_bad_inputs_are_refused(self):
        self.assertEqual(lock.main(["--root", str(self.root), "acquire", "--holder", "bad holder",
                                    "--host", "h", "--pid", "1"]), 1)
        self.assertEqual(lock.main(["--root", str(self.root), "acquire", "--holder", "a",
                                    "--host", "h", "--pid", "1", "--ttl", "0"]), 1)


class StepTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name).resolve()
        self.state = self.root / "state"
        self.state.mkdir(mode=0o700)
        (self.root / "server.env").write_text("SERVER_IMAGE=localhost/x:abc\n")
        (self.root / "bridge.env").write_text("KEY_VAULT_NAME=kv\n")

    def tearDown(self):
        self.tmp.cleanup()

    def step(self, step, *variables, what_if=False, target="handoff"):
        env = {"PATH": os.environ["PATH"], "USNM_BOINC_CONF": str(self.root),
               "USNM_BOINC_STEP_RUN": str(self.root), "USNM_BOINC_HANDOFF_ROOT": str(self.state)}
        argv = ["bash", str(SCRIPT), "--env", "prod", "--target", target, "--step", step,
                "--wait-mins", "1"]
        for variable in variables:
            argv += ["--var", variable]
        if what_if:
            argv.append("--what-if")
        return subprocess.run(argv, env=env, capture_output=True, text=True, timeout=30)

    def test_lock_steps_acquire_status_renew_release(self):
        done = self.step("lock-acquire", "LOCK_HOLDER=a-1", "LOCK_HOST=ct211", "LOCK_PID=5", "LOCK_TTL=300")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(json.loads(done.stdout)["generation"], 1)
        busy = self.step("lock-acquire", "LOCK_HOLDER=b-2", "LOCK_HOST=ct212", "LOCK_PID=6")
        self.assertNotEqual(busy.returncode, 0)
        self.assertIn("held: holder a-1 on host ct211", busy.stderr)
        status = json.loads(self.step("lock-status").stdout)
        self.assertEqual(status["lock"]["holder"], "a-1")
        self.assertFalse(status["expired"])
        self.assertEqual(self.step("lock-renew", "LOCK_HOLDER=a-1", "LOCK_GENERATION=1").returncode, 0)
        self.assertNotEqual(self.step("lock-renew", "LOCK_HOLDER=a-1", "LOCK_GENERATION=9").returncode, 0)
        self.assertEqual(self.step("lock-release", "LOCK_HOLDER=a-1", "LOCK_GENERATION=1").returncode, 0)
        self.assertTrue(json.loads(self.step("lock-status").stdout)["expired"])

    def test_lock_acquire_step_drains_the_step_lock_of_an_old_holder(self):
        first = self.step("lock-acquire", "LOCK_HOLDER=a-1", "LOCK_HOST=h", "LOCK_PID=5")
        self.assertEqual(first.returncode, 0, first.stderr)
        # a-1 is gone without a release: its record lapsed.
        record = lock.read(self.state)
        record["expires_at"] = "2020-01-01T00:00:00Z"
        record.pop("released", None)
        lock.write(self.state, record)
        with open(self.root / "usnm-boinc-handoff.lock", "a") as held:
            fcntl.flock(held, fcntl.LOCK_EX)
            threading.Timer(2, lambda: fcntl.flock(held, fcntl.LOCK_UN)).start()
            started = time.monotonic()
            second = self.step("lock-acquire", "LOCK_HOLDER=b-2", "LOCK_HOST=h", "LOCK_PID=6")
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(json.loads(second.stdout)["generation"], 2)
        self.assertGreaterEqual(time.monotonic() - started, 1.5)

    def test_clean_release_is_marked_and_an_unclean_one_is_not(self):
        self.step("lock-acquire", "LOCK_HOLDER=a-1", "LOCK_HOST=h", "LOCK_PID=5")
        self.assertEqual(self.step("lock-release", "LOCK_HOLDER=a-1", "LOCK_GENERATION=1").returncode, 0)
        self.assertNotIn("released", lock.read(self.state))
        self.step("lock-acquire", "LOCK_HOLDER=a-2", "LOCK_HOST=h", "LOCK_PID=5")
        done = self.step("lock-release", "LOCK_HOLDER=a-2", "LOCK_GENERATION=2", "LOCK_CLEAN=1")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertTrue(lock.read(self.state)["released"])
        self.assertIn("LOCK_CLEAN is for", self.step("lock-status", "LOCK_CLEAN=1").stderr)

    def test_lock_plan_selects_the_plans_own_record(self):
        done = self.step("lock-acquire", "LOCK_PLAN=plan-a", "LOCK_HOLDER=a-1", "LOCK_HOST=h", "LOCK_PID=5")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertTrue((self.state / "scheduler/plans/plan-a/controller.lock.json").is_file())
        self.assertFalse((self.state / "scheduler/controller.lock.json").exists())
        other = self.step("lock-acquire", "LOCK_PLAN=plan-b", "LOCK_HOLDER=b-1", "LOCK_HOST=h", "LOCK_PID=6")
        self.assertEqual(other.returncode, 0, other.stderr)
        status = json.loads(self.step("lock-status", "LOCK_PLAN=plan-a").stdout)
        self.assertEqual(status["lock"]["holder"], "a-1")
        ok = self.step("claim", "LOCK_PLAN=plan-a", "LOCK_HOLDER=a-1", "LOCK_GENERATION=1", "CLAIM_MAX=1")
        self.assertNotIn("not this controller's", ok.stderr)
        refused = self.step("claim", "LOCK_PLAN=plan-b", "LOCK_HOLDER=a-1", "LOCK_GENERATION=1", "CLAIM_MAX=1")
        self.assertIn("not this controller's", refused.stderr)
        # Without the plan the unscoped record is consulted, as before.
        unscoped = self.step("claim", "LOCK_HOLDER=a-1", "LOCK_GENERATION=1", "CLAIM_MAX=1")
        self.assertIn("not this controller's", unscoped.stderr)
        bad = self.step("lock-status", "LOCK_PLAN=../x")
        self.assertIn("LOCK_PLAN is a plan name", bad.stderr)

    def test_bridge_writers_are_fenced_too(self):
        lock.acquire(self.state, "a-1", "h", 1)
        for name in ("spool-out", "submit-remote", "batch-inventory"):
            refused = self.step(name, "LOCK_HOLDER=zz", "LOCK_GENERATION=1", target="bridge")
            self.assertIn("not this controller's", refused.stderr, name)
            ok = self.step(name, "LOCK_HOLDER=a-1", "LOCK_GENERATION=1", target="bridge")
            self.assertNotIn("not this controller's", ok.stderr, name)
        other = self.step("register-apps", "LOCK_HOLDER=a-1", "LOCK_GENERATION=1", target="bridge")
        self.assertIn("is not fenced", other.stderr)

    def test_lock_steps_take_no_other_variable(self):
        done = self.step("lock-status", "JOB_ID=" + "a" * 64)
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("takes only LOCK_", done.stderr)

    def test_fenced_step_is_refused_for_a_stale_holder_and_unfenced_steps_are_unchanged(self):
        lock.acquire(self.state, "a-1", "h", 1)
        refused = self.step("claim", "LOCK_HOLDER=zz", "LOCK_GENERATION=1", "CLAIM_MAX=1")
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("not this controller's", refused.stderr)
        half = self.step("claim", "LOCK_HOLDER=a-1")
        self.assertIn("go together", half.stderr)
        # The current holder passes the fence and reaches the settings check; no handoff.env here.
        ok = self.step("claim", "LOCK_HOLDER=a-1", "LOCK_GENERATION=1", "CLAIM_MAX=1")
        self.assertIn("handoff.env", ok.stderr)
        self.assertNotIn("not this controller's", ok.stderr)
        # Without the fence variables the lock is not consulted at all.
        plain = self.step("claim", "CLAIM_MAX=1")
        self.assertIn("handoff.env", plain.stderr)
        self.assertNotIn("not this controller's", plain.stderr)

    def test_fence_is_checked_again_after_the_step_lock_wait(self):
        url = "https://example.blob.core.windows.net/boinc-handoff"
        (self.root / "handoff.env").write_text("".join(f"{k}={v}\n" for k, v in {
            "HANDOFF_URL": url, "COSMOS_ENDPOINT": "https://c.documents.azure.com:443/",
            "CURATED_URL": "https://l.blob.core.windows.net/curated",
            "REFERENCE_URL": "https://l.blob.core.windows.net/reference",
            "OUT_URL": "https://e.blob.core.windows.net/ocr-out", "QUEUE_URL": "https://e.queue.core.windows.net",
            "IDENTITY_ENDPOINT": "http://localhost:1/t", "IMDS_ENDPOINT": "http://localhost:1"}.items()))
        binaries = self.root / "bin"
        binaries.mkdir()
        record = self.state / "scheduler/controller.lock.json"
        steal = binaries / "steal"
        (binaries / "podman").write_text("#!/bin/sh\nexit 0\n")
        (binaries / "flock").write_text(
            f'#!/bin/sh\n[ -e {steal} ] && python3 -c "import json;p=\'{record}\';r=json.load(open(p));'
            f'r[\'holder\']=\'b-2\';r[\'generation\']=2;json.dump(r,open(p,\'w\'))"\nexit 0\n')
        for name in ("podman", "flock"):
            (binaries / name).chmod(0o755)
        lock.acquire(self.state, "a-1", "h", 1)
        env = {"PATH": f"{binaries}:{os.environ['PATH']}", "USNM_BOINC_CONF": str(self.root),
               "USNM_BOINC_STEP_RUN": str(self.root), "USNM_BOINC_HANDOFF_ROOT": str(self.state)}
        argv = ["bash", str(SCRIPT), "--env", "prod", "--target", "handoff", "--step", "claim", "--wait-mins", "1",
                "--var", f"EXPECTED_HANDOFF_URL={url}", "--var", "LOCK_HOLDER=a-1", "--var", "LOCK_GENERATION=1"]
        done = subprocess.run(argv, env=env, capture_output=True, text=True, timeout=30)
        self.assertEqual(done.returncode, 0, done.stderr)
        steal.write_text("")
        done = subprocess.run(argv, env=env, capture_output=True, text=True, timeout=30)
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("expired while waiting", done.stderr)

    def test_deadline_retry_is_not_fenced(self):
        done = self.step("deadline-retry", "LOCK_HOLDER=a", "LOCK_GENERATION=1")
        self.assertIn("is not fenced", done.stderr)


class ControllerTests(unittest.TestCase):
    def transport(self, azure=False, **kwargs):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        config = {"env": "prod", "account": "exchange123", "plan": "queue-a",
                  "step_command": ["ssh", "root@boinc.example", "--"]}
        if kwargs.get("local_lock") and not azure:
            root = Path(tmp.name).resolve() / "root"
            root.mkdir(mode=0o700)
            config["handoff_root"] = str(root)
        return batch.Transport(config, Path(tmp.name), **kwargs)

    def test_off_by_default_and_needs_step_command(self):
        self.assertFalse(self.transport().local_lock)
        with self.assertRaisesRegex(ValueError, "step_command"):
            batch.Transport({"env": "prod", "account": "a"}, Path("."), local_lock=True)

    def test_incomplete_on_prem_routing_is_refused(self):
        with self.assertRaisesRegex(ValueError, "every hand-off step"):
            self.transport(local_lock=True, onprem_handoff=("claim",))

    def test_azure_backed_plan_is_refused(self):
        with self.assertRaisesRegex(ValueError, "not one backed by Azure"):
            self.transport(azure=True, local_lock=True, onprem_handoff=("claim", "collect", "export"))

    def test_held_pages_are_not_published_under_the_local_lock(self):
        transport = self.transport(local_lock=True, onprem_handoff=("claim", "collect", "export"))
        with self.assertRaisesRegex(RuntimeError, "not fenced"):
            transport.publish_held({"doc": ["a" * 64]})

    def test_bridge_writers_are_fenced_and_dispatch_stops_at_the_deadline(self):
        transport = self.transport(local_lock=True, onprem_handoff=("claim", "collect", "export"))
        calls = []

        def run(argv, label, **kwargs):
            calls.append(argv)
            return json.dumps({"generation": 7}) if label == "handoff/lock-acquire" else ""

        with patch.object(transport, "run", side_effect=run), \
                patch.object(batch, "LOCK_HEARTBEAT_SECS", 3600):
            with transport.lock():
                for name in ("spool-out", "submit-remote", "batch-inventory"):
                    transport.step("bridge", name, JOB_ID="1" * 64)
                    self.assertIn("LOCK_GENERATION=7", calls[-1])
                transport.step("bridge", "register-apps")
                self.assertFalse(any(v.startswith("LOCK_") for v in calls[-1]))
                transport.lock_deadline = time.monotonic() - 1
                before = len(calls)
                with self.assertRaisesRegex(RuntimeError, "controller lock lost"):
                    transport.step("bridge", "submit-remote", JOB_ID="1" * 64)
                self.assertEqual(len(calls), before)

    def test_lock_steps_run_on_the_host_and_mutating_steps_are_fenced(self):
        transport = self.transport(local_lock=True, onprem_handoff=("claim", "collect", "export"))
        calls = []

        def run(argv, label, **kwargs):
            calls.append(argv)
            return json.dumps({"generation": 7}) if label == "handoff/lock-acquire" else ""

        with patch.object(transport, "run", side_effect=run), \
                patch.object(batch, "LOCK_HEARTBEAT_SECS", 3600):
            with transport.lock():
                transport.step("handoff", "collect", JOB_ID="1" * 64)
                transport.step("handoff", "export", HANDOFF_BATCH="x")
        steps = [argv[argv.index("--step") + 1] for argv in calls]
        self.assertEqual(steps, ["lock-acquire", "collect", "export", "lock-release"])
        self.assertTrue(all(argv[:3] == ["ssh", "root@boinc.example", "--"] for argv in calls))
        holder = next(v for v in calls[0] if v.startswith("LOCK_HOLDER=")).split("=", 1)[1]
        for fenced in calls[1:3]:
            self.assertIn(f"LOCK_HOLDER={holder}", fenced)
            self.assertIn("LOCK_GENERATION=7", fenced)
        self.assertNotIn("EXPECTED_HANDOFF_URL", " ".join(calls[0]))
        self.assertTrue(any(v.startswith("EXPECTED_HANDOFF_URL=") for v in calls[1]))
        self.assertIn("LOCK_GENERATION=7", calls[3])
        self.assertIsNone(transport.lock_fence)

    def test_release_says_clean_only_when_every_fenced_step_ended(self):
        transport = self.transport(local_lock=True, onprem_handoff=("claim", "collect", "export"))
        calls = []

        def run(argv, label, **kwargs):
            calls.append(argv)
            if label == "handoff/collect" and fail[0]:
                if fail[0] is KeyboardInterrupt:
                    raise KeyboardInterrupt
                raise RuntimeError("handoff/collect failed; inspect")
            return json.dumps({"generation": 7}) if label == "handoff/lock-acquire" else ""

        for fail_it in (False, True, KeyboardInterrupt):
            fail = [fail_it]
            calls.clear()
            with patch.object(transport, "run", side_effect=run), patch.object(batch, "LOCK_HEARTBEAT_SECS", 3600):
                try:
                    with transport.lock():
                        transport.step("handoff", "collect", JOB_ID="1" * 64)
                except (RuntimeError, KeyboardInterrupt):
                    self.assertTrue(fail_it)
            self.assertEqual("LOCK_CLEAN=1" in calls[-1], not fail_it, calls[-1])
        # A step that never started (the host's step lock stayed held) leaves nothing running.
        calls.clear()
        fail = [False]

        def busy(argv, label, **kwargs):
            calls.append(argv)
            if label == "handoff/collect":
                raise batch.StepLockTimeout("nothing ran")
            return json.dumps({"generation": 7}) if label == "handoff/lock-acquire" else ""

        with patch.object(transport, "run", side_effect=busy), patch.object(batch, "LOCK_HEARTBEAT_SECS", 3600):
            with self.assertRaises(batch.StepLockTimeout):
                with transport.lock():
                    transport.step("handoff", "collect", JOB_ID="1" * 64)
        self.assertIn("LOCK_CLEAN=1", calls[-1])

    def test_unfenced_without_the_flag(self):
        transport = self.transport(onprem_handoff=("claim", "collect", "export"))
        with patch.object(transport, "run") as run:
            transport.step("handoff", "collect", JOB_ID="1" * 64)
        self.assertFalse(any(v.startswith("LOCK_") for v in run.call_args.args[0]))

    def test_held_lock_fails_the_tick(self):
        transport = self.transport(local_lock=True, onprem_handoff=("claim", "collect", "export"))
        with patch.object(transport, "run", side_effect=RuntimeError("held")):
            with self.assertRaises(RuntimeError):
                with transport.lock():
                    self.fail("entered a held lock")

    def test_lost_lock_stops_the_next_step_and_release_failure_is_not_fatal(self):
        transport = self.transport(local_lock=True, onprem_handoff=("claim", "collect", "export"))

        def run(argv, label, **kwargs):
            if label.endswith("lock-acquire"):
                return json.dumps({"generation": 3})
            if label.endswith("lock-release"):
                raise RuntimeError("ssh down")
            return ""

        moved = subprocess.CompletedProcess([], 1, "", "error: lock moved on: holder x")
        with patch.object(transport, "run", side_effect=run), \
                patch.object(batch, "LOCK_HEARTBEAT_SECS", 0.01), \
                patch.object(batch.subprocess, "run", return_value=moved):
            with transport.lock():
                for _ in range(200):
                    if transport.lock_lost:
                        break
                    threading.Event().wait(0.01)
                with self.assertRaisesRegex(RuntimeError, "controller lock lost"):
                    transport.step("handoff", "collect", JOB_ID="1" * 64)


SHARED_STEP = """
import fcntl, sys, time
d, hold = sys.argv[1], float(sys.argv[2])
gate = open(d + "/usnm-boinc-handoff.lock.gate", "a")
fcntl.flock(gate, fcntl.LOCK_EX)
lk = open(d + "/usnm-boinc-handoff.lock", "a")
fcntl.flock(lk, fcntl.LOCK_SH)
fcntl.flock(gate, fcntl.LOCK_UN)
print("held", flush=True)
time.sleep(hold)
"""


class WriterPreferenceTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def shared(self, hold):
        proc = subprocess.Popen(["python3", "-c", SHARED_STEP, str(self.dir), str(hold)],
                                stdout=subprocess.PIPE, text=True)
        self.assertEqual(proc.stdout.readline().strip(), "held")
        return proc

    def test_exclusive_drain_is_not_starved_by_a_stream_of_shared_steps(self):
        stop = threading.Event()
        procs = []

        def stream():
            # Each shared step overlaps the next, so the hand-off lock is never free.
            while not stop.is_set():
                procs.append(self.shared(0.6))
                stop.wait(0.2)

        first = self.shared(0.6)
        procs.append(first)
        thread = threading.Thread(target=stream)
        thread.start()
        try:
            started = time.monotonic()
            lock.drain(self.dir, 20)
            self.assertLess(time.monotonic() - started, 10)
        finally:
            stop.set()
            thread.join()
            for proc in procs:
                proc.wait()

    def test_drain_acquires_before_deadline_with_continuously_overlapping_holders(self):
        # Several shared holders each hold the hand-off lock for 1 s and arrive every 0.15 s,
        # so at least one always holds it; the drain's gate keeps newcomers out.
        stop = threading.Event()
        procs = []

        def stream():
            while not stop.is_set():
                procs.append(self.shared(1.0))
                stop.wait(0.15)

        procs.append(self.shared(1.0))
        thread = threading.Thread(target=stream)
        thread.start()
        try:
            time.sleep(0.5)
            started = time.monotonic()
            lock.drain(self.dir, 8)
            self.assertLess(time.monotonic() - started, 4)
        finally:
            stop.set()
            thread.join()
            for proc in procs:
                proc.wait()

    def test_shared_drain_lets_the_gate_go_before_recording_its_holder(self):
        seen = []
        real = lock.note_holder

        def spy(*args, **kwargs):
            gate = open(self.dir / lock.GATE, "a")
            try:
                fcntl.flock(gate, fcntl.LOCK_EX | fcntl.LOCK_NB)
                seen.append(True)
            except BlockingIOError:
                seen.append(False)
            finally:
                gate.close()
            return real(*args, **kwargs)

        with patch.object(lock, "note_holder", spy):
            lock.drain(self.dir, 2, plan="p-1")
        self.assertTrue(seen and all(seen))

    def test_shared_step_waits_while_an_exclusive_drain_waits(self):
        holder = self.shared(1.5)
        result = []
        thread = threading.Thread(target=lambda: result.append(lock.drain(self.dir, 20)))
        thread.start()
        time.sleep(0.4)
        started = time.monotonic()
        late = self.shared(0.1)  # blocks on the gate until the drain is through
        waited = time.monotonic() - started
        holder.wait()
        thread.join()
        late.wait()
        self.assertGreater(waited, 0.5, "a new shared step must queue behind the waiting drain")

    def test_timeout_names_the_holder_and_holders_are_cleaned_up(self):
        held = open(self.dir / lock.HANDOFF_LOCK, "a")
        fcntl.flock(held, fcntl.LOCK_EX)
        path = lock.note_holder(self.dir, lock.HANDOFF_LOCK, "handoff:export", plan="p-1")
        with self.assertRaises(lock.LockError) as caught:
            lock.drain(self.dir, 0.3)
        self.assertIn(f"pid {os.getpid()} step handoff:export plan p-1", str(caught.exception))
        self.assertIn(lock.HANDOFF_LOCK, str(caught.exception))
        # The gate is let go after the failed wait.
        gate = open(self.dir / lock.GATE, "a")
        fcntl.flock(gate, fcntl.LOCK_EX | fcntl.LOCK_NB)
        gate.close()
        lock.clear_holder(path)
        fcntl.flock(held, fcntl.LOCK_UN)
        lock.drain(self.dir, 1)
        self.assertEqual(list(self.dir.glob("*.holder*")), [])

    def test_unknown_holder_is_said_so(self):
        self.assertIn("holder unknown", lock.holder_text(self.dir, lock.STEP_LOCK))


if __name__ == "__main__":
    unittest.main()
