#!/usr/bin/env python3
"""Grid search of finder parameters on the tuning set de-rz only (#2107, step 1).

Refuses any page that is not a de-rz page, so the test sets cannot be tuned on.

Method follows jscancella/NYTribuneOCRExperiments (findText_usingSums.py, GPL-3.0), reimplemented;
no code copied. Tuning data: Reichsanzeiger-GT, CC BY 4.0, Mannheim University Library.

    tune.py --images DIR --refs ref-de-rz.json --cache-dir DIR --method banded|fullheight
            --grid '{"prominence": [0.4, 0.6], "min_gutter": [0.004, 0.008]}' --out tune.json
            [--base-json '{...}'] [--workers 6] [--shard-index I --shard-count N]

Output: keyed JSON, keys sorted: config key -> {f1, params, precision, recall}.
"""
import argparse
import itertools
import json
import multiprocessing as mp
import os
import sys
import time
from dataclasses import asdict

import columns
import score


def _page(args):
    pid, path, method, pdict, cache_dir, ref = args
    p = columns.Params(**pdict)
    gray, size = columns.load_gray(path, p.work_width, cache_dir)
    g = columns.detect(gray, size, method, p)
    return pid, score.match_page(g, ref["gutters"], ref["width"], ref["excluded"])


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--images", required=True)
    ap.add_argument("--refs", required=True)
    ap.add_argument("--cache-dir", required=True)
    ap.add_argument("--method", choices=sorted(columns.METHODS), default="banded")
    ap.add_argument("--grid", required=True, help="JSON object: parameter -> list of values")
    ap.add_argument("--base-json", help="JSON object of fixed parameter overrides")
    ap.add_argument("--workers", type=int, default=1)
    ap.add_argument("--shard-index", type=int, default=0)
    ap.add_argument("--shard-count", type=int, default=1)
    ap.add_argument("--out", required=True)
    a = ap.parse_args(argv)
    refs = json.load(open(a.refs))
    if any(not k.startswith("s9-de-rz-") for k in refs):
        raise SystemExit("tune.py works on de-rz only")
    grid = json.loads(a.grid)
    names = sorted(grid)
    combos = list(itertools.product(*(grid[n] for n in names)))[a.shard_index:: a.shard_count]
    base = json.loads(a.base_json) if a.base_json else {}
    paths = {os.path.splitext(f)[0]: os.path.join(a.images, f) for f in os.listdir(a.images)}
    res = {}
    t0 = time.time()
    pool = mp.Pool(a.workers) if a.workers > 1 else None
    for ci, combo in enumerate(combos, 1):
        over = dict(base)
        over.update(dict(zip(names, combo)))
        p = columns.params_from(json.dumps(over))
        jobs = [(pid, paths[pid], a.method, asdict(p), a.cache_dir, refs[pid]) for pid in sorted(refs)]
        rows = list(pool.imap_unordered(_page, jobs)) if pool else [_page(j) for j in jobs]
        pr, rc, f1 = score.pooled([r for _, r in rows])
        key = json.dumps(dict(zip(names, combo)), sort_keys=True)
        res[key] = {"f1": round(f1, 4), "params": over, "precision": round(pr, 4), "recall": round(rc, 4)}
        print(f"configs {ci}/{len(combos)} f1={f1:.3f} p={pr:.3f} r={rc:.3f} {key}", flush=True)
    with open(a.out, "w") as f:
        json.dump(res, f, sort_keys=True, indent=1)
    print(f"done in {time.time() - t0:.0f}s")
    return 0


if __name__ == "__main__":
    sys.exit(main())
