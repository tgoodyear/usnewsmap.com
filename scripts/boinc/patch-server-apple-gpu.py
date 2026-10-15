#!/usr/bin/env python3
"""Patch BOINC's sched/plan_class_spec.cpp so both eligibility and resource dispatch accept apple / apple_gpu."""
from __future__ import annotations

import argparse
from pathlib import Path

ELIGIBILITY_ORIGINAL = '} else if (!strcmp(gpu_type, "apple")) {'
DISPATCH_ORIGINAL = '} else if (!strcmp(gpu_type, "apple_gpu")) {'
PATCHED = '} else if (!strcmp(gpu_type, "apple") || !strcmp(gpu_type, "apple_gpu")) {'


def patch_source(text: str) -> str:
    for label, target in (("eligibility", ELIGIBILITY_ORIGINAL), ("dispatch", DISPATCH_ORIGINAL)):
        count = text.count(target)
        if count != 1:
            raise ValueError(f"expected exactly 1 occurrence of the {label} branch {target!r}, found {count}")
    text = text.replace(ELIGIBILITY_ORIGINAL, PATCHED, 1)
    text = text.replace(DISPATCH_ORIGINAL, PATCHED, 1)
    return text


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
