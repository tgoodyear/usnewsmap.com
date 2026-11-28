#!/usr/bin/env python3
"""Descriptive numbers for the German fresh-page test (#2107), outside the decision rule.

Reads the files combo_eval.py run wrote (gutters.json, as-boxes.json, refs.json, page-ids.txt) and scores,
with the frozen scorer, banded alone, banded plus AS, and AS `article` gutters alone, pooled over all
pages and per title and per decade (page id prefix, first four-digit year in the id). No detector runs.

    cg_extra.py --out-dir D
"""
import argparse
import json
import os
import re
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "layout_columns"))
import diag  # noqa: E402
import score  # noqa: E402


def prf(v):
    p, r = score.set_pr(v)
    return {"f1": 2 * p * r / (p + r) if p + r else 0.0, "precision": p, "recall": r}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--out-dir", required=True)
    a = ap.parse_args(argv)
    d = a.out_dir
    refs = json.load(open(os.path.join(d, "refs.json")))["pages"]
    ids = [ln.strip() for ln in open(os.path.join(d, "page-ids.txt")) if ln.strip()]
    sets = json.load(open(os.path.join(d, "gutters.json")))
    boxes = json.load(open(os.path.join(d, "as-boxes.json")))
    sets["as_alone"] = {p: diag.gutters_generic(boxes[p], (refs[p]["width"], refs[p]["height"]), 0.01, diag.ARTICLE, diag.OBSTACLE) for p in ids}
    vec = {}
    for name, g in sets.items():
        rows = score.page_rows({p: {"gutters": g[p]} for p in ids}, refs)
        vec[name] = score._vec([rows[p] for p in ids], "primary")
    title = [re.sub(r"_\d.*", "", p) for p in ids]
    dec = [str(int(re.search(r"_(\d{4})", p).group(1)) // 10 * 10) + "s" for p in ids]
    out = {"all": {n: prf(v) for n, v in vec.items()}, "n_gutters_as_alone": sum(len(sets["as_alone"][p]) for p in ids)}
    for label, groups in (("per_title", title), ("per_decade", dec)):
        res = {}
        for g in sorted(set(groups)):
            idx = np.array([i for i, x in enumerate(groups) if x == g])
            res[g] = dict({"pages": len(idx)}, **{n: prf(v[idx]) for n, v in vec.items()})
        out[label] = res
    json.dump(out, open(os.path.join(d, "extra.json"), "w"), indent=1, sort_keys=True)
    print(json.dumps(out, indent=1, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
