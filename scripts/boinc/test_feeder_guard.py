"""Exercise the actual feeder guard state machine and its pinned-source patch."""

import importlib.util
import json
import os
from pathlib import Path
import select
import shutil
import socket
import subprocess
import unittest
from unittest.mock import patch


HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("feeder_patch", HERE / "patch-server-feeder-guard.py")
PATCH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PATCH)

HARNESS = r"""
#include <iostream>
#include <limits>
#include <string>
struct APP { long id; double weight; };
#define MAX_APPS 10
struct SCHED_SHMEM { int napps = 3; APP apps[10] = {{1,1}, {6,16}, {9,1}}; };
#include "usnm-feeder-guard.h"

static void show(const char* path) {
    std::ifstream file(path);
    std::string line;
    std::getline(file, line);
    std::cout << line << "\n";
}

int main(int argc, char** argv) {
    if (argc != 3 && argc != 4) return 2;
    const std::string which = argv[2];
    SCHED_SHMEM ssp;
    UsnmFeederGuard guard;
    if (which == "invalid") ssp.apps[1].weight = std::numeric_limits<double>::quiet_NaN();
    if (which == "invalid-id") ssp.apps[1].id = -1;
    if (which == "zero") ssp.apps[0].weight = 0;
    guard.capture(&ssp, which != "disabled", 0);
    if (which == "live") {
        guard.start();
        std::cout << "ready" << std::endl;
        std::string command;
        while (std::getline(std::cin, command)) {
            if (command == "weight") {
                guard.begin_reload();
                ssp.apps[1].weight = 8;
                guard.end_reload(&ssp, 0);
                std::cout << "reloaded" << std::endl;
            } else {
                break;
            }
        }
        return 0;
    }
    guard.activate(argv[1], static_cast<unsigned long>(getpid()), "1234",
                   "01234567-89ab-cdef-0123-456789abcdef", 1000);
    if (which == "disabled" || which == "write-failure") return 0;
    if (which == "publish-failure") {
        std::string collision = std::string(argv[1]) + "." + std::to_string(getpid()) + ".1234.2.new";
        int fd = open(collision.c_str(), O_WRONLY | O_CREAT | O_EXCL, 0600);
        if (fd < 0) return 3;
        close(fd);
        guard.begin_reload();
        std::cout << "{\"old_removed\":" << (access(argv[1], F_OK) ? "true" : "false")
                  << ",\"collision_preserved\":" << (!access(collision.c_str(), F_OK) ? "true" : "false")
                  << "}\n";
        guard.end_reload(&ssp, 0);
        show(argv[1]);
        unlink(collision.c_str());
        return 0;
    }
    show(argv[1]);
    guard.begin_reload();
    show(argv[1]);
    if (which == "add") ssp.apps[ssp.napps++] = {10, 1};
    if (which == "remove") ssp.napps--;
    if (which == "replace") ssp.apps[0].id = 5;
    if (which == "weight") ssp.apps[1].weight = 16.000001;
    if (which == "reorder") std::swap(ssp.apps[0], ssp.apps[1]);
    guard.end_reload(&ssp, which == "scan-failure" ? 1 : 0);
    show(argv[1]);
    ssp = SCHED_SHMEM{};
    guard.begin_reload();
    guard.end_reload(&ssp, 0);
    show(argv[1]);
}
"""


class FeederPatch(unittest.TestCase):
    def test_patch_requires_all_unique_anchors_and_cannot_be_applied_twice(self):
        source = "\n".join(old for old, _ in PATCH.REPLACEMENTS)
        patched = PATCH.patch_source(source)
        for _, new in PATCH.REPLACEMENTS:
            self.assertIn(new, patched)
        with self.assertRaises(ValueError):
            PATCH.patch_source(patched)
        with self.assertRaises(ValueError):
            PATCH.patch_source(source.replace(PATCH.REPLACEMENTS[0][0], ""))
        with self.assertRaises(ValueError):
            PATCH.patch_source(source + PATCH.REPLACEMENTS[-1][0])

    def test_pinned_source_when_available(self):
        source = os.environ.get("BOINC_FEEDER_SOURCE")
        if not source and os.environ.get("BOINC_SOURCE_ROOT"):
            source = str(Path(os.environ["BOINC_SOURCE_ROOT"]) / "sched/feeder.cpp")
        if not source:
            self.skipTest("pinned feeder source not supplied")
        patched = PATCH.patch_source(Path(source).read_text())
        self.assertLess(patched.index("usnm_feeder_guard.capture"),
                        patched.index("weighted_interleave(\n"))
        self.assertLess(patched.index("weighted_interleave(\n"),
                        patched.index("usnm_feeder_guard.start()"))

    def test_base_image_build_and_cache_include_guard_inputs(self):
        root = HERE.parents[1]
        base = root / "contrib-compute/images/base/Containerfile"
        builder = root / "scripts/boinc/build-boinc-images.sh"
        if not base.is_file() or not builder.is_file():
            self.skipTest("repository build files not present in the base-image build context")
        for name in ("patch-server-feeder-guard.py", "usnm-feeder-guard.h", "test_feeder_guard.py"):
            with self.subTest(name=name):
                self.assertIn(name, base.read_text())
                self.assertIn("HEAD:scripts/boinc/" + name, builder.read_text())


@unittest.skipUnless(shutil.which("c++"), "C++ compiler required")
class FeederState(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.binary = Path.cwd() / f".feeder-guard-test-{os.getpid()}"
        if cls.binary.exists() or cls.binary.is_symlink():
            raise unittest.SkipTest("fixture binary path is already in use")
        cls.addClassCleanup(cls.binary.unlink, missing_ok=True)
        subprocess.run(
            ["c++", "-std=c++17", "-Wall", "-Wextra", "-Werror", "-I", str(HERE),
             "-x", "c++", "-", "-o", str(cls.binary)],
            input=HARNESS, text=True, check=True,
            env={**os.environ, "TMPDIR": str(Path.cwd())},
        )

    def setUp(self):
        self.status = Path.cwd() / f".feeder-guard-status-{os.getpid()}"
        if self.status.exists() or self.status.is_symlink():
            self.skipTest("fixture status path is already in use")
        self.addCleanup(self.status.unlink, missing_ok=True)

    def run_case(self, case):
        result = subprocess.run([self.binary, self.status, case], capture_output=True, text=True, check=True)
        return [json.loads(line) for line in result.stdout.splitlines()]

    def test_unchanged_reload_preserves_the_allocation_and_exact_weight_bits(self):
        start, pending, loaded, again = self.run_case("unchanged")
        self.assertEqual(start["apps"], [[1, "3ff0000000000000"], [6, "4030000000000000"],
                                         [9, "3ff0000000000000"]])
        self.assertFalse(start["drift"])
        self.assertTrue(pending["reloading"])
        self.assertFalse(loaded["reloading"])
        self.assertFalse(loaded["drift"])
        self.assertFalse(again["drift"])
        self.assertGreater(again["sequence"], start["sequence"])

    def test_add_remove_replace_weight_and_reorder_are_latched_even_after_reversion(self):
        for case in ("add", "remove", "replace", "weight", "reorder"):
            with self.subTest(case=case):
                self.status.unlink(missing_ok=True)
                start, _, changed, reverted = self.run_case(case)
                self.assertFalse(start["drift"])
                self.assertTrue(changed["drift"])
                self.assertTrue(reverted["drift"])

    def test_zero_weight_does_not_make_an_unchanged_reload_look_stale(self):
        start, _, loaded, _ = self.run_case("zero")
        self.assertEqual(start["apps"][0], [1, "0000000000000000"])
        self.assertFalse(loaded["drift"])
        self.assertFalse(loaded["uncertain"])

    def test_scan_failure_and_invalid_start_are_never_reported_as_certain(self):
        for case in ("scan-failure", "invalid", "invalid-id"):
            with self.subTest(case=case):
                self.status.unlink(missing_ok=True)
                start, _, changed, reverted = self.run_case(case)
                self.assertEqual(start["uncertain"], case != "scan-failure")
                self.assertTrue(changed["uncertain"])
                self.assertTrue(reverted["uncertain"])

    def test_non_allapps_and_unwritable_status_do_not_stop_the_feeder(self):
        self.assertEqual(self.run_case("disabled"), [])
        self.assertFalse(self.status.exists())
        result = subprocess.run([self.binary, self.status / "missing", "write-failure"],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0)
        self.assertIn("allocation status unavailable", result.stderr)

    def test_failed_reload_publication_cannot_leave_an_old_healthy_snapshot(self):
        during_failure, recovered = self.run_case("publish-failure")
        self.assertTrue(during_failure["old_removed"])
        self.assertTrue(during_failure["collision_preserved"])
        self.assertTrue(recovered["uncertain"])

    @unittest.skipUnless(Path("/proc/self/stat").is_file(), "Linux procfs required")
    def test_real_feeder_identity_snapshot_and_monitor(self):
        guard_path = HERE.parents[1] / "contrib-compute/images/server/feeder-slot-guard.py"
        if not guard_path.is_file():
            self.skipTest("server monitor not present in the base-image build context")
        spec = importlib.util.spec_from_file_location("live_feeder_slot_guard", guard_path)
        monitor = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(monitor)
        root = Path.cwd()
        pid_dir = root / ("pid_" + socket.gethostname().split(".")[0])
        pid_file, status = root / "feeder.pid", root / "usnm-feeder-slots.json"
        if pid_dir.exists() or pid_dir.is_symlink() or pid_file.exists() or status.exists():
            self.skipTest("test fixture paths are already in use")
        pid_dir.symlink_to(".")
        self.addCleanup(pid_dir.unlink, missing_ok=True)
        self.addCleanup(status.unlink, missing_ok=True)
        child = subprocess.Popen(
            ["feeder", str(root) + "/", "live", "--allapps"], executable=str(self.binary),
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            env={**os.environ, "USNM_PID_ROOT": str(root)},
        )
        try:
            with pid_file.open("x") as stream:
                self.addCleanup(pid_file.unlink, missing_ok=True)
                stream.write(str(child.pid) + "\n")
            self.assertTrue(select.select([child.stdout], [], [], 10)[0], "fixture did not start")
            self.assertEqual(child.stdout.readline().strip(), "ready")
            self.assertIsNone(child.poll())
            with patch.dict(os.environ, {"USNM_PID_ROOT": str(root)}), \
                 patch.object(monitor, "database_apps", return_value={1: 1.0, 6: 16.0, 9: 1.0}):
                self.assertEqual(monitor.check(root)["status"], "match")
            with patch.dict(os.environ, {"USNM_PID_ROOT": str(root)}), \
                 patch.object(monitor, "database_apps", return_value={1: 1.0, 6: 16.0, 9: 1.0, 10: 1.0}):
                self.assertEqual(monitor.check(root)["added_app_ids"], [10])
            child.stdin.write("weight\n")
            child.stdin.flush()
            self.assertTrue(select.select([child.stdout], [], [], 10)[0], "fixture did not reload")
            self.assertEqual(child.stdout.readline().strip(), "reloaded")
            with patch.dict(os.environ, {"USNM_PID_ROOT": str(root)}), \
                 patch.object(monitor, "database_apps", return_value={1: 1.0, 6: 16.0, 9: 1.0}):
                self.assertEqual(monitor.check(root)["reason"], "reload_changed_app_order_or_weights")
            child.stdin.write("exit\n")
            child.stdin.flush()
            child.wait(timeout=10)
            self.assertEqual(monitor.check(root)["status"], "unknown")
        finally:
            if child.poll() is None:
                child.terminate()
                child.wait(timeout=10)
            for stream in (child.stdin, child.stdout, child.stderr):
                stream.close()


if __name__ == "__main__":
    unittest.main()
