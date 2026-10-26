import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


runner = load("run-unit-tests")
rust = load("rust-test-times")


class Shards(unittest.TestCase):
    def test_every_selected_module_runs_in_exactly_one_balanced_shard(self):
        files = [(name, None) for name in runner.WEIGHTS]
        files += [(f"scripts/new/test_{i}.py", None) for i in range(40)]
        shards = [runner.shard_files(files, i, 3) for i in range(3)]
        flattened = [item for shard in shards for item in shard]
        self.assertEqual(sorted(flattened), sorted(files))
        self.assertEqual(len({item[0] for item in flattened}), len(files))
        for name in list(runner.WEIGHTS)[:3]:
            self.assertEqual(sum(name in {item[0] for item in s} for s in shards), 1)
        heavy = [next(i for i, s in enumerate(shards) if (name, None) in s)
                 for name in list(runner.WEIGHTS)[:3]]
        self.assertEqual(len(set(heavy)), 3)
        self.assertEqual(runner.shard_files(list(reversed(files)), 0, 3), shards[0])

    def test_module_failure_and_zero_collection_still_fail(self):
        for code, output in [(1, "Ran 2 tests in 0.1s\nFAILED\n"), (0, "Ran 0 tests in 0.0s\nOK\n")]:
            with self.subTest(code=code, output=output), \
                    patch.object(runner, "selected", return_value=([("scripts/test_x.py", None)], [])), \
                    patch.object(runner.subprocess, "run",
                                 return_value=subprocess.CompletedProcess([], code, output)):
                self.assertEqual(runner.main([]), 1)

    def test_timings_are_written_for_failed_modules(self):
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(runner, "selected", return_value=([("scripts/test_x.py", None)], [])), \
                patch.object(runner.subprocess, "run",
                             return_value=subprocess.CompletedProcess([], 1, "Ran 1 test in 0.1s\nFAILED\n")):
            import json
            path = Path(directory) / "timings.json"
            self.assertEqual(runner.main(["--timings", str(path)]), 1)
            self.assertEqual(json.loads(path.read_text())[0]["exitcode"], 1)


class RustTimings(unittest.TestCase):
    def test_ansi_and_failed_binaries_are_reported_without_reusing_old_name(self):
        text = ("\x1b[92mRunning\x1b[0m tests/a.rs (target/a)\n"
                "test result: ok. 2 passed; finished in 1.20s\n"
                "test result: ok. 0 passed; finished in 50.00s\n"
                "Running tests/b.rs (target/b)\n"
                "test result: FAILED. 1 failed; finished in 2.50s\n"
                "Doc-tests crate_a\n"
                "test result: ok. 0 passed; finished in 0.00s\n")
        self.assertEqual(rust.timings(text), [
            (2.5, "FAILED", "tests/b.rs (target/b)"),
            (1.2, "ok", "tests/a.rs (target/a)"), (0.0, "ok", "crate_a"),
        ])
