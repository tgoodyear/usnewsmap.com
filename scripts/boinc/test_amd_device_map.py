"""Compile the native identity boundary against fake ICD/ggml libraries; no GPU needed."""
import os
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


@unittest.skipUnless(os.name == "posix" and shutil.which("c++"), "POSIX C++ compiler required")
class AmdDeviceMap(unittest.TestCase):
    def test_pci_mapping_uses_opencl_index_not_vendor_or_vulkan_ordinal(self):
        self.assertEqual(json.loads((ROOT / "ocr/boinc/qwen/llama.lock").read_text())["tag"], "b8799",
                         "Re-audit the helper's ggml device-properties ABI before changing the runtime pin")
        with tempfile.TemporaryDirectory(prefix=".amd-map-", dir=ROOT) as folder:
            root = Path(folder)
            helper = root / "usnm-amd-device-map"
            subprocess.run(["c++", "-std=c++17", "-Wall", "-Wextra", "-Werror",
                            str(ROOT / "ocr/boinc/qwen/amd-device-map.cpp"), "-o", str(helper),
                            *([] if sys.platform == "darwin" else ["-ldl"])], check=True)
            subprocess.run([str(helper), "--self-test"], check=True)
            source = root / "fake.cpp"
            source.write_text(r'''
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <string>
#include <cstddef>
static std::string mode() { const char *s = std::getenv("AMD_MAP_CASE"); return s ? s : ""; }
extern "C" {
int clGetPlatformIDs(uint32_t, void **p, uint32_t *n) { *n=1; p[0]=(void*)1; return 0; }
int clGetDeviceIDs(void *, uint64_t, uint32_t, void **d, uint32_t *n) {
    *n=2; d[0]=(void*)1; d[1]=(void*)2; return 0;
}
int clGetDeviceInfo(void *d, uint32_t key, size_t, void *out, size_t *) {
    uint32_t *v=(uint32_t*)out;
    if (key==0x1001) { *v=mode()=="wrong_vendor" ? 0x8086 : 0x1002; return 0; }
    if (mode()=="no_pci") return -1;
    if (key==0x410F) {
        if (mode()=="amd_topology" || mode()=="domain_ambiguous") return -1;
        v[0]=0; v[1]=d==(void*)1 ? 3 : 4; v[2]=0; v[3]=0; return 0;
    }
    if (key==0x4037) {
        std::memset(out,0,24); *v=1; ((unsigned char*)out)[21]=d==(void*)1 ? 3 : 4; return 0;
    }
    return -1;
}
struct Props {
    const char *name, *description; size_t free, total; int type;
    const char *id; struct {bool a,b,c,d;} caps;
};
void *ggml_backend_load(const char *) { return (void*)1; }
size_t ggml_backend_reg_dev_count(void *) { return mode()=="missing" ? 1 : 2; }
void *ggml_backend_reg_dev_get(void *, size_t n) { return (void*)(n+1); }
void ggml_backend_dev_get_props(void *d, Props *p) {
    p->name=d==(void*)1 ? "Vulkan9" : "Vulkan2";
    p->description=mode()=="software" ? "AMD lavapipe" : "AMD Radeon";
    p->id=d==(void*)1 ? "0000:04:00.0" : "0000:03:00.0";
    if (mode()=="duplicate") p->id="0000:03:00.0";
    if (mode()=="domain_ambiguous" && d==(void*)1) p->id="0001:03:00.0";
    if (mode()=="no_vk_pci") p->id=nullptr;
}
}
''')
            shared = ["-dynamiclib"] if sys.platform == "darwin" else ["-shared", "-fPIC"]
            for name in ("libOpenCL.so.1", "libggml-base.so"):
                subprocess.run(["c++", "-std=c++17", *shared, str(source), "-o", str(root / name)], check=True)
            # The fake base handles backend loading; production loads the signed ggml Vulkan library.
            env = {**os.environ, "LD_LIBRARY_PATH": str(root), "DYLD_LIBRARY_PATH": str(root)}
            for index, expected in [(0, "Vulkan2"), (1, "Vulkan9")]:
                result = subprocess.run([str(helper), str(root), str(index)], capture_output=True, text=True, env=env)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.strip(), expected)
            for mode in ("missing", "duplicate", "domain_ambiguous", "wrong_vendor",
                         "software", "no_pci", "no_vk_pci"):
                with self.subTest(mode=mode):
                    result = subprocess.run([str(helper), str(root), "0"], capture_output=True, text=True,
                                            env={**env, "AMD_MAP_CASE": mode})
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(result.stdout, "")
            result = subprocess.run([str(helper), str(root), "0"], capture_output=True, text=True,
                                    env={**env, "AMD_MAP_CASE": "amd_topology"})
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), "Vulkan2")
