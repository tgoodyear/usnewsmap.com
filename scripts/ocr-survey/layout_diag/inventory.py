#!/usr/bin/env python3
"""Inventory of pages never scored, tuned on, trained on, held out or evaluated in #2107 (layout study).

Reads only reference rows (no image, no detector output). A page is "used" if it is in any of:
  - step 1 tuning: finlam train (all 623), La Liberte train's first 500 in sha256("2107:ll:"+id) order,
    Colorado train, de-rz, and (first attempt) de-rz / en-asgold / de-cg;
  - step 1 test and secondary sets: Europeana (all 50), finlam val and test, La Liberte val and test,
    Colorado val, en-asgold, de-cg;
  - the trained detector's data (#2107 training plan): finlam train, La Liberte train's first 2,000 in
    sha256("2107:tr:"+id) order (1,800 train + 200 holdout), Beyond Words train (2,846) -- and its
    evaluation sets, which are the step 1 sets plus Beyond Words val (712).
Data: Teklia/Newspapers-finlam and -La-Liberte (MIT), biglam/europeana-newspapers-ground-truth and
biglam/loc_beyond_words (CC0).

    inventory.py --refs-dir /data/layout-refs/refs --out-dir DIR [--prefix 2107:fresh:]
"""
import argparse
import collections
import hashlib
import json
import os
import sys

STEMS = {
    "finlam": "Teklia__Newspapers-finlam", "liberte": "Teklia__Newspapers-finlam-La-Liberte",
    "europeana": "biglam__europeana-newspapers-ground-truth", "beyondwords": "biglam__loc_beyond_words",
    "colorado": "NealCaren__co-newspaper-layout-yolo",
}


def rows(path):
    with open(path) as f:
        for ln in f:
            if ln.strip():
                yield json.loads(ln)


def sha_key(prefix, pid):
    return hashlib.sha256((prefix + str(pid)).encode()).hexdigest()


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--refs-dir", required=True)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--prefix", default="2107:fresh:")
    a = ap.parse_args(argv)
    os.makedirs(a.out_dir, exist_ok=True)
    inv = {}
    for src, stem in STEMS.items():
        for sp in ("train", "val", "test"):
            fn = os.path.join(a.refs_dir, "%s__%s.jsonl" % (stem, sp))
            if not os.path.exists(fn):
                continue
            rs = list(rows(fn))
            used = set()
            why = {}
            if src == "finlam":
                used = {r["page_id"] for r in rs}
                why = {"train": "step 1 tuning and detector training/holdout", "val": "step 1 secondary", "test": "step 1 test"}[sp]
            elif src == "liberte":
                if sp == "train":
                    ll = {r["page_id"] for r in sorted(rs, key=lambda r: sha_key("2107:ll:", r["page_id"]))[:500]}
                    tr = set(sorted((r["page_id"] for r in rs), key=lambda p: (sha_key("2107:tr:", p), p))[:2000])
                    used = ll | tr
                    why = "500 tuning (ll order) + 2000 detector training/holdout (tr order); overlap %d" % len(ll & tr)
                else:
                    used = {r["page_id"] for r in rs}
                    why = "step 1 " + ("secondary" if sp == "val" else "test")
            elif src == "europeana":
                used = {r["page_id"] for r in rs}
                why = "step 1 test (all 50 pages)"
            elif src == "beyondwords":
                used = {r["page_id"] for r in rs}
                why = "detector training/holdout (train) or mask test and evaluation (val)"
            elif src == "colorado":
                used = {r["page_id"] for r in rs}
                why = "step 1 tuning (train) or secondary (val)"
            fresh = [r for r in rs if r["page_id"] not in used]
            usable = [r for r in fresh if r["column_gutters"] or r["separators"]]
            lang = collections.Counter(r["meta"].get("language") for r in fresh)
            lang_u = collections.Counter(r["meta"].get("language") for r in usable)
            inv["%s/%s" % (src, sp)] = {"pages": len(rs), "used": len(used), "used_for": why, "fresh": len(fresh),
                                        "fresh_with_column_reference": len(usable), "fresh_languages": dict(lang),
                                        "fresh_with_reference_languages": dict(lang_u)}
            if fresh:
                order = sorted(fresh, key=lambda r: (sha_key(a.prefix, r["page_id"]), r["page_id"]))
                with open(os.path.join(a.out_dir, "fresh-%s-%s.tsv" % (src, sp)), "w") as f:
                    f.write("rank\tsha256_key\tpage_id\tcolumn_gutters\twidth\theight\tyear\tlanguage\timage_path\n")
                    for i, r in enumerate(order, 1):
                        f.write("%d\t%s\t%s\t%d\t%d\t%d\t%s\t%s\t%s\n" % (
                            i, sha_key(a.prefix, r["page_id"]), r["page_id"], len(r["column_gutters"]), r["width"], r["height"],
                            r["meta"].get("year"), r["meta"].get("language"), r["image_path"]))
                if src == "liberte":
                    for k, label in (("column_gutters", "gutters"),):
                        hs = [sum(s[3] - s[2] for g in r[k] for s in g) / r["height"] for r in fresh]
                        inv["%s/%s" % (src, sp)]["fresh_gutter_height_per_page_median"] = sorted(hs)[len(hs) // 2]
                    inv["%s/%s" % (src, sp)]["fresh_pages_by_year"] = dict(collections.Counter(r["meta"].get("year") for r in fresh))
                    inv["%s/%s" % (src, sp)]["fresh_image_size_median"] = [sorted(r["width"] for r in fresh)[len(fresh) // 2], sorted(r["height"] for r in fresh)[len(fresh) // 2]]
    json.dump(inv, open(os.path.join(a.out_dir, "inventory.json"), "w"), indent=1, sort_keys=True)
    print(json.dumps(inv, indent=1, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
