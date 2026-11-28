#!/usr/bin/env python3
"""Build references, image paths and id lists for the #2107 early-stopping holdouts (finlam, La Liberte).

Diagnostics only. The holdouts are the last 10% of each training set in sha256("2107:tr:"+page_id) order
(62 finlam train pages, 200 of La Liberte's first 2,000). They are listed in the trained model's pages.json
(split "val"); no test page is read. Also reports whether the banded finder's step 1 tuning used them.

    prep.py --refs-dir /data/layout-refs/refs --pages-json .../pages.json --out-dir DIR
"""
import argparse
import copy
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "layout_columns"))
sys.path.insert(0, os.path.join(HERE, "..", "layout_train"))
import refs as refsmod  # noqa: E402
import classmap  # noqa: E402

SETS = {"finlam": ("Teklia__Newspapers-finlam", "finlam-train"),
        "liberte": ("Teklia__Newspapers-finlam-La-Liberte", "liberte-train")}


def rows(path):
    with open(path) as f:
        for ln in f:
            if ln.strip():
                yield json.loads(ln)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--refs-dir", required=True)
    ap.add_argument("--pages-json", required=True)
    ap.add_argument("--out-dir", required=True)
    a = ap.parse_args(argv)
    os.makedirs(a.out_dir, exist_ok=True)
    pages = json.load(open(a.pages_json))
    report = {}
    for short, (stem, tuning_name) in SETS.items():
        hold = sorted(p["page_id"] for p in pages if p["source"] == short and p["split"] == "val")
        train_rows = list(rows(os.path.join(a.refs_dir, stem + "__train.jsonl")))
        by_id = {r["page_id"]: r for r in train_rows}
        other = set()
        for sp in ("val", "test"):
            fn = os.path.join(a.refs_dir, "%s__%s.jsonl" % (stem, sp))
            if os.path.exists(fn):
                other |= {r["page_id"] for r in rows(fn)}
        # step 1 tuning pages (refs.build_set): finlam = every train page; La Liberte = first 500 in ll_order
        if short == "liberte":
            tuned = {r["page_id"] for r in sorted(train_rows, key=lambda r: refsmod.ll_order(r["page_id"]))[:refsmod.LL_PAGES]}
        else:
            tuned = set(by_id)
        # the training plan's order
        order = classmap.sha_order(list(by_id))
        if short == "liberte":
            order = order[:2000]
        tr, ho = classmap.split_in_order(order)
        report[short] = {
            "holdout_pages": len(hold), "holdout_equals_plan_order_tail": sorted(ho) == hold,
            "in_step1_tuning": len([p for p in hold if p in tuned]),
            "step1_tuning_pages": len(tuned),
            "in_val_or_test_splits": len([p for p in hold if p in other]),
            "in_trained_model_train_split": len([p for p in hold if p in set(tr)]),
        }
        refs, paths, regions = {}, {}, {}
        for pid in hold:
            row = copy.deepcopy(by_id[pid])
            ref = refsmod.row_reference(row)  # also rescales finlam percentage boxes in place
            refs[pid] = ref
            paths[pid] = row["image_path"]
            m = classmap.MAPS[row["dataset"]]
            regions[pid] = {"width": row["width"], "height": row["height"],
                            "regions": [[*r["box"], r["class"], m.get(r["class"], "?")] for k in ("text_regions", "nontext_regions") for r in row[k]]}
        json.dump({"for_publication": True, "pages": refs, "set": short + "-holdout"},
                  open(os.path.join(a.out_dir, "refs-%s.json" % short), "w"), sort_keys=True)
        json.dump(paths, open(os.path.join(a.out_dir, "paths-%s.json" % short), "w"), sort_keys=True)
        json.dump(regions, open(os.path.join(a.out_dir, "regions-%s.json" % short), "w"), sort_keys=True)
        report[short]["reference"] = refsmod.counts(refs)
    json.dump(report, open(os.path.join(a.out_dir, "prep-report.json"), "w"), indent=1, sort_keys=True)
    print(json.dumps(report, indent=1, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
