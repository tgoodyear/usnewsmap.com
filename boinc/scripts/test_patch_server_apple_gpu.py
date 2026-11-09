"""Unit tests for boinc/scripts/patch-server-apple-gpu.py."""
from __future__ import annotations

import importlib.util
from pathlib import Path
import tempfile
import unittest
import shutil
import subprocess
import sys

spec = importlib.util.spec_from_file_location(
    "patch_server_apple_gpu",
    Path(__file__).with_name("patch-server-apple-gpu.py"),
)
assert spec is not None and spec.loader is not None
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

SNIPPET = """
static bool wu_is_infeasible_for_plan_class(
    const PLAN_CLASS_SPEC* pc, const WORKUNIT* wu
) {
    if (pc->max_batch && wu->batch > pc->max_batch) {
        if (config.debug_version_select) {
            log_messages.printf(MSG_NORMAL,
                "[version] batch#%d too new for plan class '%s' (%ld)\\n",
                wu->batch, pc->name, pc->max_batch
            );
        }
        return true;
    }
    return false;
}

bool PLAN_CLASS_SPEC::check(SCHEDULER_REQUEST& sreq, HOST_USAGE& hu) {
    if (min_wu_id || max_wu_id || min_batch || max_batch) {
        if (wu_is_infeasible_for_plan_class(this, wu)) {
            return false;
        }
    }
        bool p = parse_str(buf, tag, value, sizeof(value));

    if (!strcmp(gpu_type, "nvidia")) {
        // nvidia eligibility
    } else if (!strcmp(gpu_type, "apple")) {
        COPROC& cp = sreq.coprocs.apple_gpu;
        cpp = &cp;
    }

    if (strstr(gpu_type, "nvidia") == gpu_type) {
        hu.proc_type = PROC_TYPE_NVIDIA_GPU;
        hu.gpu_usage = gpu_usage;
    } else if (strstr(gpu_type, "intel") == gpu_type) {
        hu.proc_type = PROC_TYPE_INTEL_GPU;
        hu.gpu_usage = gpu_usage;
    } else if (strstr(gpu_type, "apple_gpu") == gpu_type) {
        hu.proc_type = PROC_TYPE_APPLE_GPU;
        hu.gpu_usage = gpu_usage;
    }
    return true;
}
"""


class PatchServerAppleGpuTest(unittest.TestCase):
    @unittest.skipUnless(shutil.which("c++"), "C++ compiler unavailable")
    def test_workunit_restrictions_do_not_reuse_cached_null_or_other_batch(self):
        source = r"""
#define assert(value) do { if (!(value)) return __LINE__; } while (false)
bool wu_restricted_plan_class = false;
struct { struct { int end() { return 1; } } best_app_versions; } request;
auto* g_wreq = &request;
int cached_version = 0;
int choose_version(int batch, int id = 16900) {
    int bavi = 0;
CACHE
        return cached_version;
    }
    return batch == 8 && id >= 16899 && id <= 16908 ? 47 : 0;
}
int main() {
    // Preserve memoization for ordinary unrestricted app selection.
    cached_version = 36;
    assert(choose_version(18) == 36);
    wu_restricted_plan_class = true;
    // A closed Docker job must not cache NULL for later native jobs.
    cached_version = 0;
    assert(choose_version(18) == 0);
    assert(choose_version(8) == 47);
    // Nor can a cached compatible payload escape its batch or ID window.
    cached_version = 47;
    assert(choose_version(18) == 0);
    assert(choose_version(8, 16898) == 0);
    assert(choose_version(8, 16909) == 0);
    cached_version = 36;
    assert(choose_version(8) == 47);
}
"""
        with tempfile.TemporaryDirectory() as d:
            binary = str(Path(d) / "version-cache")
            for patched in (False, True):
                loop = module.VERSION_CACHE_ORIGINAL
                if patched:
                    loop = module.patch_version_source(loop)
                subprocess.run(
                    ["c++", "-std=c++17", "-x", "c++", "-", "-o", binary],
                    input=source.replace("CACHE", loop), text=True,
                    check=True, capture_output=True,
                )
                result = subprocess.run([binary], check=False)
                self.assertEqual(result.returncode == 0, patched)

    def test_version_cache_patch_fails_closed_on_source_drift_or_reapplication(self):
        for source in ("", module.VERSION_CACHE_ORIGINAL * 2, module.VERSION_CACHE_PATCHED):
            with self.subTest(source=source):
                with self.assertRaisesRegex(ValueError, "expected exactly 1"):
                    module.patch_version_source(source)

    def test_cli_validates_both_sources_before_writing(self):
        with tempfile.TemporaryDirectory() as d:
            plan = Path(d) / "plan_class_spec.cpp"
            version = Path(d) / "sched_version.cpp"
            plan.write_text(SNIPPET)
            version.write_text("source drift")
            command = [
                sys.executable, str(Path(module.__file__)),
                "--source", str(plan), "--version-source", str(version),
            ]
            failed = subprocess.run(command, capture_output=True, text=True, check=False)
            self.assertNotEqual(failed.returncode, 0)
            self.assertEqual(plan.read_text(), SNIPPET)
            version.write_text(module.VERSION_CACHE_ORIGINAL)
            subprocess.run(command, capture_output=True, text=True, check=True)
            self.assertIn(module.BATCH_CHECK_PATCHED, plan.read_text())
            self.assertEqual(version.read_text(), module.VERSION_CACHE_PATCHED)

    @unittest.skipUnless(shutil.which("c++"), "C++ compiler unavailable")
    def test_model_batches_are_exclusive_in_executable_scheduler_gate(self):
        source = r"""
// Header-free fixture also runs with minimal Command Line Tools installations.
#define NULL nullptr
#define assert(value) do { if (!(value)) return __LINE__; } while (false)
int strcmp(const char* a, const char* b) {
    while (*a && *a == *b) { ++a; ++b; }
    return *a - *b;
}
const char* strstr(const char* a, const char* b) {
    for (; *a; ++a) {
        const char* x = a; const char* y = b;
        while (*x && *x == *y) { ++x; ++y; }
        if (!*y) return a;
    }
    return nullptr;
}
struct PC { int max_batch = 0; bool docker; const char* name; int min_wu_id=0, max_wu_id=0; };
struct WU { int batch; int id=100; };
struct { bool debug_version_select = false; } config;
struct { template<class... T> void printf(T...) {} } log_messages;
const int MSG_NORMAL = 0;
bool infeasible(const PC* pc, const WU* wu) {
    // Unchanged pinned BOINC WU range checks precede the patched batch gate.
    if (pc->min_wu_id && wu->id < pc->min_wu_id) return true;
    if (pc->max_wu_id && wu->id > pc->max_wu_id) return true;
GATE
}
int main() {
    const PC classes[] = {
        {0, true, "docker_mt"}, {0, true, "docker_nvidia_cuda"},
        {12, true, "docker_mt_2b"}, {18, true, "docker_nvidia_cuda_8b"},
        {0, false, "mt"}, {8, false, "apple_gpu_8b"},
        {8, false, "nvidia_cuda_8b"}, {2, false, "nvidia_cuda_2b"},
        {104, true, "docker_intel_opencl_4b"}
    };
    const int expected[] = {1, 1, 12, 18, 2, 8, 8, 2, 104};
    const int batches[] = {1, 12, 18, 2, 8, 104};
    for (int i = 0; i < 9; ++i) {
        for (int b : batches) { WU wu{b}; assert(infeasible(&classes[i], &wu) == (b != expected[i])); }
        WU legacy{0}; assert(!infeasible(&classes[i], &legacy));
    }
    const char* native_names[] = {"nvidia_cuda_2b", "nvidia_cuda_8b"};
    const int ids[] = {0, 1, 2, 100, 2147483647};
    for (const char* name : native_names) {
        int batch = strcmp(name, "nvidia_cuda_2b") ? 8 : 2;
        PC closed{batch, false, name, 2, 1};
        for (int id : ids) {
            WU wu{batch, id}; assert(infeasible(&closed, &wu));
        }
        PC bounded{batch, false, name, 100, 100};
        WU allowed{batch,100}, before{batch,99}, after{batch,101}, wrong{12,100};
        assert(!infeasible(&bounded, &allowed));
        assert(infeasible(&bounded, &before) && infeasible(&bounded, &after));
        assert(infeasible(&bounded, &wrong));
    }
}
""".replace("GATE", module.BATCH_INFEASIBLE_PATCHED)
        with tempfile.TemporaryDirectory() as d:
            binary = str(Path(d) / "scheduler-gate")
            subprocess.run(["c++", "-std=c++17", "-x", "c++", "-", "-o", binary], input=source, text=True, check=True, capture_output=True)
            subprocess.run([binary], check=True)

    def test_patches_both_eligibility_and_dispatch_branches(self):
        with tempfile.TemporaryDirectory() as d:
            cpp = Path(d) / "plan_class_spec.cpp"
            cpp.write_text(SNIPPET)
            module.patch_file(cpp)
            patched = cpp.read_text()
            self.assertEqual(patched.count(module.ELIGIBILITY_PATCHED), 1)
            self.assertEqual(patched.count(module.DISPATCH_PATCHED), 1)
            self.assertEqual(patched.count(module.BATCH_INFEASIBLE_PATCHED), 1)
            self.assertEqual(patched.count(module.BATCH_CHECK_PATCHED), 1)
            self.assertNotIn("qwen_native_cuda_canary", patched)
            self.assertNotIn(module.ELIGIBILITY_ORIGINAL, patched)
            self.assertNotIn(module.DISPATCH_ORIGINAL, patched)
            self.assertNotIn(module.BATCH_INFEASIBLE_ORIGINAL, patched)
            self.assertNotIn(module.BATCH_CHECK_ORIGINAL, patched)
            self.assertIn("hu.proc_type = PROC_TYPE_APPLE_GPU;", patched)
            # Re-patching an already patched file must fail fast.
            with self.assertRaisesRegex(ValueError, "expected exactly 1"):
                module.patch_file(cpp)

    def test_fails_fast_when_either_branch_is_missing_or_duplicated(self):
        for bad in (
            SNIPPET.replace(module.ELIGIBILITY_ORIGINAL, "// missing eligibility"),
            SNIPPET.replace(module.DISPATCH_ORIGINAL, "// missing dispatch"),
            SNIPPET.replace(module.BATCH_INFEASIBLE_ORIGINAL, "// missing batch gate"),
            SNIPPET.replace(module.BATCH_CHECK_ORIGINAL, "// missing batch check"),
            SNIPPET + "\n" + module.ELIGIBILITY_ORIGINAL + "\n",
            SNIPPET + "\n" + module.DISPATCH_ORIGINAL + "\n",
        ):
            with self.subTest(bad=bad):
                with self.assertRaisesRegex(ValueError, "expected exactly 1"):
                    module.patch_source(bad)


if __name__ == "__main__":
    unittest.main()
