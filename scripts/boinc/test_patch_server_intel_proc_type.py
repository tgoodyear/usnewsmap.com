"""Unit tests for scripts/boinc/patch-server-intel-proc-type.py."""
from __future__ import annotations

import importlib.util
from pathlib import Path
import re
import unittest

spec = importlib.util.spec_from_file_location(
    "patch_server_intel_proc_type", Path(__file__).with_name("patch-server-intel-proc-type.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

SNIPPET = """\
int plan_class_to_proc_type(const char* plan_class) {
    if (strstr(plan_class, "amd")) {
        return PROC_TYPE_AMD_GPU;
    }
    if (strstr(plan_class, "intel_gpu")) {
        return PROC_TYPE_INTEL_GPU;
    }
    return PROC_TYPE_CPU;
}
"""
PLAN_CLASSES = Path(__file__).resolve().parents[2] / "contrib-compute/boinc/plan_class_spec.xml"


class PatchTest(unittest.TestCase):
    def test_intel_plan_classes_are_intel_gpu(self):
        patched = module.patch_source(SNIPPET)
        self.assertIn('strstr(plan_class, "intel")', patched)
        self.assertNotIn('"intel_gpu"', patched)

    def test_refuses_unexpected_source(self):
        with self.assertRaises(ValueError):
            module.patch_source(module.patch_source(SNIPPET))

    def test_every_intel_named_plan_class_is_an_intel_gpu_class(self):
        text = PLAN_CLASSES.read_text()
        classes = re.findall(r"<plan_class>(.*?)</plan_class>", text, re.S)
        named = [c for c in classes if "intel" in re.search(r"<name>(.*?)</name>", c).group(1)]
        self.assertIn("docker_intel_opencl_4b", "".join(named))
        for body in named:
            self.assertIn("<gpu_type>intel</gpu_type>", body)


if __name__ == "__main__":
    unittest.main()
