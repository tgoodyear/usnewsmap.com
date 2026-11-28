#!/usr/bin/env python3
"""One-time evaluation of the banded column finder plus American Stories gutters (#2107), fresh La Liberte pages.

Plan frozen before scoring (issue #2107, plan comment). Contender: the frozen banded finder, plus
American Stories (conf 0.01, agnostic NMS IoU 0.1, letterbox 1280) `article` gutters at x positions where
no banded gutter sits (diag.add_where_none); banded gutters are not removed. Baseline: banded alone.
Metric: layout_columns/score.py primary (a/(a+b) precision, coverage recall, gap cap 3%, zone shrink 1%).
Interval: paired page bootstrap, 1,000 draws, seed 2107, 2.5/97.5 percentiles.
Method for the finder follows jscancella/NYTribuneOCRExperiments (findText_usingSums.py, GPL-3.0),
reimplemented; detector: American Stories (Dell et al., arXiv:2308.12477) via Ultralytics-format ONNX.
Data: Teklia/Newspapers-finlam-La-Liberte (MIT).

    combo_eval.py select --refs-dir D --out-dir D
    combo_eval.py run --out-dir D --model PATH [--threads 8]      (after pagecache.py on out-dir/paths.json)
"""
import argparse
import hashlib
import json
import os
import sys
import time

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "layout_columns"))
sys.path.insert(0, os.path.join(HERE, "..", "layout_train"))
import columns  # noqa: E402
import refs as refsmod  # noqa: E402
import score  # noqa: E402
import asmodel  # noqa: E402
import diag  # noqa: E402

N_PAGES = 500
LL_EXCLUDE, TR_EXCLUDE = 500, 2000
MODEL_SHA_PREFIX, MODEL_SHA_SUFFIX = "045b2e55", "9155e"


def key(prefix, pid):
    return hashlib.sha256((prefix + pid).encode()).hexdigest()


def select(a):
    rows = list(refsmod.read_rows(os.path.join(a.refs_dir, refsmod.FILES["liberte-train"])))
    ids = sorted({r["page_id"] for r in rows})
    ll = set(sorted(ids, key=lambda p: (key("2107:ll:", p), p))[:LL_EXCLUDE])
    tr = set(sorted(ids, key=lambda p: (key("2107:tr:", p), p))[:TR_EXCLUDE])
    by = {r["page_id"]: r for r in rows}
    fresh = sorted((p for p in ids if p not in ll and p not in tr), key=lambda p: (key("2107:fresh:", p), p))
    chosen, refs, paths, skipped = [], {}, {}, 0
    for p in fresh:
        if len(chosen) == N_PAGES:
            break
        ref = refsmod.row_reference(by[p])
        if not ref["gutters"]:
            skipped += 1
            continue
        chosen.append(p)
        refs[p] = ref
        paths[p] = by[p]["image_path"]
    os.makedirs(a.out_dir, exist_ok=True)
    listing = "\n".join(chosen) + "\n"
    open(os.path.join(a.out_dir, "page-ids.txt"), "w").write(listing)
    digest = hashlib.sha256(listing.encode()).hexdigest()
    open(os.path.join(a.out_dir, "page-ids.sha256"), "w").write(digest + "\n")
    json.dump({"for_publication": True, "pages": refs, "set": "liberte-fresh"}, open(os.path.join(a.out_dir, "refs.json"), "w"), sort_keys=True)
    json.dump(paths, open(os.path.join(a.out_dir, "paths.json"), "w"), sort_keys=True)
    info = {"train_pages": len(ids), "excluded_ll": len(ll), "excluded_tr": len(tr), "excluded_union": len(ll | tr),
            "fresh_pages": len(fresh), "skipped_without_gutter": skipped, "selected": len(chosen), "page_ids_sha256": digest}
    json.dump(info, open(os.path.join(a.out_dir, "selection.json"), "w"), indent=1, sort_keys=True)
    print(json.dumps(info, sort_keys=True))
    return 0


def bootstrap_paired(va, vb):
    """Paired page bootstrap of contender (vb) minus baseline (va); rows [det, matched det, ref, matched ref]."""
    rng = np.random.default_rng(score.SEED)
    n = len(va)
    out = []
    for _ in range(score.DRAWS):
        i = rng.integers(0, n, n)
        pa, ra = score.set_pr(va[i])
        pb, rb = score.set_pr(vb[i])
        f = lambda p, r: 2 * p * r / (p + r) if p + r > 0 else 0.0
        out.append((f(pb, rb) - f(pa, ra), pb - pa, rb - ra, f(pa, ra), f(pb, rb)))
    out = np.array(out)
    res = {}
    for j, k in enumerate(("f1_diff", "precision_diff", "recall_diff", "f1_banded", "f1_combo")):
        lo, hi = np.percentile(out[:, j], [2.5, 97.5])
        res[k] = [float(lo), float(hi)]
    return res


def run(a):
    import onnxruntime  # noqa: F401
    sha = hashlib.sha256(open(a.model, "rb").read()).hexdigest()
    assert sha.startswith(MODEL_SHA_PREFIX) and sha.endswith(MODEL_SHA_SUFFIX), "model sha256 " + sha
    ids = [ln.strip() for ln in open(os.path.join(a.out_dir, "page-ids.txt")) if ln.strip()]
    assert hashlib.sha256(open(os.path.join(a.out_dir, "page-ids.txt"), "rb").read()).hexdigest() == open(os.path.join(a.out_dir, "page-ids.sha256")).read().strip()
    cache = os.path.join(a.out_dir, "cache")
    det = asmodel.Detector(a.model, a.threads)
    p0 = columns.Params()
    banded, boxes, combo, sizes = {}, {}, {}, {}
    t0 = time.time()
    for n, pid in enumerate(ids, 1):
        gray, size = columns.load_cached(cache, pid)
        sizes[pid] = size
        banded[pid] = columns.detect(gray, size, "banded", p0)
        dets = det.boxes(gray)
        sx, sy = size[0] / gray.shape[1], size[1] / gray.shape[0]
        boxes[pid] = [[round(d[0] * sx), round(d[1] * sy), round(d[2] * sx), round(d[3] * sy), round(d[4], 3), asmodel.NAMES[d[5]]] for d in dets]
        art = diag.gutters_generic(boxes[pid], size, 0.01, diag.ARTICLE, diag.OBSTACLE)
        combo[pid] = banded[pid] + diag.add_where_none(banded[pid], art, size)
        if n % 25 == 0:
            print("pages %d/%d %.0fs" % (n, len(ids), time.time() - t0), flush=True)
    json.dump(boxes, open(os.path.join(a.out_dir, "as-boxes.json"), "w"), sort_keys=True)
    json.dump({"banded": banded, "combo": combo}, open(os.path.join(a.out_dir, "gutters.json"), "w"), sort_keys=True)
    return score_saved(a.out_dir, sha)


def score_saved(out_dir, sha=None):
    """Score the gutters saved by run (also used alone: scoring is deterministic, no detector involved)."""
    refs = json.load(open(os.path.join(out_dir, "refs.json")))["pages"]
    ids = [ln.strip() for ln in open(os.path.join(out_dir, "page-ids.txt")) if ln.strip()]
    saved = json.load(open(os.path.join(out_dir, "gutters.json")))
    banded, combo = saved["banded"], saved["combo"]
    vec = {}
    res = {"pages": len(ids), "model_sha256": sha}
    for name, g in (("banded", banded), ("combo", combo)):
        dets_ = {p: {"gutters": g[p]} for p in ids}
        rows = score.page_rows(dets_, refs)
        v = score._vec([rows[p] for p in ids], "primary")
        vec[name] = v
        p, r = score.set_pr(v)
        res[name] = {"precision": p, "recall": r, "f1": 2 * p * r / (p + r), "det_height": float(v[:, 0].sum())}
    res["paired_bootstrap"] = bootstrap_paired(vec["banded"], vec["combo"])
    d_f1 = res["combo"]["f1"] - res["banded"]["f1"]
    d_p = res["combo"]["precision"] - res["banded"]["precision"]
    res["diff"] = {"f1": d_f1, "precision": d_p, "recall": res["combo"]["recall"] - res["banded"]["recall"]}
    res["rules"] = {"1_f1_diff_ge_0.02": bool(d_f1 >= 0.02), "2_f1_interval_above_0": bool(res["paired_bootstrap"]["f1_diff"][0] > 0),
                    "3_precision_drop_le_0.02": bool(-d_p <= 0.02)}
    res["adopt"] = bool(all(res["rules"].values()))
    json.dump(res, open(os.path.join(out_dir, "results.json"), "w"), indent=1, sort_keys=True)
    print(json.dumps(res, indent=1, sort_keys=True))
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sp = ap.add_subparsers(dest="cmd", required=True)
    s = sp.add_parser("select")
    s.add_argument("--refs-dir", default="/data/layout-refs/refs")
    s.add_argument("--out-dir", required=True)
    r = sp.add_parser("run")
    r.add_argument("--out-dir", required=True)
    r.add_argument("--model", required=True)
    r.add_argument("--threads", type=int, default=8)
    c = sp.add_parser("score")
    c.add_argument("--out-dir", required=True)
    a = ap.parse_args(argv)
    return select(a) if a.cmd == "select" else run(a) if a.cmd == "run" else score_saved(a.out_dir)


if __name__ == "__main__":
    sys.exit(main())
