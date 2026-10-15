#!/usr/bin/env python3
"""Verify a Linux docker_wrapper artifact's checksum, upstream source pin and ELF64 e_machine."""
import argparse
import hashlib
from pathlib import Path
import struct

EM_AARCH64 = 0xB7
EM_X86_64 = 0x3E
CPU = {
    "aarch64-unknown-linux-gnu": EM_AARCH64,
    "x86_64-pc-linux-gnu": EM_X86_64,
}
MACHINE = CPU


def machine(data: bytes) -> int:
    if len(data) < 20:
        raise ValueError("truncated ELF header")
    if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
        raise ValueError("wrapper is not a 64-bit little-endian ELF executable")
    e_type, e_machine = struct.unpack_from("<HH", data, 16)
    if e_type not in (0, 2, 3):
        raise ValueError("wrapper is not an ELF executable")
    return e_machine


def architectures(data: bytes) -> set[int]:
    return {machine(data)}


def check(wrapper: Path, platform: str, digest: str, source_pin: Path) -> None:
    data = wrapper.read_bytes()
    if hashlib.sha256(data).hexdigest() != digest:
        raise ValueError("wrapper SHA-256 mismatch")
    if CPU[platform] not in architectures(data):
        raise ValueError("wrapper lacks the selected Linux architecture")
    root = Path(__file__).resolve().parents[2]
    pinned = [
        line.removeprefix("ARG BOINC_COMMIT=")
        for line in (root / "contrib-compute/images/base/Containerfile").read_text().splitlines()
        if line.startswith("ARG BOINC_COMMIT=")
    ]
    if len(pinned) != 1 or source_pin.read_text().strip() != pinned[0]:
        raise ValueError("wrapper BOINC_COMMIT differs from the server source pin")


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--wrapper", type=Path, required=True)
    p.add_argument("--platform", choices=sorted(CPU), default="aarch64-unknown-linux-gnu")
    p.add_argument("--sha256", required=True)
    p.add_argument("--source-pin", type=Path, required=True)
    a = p.parse_args()
    check(a.wrapper, a.platform, a.sha256, a.source_pin)


if __name__ == "__main__":
    main()
