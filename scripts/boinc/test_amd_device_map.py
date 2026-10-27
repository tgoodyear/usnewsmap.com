"""Test AMD identity mapping and optional pinned release libraries; no AMD GPU needed."""
import os
import json
import hashlib
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tarfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[2]


class PinnedIdentityPatch(unittest.TestCase):
    def test_patch_rejects_unpinned_source_without_modifying_it(self):
        from scripts.boinc.patch_amd_vulkan_identity import patch
        with tempfile.TemporaryDirectory(prefix=".amd-patch-", dir=ROOT) as folder:
            source = Path(folder) / "ggml-vulkan.cpp"
            source.write_text("unreviewed runtime")
            with self.assertRaisesRegex(ValueError, "unchanged pinned b8799"):
                patch(source)
            self.assertEqual(source.read_text(), "unreviewed runtime")

    @unittest.skipUnless(os.environ.get("AMD_PINNED_VULKAN_SOURCE"), "pinned Vulkan source required")
    def test_patch_actual_pinned_source_and_compile_identity_function(self):
        from scripts.boinc.patch_amd_vulkan_identity import patch
        with tempfile.TemporaryDirectory(prefix=".amd-source-", dir=ROOT) as folder:
            source = Path(folder) / "ggml-vulkan.cpp"
            shutil.copyfile(os.environ["AMD_PINNED_VULKAN_SOURCE"], source)
            patch(source)
            text = source.read_text()
            function = text[text.index("static std::string ggml_backend_vk_get_device_pci_id("):
                            text.index("//////////////////////////",
                                       text.index("static std::string ggml_backend_vk_get_device_pci_id("))]
            # Compile the exact patched function with deterministic Vulkan API responses.
            probe = Path(folder) / "probe.cpp"
            probe.write_text(r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>
#ifdef USNM_TEST_WINDOWS
#define _WIN32
#endif
#define GGML_ASSERT assert
#define VK_LUID_SIZE 8
static bool pci_extension = false, luid_valid = true, zero_luid = false;
namespace vk {
struct ExtensionProperties { char extensionName[64] = "VK_EXT_pci_bus_info"; };
struct PhysicalDeviceProperties2 { void *pNext = nullptr; };
struct PhysicalDeviceIDProperties { bool deviceLUIDValid = false; uint8_t deviceLUID[8]{}; };
struct PhysicalDevicePCIBusInfoPropertiesEXT {
    uint32_t pciDomain = 1, pciBus = 3, pciDevice = 0, pciFunction = 0;
};
struct PhysicalDevice {
    std::vector<ExtensionProperties> enumerateDeviceExtensionProperties() {
        return pci_extension ? std::vector<ExtensionProperties>{{}} : std::vector<ExtensionProperties>{};
    }
    void getProperties2(PhysicalDeviceProperties2 *props) {
        if (!pci_extension) {
            auto *id = static_cast<PhysicalDeviceIDProperties *>(props->pNext);
            id->deviceLUIDValid = luid_valid;
            for (int i = 0; i < 8; ++i) id->deviceLUID[i] = zero_luid ? 0 : i + 1;
        }
    }
};
struct Instance {
    std::vector<PhysicalDevice> enumeratePhysicalDevices() { return {{}}; }
};
}
static struct { std::vector<size_t> device_indices{0}; vk::Instance instance; } vk_instance;
''' + function + r'''
int main() {
#ifdef _WIN32
    assert(ggml_backend_vk_get_device_pci_id(0) == "luid:0102030405060708");
#else
    assert(ggml_backend_vk_get_device_pci_id(0).empty());
#endif
    luid_valid = false;
    assert(ggml_backend_vk_get_device_pci_id(0).empty());
    luid_valid = true; zero_luid = true;
    assert(ggml_backend_vk_get_device_pci_id(0).empty());
    pci_extension = true;
    assert(ggml_backend_vk_get_device_pci_id(0) == "0001:03:00.0");
}
''')
            for flags in ([], ["-DUSNM_TEST_WINDOWS"]):
                binary = Path(folder) / "probe"
                subprocess.run(["c++", "-std=c++17", "-Wall", "-Wextra", "-Werror",
                                *flags, str(probe), "-o", str(binary)], check=True)
                subprocess.run([str(binary)], check=True)
            with self.assertRaisesRegex(ValueError, "unchanged pinned b8799"):
                patch(source)


@unittest.skipUnless(os.environ.get("AMD_PINNED_LLAMA_ARCHIVES"), "pinned Vulkan release archives required")
class PinnedLibraries(unittest.TestCase):
    def test_actual_release_exports_match_helper_library_split(self):
        lock = json.loads((ROOT / "ocr/boinc/qwen/llama.lock").read_text())
        archives = Path(os.environ["AMD_PINNED_LLAMA_ARCHIVES"])
        with tempfile.TemporaryDirectory(prefix=".amd-pinned-", dir=ROOT) as folder:
            for key in ("ubuntu-vulkan-x64", "win-vulkan-x64"):
                asset = lock["assets"][key]
                archive = archives / asset["file"]
                self.assertEqual(hashlib.sha256(archive.read_bytes()).hexdigest(), asset["sha256"])
                out = Path(folder) / key
                out.mkdir()
                if key.startswith("win"):
                    with zipfile.ZipFile(archive) as packed:
                        for name in ("ggml.dll", "ggml-base.dll", "ggml-vulkan.dll"):
                            (out / name).write_bytes(packed.read(name))
                    registry, base, flags = out / "ggml.dll", out / "ggml-base.dll", ["-p"]
                else:
                    with tarfile.open(archive) as packed:
                        for name in ("libggml.so", "libggml-base.so", "libggml-vulkan.so"):
                            member = packed.getmember("llama-b8799/" + name)
                            (out / name).write_bytes(packed.extractfile(member).read())
                    registry, base, flags = out / "libggml.so", out / "libggml-base.so", ["-T"]
                def exports(path):
                    return subprocess.check_output(["objdump", *flags, str(path)], text=True)
                self.assertRegex(exports(registry), r"\bggml_backend_load\b")
                base_exports = exports(base)
                self.assertNotRegex(base_exports, r"\bggml_backend_load\b")
                for name in ("ggml_backend_reg_dev_count", "ggml_backend_reg_dev_get",
                             "ggml_backend_dev_get_props"):
                    self.assertRegex(base_exports, r"\b" + name + r"\b")

    @unittest.skipUnless(sys.platform.startswith("linux"), "actual Linux Vulkan loader required")
    def test_actual_linux_registry_and_backend_load_with_helper_code(self):
        archive = Path(os.environ["AMD_PINNED_LLAMA_ARCHIVES"]) / "llama-b8799-bin-ubuntu-vulkan-x64.tar.gz"
        lock = json.loads((ROOT / "ocr/boinc/qwen/llama.lock").read_text())
        self.assertEqual(hashlib.sha256(archive.read_bytes()).hexdigest(),
                         lock["assets"]["ubuntu-vulkan-x64"]["sha256"])
        with tempfile.TemporaryDirectory(prefix=".amd-runtime-", dir=ROOT) as folder:
            out = Path(folder)
            bundle = out / "llama-b8799"
            bundle.mkdir()
            with tarfile.open(archive) as packed:
                for name in ("libggml.so", "libggml.so.0", "libggml-base.so",
                             "libggml-base.so.0", "libggml-vulkan.so"):
                    member = packed.getmember("llama-b8799/" + name)
                    (bundle / name).write_bytes(packed.extractfile(member).read())
            source = out / "probe.cpp"
            source.write_text('#define main helper_main\n#include "' +
                              str(ROOT / "ocr/boinc/qwen/amd-device-map.cpp") +
                              '"\n#undef main\nint main(int argc, char **argv) {\n'
                              '  if (argc != 2) return 2;\n'
                              '  try { vulkan_devices(argv[1]); return 0; }\n'
                              '  catch (const std::exception &e) { std::fprintf(stderr, "%s\\n", e.what()); return 1; }\n}\n')
            subprocess.run(["c++", "-std=c++17", str(source), "-ldl", "-o", str(out / "probe")], check=True)
            # A software Vulkan ICD is enough to exercise actual registry/plugin loading.
            # It is not evidence of assigned AMD hardware or inference.
            result = subprocess.run([str(out / "probe"), str(bundle)], text=True, capture_output=True,
                                    env={**os.environ, "LD_LIBRARY_PATH": str(bundle)}, timeout=30)
            self.assertEqual(result.returncode, 0, result.stderr)


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
#ifdef REGISTRY
void *ggml_backend_load(const char *) { return (void*)1; }
#else
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
#endif
}
''')
            shared = ["-dynamiclib"] if sys.platform == "darwin" else ["-shared", "-fPIC"]
            for name in ("libOpenCL.so.1", "libggml-base.so"):
                subprocess.run(["c++", "-std=c++17", *shared, str(source), "-o", str(root / name)], check=True)
            subprocess.run(["c++", "-std=c++17", *shared, "-DREGISTRY", str(source),
                            "-o", str(root / "libggml.so")], check=True)
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
