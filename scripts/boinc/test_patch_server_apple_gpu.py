"""Unit tests for scripts/boinc/patch-server-apple-gpu.py."""
from __future__ import annotations

import importlib.util
from pathlib import Path
import tempfile
import unittest

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
