"""Unit tests for boinc/scripts/patch-server-gpu-ram-cap.py.

Set BOINC_SOURCE_ROOT to a checkout of the pinned BOINC commit to patch its real sources too.
"""
from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "patch_server_gpu_ram_cap", Path(__file__).with_name("patch-server-gpu-ram-cap.py"))
assert spec is not None and spec.loader is not None
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

SOURCE = ("int PLAN_CLASS_SPEC::parse(XML_PARSER& xp) {\n" + module.PARSE_ORIGINAL + "}\n"
          "bool PLAN_CLASS_SPEC::check() {\n    if (cpp) {\n" + module.CHECK_ORIGINAL + "    }\n}\n"
          "PLAN_CLASS_SPEC::PLAN_CLASS_SPEC() {\n" + module.INIT_ORIGINAL + "}\n")
HEADER = "struct PLAN_CLASS_SPEC {\n" + module.HEADER_ORIGINAL + "    double gpu_ram_used_mb;\n};\n"


class PatchServerGpuRamCapTest(unittest.TestCase):
    def test_patches_parse_default_check_and_member_once(self):
        source = module.patch_source(SOURCE)
        header = module.patch_header(HEADER)
        for patched in (module.PARSE_PATCHED, module.INIT_PATCHED, module.CHECK_PATCHED):
            self.assertEqual(source.count(patched), 1)
        self.assertEqual(header.count("double max_gpu_ram_mb;"), 1)
        with self.assertRaisesRegex(ValueError, "expected exactly 1"):
            module.patch_source(source)
        with self.assertRaisesRegex(ValueError, "expected exactly 1"):
            module.patch_header(header)

    def test_fails_closed_on_source_drift(self):
        for original in (module.PARSE_ORIGINAL, module.INIT_ORIGINAL, module.CHECK_ORIGINAL):
            for bad in (SOURCE.replace(original, ""), SOURCE + original):
                with self.subTest(original=original[:40], duplicated=bad.endswith(original)):
                    with self.assertRaisesRegex(ValueError, "expected exactly 1"):
                        module.patch_source(bad)
        with self.assertRaisesRegex(ValueError, "expected exactly 1"):
            module.patch_header("struct PLAN_CLASS_SPEC {};\n")

    def test_cli_validates_both_files_before_writing(self):
        with tempfile.TemporaryDirectory() as folder:
            source, header = Path(folder) / "plan_class_spec.cpp", Path(folder) / "plan_class_spec.h"
            source.write_text(SOURCE)
            header.write_text("drift")
            command = [sys.executable, str(Path(module.__file__)),
                       "--source", str(source), "--header", str(header)]
            self.assertNotEqual(subprocess.run(command, capture_output=True).returncode, 0)
            self.assertEqual(source.read_text(), SOURCE)
            header.write_text(HEADER)
            subprocess.run(command, capture_output=True, check=True)
            self.assertIn(module.CHECK_PATCHED, source.read_text())
            self.assertIn("double max_gpu_ram_mb;", header.read_text())

    @unittest.skipUnless(shutil.which("c++"), "C++ compiler unavailable")
    def test_six_gb_class_admits_only_cards_below_the_full_offload_floor(self):
        # The patched block in a header-free harness; MEGA is BOINC's (lib/util.h, 1024*1024).
        program = r"""
#define MEGA (1024.*1024.)
#define assert(value) do { if (!(value)) return __LINE__; } while (false)
struct { bool debug_version_select = false; } config;
struct { template<class... T> void printf(T...) {} } log_messages;
const int MSG_NORMAL = 0;
struct COPROC { int count; };
bool admits(double min_gpu_ram_mb, double max_gpu_ram_mb, double gpu_ram, int count = 1) {
    COPROC cp{count};
    COPROC* cpp = &cp;
    if (cpp) {
CHECK
    }
    return true;
}
int main() {
    // nvidia_vulkan_6gb_8b: min 5500, max 6999. nvidia_vulkan_8b: min 7000, no max.
    assert(!admits(5500, 6999, 5499 * MEGA));
    assert(admits(5500, 6999, 5500 * MEGA));
    assert(admits(5500, 6999, 5750 * MEGA));     // GTX 1660 SUPER
    assert(admits(5500, 6999, 6144 * MEGA));
    assert(admits(5500, 6999, 6999 * MEGA));
    assert(!admits(5500, 6999, 7000 * MEGA));
    assert(!admits(5500, 6999, 8192 * MEGA));    // RTX 2060 SUPER
    assert(!admits(7000, 0, 6999 * MEGA));
    assert(admits(7000, 0, 7000 * MEGA));
    assert(admits(7000, 0, 24576 * MEGA));       // no max: unchanged behavior
    // A mixed 6/8 GiB pool: either card may be the representative; a capped class refuses both.
    assert(!admits(5500, 6999, 6144 * MEGA, 2));
    assert(!admits(5500, 6999, 8192 * MEGA, 2));
    assert(admits(7000, 0, 8192 * MEGA, 2));     // uncapped classes keep multi-GPU hosts
}
""".replace("CHECK", module.CHECK_PATCHED)
        with tempfile.TemporaryDirectory() as folder:
            binary = str(Path(folder) / "gpu-ram-cap")
            subprocess.run(["c++", "-std=c++17", "-x", "c++", "-", "-o", binary],
                           input=program, text=True, check=True, capture_output=True)
            subprocess.run([binary], check=True)

    @unittest.skipUnless(os.environ.get("BOINC_SOURCE_ROOT"), "BOINC_SOURCE_ROOT unset")
    def test_patches_the_pinned_boinc_sources(self):
        sched = Path(os.environ["BOINC_SOURCE_ROOT"]) / "sched"
        source = module.patch_source((sched / "plan_class_spec.cpp").read_text())
        header = module.patch_header((sched / "plan_class_spec.h").read_text())
        self.assertIn('parse_double("max_gpu_ram_mb", max_gpu_ram_mb)', source)
        self.assertIn("double max_gpu_ram_mb;", header)


if __name__ == "__main__":
    unittest.main()
