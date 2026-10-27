#!/usr/bin/env python3
"""Patch BOINC's docker_wrapper.cpp so Run_Podman on macOS preserves the pipe in podman ps --format."""
import argparse
from pathlib import Path

ORIGINAL = '"ps --all --filter \\"name=^%s$\\" --format \\"{{.Names}}|{{.Status}}\\""'
PATCHED = '"ps --all --filter \\"name=^%s$\\" --format \\"\'{{.Names}}|{{.Status}}\'\\""'


def patch_source(text: str) -> str:
    count = text.count(ORIGINAL)
    if count != 1:
        raise ValueError(f"expected exactly 1 occurrence of the get_container_state format string, found {count}")
    return text.replace(ORIGINAL, PATCHED, 1)


def patch_file(path: Path) -> None:
    path = Path(path)
    path.write_text(patch_source(path.read_text()))


patch = patch_file


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    args = parser.parse_args()
    patch_file(args.source)


if __name__ == "__main__":
    main()
