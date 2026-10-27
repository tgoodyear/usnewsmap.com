"""Hermetic transport checks; no project, Podman, SSH or database is contacted."""
import argparse
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import Mock, mock_open, patch

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
        self.assertEqual(adapter.execute(self.args, self.request, self.runner),
                         {"state": "waiting"})
        argv = self.runner.call_args.args[0]
        self.assertEqual(argv[:12], ["podman", "exec", "--interactive", "--user",
                                    "1000:1000", "c" * 64, "timeout", "--signal=TERM",
                                    "--kill-after=5s", "90s", "python3", "-c"])
        self.assertIn("def isolated_account(", argv[12])
        self.assertEqual(argv[13:], ["--project", "/project/usnmocr",
                                   "--fixtures", "/project/usnmocr/acceptance"])
        self.assertEqual(self.runner.call_args.kwargs["input"], self.request)
        self.assertEqual(self.runner.call_args.kwargs["timeout"], 105)

    def test_changed_or_stopped_server_never_receives_request(self):
        for value in (None, {}, [None], [],
                      [{"Image": "sha256:" + "b" * 64, "State": {"Running": True}}],
                      [{"Image": self.args.expected_image, "State": None}],
                      [{"Id": 1, "Image": self.args.expected_image, "State": {"Running": True}}],
                      [{"Image": self.args.expected_image, "State": {"Running": False}}]):
            with self.subTest(value=value):
                runner = Mock(return_value=subprocess.CompletedProcess([], 0, json.dumps(value)))
                with self.assertRaises(ValueError):
                    adapter.execute(self.args, self.request, runner)
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
                    self.assertEqual(adapter.execute(self.args, self.request, runner),
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
                    adapter.execute(args, self.request, runner)
                runner.assert_not_called()
        for request in ("[]", "not-json", " " * 65537):
            runner = Mock()
            with self.assertRaises(ValueError):
                adapter.execute(self.args, request, runner)
            runner.assert_not_called()
        with patch.object(adapter.os, "geteuid", return_value=1000):
            runner = Mock()
            with self.assertRaises(ValueError):
                adapter.execute(self.args, self.request, runner)
            runner.assert_not_called()

    def test_transport_timeout_is_not_replayed(self):
        self.runner.side_effect = [
            subprocess.CompletedProcess([], 0, json.dumps([{
                "Id": "c" * 64, "Image": self.args.expected_image, "State": {"Running": True}}])),
            subprocess.TimeoutExpired("podman", 105),
        ]
        with self.assertRaises(subprocess.TimeoutExpired):
            adapter.execute(self.args, self.request, self.runner)
        self.assertEqual(self.runner.call_count, 2)

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
