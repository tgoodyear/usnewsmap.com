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
bool PLAN_CLASS_SPEC::check(SCHEDULER_REQUEST& sreq, HOST_USAGE& hu) {
    if (!strcmp(gpu_type, "nvidia")) {
        // nvidia eligibility
    } else if (!strcmp(gpu_type, "apple")) {
        // apple eligibility
    }

    if (!strcmp(gpu_type, "nvidia")) {
        hu.proc_type = PROC_TYPE_NVIDIA_GPU;
    } else if (!strcmp(gpu_type, "apple_gpu")) {
        hu.proc_type = PROC_TYPE_APPLE_GPU;
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
            self.assertEqual(patched.count(module.PATCHED), 2)
            self.assertNotIn(module.ELIGIBILITY_ORIGINAL, patched)
            self.assertNotIn(module.DISPATCH_ORIGINAL, patched)
            self.assertIn("hu.proc_type = PROC_TYPE_APPLE_GPU;", patched)
            # Re-patching an already patched file must fail fast.
            with self.assertRaisesRegex(ValueError, "expected exactly 1"):
                module.patch_file(cpp)

    def test_fails_fast_when_either_branch_is_missing_or_duplicated(self):
        for bad in (
            SNIPPET.replace(module.ELIGIBILITY_ORIGINAL, "// missing eligibility"),
            SNIPPET.replace(module.DISPATCH_ORIGINAL, "// missing dispatch"),
            SNIPPET + "\n" + module.ELIGIBILITY_ORIGINAL + "\n",
            SNIPPET + "\n" + module.DISPATCH_ORIGINAL + "\n",
        ):
            with self.subTest(bad=bad):
                with self.assertRaisesRegex(ValueError, "expected exactly 1"):
                    module.patch_source(bad)


if __name__ == "__main__":
    unittest.main()
