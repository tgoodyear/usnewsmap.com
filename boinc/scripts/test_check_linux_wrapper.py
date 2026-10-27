"""Reject altered, wrong-architecture and wrong-source Linux wrapper build artifacts before signing."""
import hashlib
import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("linux_wrapper", Path(__file__).with_name("check-linux-wrapper.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def elf64_header(e_machine: int, e_type: int = 2) -> bytes:
    # 16-byte e_ident (ELF64, little-endian, version 1) followed by e_type (<H) and e_machine (<H)
    ident = b"\x7fELF\x02\x01\x01" + b"\x00" * 9
    return ident + struct.pack("<HH", e_type, e_machine) + b"payload"


class CheckLinuxWrapper(unittest.TestCase):
    def test_architecture_checksum_and_source_are_all_required(self):
        self.assertEqual(module.EM_AARCH64, 0xB7)
        self.assertEqual(module.EM_AARCH64, 183)
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            wrapper = root / "docker_wrapper_aarch64-unknown-linux-gnu"
            wrapper.write_bytes(elf64_header(module.CPU["aarch64-unknown-linux-gnu"]))
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
            module.check(wrapper, "aarch64-unknown-linux-gnu", digest, pin)
            with self.assertRaisesRegex(ValueError, "architecture"):
                module.check(wrapper, "x86_64-pc-linux-gnu", digest, pin)
            with self.assertRaisesRegex(ValueError, "SHA-256"):
                module.check(wrapper, "aarch64-unknown-linux-gnu", "0" * 64, pin)
            pin.write_text("0" * 40)
            with self.assertRaisesRegex(ValueError, "source pin"):
                module.check(wrapper, "aarch64-unknown-linux-gnu", digest, pin)

    def test_invalid_and_truncated_elf_headers_are_rejected(self):
        valid = elf64_header(module.EM_AARCH64)
        self.assertEqual(module.architectures(valid), {0xB7})
        with self.assertRaisesRegex(ValueError, "truncated"):
            module.architectures(valid[:12])
        with self.assertRaisesRegex(ValueError, "ELF"):
            module.architectures(b"\xcf\xfa\xed\xfe" + b"\x00" * 20)
        # 32-bit ELF (EI_CLASS = 1) must be rejected
        elf32 = b"\x7fELF\x01\x01\x01" + b"\x00" * 9 + struct.pack("<HH", 2, module.EM_AARCH64)
        with self.assertRaisesRegex(ValueError, "64-bit"):
            module.architectures(elf32)


if __name__ == "__main__":
    unittest.main()
