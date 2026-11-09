#!/usr/bin/env python3
"""Expose stock BOINC's targeted-work transitioner flag without an assignment.

Used to stage an exact audited acceptance WU before its target hosts are known.
The scheduler still creates results, with the original quorum and bounds.
"""
import argparse
from pathlib import Path

REPLACEMENTS = (
    ('        "   [ --stdin ]\\n"\n',
     '        "   [ --stdin ]\\n"\n'
     '        "   [ --no_new_results ]   stage for later targeted assignment\\n"\n'),
    ('    bool assign_flag;\n', '    bool assign_flag;\n    bool no_new_results = false;\n'),
    ('        } else if (arg(argv, i, "target_host")) {\n            jd.assign_flag = true;\n',
     '        } else if (arg(argv, i, "no_new_results")) {\n'
     '            jd.no_new_results = true;\n'
     '        } else if (arg(argv, i, "target_host")) {\n            jd.assign_flag = true;\n'),
    ('void JOB_DESC::create() {\n',
     'void JOB_DESC::create() {\n'
     '    if (no_new_results) {\n'
     '        wu.transitioner_flags = TRANSITION_NO_NEW_RESULTS;\n'
     '    }\n'),
    ('    if (!strlen(app.name)) {\n',
     '    if (jd.no_new_results && (use_stdin || jd.assign_flag || jd.wu.id)) {\n'
     '        fprintf(stderr, "--no_new_results requires a new single unassigned workunit\\n");\n'
     '        exit(1);\n'
     '    }\n'
     '    if (!strlen(app.name)) {\n'),
)


def patch_source(text):
    if "no_new_results" in text:
        raise ValueError('create_work staging patch already present or upstream changed')
    for old, new in REPLACEMENTS:
        if text.count(old) != 1:
            raise ValueError('unexpected pinned create_work source: ' + old.strip())
        text = text.replace(old, new, 1)
    return text


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    args = parser.parse_args()
    args.source.write_text(patch_source(args.source.read_text()))
