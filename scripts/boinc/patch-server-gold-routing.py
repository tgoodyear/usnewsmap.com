#!/usr/bin/env python3
"""Bind Gold scheduling to the staged job and selected immutable app payload."""
import argparse
from pathlib import Path

ORIGINAL = """bool wu_is_infeasible_custom(
    WORKUNIT& wu,
    APP& /*app*/,
    BEST_APP_VERSION& bav
) {"""
PATCHED = """bool wu_is_infeasible_custom(
    WORKUNIT& wu,
    APP& app,
    BEST_APP_VERSION& bav
) {
    if (!strcmp(app.name, "usnm_gold")) {
        try {
            if (!bav.avp) throw std::runtime_error("anonymous Gold app has no registered payload");
            const PLATFORM* platform = ssp->lookup_platform_id(bav.avp->platformid);
            if (!platform) throw std::runtime_error("Gold app platform missing");
            static const auto bindings = nlohmann::json::parse(usnm_gold::read_file(
                "/usr/local/lib/usnm-boinc/gold-engine-platforms.json"
            ));
            usnm_gold::compatible(wu.name, platform->name, bav.avp->xml_doc, bindings,
                [](const std::string& name) {
                    char path[4096];
                    if (config.download_path(name.c_str(), path)) {
                        throw std::runtime_error("Gold download path lookup failed");
                    }
                    return std::string(path);
                }
            );
        } catch (const std::exception& e) {
            log_messages.printf(MSG_NORMAL,
                "[Gold routing] refusing WU %s: %s\\n", wu.name, e.what()
            );
            return true;
        }
    }"""

ASSIGNMENT_ORIGINAL = '    rtfpath = config.project_path("%s", wu.result_template_file);'
ASSIGNMENT_PATCHED = """    APP* gold_app = ssp->lookup_app(wu.appid);
    if (gold_app && !strcmp(gold_app->name, "usnm_gold")
        && wu_is_infeasible_custom(wu, *gold_app, *bavp)) {
        return ERR_NOT_FOUND;
    }

""" + ASSIGNMENT_ORIGINAL


def patch_source(text):
    if text.count(ORIGINAL) != 1 or text.count('#include "sched_customize.h"') != 1:
        raise ValueError("expected exactly one pinned scheduler hook and include")
    return text.replace(
        '#include "sched_customize.h"',
        '#include "sched_customize.h"\n#include "gold-engine-routing.h"',
        1,
    ).replace(ORIGINAL, PATCHED, 1)


def patch_assignment(text):
    if text.count(ASSIGNMENT_ORIGINAL) != 1 or text.count('#include "sched_assign.h"') != 1:
        raise ValueError("expected exactly one pinned assignment send point and include")
    if 'APP* gold_app' in text:
        raise ValueError("Gold assignment gate already patched")
    return text.replace(
        '#include "sched_assign.h"',
        '#include "sched_assign.h"\n#include "sched_customize.h"\n#include "sched_shmem.h"',
        1,
    ).replace(ASSIGNMENT_ORIGINAL, ASSIGNMENT_PATCHED, 1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--assignment", type=Path, required=True)
    args = parser.parse_args()
    source = patch_source(args.source.read_text())
    assignment = patch_assignment(args.assignment.read_text())
    args.source.write_text(source)
    args.assignment.write_text(assignment)


if __name__ == "__main__":
    main()
