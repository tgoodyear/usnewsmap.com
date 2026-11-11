#!/usr/bin/env python3
"""Patch BOINC's plan class specs with <max_gpu_ram_mb>, a ceiling on the GPU's RAM.

The pinned BOINC commit parses min_gpu_ram_mb but has no maximum, and the plan class parser
skips unknown tags without an error, so <max_gpu_ram_mb> alone would cap nothing. With this
patch a GPU plan class with max_gpu_ram_mb rejects a GPU reporting more RAM than that, measured
as min_gpu_ram_mb is (MiB of the CUDA, OpenCL or Apple total). nvidia_vulkan_6gb_8b uses it
to stay off the cards that nvidia_vulkan_8b admits (#1234).
"""
from __future__ import annotations

import argparse
from pathlib import Path

HEADER_ORIGINAL = "    double min_gpu_ram_mb;\n"
HEADER_PATCHED = HEADER_ORIGINAL + "    double max_gpu_ram_mb;\n"

PARSE_ORIGINAL = '        if (xp.parse_double("min_gpu_ram_mb", min_gpu_ram_mb)) continue;\n'
PARSE_PATCHED = PARSE_ORIGINAL + '        if (xp.parse_double("max_gpu_ram_mb", max_gpu_ram_mb)) continue;\n'

INIT_ORIGINAL = "    min_gpu_ram_mb = 0;\n"
INIT_PATCHED = INIT_ORIGINAL + "    max_gpu_ram_mb = 0;\n"

CHECK_ORIGINAL = """\
        if (min_gpu_ram_mb && min_gpu_ram_mb * MEGA > gpu_ram) {
            if (config.debug_version_select) {
                log_messages.printf(MSG_NORMAL,
                    "[version] plan_class_spec: GPU RAM required min: %f, supplied: %f\\n",
                    min_gpu_ram_mb * MEGA, gpu_ram
                );
            }
            return false;
        }
"""
CHECK_PATCHED = CHECK_ORIGINAL + """\
        if (max_gpu_ram_mb && gpu_ram > max_gpu_ram_mb * MEGA) {
            if (config.debug_version_select) {
                log_messages.printf(MSG_NORMAL,
                    "[version] plan_class_spec: GPU RAM allowed max: %f, supplied: %f\\n",
                    max_gpu_ram_mb * MEGA, gpu_ram
                );
            }
            return false;
        }
"""


def replace_once(text: str, label: str, original: str, patched: str) -> str:
    count = text.count(original)
    if count != 1 or patched in text:
        raise ValueError(f"expected exactly 1 unpatched {label}, found {count}")
    return text.replace(original, patched, 1)


def patch_header(text: str) -> str:
    return replace_once(text, "min_gpu_ram_mb member", HEADER_ORIGINAL, HEADER_PATCHED)


def patch_source(text: str) -> str:
    for label, original, patched in (
        ("min_gpu_ram_mb parse", PARSE_ORIGINAL, PARSE_PATCHED),
        ("min_gpu_ram_mb default", INIT_ORIGINAL, INIT_PATCHED),
        ("min_gpu_ram_mb check", CHECK_ORIGINAL, CHECK_PATCHED),
    ):
        text = replace_once(text, label, original, patched)
    return text


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True, help="sched/plan_class_spec.cpp")
    parser.add_argument("--header", type=Path, required=True, help="sched/plan_class_spec.h")
    args = parser.parse_args()
    # Validate both before writing either.
    updates = {args.source: patch_source(args.source.read_text()),
               args.header: patch_header(args.header.read_text())}
    for path, text in updates.items():
        path.write_text(text)


if __name__ == "__main__":
    main()
