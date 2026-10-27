"""Hermetic transport checks; no project, Podman, SSH or database is contacted."""
import argparse
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time
import unittest
from unittest.mock import Mock, mock_open, patch
import uuid

SPEC = importlib.util.spec_from_file_location(
    "onprem_acceptance", Path(__file__).with_name("onprem_acceptance.py"))
adapter = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(adapter)


class OnpremAcceptance(unittest.TestCase):
    def setUp(self):
        self.args = argparse.Namespace(expected_image="sha256:" + "a" * 64,
                                       project="/project/usnmocr")
        self.request = json.dumps({"action": "status", "run_id": "test"})
        self.runner = Mock(side_effect=[
            subprocess.CompletedProcess([], 0, json.dumps([{
                "Id": "c" * 64, "Image": self.args.expected_image, "State": {"Running": True}}])),
            subprocess.CompletedProcess([], 0, '{"state":"waiting"}'),
        ])
        self.root = patch.object(adapter.os, "geteuid", return_value=0)
        self.root.start()
        self.addCleanup(self.root.stop)

    def test_existing_server_receives_exact_request_as_project_owner(self):
        self.assertEqual(adapter.execute(self.args, self.request, self.runner, lock_fd=3),
                         {"state": "waiting"})
        forwarded = self.runner.call_args.args[0]
        argv = json.loads(forwarded[forwarded.index("--command-json") + 1])
        self.assertEqual(argv[:13], ["podman", "exec", "--interactive", "--preserve-fds=1", "--user",
                                    "1000:1000", "c" * 64, "timeout", "--signal=TERM",
                                    "--kill-after=5s", "90s", "python3", "-c"])
        self.assertIn("def isolated_account(", argv[13])
        self.assertEqual(argv[14:], ["--project", "/project/usnmocr",
                                   "--fixtures", "/project/usnmocr/acceptance"])
        self.assertEqual(self.runner.call_args.kwargs["input"], self.request)
        self.assertEqual(self.runner.call_args.kwargs["timeout"], 105)
        self.assertEqual(self.runner.call_args.kwargs["pass_fds"], (3,))

    def test_changed_or_stopped_server_never_receives_request(self):
        for value in (None, {}, [None], [],
                      [{"Image": "sha256:" + "b" * 64, "State": {"Running": True}}],
                      [{"Image": self.args.expected_image, "State": None}],
                      [{"Id": 1, "Image": self.args.expected_image, "State": {"Running": True}}],
                      [{"Image": self.args.expected_image, "State": {"Running": False}}]):
            with self.subTest(value=value):
                runner = Mock(return_value=subprocess.CompletedProcess([], 0, json.dumps(value)))
                with self.assertRaises(ValueError):
                    adapter.execute(self.args, self.request, runner, lock_fd=3)
                self.assertEqual(runner.call_count, 1)

    def test_native_podman_image_ids_bind_the_same_full_hash(self):
        for expected in ("a" * 64, "sha256:" + "a" * 64):
            for inspected in ("a" * 64, "sha256:" + "a" * 64):
                with self.subTest(expected=expected, inspected=inspected):
                    self.args.expected_image = expected
                    runner = Mock(side_effect=[
                        subprocess.CompletedProcess([], 0, json.dumps([{
                            "Id": "c" * 64, "Image": inspected, "State": {"Running": True}}])),
                        subprocess.CompletedProcess([], 0, '{"state":"waiting"}'),
                    ])
                    self.assertEqual(adapter.execute(self.args, self.request, runner, lock_fd=3),
                                     {"state": "waiting"})

    def test_invalid_inputs_never_contact_podman(self):
        for field, value in (("expected_image", "localhost/server:latest"),
                             ("expected_image", "a" * 12),
                             ("expected_image", "sha256:" + "a" * 63),
                             ("project", "/project/../etc"),
                             ("project", "/etc/project")):
            with self.subTest(field=field):
                args = argparse.Namespace(**{**vars(self.args), field: value})
                runner = Mock()
                with self.assertRaises(ValueError):
                    adapter.execute(args, self.request, runner, lock_fd=3)
                runner.assert_not_called()
        for request in ("[]", "not-json", " " * 65537):
            runner = Mock()
            with self.assertRaises(ValueError):
                adapter.execute(self.args, request, runner, lock_fd=3)
            runner.assert_not_called()
        with patch.object(adapter.os, "geteuid", return_value=1000):
            runner = Mock()
            with self.assertRaises(ValueError):
                adapter.execute(self.args, self.request, runner, lock_fd=3)
            runner.assert_not_called()

    def test_transport_timeout_is_not_replayed(self):
        self.runner.side_effect = [
            subprocess.CompletedProcess([], 0, json.dumps([{
                "Id": "c" * 64, "Image": self.args.expected_image, "State": {"Running": True}}])),
            subprocess.TimeoutExpired("podman", 105),
        ]
        with self.assertRaises(subprocess.TimeoutExpired):
            adapter.execute(self.args, self.request, self.runner, lock_fd=3)
        self.assertEqual(self.runner.call_count, 2)

    def test_interrupted_transport_keeps_lock_until_backend_stops(self):
        directory = Path(__file__).resolve().parents[2] / "target" / ("acceptance-lock-" + uuid.uuid4().hex)
        directory.mkdir(parents=True)
        self.addCleanup(shutil.rmtree, directory)
        marker, path = directory / "backend-pid", directory / "bridge.lock"
        backend = ("import os, pathlib, time\n"
                   "if os.fork() == 0:\n"
                   "    os.setsid()\n"
                   f"    pathlib.Path({str(marker)!r}).write_text(str(os.getpid()))\n"
                   "    time.sleep(20)\n"
                   "    os._exit(0)\n"
                   "time.sleep(20)\n")
        process = None
        try:
            with path.open("a") as lock:
                adapter.fcntl.flock(lock, adapter.fcntl.LOCK_EX | adapter.fcntl.LOCK_NB)
                process = subprocess.Popen(
                    [sys.executable, "-c", adapter.LOCK_EXEC, "--lock-fd", str(lock.fileno()),
                     "--command-json", json.dumps([sys.executable, "-c", backend])],
                    pass_fds=(lock.fileno(),), start_new_session=True,
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                deadline = time.monotonic() + 5
                while not marker.exists() and time.monotonic() < deadline and process.poll() is None:
                    time.sleep(.02)
                self.assertTrue(marker.exists(), "fake container backend did not start")
            os.killpg(process.pid, signal.SIGTERM)
            process.communicate(timeout=5)
            with path.open("a") as probe:
                with self.assertRaises(BlockingIOError):
                    adapter.fcntl.flock(probe, adapter.fcntl.LOCK_EX | adapter.fcntl.LOCK_NB)
                os.kill(int(marker.read_text()), signal.SIGTERM)
                deadline = time.monotonic() + 5
                while True:
                    try:
                        adapter.fcntl.flock(probe, adapter.fcntl.LOCK_EX | adapter.fcntl.LOCK_NB)
                        break
                    except BlockingIOError:
                        if time.monotonic() >= deadline:
                            self.fail("project lock was not released after backend stopped")
                        time.sleep(.02)
        finally:
            if marker.exists():
                try:
                    os.kill(int(marker.read_text()), signal.SIGTERM)
                except ProcessLookupError:
                    pass
            if process is not None:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGTERM)
                process.communicate(timeout=5)

    def test_busy_bridge_lock_refuses_before_reading_or_forwarding_request(self):
        argv = ["onprem_acceptance.py", "--expected-image", self.args.expected_image]
        with patch.object(adapter.sys, "argv", argv), \
                patch("builtins.open", mock_open()), \
                patch.object(adapter.fcntl, "flock", side_effect=BlockingIOError), \
                patch.object(adapter, "execute") as execute, \
                patch.object(adapter.sys, "stdin") as stdin, \
                patch.object(adapter.sys, "stderr", new_callable=io.StringIO) as stderr:
            self.assertEqual(adapter.main(), 1)
        execute.assert_not_called()
        stdin.read.assert_not_called()
        self.assertIn("reconcile", stderr.getvalue())

    def test_failed_transport_never_prints_child_output(self):
        argv = ["onprem_acceptance.py", "--expected-image", self.args.expected_image]
        failure = subprocess.CalledProcessError(
            1, "podman", output="private receipt", stderr="private database details")
        with patch.object(adapter.sys, "argv", argv), \
                patch("builtins.open", mock_open()), \
                patch.object(adapter.fcntl, "flock"), \
                patch.object(adapter, "execute", side_effect=failure), \
                patch.object(adapter.sys, "stdin", io.StringIO(self.request)), \
                patch.object(adapter.sys, "stdout", new_callable=io.StringIO) as stdout, \
                patch.object(adapter.sys, "stderr", new_callable=io.StringIO) as stderr:
            self.assertEqual(adapter.main(), 1)
        self.assertEqual(stdout.getvalue(), "")
        self.assertNotIn("private", stderr.getvalue().replace("private reservation journal", "journal"))


if __name__ == "__main__":
    unittest.main()
