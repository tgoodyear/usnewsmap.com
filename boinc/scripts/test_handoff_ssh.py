"""Tests for handoff_ssh.py (#1367): the hand-off root on the BOINC host, reached through its step
runner. A scratch root and `Host`, which stands in for onprem-step.sh over ssh: it runs the real
handoff_archive.py and handoff_lock.py on the root and checks the controller's fence as the
script does."""

import contextlib
import importlib.util
import io
import json
import re
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(f"{name}.py"))
    module = importlib.util.module_from_spec(spec)
    sys.modules.setdefault(name, module)
    spec.loader.exec_module(module)
    return module


S = load("handoff_ssh")
A = load("handoff_archive")
L = load("handoff_lock")
JOB = "c" * 64
COMMAND = ["ssh", "-i", "/root/.ssh/usnm-boinc-step", "root@usnm-boinc.tjg.fyi", "--"]
FENCED = {"archive-delete", "archive-write"}


class Host:
    """The BOINC host's step runner over a hand-off root, as the ssh forced command runs it."""

    def __init__(self, root):
        self.root = Path(root)
        (self.root / "scheduler").mkdir(parents=True, exist_ok=True)
        self.calls = []
        self.lock_waits = []
        self.fail = None  # a callable (step, variables) -> CompletedProcess | None
        self.lock = threading.Lock()
        # contextlib's redirects swap the process's streams: one at a time.
        self.streams = threading.Lock()

    def __call__(self, argv, stdin, timeout):
        assert argv[:len(COMMAND)] == COMMAND, argv
        rest = argv[len(COMMAND):]
        assert rest[0] == "--env" and rest[2:5] == ["--target", "handoff", "--step"], rest
        step, variables = rest[5], {}
        assert rest[6] == "--wait-mins"
        if "--lock-wait-secs" in rest:
            self.lock_waits.append((step, int(rest[rest.index("--lock-wait-secs") + 1])))
            rest = rest[:rest.index("--lock-wait-secs")] + rest[rest.index("--lock-wait-secs") + 2:]
        pairs = [rest[i + 1] for i in range(8, len(rest), 2) if rest[i] == "--var"]
        for pair in pairs:
            key, _, value = pair.partition("=")
            variables[key] = value
        with self.lock:
            self.calls.append((step, variables, stdin))
        if self.fail is not None:
            failed = self.fail(step, variables)
            if failed is not None:
                return failed
        plan = ["--plan", variables["LOCK_PLAN"]] if "LOCK_PLAN" in variables else []
        if step.startswith("lock-"):
            args = [step[5:]]
            for var, flag in (("LOCK_HOLDER", "--holder"), ("LOCK_GENERATION", "--generation"), ("LOCK_HOST", "--host"),
                              ("LOCK_PID", "--pid"), ("LOCK_TTL", "--ttl")):
                if var in variables:
                    args += [flag, variables[var]]
            if "LOCK_CLEAN" in variables:
                args.append("--clean")
            return self.run(L.main, ["--root", str(self.root), *plan, *args], b"")
        if step in FENCED and "LOCK_HOLDER" in variables:
            code = self.run(L.main, ["--root", str(self.root), *plan, "fence", "--holder", variables["LOCK_HOLDER"],
                                     "--generation", variables["LOCK_GENERATION"]], b"")
            if code.returncode:
                return subprocess.CompletedProcess(argv, 1, b"", b"error: the controller lock is not this controller's; not run\n")
        tool = ["--root", str(self.root), step[8:]]
        if step == "archive-list":
            tool += ["--prefix", variables["ARCHIVE_PREFIX"]]
            if "ARCHIVE_SUFFIX" in variables:
                tool += ["--suffix", variables["ARCHIVE_SUFFIX"]]
            if "ARCHIVE_MAX_AGE_SECS" in variables:
                tool += ["--max-age-secs", variables["ARCHIVE_MAX_AGE_SECS"]]
        elif step in ("archive-read", "archive-write"):
            tool += ["--object", variables["ARCHIVE_OBJECT"]]
            if "ARCHIVE_MD5" in variables:
                tool += ["--md5", variables["ARCHIVE_MD5"]]
        out, err = io.BytesIO(), io.StringIO()
        with self.streams, contextlib.redirect_stderr(err):
            code = A.main(tool, stdin=io.BytesIO(stdin or b""), stdout=out)
        return subprocess.CompletedProcess(argv, code, out.getvalue(), err.getvalue().encode())

    def run(self, main, argv, stdin):
        out, err = io.StringIO(), io.StringIO()
        with self.streams, contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = main(argv)
        return subprocess.CompletedProcess([], code, out.getvalue().encode(), err.getvalue().encode())


def config(**extra):
    return {"env": "prod", "plan": "native-feed-7", "step_command": COMMAND, **extra}


class SshHandoffTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name).resolve() / "root"
        self.root.mkdir(mode=0o700)
        self.host = Host(self.root)
        self.name = f"spool/{JOB}/host_id"
        (self.root / f"spool/{JOB}").mkdir(parents=True)
        (self.root / self.name).write_bytes(b"13\n")

    def tearDown(self):
        self.tmp.cleanup()

    def handoff(self, **kwargs):
        return S.SshHandoff(config(), runner=self.host, **kwargs)

    def test_list_and_get_give_what_the_root_holds(self):
        box = self.handoff()
        entries = box.list("spool/c")
        self.assertEqual([entry["name"] for entry in entries], [self.name])
        data, etag = box.get(self.name)
        self.assertEqual((data, etag), (b"13\n", entries[0]["etag"]))
        self.assertEqual(self.host.calls[0][:2], ("archive-list", {"ARCHIVE_PREFIX": "spool/c"}))

    def test_list_passes_its_filters(self):
        self.handoff().list("spool/", suffix="host_id", max_age_secs=86400)
        self.assertEqual(self.host.calls[0][1], {"ARCHIVE_PREFIX": "spool/", "ARCHIVE_SUFFIX": "host_id",
                                                 "ARCHIVE_MAX_AGE_SECS": "86400"})

    def test_a_damaged_read_is_refused(self):
        box = self.handoff()
        real = self.host.__call__

        def damaged(argv, stdin, timeout):
            done = real(argv, stdin, timeout)
            return subprocess.CompletedProcess(argv, 0, done.stdout[:-1], b"")

        box.runner = damaged
        with self.assertRaisesRegex(S.HandoffRefused, "came short or damaged"):
            box.get(self.name)

    def test_changes_need_the_lock_and_carry_its_fence(self):
        box = self.handoff()
        entry = box.list(self.name)[0]
        with self.assertRaisesRegex(S.HandoffRefused, "no controller lock is held"):
            box.delete(self.name, entry["etag"])
        lease = box.acquire("scheduler/controller.lock")
        try:
            self.assertEqual(box.lease_state("x"), "leased")
            self.assertTrue(box.delete(self.name, entry["etag"]))
            fenced = [call for call in self.host.calls if call[0] == "archive-delete"][0]
            self.assertEqual(fenced[1]["LOCK_HOLDER"], lease)
            self.assertEqual(fenced[1]["LOCK_GENERATION"], "1")
            self.assertEqual(fenced[1]["EXPECTED_HANDOFF_URL"], "/handoff-state")
            self.assertNotIn("PLAN_SCOPE", fenced[1])
            self.assertEqual(json.loads(fenced[2]), {"items": [{"name": self.name, "etag": entry["etag"]}]})
        finally:
            box.release("scheduler/controller.lock", lease)
        self.assertFalse((self.root / self.name).exists())
        self.assertEqual(box.lease_state("x"), "expired")
        released = [call for call in self.host.calls if call[0] == "lock-release"][0]
        self.assertIn("LOCK_CLEAN", released[1])

    def test_changes_wait_for_the_hosts_lock_and_reads_and_lock_steps_do_not(self):
        box = self.handoff()
        entry = box.list(self.name)[0]
        lease = box.acquire("x")
        box.delete(self.name, entry["etag"])
        box.upload(f"dispatch/jobs/{JOB}/record.json", b"{}")
        box.release("x", lease)
        self.assertEqual(self.host.lock_waits, [("archive-delete", S.LOCK_WAIT_SECS), ("archive-write", S.LOCK_WAIT_SECS)])

    def test_a_lock_held_by_a_tick_refuses_the_archive(self):
        L.acquire(self.root, "a-tick", "ct211", 1, 300, plan="native-feed-7")
        box = S.SshHandoff(config(), plan_scope=True, runner=self.host)
        self.assertEqual(box.lease_state("x"), "leased")
        with self.assertRaisesRegex(S.HandoffRefused, "could not take the controller lock"):
            box.acquire("x")

    def test_plan_scope_gives_the_plan_its_own_record_and_scoped_steps(self):
        box = S.SshHandoff(config(), plan_scope=True, runner=self.host)
        entry = box.list(self.name)[0]
        lease = box.acquire("x")
        box.delete(self.name, entry["etag"])
        box.release("x", lease)
        delete = [call for call in self.host.calls if call[0] == "archive-delete"][0]
        self.assertEqual((delete[1]["PLAN_SCOPE"], delete[1]["LOCK_PLAN"]), ("native-feed-7", "native-feed-7"))
        self.assertTrue((self.root / "scheduler/plans/native-feed-7/controller.lock.json").exists())
        self.assertFalse((self.root / "scheduler/controller.lock.json").exists())

    def test_a_taken_over_lock_fences_the_change_out(self):
        box = self.handoff()
        entry = box.list(self.name)[0]
        lease = box.acquire("x")
        # The record moves on (an expired lease taken by another holder).
        record = json.loads((self.root / L.RECORD).read_text())
        record["expires_at"] = "2000-01-01T00:00:00Z"
        (self.root / L.RECORD).write_text(json.dumps(record))
        L.acquire(self.root, "another", "ct211", 2, 300)
        with self.assertRaisesRegex(S.HandoffRefused, "not this controller's"):
            box.delete(self.name, entry["etag"])
        self.assertTrue((self.root / self.name).exists())
        box.release("x", lease)
        # A release by the old holder leaves the new holder's record alone.
        self.assertEqual(json.loads((self.root / L.RECORD).read_text())["holder"], "another")

    def test_the_heartbeat_renews_and_notices_a_lost_lock(self):
        box = self.handoff(heartbeat_secs=0.05)
        lease = box.acquire("x")
        time.sleep(0.4)
        renewals = [call for call in self.host.calls if call[0] == "lock-renew"]
        self.assertGreaterEqual(len(renewals), 2)
        self.assertEqual(renewals[0][1]["LOCK_HOLDER"], lease)
        record = json.loads((self.root / L.RECORD).read_text())
        record["holder"] = "taken"
        (self.root / L.RECORD).write_text(json.dumps(record))
        time.sleep(0.4)
        self.assertIn("taken or expired", box.lost)
        with self.assertRaisesRegex(S.HandoffRefused, "lock was lost"):
            box.delete(self.name, "e")
        box.release("x", lease)
        released = [call for call in self.host.calls if call[0] == "lock-release"][0]
        self.assertNotIn("LOCK_CLEAN", released[1])

    def test_a_transport_failure_is_tried_again_for_reads_but_never_for_a_change(self):
        box = self.handoff()
        attempts = []

        def flaky(step, variables):
            attempts.append(step)
            if len(attempts) < 3:
                return subprocess.CompletedProcess([], 255, b"", b"ssh: connect to host: timed out\n")

        self.host.fail = flaky
        with mock_sleep():
            self.assertEqual(len(box.list(self.name)), 1)
        self.assertEqual(attempts, ["archive-list"] * 3)
        lease = box.acquire("x")
        attempts.clear()
        self.host.fail = lambda step, variables: subprocess.CompletedProcess([], 255, b"", b"ssh: no route\n") \
            if step == "archive-delete" else None
        with self.assertRaisesRegex(S.HandoffRefused, "no route"):
            box.delete(self.name, "e")
        self.assertEqual([c[0] for c in self.host.calls].count("archive-delete"), 1)
        # The failed change leaves the lock released unmarked, so the next acquire drains.
        box.release("x", lease)
        released = [call for call in self.host.calls if call[0] == "lock-release"][0]
        self.assertNotIn("LOCK_CLEAN", released[1])

    def test_a_refusal_by_the_host_is_not_retried(self):
        box = self.handoff()
        self.host.fail = lambda step, variables: subprocess.CompletedProcess([], 1, b"", b"error: nope\n")
        with self.assertRaisesRegex(S.HandoffRefused, "nope"):
            box.list("spool/")
        self.assertEqual(len(self.host.calls), 1)

    def test_upload_creates_only_where_none_is(self):
        box = self.handoff()
        lease = box.acquire("x")
        name = f"dispatch/jobs/{JOB}/record.json"
        self.assertTrue(box.upload(name, b"{}"))
        self.assertFalse(box.upload(name, b"{}"))
        box.release("x", lease)

    def test_a_config_without_a_step_command_is_refused(self):
        with self.assertRaisesRegex(S.HandoffRefused, "no step_command"):
            S.SshHandoff({"env": "prod", "plan": "x"}, runner=self.host)


@contextlib.contextmanager
def mock_sleep():
    original = time.sleep
    time.sleep = lambda seconds: None
    try:
        yield
    finally:
        time.sleep = original


class DetectTests(unittest.TestCase):
    def test_the_plans_flags_file_decides_and_side_by_side_follows(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            config_path = tmp / "boinc-priority-controller.native-feed-7.json"
            config_path.write_text("{}")
            switch = tmp / "side-by-side"
            self.assertEqual(S.detect(config_path, switch), (False, False))
            flags = tmp / "boinc-priority-controller.native-feed-7.flags"
            flags.write_text("@kind native\n--refill-cap 96\n# --ssh-reads is for later\n")
            self.assertEqual(S.detect(config_path, switch), (False, False))
            flags.write_text("@kind native\n--local-lock\n--ssh-reads\n")
            self.assertEqual(S.detect(config_path, switch), (True, False))
            switch.write_text("")
            self.assertEqual(S.detect(config_path, switch), (True, True))
            flags.write_text("--ssh-reads\n")
            self.assertEqual(S.detect(config_path, switch), (False, False))
            # controller-tick.sh splits each line on blanks: two flags on one line are two flags.
            flags.write_text("@kind native\n--local-lock --ssh-reads\n")
            self.assertEqual(S.detect(config_path, switch), (True, True))


if __name__ == "__main__":
    unittest.main()
