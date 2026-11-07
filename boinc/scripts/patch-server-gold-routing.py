#!/usr/bin/env python3
"""Bind Gold scheduling to the staged job and selected immutable app payload."""
import argparse
from pathlib import Path

ORIGINAL = """bool wu_is_infeasible_custom(
    WORKUNIT& wu,
    APP& /*app*/,
    BEST_APP_VERSION& bav
) {"""
GATE = """bool usnm_gold_is_infeasible(
    WORKUNIT& wu,
    APP& app,
    BEST_APP_VERSION& bav
) {
    const bool native = !strcmp(app.name, "usnm_gold_native");
    if (native || !strcmp(app.name, "usnm_gold")) {
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
                },
                [](const std::string& name) {
                    return std::string(config.download_dir) + "/" + name;
                },
                native
            );
        } catch (const std::exception& e) {
            log_messages.printf(MSG_NORMAL,
                "[Gold routing] refusing WU %s: %s\\n", wu.name, e.what()
            );
            return true;
        }
    }
    return false;
}
"""
PATCHED = GATE + "\n" + ORIGINAL

ASSIGNMENT_ORIGINAL = '    rtfpath = config.project_path("%s", wu.result_template_file);'
ASSIGNMENT_PATCHED = """    APP* gold_app = ssp->lookup_app(wu.appid);
    if (gold_app && (!strcmp(gold_app->name, "usnm_gold") || !strcmp(gold_app->name, "usnm_gold_native"))
        && usnm_gold_is_infeasible(wu, *gold_app, *bavp)) {
        return ERR_NOT_FOUND;
    }

""" + ASSIGNMENT_ORIGINAL


def patch_source(text):
    if (text.count(ORIGINAL) != 1 or text.count('#include "sched_customize.h"') != 1
            or "usnm_gold_is_infeasible" in text):
        raise ValueError("expected exactly one pinned scheduler hook and include")
    return text.replace(
        '#include "sched_customize.h"',
        '#include "sched_customize.h"\n#include "gold-engine-routing.h"',
        1,
    ).replace(ORIGINAL, PATCHED, 1)


def patch_slow_check(text):
    target = "    WORKUNIT& wu = wu_result.workunit;"
    return insert_gate(text, target, target + """
    if (usnm_gold_is_infeasible(wu, *app, *bavp)) {
        return CHECK_NO_HOST;
    }
""", '#include "sched_check.h"')


def patch_locality(text):
    target = "    APP* app = ssp->lookup_app(wu.appid);"
    return insert_gate(text, target, target + """
    if (usnm_gold_is_infeasible(wu, *app, *bavp)) {
        return INFEASIBLE_CUSTOM;
    }
""", '#include "sched_check.h"')


RESEND_ORIGINAL = "        if (can_resend && wu.error_mask) {"
RESEND_GATE = """        if (can_resend && usnm_gold_is_infeasible(wu, *app, *bavp)) {
            continue;
        }
"""


def patch_resend(text):
    return insert_gate(text, RESEND_ORIGINAL, RESEND_GATE + RESEND_ORIGINAL,
                       '#include "sched_check.h"')


def insert_gate(text, target, replacement, include):
    if text.count(target) != 1 or text.count(include) != 1 or "usnm_gold_is_infeasible" in text:
        raise ValueError("expected one unpatched unlocked dispatch point and include")
    return text.replace(include, include + '\n#include "sched_customize.h"', 1).replace(
        target, replacement, 1
    )


def patch_declaration(text):
    target = "extern bool wu_is_infeasible_custom(WORKUNIT&, APP&, BEST_APP_VERSION&);"
    if text.count(target) != 1 or "usnm_gold_is_infeasible" in text:
        raise ValueError("expected exactly one unpatched scheduler declaration")
    return text.replace(target, target + "\nextern bool usnm_gold_is_infeasible(WORKUNIT&, APP&, BEST_APP_VERSION&);", 1)


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
    parser.add_argument("--sched-dir", type=Path, required=True)
    args = parser.parse_args()
    patches = {
        "sched_customize.cpp": patch_source,
        "sched_customize.h": patch_declaration,
        "sched_assign.cpp": patch_assignment,
        "sched_check.cpp": patch_slow_check,
        "sched_locality.cpp": patch_locality,
        "sched_resend.cpp": patch_resend,
    }
    updates = {args.sched_dir / name: patch((args.sched_dir / name).read_text())
               for name, patch in patches.items()}
    for path, text in updates.items():
        path.write_text(text)


if __name__ == "__main__":
    main()
