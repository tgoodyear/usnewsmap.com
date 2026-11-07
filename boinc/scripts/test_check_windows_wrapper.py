"""Reject altered, wrong-architecture and wrong-source Windows PE32+ wrapper artifacts before signing."""
from __future__ import annotations

import hashlib
import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("windows_wrapper", Path(__file__).with_name("check-windows-wrapper.py"))
assert spec is not None and spec.loader is not None
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def pe32_plus_header(
    coff_machine: int = module.IMAGE_FILE_MACHINE_AMD64,
    opt_magic: int = module.PE32_PLUS_MAGIC,
    pe_offset: int = 0x80,
) -> bytes:
    dos = bytearray(pe_offset)
    dos[:2] = b"MZ"
    struct.pack_into("<I", dos, 0x3C, pe_offset)
    coff = struct.pack("<HHIIIHH", coff_machine, 1, 0, 0, 0, 0xF0, 0x0022)
    opt = struct.pack("<H", opt_magic) + b"\x00" * 30
    return bytes(dos) + b"PE\x00\x00" + coff + opt


class CheckWindowsWrapper(unittest.TestCase):
    def test_architecture_checksum_and_source_are_all_required(self):
        self.assertEqual(module.IMAGE_FILE_MACHINE_AMD64, 0x8664)
        self.assertEqual(module.PE32_PLUS_MAGIC, 0x020B)
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            wrapper = root / "docker_wrapper_windows_x86_64.exe"
            wrapper.write_bytes(pe32_plus_header())
            digest = hashlib.sha256(wrapper.read_bytes()).hexdigest()
            pin = root / "BOINC_COMMIT"
            repo = Path(__file__).resolve().parents[2]
            pin.write_text(
                next(
                    line.split("=", 1)[1]
                    for line in (repo / "boinc/images/base/Containerfile").read_text().splitlines()
                    if line.startswith("ARG BOINC_COMMIT=")
                )
            )
            module.check(wrapper, "windows_x86_64", digest, pin)
            with self.assertRaisesRegex(ValueError, "SHA-256"):
                module.check(wrapper, "windows_x86_64", "0" * 64, pin)
            pin.write_text("0" * 40)
            with self.assertRaisesRegex(ValueError, "source pin"):
                module.check(wrapper, "windows_x86_64", digest, pin)

            # Wrong COFF machine (e.g., ARM64 = 0xAA64 or i386 = 0x014C) must fail.
            wrapper_arm64 = root / "docker_wrapper_arm64.exe"
            wrapper_arm64.write_bytes(pe32_plus_header(coff_machine=0xAA64))
            arm64_digest = hashlib.sha256(wrapper_arm64.read_bytes()).hexdigest()
            with self.assertRaisesRegex(ValueError, "architecture"):
                module.check(wrapper_arm64, "windows_x86_64", arm64_digest, pin)

    def test_invalid_elf_pe32_and_truncated_headers_are_rejected(self):
        valid = pe32_plus_header()
        self.assertEqual(module.architectures(valid), {0x8664})
        with self.assertRaisesRegex(ValueError, "DOS header"):
            module.architectures(b"\x7fELF" + b"\x00" * 80)
        with self.assertRaisesRegex(ValueError, "DOS header"):
            module.architectures(valid[:16])
        with self.assertRaisesRegex(ValueError, "truncated PE"):
            module.architectures(valid[:0x80 + 10])
        # Corrupt PE\0\0 signature
        bad_sig = bytearray(valid)
        bad_sig[0x80 : 0x84] = b"NE\x00\x00"
        with self.assertRaisesRegex(ValueError, "PE executable"):
            module.architectures(bytes(bad_sig))
        # 32-bit PE32 (Optional Header magic 0x010B) must be rejected
        pe32 = pe32_plus_header(opt_magic=0x010B)
        with self.assertRaisesRegex(ValueError, "PE32\\+"):
            module.architectures(pe32)


if __name__ == "__main__":
    unittest.main()
