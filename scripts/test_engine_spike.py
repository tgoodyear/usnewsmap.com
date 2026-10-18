#!/usr/bin/env python3
"""Regression checks for the engine spike's process-cgroup lookup."""

import os
from pathlib import Path
import re
import subprocess
import unittest


SOURCE = Path(__file__).with_name("ocr-node") / "engine-spike.sh"
CGROUP_OF = re.search(
    r"^  cgroup_of\(\) \{.*?^  \}", SOURCE.read_text(), re.MULTILINE | re.DOTALL
).group()


class ProcessCgroupTests(unittest.TestCase):
    def lookup(self, pid="123", proc="0::/libpod-test.scope/container", status="0"):
        return subprocess.run(
            [
                "bash", "-c",
                CGROUP_OF + r'''
pod() {
  [ "$*" = "container inspect --format {{.State.Pid}} engine" ] || return 90
  printf '%s\n' "$INSPECT_PID"
  return "$INSPECT_STATUS"
}
awk() {
  [ "$1" = "-F:" ] && [ "$3" = "/proc/123/cgroup" ] || return 91
  printf '%s\n' "$PROC_CGROUP" | command awk "$1" "$2"
}
cgroup_of engine
''',
            ],
            env={**os.environ, "INSPECT_PID": pid, "PROC_CGROUP": proc,
                 "INSPECT_STATUS": status},
            text=True, capture_output=True, check=False,
        )

    def test_uses_process_leaf_not_reported_parent_scope(self):
        result = self.lookup()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "/libpod-test.scope/container")

    def test_reads_unified_hierarchy_among_other_entries(self):
        result = self.lookup(proc="1:cpu:/legacy\n0::/libpod-test.scope/container")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "/libpod-test.scope/container")

    def test_rejects_stopped_or_invalid_process(self):
        for pid in ("0", "", "abc", "-1", "12/../self"):
            with self.subTest(pid=pid):
                self.assertNotEqual(self.lookup(pid=pid).returncode, 0)

    def test_rejects_inspect_failure(self):
        self.assertNotEqual(self.lookup(status="1").returncode, 0)

    def test_requires_unified_absolute_cgroup_path(self):
        for proc in ("", "1:cpu:/legacy", "0::relative", "0::"):
            with self.subTest(proc=proc):
                self.assertNotEqual(self.lookup(proc=proc).returncode, 0)


if __name__ == "__main__":
    unittest.main()
