#!/usr/bin/env python3
"""Grid search of finder parameters on the tuning pages of the second attempt (#2107, step 1).

Tuning sets only: liberte-train (500 pages), finlam-train, colorado-train, derz. The tool refuses any
other set name, so the test and secondary sets cannot be run through it.

Method follows jscancella/NYTribuneOCRExperiments (findText_usingSums.py, GPL-3.0), reimplemented;
no code copied. Data: Teklia/Newspapers-finlam and -La-Liberte (MIT), NealCaren/co-newspaper-layout-
yolo (no licence stated; private, aggregates only), Reichsanzeiger-GT (CC BY 4.0, Mannheim University
Library).

    tune.py --sets-json '{"finlam-train": {"refs": "refs-finlam-train.json", "cache": "cache/finlam-train"}, ...}'
            --method banded|fullheight --grid '{"prominence": [0.4, 0.6]}' [--base-json '{...}']
            [--shard-index I --shard-count N] [--workers K] --out part-I.json
    tune.py --merge part-0.json part-1.json ... [--top 10]

A run is sharded by page: every shard writes, per configuration and set, the summed heights
[a+b, a, ref union, ref covered; coverage-precision det union, det covered, ref union, ref covered; one-to-one det, matched det, ref, matched ref; pages]; --merge adds the shards and ranks the configurations by
the equal-weight pooled F1 under the amended primary metric (precision a/(a+b) and coverage recall per
set, then the mean over sets); the coverage-precision and one-to-one pools are printed beside it.
"""
import argparse
import itertools
import json
import multiprocessing as mp
import sys
import time
from dataclasses import asdict

import numpy as np

import columns
import refs as refsmod
import score

_G = {}


def _init(sets, method, configs):
    _G["refs"] = {name: json.load(open(d["refs"]))["pages"] for name, d in sets.items()}
    _G["cache"] = {name: d["cache"] for name, d in sets.items()}
    _G["method"] = method
    _G["configs"] = configs


def _page(task):
    name, pid = task
    ref = _G["refs"][name][pid]
    gray, size = columns.load_cached(_G["cache"][name], pid)
    out = {}
    cur_key, ink = None, None
    for key, pd in _G["configs"]:
        p = columns.Params(**pd)
        ik = (p.ink_ratio, p.bg_window)
        if ik != cur_key:
            ink, cur_key = columns.binarize(gray, p), ik
        g = columns.detect(gray, size, _G["method"], p, ink)
        m = score.match_page(g, ref["gutters"], ref["width"], ref["excluded"], ref.get("zones"))
        out[key] = [m["ab_det"], m["a_det"], m["cov_ref_union"], m["cov_ref"],
                    m["cov_det_union"], m["cov_det"], m["cov_ref_union"], m["cov_ref"],
                    m["det_height"], m["matched_det_height"], m["ref_height"], m["matched_ref_height"]]
    return name, out


def run(a):
    sets = json.loads(a.sets_json) if a.sets_json.lstrip().startswith("{") else json.load(open(a.sets_json))
    bad = [n for n in sets if n not in refsmod.TUNING_SETS]
    if bad:
        raise SystemExit(f"tune.py works on the tuning sets only, not {bad}")
    grid = json.loads(a.grid)
    names = sorted(grid)
    base = json.loads(a.base_json) if a.base_json else {}
    configs = []
    for combo in itertools.product(*(grid[n] for n in names)):
        over = dict(base)
        over.update(dict(zip(names, combo)))
        p = columns.params_from(json.dumps(over))
        configs.append((json.dumps(dict(zip(names, combo)), sort_keys=True), asdict(p)))
    configs.sort(key=lambda c: (c[1]["ink_ratio"], c[1]["bg_window"], c[0]))
    tasks = []
    for name, d in sorted(sets.items()):
        for pid in sorted(json.load(open(d["refs"]))["pages"]):
            tasks.append((name, pid))
    idx = [int(x) for x in str(a.shard_index).split(",")]
    tasks = [t for i in idx for t in tasks[i:: a.shard_count]]
    acc = {k: {n: np.zeros(13) for n in sets} for k, _ in configs}
    t0 = time.time()
    done = 0
    with mp.Pool(a.workers, initializer=_init, initargs=(sets, a.method, configs)) as pool:
        for name, out in pool.imap_unordered(_page, tasks, chunksize=2):
            for k, v in out.items():
                acc[k][name][:12] += v
                acc[k][name][12] += 1
            done += 1
            if done % 50 == 0 or done == len(tasks):
                print(f"pages {done}/{len(tasks)} ({len(configs)} configs, {time.time() - t0:.0f}s)", flush=True)
    res = {k: {n: [float(x) for x in v] for n, v in s.items()} for k, s in acc.items()}
    res["_meta"] = {"base": base, "method": a.method, "shard_count": a.shard_count,
                    "shard_index": a.shard_index, "wall_seconds": round(time.time() - t0, 1)}
    with open(a.out, "w") as f:
        json.dump(res, f, sort_keys=True)
    return 0


def merge(a):
    tot, meta = {}, []
    for path in a.merge:
        d = json.load(open(path))
        meta.append(d.pop("_meta"))
        for k, s in d.items():
            for n, v in s.items():
                tot.setdefault(k, {}).setdefault(n, np.zeros(13))
                tot[k][n] += v
    rows = []
    for k, s in tot.items():
        pools = []
        for off in (0, 4, 8):
            v = {n: np.array([[x[off], x[off + 1], x[off + 2], x[off + 3]]]) for n, x in s.items()}
            pools.append(score.pool(v))
        rows.append((pools[0][2], k, pools))
    rows.sort(key=lambda x: -x[0])
    for f, k, pools in rows[: a.top]:
        pp, pr, pf, prs = pools[0]
        per = " ".join(f"{n}:{x[0]:.3f}/{x[1]:.3f}" for n, x in sorted(prs.items()))
        cov = f"P={pools[1][0]:.3f} R={pools[1][1]:.3f} F1={pools[1][2]:.3f}"
        o2o = f"P={pools[2][0]:.3f} R={pools[2][1]:.3f} F1={pools[2][2]:.3f}"
        print(f"F1={pf:.4f} P={pp:.4f} R={pr:.4f} {k} | primary {per} | coverage {cov} | 1:1 {o2o}")
    print(f"{len(rows)} configs; wall seconds per shard: {[m['wall_seconds'] for m in meta]}")
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--sets-json")
    ap.add_argument("--method", choices=sorted(columns.METHODS), default="banded")
    ap.add_argument("--grid")
    ap.add_argument("--base-json")
    ap.add_argument("--workers", type=int, default=1)
    ap.add_argument("--shard-index", default="0", help="shard number, or a comma list of them")
    ap.add_argument("--shard-count", type=int, default=1)
    ap.add_argument("--out")
    ap.add_argument("--merge", nargs="+")
    ap.add_argument("--top", type=int, default=10)
    a = ap.parse_args(argv)
    if a.merge:
        return merge(a)
    if not (a.sets_json and a.grid and a.out):
        ap.error("--sets-json, --grid and --out are required")
    return run(a)


if __name__ == "__main__":
    sys.exit(main())
