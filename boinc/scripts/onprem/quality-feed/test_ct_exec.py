"""Batch-container routing: `pct exec <id> --` for a numeric id, the bare command for `local`."""
import argparse
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent
import sys
sys.path.insert(0, str(HERE))
import ct_exec  # noqa: E402

spec = importlib.util.spec_from_file_location("quality_results_partial", HERE / "quality_results_partial.py")
partial = importlib.util.module_from_spec(spec)
spec.loader.exec_module(partial)


class CtExecTest(unittest.TestCase):
    def test_argv_numeric_and_local(self):
        for module in (ct_exec, partial):
            with self.subTest(module=module.__name__):
                self.assertEqual(module.ct_argv(215, ["ls", "-l"]), ["pct", "exec", "215", "--", "ls", "-l"])
                self.assertEqual(module.ct_argv("215", []), ["pct", "exec", "215", "--"])
                self.assertEqual(module.ct_argv("local", ["ls", "-l"]), ["ls", "-l"])
                self.assertEqual(module.ct_argv("local", []), [])

    def test_run_passes_keywords_through(self):
        with mock.patch.object(ct_exec.subprocess, "run") as run:
            ct_exec.ct_run("local", ["true"], check=True, timeout=5)
            ct_exec.ct_run(7, ["true"], input=b"x")
        self.assertEqual(run.call_args_list[0], mock.call(["true"], check=True, timeout=5))
        self.assertEqual(run.call_args_list[1], mock.call(["pct", "exec", "7", "--", "true"], input=b"x"))

    def test_ctid_arg_accepts_positive_ids_and_local_only(self):
        for module in (ct_exec, partial):
            self.assertEqual(module.ctid_arg("215"), 215)
            self.assertEqual(module.ctid_arg("local"), "local")
            for bad in ("0", "-1", "", "abc", "Local", "2.5", "215; id"):
                with self.subTest(bad=bad), self.assertRaises(argparse.ArgumentTypeError):
                    module.ctid_arg(bad)

    def test_default_follows_environment(self):
        for module in (ct_exec, partial):
            with mock.patch.dict(os.environ, {"CTID": "local"}):
                self.assertEqual(module.default_ctid(), "local")
            with mock.patch.dict(os.environ, {"CTID": "216"}):
                self.assertEqual(module.default_ctid(), 216)
            with mock.patch.dict(os.environ):
                os.environ.pop("CTID", None)
                self.assertEqual(module.default_ctid(), 215)

    def test_partial_main_accepts_local_ctid(self):
        for value, expected in (("local", "local"), ("216", 216)):
            with self.subTest(value=value), tempfile.TemporaryDirectory(dir=HERE) as folder, \
                    mock.patch.object(partial, "host_passes", return_value={}) as passes:
                partial.main(["--campaign", "test", "--engine-image", "sha256:" + "a" * 64,
                              "--collector", "/project/x", "--ctid", value,
                              "--lock", str(Path(folder) / "lock")])
                self.assertEqual(passes.call_args.args[0].ctid, expected)

    def test_partial_main_rejects_bad_ctid(self):
        with self.assertRaises(SystemExit) as error:
            partial.main(["--campaign", "test", "--engine-image", "sha256:" + "a" * 64,
                          "--collector", "/project/x", "--ctid", "0"])
        self.assertEqual(error.exception.code, 2)


class PullScriptTest(unittest.TestCase):
    """quality-results-pull.sh with stub pct and ssh: only the batch container side changes."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory(dir=HERE)
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for name in ("pct", "ssh"):
            stub = self.bin / name
            stub.write_text('#!/bin/sh\necho "%s $*" >> "$LOG"\n' % name)
            stub.chmod(0o755)
        self.log = self.root / "log"

    def pull(self, *extra, env=None):
        environment = {**os.environ, "PATH": f"{self.bin}{os.pathsep}{os.environ['PATH']}",
                       "LOG": str(self.log), **(env or {})}
        environment.pop("CTID", None) if env is None else None
        return subprocess.run(["bash", str(HERE / "quality-results-pull.sh"), "--campaign", "test",
                               "--server-host", "server.example", *extra],
                              env=environment, capture_output=True, text=True)

    def calls(self):
        return self.log.read_text().splitlines() if self.log.exists() else []

    @unittest.skipUnless(shutil.which("bash"), "needs bash")
    def test_numeric_ctid_uses_pct_exec_for_batch_side(self):
        result = self.pull("--ctid", "216")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.calls()
        self.assertIn("pct exec 216 -- install -d /var/lib/usnm-quality-feed/results/test", calls)
        self.assertTrue(any(c.startswith("pct exec 216 -- timeout") for c in calls))
        self.assertTrue(any(c.startswith("pct exec 216 -- find") for c in calls))
        self.assertTrue(any(c.startswith("ssh ") and "pct exec 230 -- bash -c" in c for c in calls))

    @unittest.skipUnless(shutil.which("bash"), "needs bash")
    def test_local_ctid_runs_directly_but_keeps_server_hop(self):
        for kwargs in ({"extra": ("--ctid", "local")}, {"extra": (), "env": {"CTID": "local"}}):
            with self.subTest(**{k: v for k, v in kwargs.items()}):
                if self.log.exists():
                    self.log.unlink()
                # With a real install in PATH, `local` runs it; stub the batch-side tools.
                for name in ("install", "timeout", "find"):
                    stub = self.bin / name
                    stub.write_text('#!/bin/sh\necho "direct %s $*" >> "$LOG"\n' % name)
                    stub.chmod(0o755)
                result = self.pull(*kwargs["extra"], env=kwargs.get("env"))
                self.assertEqual(result.returncode, 0, result.stderr)
                calls = self.calls()
                self.assertIn("direct install -d /var/lib/usnm-quality-feed/results/test", calls)
                self.assertTrue(any(c.startswith("direct timeout") for c in calls))
                self.assertTrue(any(c.startswith("direct find") for c in calls))
                self.assertFalse(any(c.startswith("pct ") for c in calls), calls)
                self.assertTrue(any(c.startswith("ssh ") and "pct exec 230 -- bash -c" in c for c in calls))

    @unittest.skipUnless(shutil.which("bash"), "needs bash")
    def test_bad_ctid_is_rejected(self):
        for bad in ("abc", "216x", "Local"):
            with self.subTest(bad=bad):
                self.assertNotEqual(self.pull("--ctid", bad).returncode, 0)
        self.assertEqual(self.calls(), [])


if __name__ == "__main__":
    unittest.main()
