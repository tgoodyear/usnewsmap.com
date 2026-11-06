#!/usr/bin/env python3
"""Expose the pinned feeder's startup slot allocation for read-only drift detection."""

import argparse
from pathlib import Path


REPLACEMENTS = (
    (
        '#include "hr_info.h"\n',
        '#include "hr_info.h"\n#include "usnm-feeder-guard.h"\n',
    ),
    (
        "bool is_main_feeder = true;\n",
        "UsnmFeederGuard usnm_feeder_guard;\nbool is_main_feeder = true;\n",
    ),
    (
        "        ssp->init(num_work_items);\n"
        "        ssp->scan_tables();\n"
        "        ssp->perf_info.get_from_db();\n",
        "        usnm_feeder_guard.begin_reload();\n"
        "        ssp->init(num_work_items);\n"
        "        int usnm_scan_result = ssp->scan_tables();\n"
        "        usnm_feeder_guard.end_reload(ssp, usnm_scan_result);\n"
        "        ssp->perf_info.get_from_db();\n",
    ),
    (
        "        check_reread_trigger();\n",
        "        check_reread_trigger();\n"
        "        usnm_feeder_guard.heartbeat();\n",
    ),
    (
        '    ssp->scan_tables();\n\n    log_messages.printf(MSG_NORMAL,\n        "read "\n',
        '    int usnm_scan_result = ssp->scan_tables();\n'
        '    usnm_feeder_guard.capture(ssp, all_apps, usnm_scan_result);\n\n'
        '    log_messages.printf(MSG_NORMAL,\n        "read "\n',
    ),
    (
        "    hr_init();\n\n    if (using_hr && strlen(order_clause)) {\n",
        '    hr_init();\n    usnm_feeder_guard.start();\n\n'
        "    if (using_hr && strlen(order_clause)) {\n",
    ),
)


def patch_source(text):
    if "usnm-feeder-guard.h" in text:
        raise ValueError("feeder guard patch already present")
    for old, new in REPLACEMENTS:
        if text.count(old) != 1:
            raise ValueError("unexpected pinned feeder source: " + old.strip())
        text = text.replace(old, new, 1)
    return text


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    args = parser.parse_args()
    args.source.write_text(patch_source(args.source.read_text()))
