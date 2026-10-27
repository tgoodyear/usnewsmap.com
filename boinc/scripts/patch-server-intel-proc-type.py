#!/usr/bin/env python3
"""Patch BOINC's sched/sched_util_basic.cpp so Intel plan classes count as Intel GPU apps.

Stock plan_class_to_proc_type() knows Intel only by "intel_gpu" in the name, so the feeder
records no Intel GPU app for docker_intel_opencl_4b or intel_vulkan_8b. Every scheduler
reply then carries <no_rsc_apps>intel_gpu</no_rsc_apps>, and the client stops asking for
Intel GPU work (#736). Every project plan class naming "intel" has <gpu_type>intel</gpu_type>.
"""
from __future__ import annotations

import argparse
from pathlib import Path

ORIGINAL = """\
    if (strstr(plan_class, "intel_gpu")) {
        return PROC_TYPE_INTEL_GPU;
    }"""

PATCHED = """\
    if (strstr(plan_class, "intel")) {
        return PROC_TYPE_INTEL_GPU;
    }"""


def patch_source(text: str) -> str:
    count = text.count(ORIGINAL)
    if count != 1:
        raise ValueError(f"expected exactly 1 Intel branch in plan_class_to_proc_type, found {count}")
    return text.replace(ORIGINAL, PATCHED, 1)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    args = parser.parse_args()
    args.source.write_text(patch_source(args.source.read_text()))


if __name__ == "__main__":
    main()
