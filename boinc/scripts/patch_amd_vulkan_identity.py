#!/usr/bin/env python3
"""Patch only b8799's missing Windows Vulkan identity; never infer GPU ordinals."""
import argparse
import hashlib
from pathlib import Path

SOURCE_COMMIT = "e1a9a6dcbefccb4b864d9385ce8494f2a7f2ffcd"
SOURCE_SHA256 = "2461eddbbc2c71922fbebe6e4bd42091597c3df4bad8acf2b56287447019887e"
ANCHOR = '''    if (!ext_support) {
        return "";
    }
'''
REPLACEMENT = '''    if (!ext_support) {
#ifdef _WIN32
        // Export this physical device's identity, not its filtered ggml ordinal.
        // The signed identity helper resolves the LUID through D3DKMT.
        vk::PhysicalDeviceProperties2 properties = {};
        vk::PhysicalDeviceIDProperties identity = {};
        properties.pNext = &identity;
        device.getProperties2(&properties);
        if (identity.deviceLUIDValid) {
            char luid[22] = "luid:";
            bool nonzero = false;
            for (size_t i = 0; i < VK_LUID_SIZE; ++i) {
                nonzero = nonzero || identity.deviceLUID[i] != 0;
                snprintf(luid + 5 + i * 2, 3, "%02x", static_cast<unsigned int>(identity.deviceLUID[i]));
            }
            if (nonzero) {
                return std::string(luid);
            }
        }
#endif
        return "";
    }
'''


def patch(source: Path) -> None:
    original = source.read_bytes()
    if hashlib.sha256(original).hexdigest() != SOURCE_SHA256:
        raise ValueError("expected unchanged pinned b8799 ggml-vulkan.cpp")
    text = original.decode()
    start = text.index("static std::string ggml_backend_vk_get_device_pci_id(")
    prefix, function = text[:start], text[start:]
    if function.count(ANCHOR) != 1:
        raise ValueError("unexpected pinned Vulkan PCI function")
    source.write_bytes((prefix + function.replace(ANCHOR, REPLACEMENT, 1)).encode())


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    args = parser.parse_args()
    patch(args.source)


if __name__ == "__main__":
    main()
