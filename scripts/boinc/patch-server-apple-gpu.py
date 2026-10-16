#!/usr/bin/env python3
"""Patch BOINC's sched/plan_class_spec.cpp so both eligibility and resource dispatch accept apple / apple_gpu."""
from __future__ import annotations

import argparse
from pathlib import Path

ELIGIBILITY_ORIGINAL = '} else if (!strcmp(gpu_type, "apple")) {'
ELIGIBILITY_PATCHED = '} else if (!strcmp(gpu_type, "apple") || !strcmp(gpu_type, "apple_gpu")) {'
DISPATCH_ORIGINAL = '} else if (strstr(gpu_type, "apple_gpu") == gpu_type) {'
DISPATCH_PATCHED = '} else if (strstr(gpu_type, "apple_gpu") == gpu_type || !strcmp(gpu_type, "apple")) {'

BATCH_INFEASIBLE_ORIGINAL = """\
    if (pc->max_batch && wu->batch > pc->max_batch) {
        if (config.debug_version_select) {
            log_messages.printf(MSG_NORMAL,
                "[version] batch#%d too new for plan class '%s' (%ld)\\n",
                wu->batch, pc->name, pc->max_batch
            );
        }
        return true;
    }
    return false;"""

BATCH_INFEASIBLE_PATCHED = """\
    if (pc->max_batch && wu->batch > pc->max_batch) {
        if (config.debug_version_select) {
            log_messages.printf(MSG_NORMAL,
                "[version] batch#%d too new for plan class '%s' (%ld)\\n",
                wu->batch, pc->name, pc->max_batch
            );
        }
        return true;
    }
    if (wu && wu->batch > 0) {
        const int expected_batch = pc->docker
            ? (!strcmp(pc->name, "docker_mt_2b") ? 12
            : (!strcmp(pc->name, "docker_nvidia_cuda_8b") ? 18
            : (!strcmp(pc->name, "docker_intel_opencl_4b") ? 104 : 1)))
            : (strstr(pc->name, "_8b") != NULL ? 8
            : ((strstr(pc->name, "_2b") != NULL || !strcmp(pc->name, "mt")) ? 2 : 0));
        if (expected_batch && wu->batch != expected_batch) {
            return true;
        }
    }
    return false;"""

BATCH_CHECK_ORIGINAL = """\
    if (min_wu_id || max_wu_id || min_batch || max_batch) {
        if (wu_is_infeasible_for_plan_class(this, wu)) {
            return false;
        }
    }"""

BATCH_CHECK_PATCHED = """\
    if (min_wu_id || max_wu_id || min_batch || max_batch || (wu && wu->batch > 0)) {
        wu_restricted_plan_class = true;
        if (wu_is_infeasible_for_plan_class(this, wu)) {
            return false;
        }
    }"""

def patch_source(text: str) -> str:
    for label, target, replacement in (
        ("eligibility", ELIGIBILITY_ORIGINAL, ELIGIBILITY_PATCHED),
        ("dispatch", DISPATCH_ORIGINAL, DISPATCH_PATCHED),
        ("batch infeasibility gate", BATCH_INFEASIBLE_ORIGINAL, BATCH_INFEASIBLE_PATCHED),
        ("batch restriction check", BATCH_CHECK_ORIGINAL, BATCH_CHECK_PATCHED),
    ):
        count = text.count(target)
        if count != 1:
            raise ValueError(f"expected exactly 1 occurrence of the {label} branch {target!r}, found {count}")
        text = text.replace(target, replacement, 1)
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
