#!/usr/bin/env python3
"""Verify a Windows docker_wrapper artifact's checksum, upstream source pin and PE32+ x86_64 header."""
from __future__ import annotations

import argparse
import hashlib
from pathlib import Path
import struct

IMAGE_FILE_MACHINE_AMD64 = 0x8664
PE32_PLUS_MAGIC = 0x020B
CPU = {
    "windows_x86_64": IMAGE_FILE_MACHINE_AMD64,
}
MACHINE = CPU


def machine(data: bytes) -> int:
    if len(data) < 0x40 or data[:2] != b"MZ":
        raise ValueError("truncated or non-MZ DOS header")
    pe_offset = struct.unpack_from("<I", data, 0x3C)[0]
    if pe_offset < 0x40 or len(data) < pe_offset + 26:
        raise ValueError("truncated PE header")
    if data[pe_offset : pe_offset + 4] != b"PE\x00\x00":
        raise ValueError("wrapper is not a PE executable")
    coff_machine, _sections, _ts, _sym, _nsym, opt_size, _chars = struct.unpack_from(
        "<HHIIIHH", data, pe_offset + 4
    )
    opt_magic = struct.unpack_from("<H", data, pe_offset + 24)[0]
    if opt_size < 2 or opt_magic != PE32_PLUS_MAGIC:
        raise ValueError("wrapper is not a 64-bit PE32+ executable")
    return coff_machine


def architectures(data: bytes) -> set[int]:
    return {machine(data)}


def check(wrapper: Path, platform: str, digest: str, source_pin: Path) -> None:
    data = wrapper.read_bytes()
    if hashlib.sha256(data).hexdigest() != digest:
        raise ValueError("wrapper SHA-256 mismatch")
    if CPU[platform] not in architectures(data):
        raise ValueError("wrapper lacks the selected Windows architecture")
    root = Path(__file__).resolve().parents[2]
    pinned = [
        line.removeprefix("ARG BOINC_COMMIT=")
        for line in (root / "boinc/images/base/Containerfile").read_text().splitlines()
        if line.startswith("ARG BOINC_COMMIT=")
    ]
    if len(pinned) != 1 or source_pin.read_text().strip() != pinned[0]:
        raise ValueError("wrapper BOINC_COMMIT differs from the server source pin")


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--wrapper", type=Path, required=True)
    p.add_argument("--platform", choices=sorted(CPU), default="windows_x86_64")
    p.add_argument("--sha256", required=True)
    p.add_argument("--source-pin", type=Path, required=True)
    a = p.parse_args()
    check(a.wrapper, a.platform, a.sha256, a.source_pin)


if __name__ == "__main__":
    main()
