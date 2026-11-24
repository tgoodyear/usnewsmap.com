#!/usr/bin/env python3
"""Banded v2 step 1 (#2107): does v2 keep column accuracy on fresh La Liberte pages? Plan: comment 7122420362.

Pages: unused La Liberte train pages with a non-empty scoring reference (row_reference gutters after the 3% gap
cap, as combo_eval.py and thr.py), outside inventory.py's used set (ll 500 + tr 2,000), the combination test
(500, page list sha256 eddf61be...) and the threshold test (dev 300 + test 500, sha256 184b3997... / 71708bf3...),
ranked by sha256("2107:v2:"+page_id): the first 500 are the test. Check pages, never measured: the last 10 of
the same ranking.
Scoring: layout_columns/score.py primary metric (page_rows/set_pr, as combo_eval.py); v2 lines through the
refit.py adapter; paired page bootstrap, 1,000 draws, seed 2107.
Rule 1 (with rule 2 from bwfalse.py): the 95% interval of F1(v2) - F1(v1) has its lower bound above -0.01.

    v2eval.py select --refs-dir R --combo-ids F --thr-dir D --out-dir D
    v2eval.py extract --split-dir D/x --parquet P --img-dir I         (per parquet; image bytes only)
    v2eval.py run --split-dir D/x --img-dir I [--workers 3] [--measure-run]
    v2eval.py score --split-dir D/x [--measure-run]

`run` and `score` refuse the test split without --measure-run (phase 2 only).
Data: Teklia/Newspapers-finlam-La-Liberte (MIT). Method for the finder follows jscancella/NYTribuneOCRExperiments
(findText_usingSums.py, GPL-3.0), reimplemented.
"""
import argparse
import hashlib
import json
import multiprocessing as mp
import os
import sys
import time

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import v2common  # noqa: E402
import refit  # noqa: E402
import refs as refsmod  # noqa: E402
import score  # noqa: E402

PREFIX = "2107:v2:"
N_TEST, N_CHECK = 500, 10
COMBO_SHA = "eddf61bedddaefa714d000446a0043f7d10b62f79e93679038c51251e47bb523"
THR_SHA = {"dev-liberte": "184b3997480d5ca196c37990998b2e2d6fbeee87d3d442718f3214a802103ca8",
           "test-liberte": "71708bf332b9c40152e5af0e80e72ac337c63bd2bbbf58f9e4d20dd761bceca0"}
UNUSED_WITH_REF, AFTER_THR = 5000, 4200
F1_FLOOR = -0.01


def key(prefix, pid):
    return hashlib.sha256((prefix + str(pid)).encode()).hexdigest()


def read_ids(path):
    return [ln.strip() for ln in open(path) if ln.strip()]


def write_split(d, ids, refs, paths, name):
    os.makedirs(d, exist_ok=True)
    listing = "\n".join(ids) + "\n"
    open(os.path.join(d, "page-ids.txt"), "w").write(listing)
    digest = hashlib.sha256(listing.encode()).hexdigest()
    open(os.path.join(d, "page-ids.sha256"), "w").write(digest + "\n")
    json.dump({"for_publication": True, "pages": {p: refs[p] for p in ids}, "set": name},
              open(os.path.join(d, "refs.json"), "w"), sort_keys=True)
    json.dump({p: paths[p] for p in ids}, open(os.path.join(d, "paths.json"), "w"), sort_keys=True)
    return digest


def select(a):
    assert v2common.file_sha(a.combo_ids) == COMBO_SHA, "combo page list"
    thr = set()
    for s, sha in THR_SHA.items():
        f = os.path.join(a.thr_dir, s, "page-ids.txt")
        assert v2common.file_sha(f) == sha, "threshold page list " + s
        thr |= set(read_ids(f))
    assert len(thr) == 800
    combo = set(read_ids(a.combo_ids))
    rows = list(refsmod.read_rows(os.path.join(a.refs_dir, refsmod.FILES["liberte-train"])))
    ids = sorted({r["page_id"] for r in rows})
    assert len(ids) == len(rows), "duplicate page ids"
    ll = {r["page_id"] for r in sorted(rows, key=lambda r: key("2107:ll:", r["page_id"]))[:500]}  # inventory.py
    tr = set(sorted(ids, key=lambda p: (key("2107:tr:", p), p))[:2000])
    fresh = [p for p in ids if p not in ll | tr]
    assert combo <= set(fresh)
    by = {r["page_id"]: r for r in rows}
    refs, paths, no_gut = {}, {}, 0
    for p in fresh:
        if p in combo:
            continue
        ref = refsmod.row_reference(by[p])
        if not ref["gutters"]:
            no_gut += 1
            continue
        refs[p], paths[p] = ref, by[p]["image_path"]
    assert len(refs) == UNUSED_WITH_REF, len(refs)
    assert thr <= set(refs), "threshold pages outside the unused pool"
    pool = [p for p in refs if p not in thr]
    assert len(pool) == AFTER_THR, len(pool)
    order = sorted(pool, key=lambda p: (key(PREFIX, p), p))
    test, check = order[:N_TEST], order[-N_CHECK:]
    sh = {"test-liberte": write_split(os.path.join(a.out_dir, "test-liberte"), test, refs, paths, "liberte-v2-test"),
          "check-liberte": write_split(os.path.join(a.out_dir, "check-liberte"), check, refs, paths, "liberte-v2-check")}
    info = {"train_pages": len(ids), "used_inventory": len(ll | tr), "fresh": len(fresh), "combo_test": len(combo),
            "unused_without_reference_gutter": no_gut, "unused_with_reference_gutter": len(refs), "threshold_test": len(thr),
            "pool": len(pool), "test": len(test), "check": len(check), "check_ranks": [len(order) - N_CHECK + 1, len(order)],
            "left_unused_after_test": len(pool) - len(test), "page_ids_sha256": sh, "prefix": PREFIX}
    json.dump(info, open(os.path.join(a.out_dir, "selection.json"), "w"), indent=1, sort_keys=True)
    print(json.dumps(info, indent=1, sort_keys=True))
    return 0


def guard(split_dir, measure):
    sel = json.load(open(os.path.join(os.path.dirname(os.path.abspath(split_dir)), "selection.json")))
    sha = v2common.file_sha(os.path.join(split_dir, "page-ids.txt"))
    is_test = sha == sel["page_ids_sha256"]["test-liberte"]
    if is_test and not measure:
        raise SystemExit("this is the test split: phase 2 only (--measure-run)")
    return is_test


def extract(a):
    """Copy the image bytes of the split's pages held in one parquet to img-dir/<page id>.img, unchanged."""
    import pyarrow.parquet as pq
    want = {}
    for pid, p in json.load(open(os.path.join(a.split_dir, "paths.json"))).items():
        if "#row=" in p and os.path.basename(p.split("#row=")[0]) == os.path.basename(a.parquet):
            want[int(p.rsplit("#row=", 1)[1])] = pid
    os.makedirs(a.img_dir, exist_ok=True)
    pf = pq.ParquetFile(a.parquet)
    start, n_done = 0, 0
    for rg in range(pf.metadata.num_row_groups):
        n = pf.metadata.row_group(rg).num_rows
        need = [i for i in want if start <= i < start + n]
        if need:
            col = pf.read_row_group(rg, columns=["page_image"]).column("page_image").to_pylist()
            for i in need:
                open(os.path.join(a.img_dir, want[i] + ".img"), "wb").write(col[i - start]["bytes"])
                n_done += 1
        start += n
    assert n_done == len(want)
    print("%s: %d pages" % (os.path.basename(a.parquet), n_done), flush=True)
    return 0


def _page(args):
    pid, src, ref_size = args
    data = v2common.read_bytes(src)
    gray, size, full = v2common.load_page(data)
    assert list(size) == list(ref_size), "%s: image %s, reference %s" % (pid, size, ref_size)
    t0 = time.time()
    v1 = v2common.run_v1(gray, size)
    t1 = time.time()
    thr, lines = v2common.run_v2(v1, full)
    return pid, {"v1": v1, "v2_lines": lines, "otsu": thr, "size": list(size),
                 "seconds": {"v1": round(t1 - t0, 2), "v2_refit": round(time.time() - t1, 2)}}


def run(a):
    is_test = guard(a.split_dir, a.measure_run)
    refs = json.load(open(os.path.join(a.split_dir, "refs.json")))["pages"]
    ids = read_ids(os.path.join(a.split_dir, "page-ids.txt"))
    jobs = [(p, os.path.join(a.img_dir, p + ".img"), [refs[p]["width"], refs[p]["height"]]) for p in ids]
    out, t0 = {}, time.time()
    with mp.Pool(a.workers) as pool:
        for n, (pid, r) in enumerate(pool.imap_unordered(_page, jobs), 1):
            out[pid] = r
            print("pages %d/%d %.0fs" % (n, len(jobs), time.time() - t0), flush=True)
    meta = {"refit": refit.SETTINGS, "pages": len(ids), "test_split": is_test, "wall_seconds": round(time.time() - t0, 1)}
    json.dump({"_meta": meta, "pages": out}, open(os.path.join(a.split_dir, "v1v2.json"), "w"), sort_keys=True)
    return 0


def paired(va, vb):
    """Paired page bootstrap of b minus a; rows [det, matched det, ref, matched ref] (combo_eval.py's)."""
    rng = np.random.default_rng(score.SEED)
    n = len(va)
    out = []

    def f(p, r):
        return 2 * p * r / (p + r) if p + r > 0 else 0.0
    for _ in range(score.DRAWS):
        i = rng.integers(0, n, n)
        pa, ra = score.set_pr(va[i])
        pb, rb = score.set_pr(vb[i])
        out.append((f(pb, rb) - f(pa, ra), pb - pa, rb - ra, f(pa, ra), f(pb, rb)))
    out = np.array(out)
    res = {}
    for j, k in enumerate(("f1_diff", "precision_diff", "recall_diff", "f1_v1", "f1_v2")):
        lo, hi = np.percentile(out[:, j], [2.5, 97.5])
        res[k] = [float(lo), float(hi)]
    return res


def adapter_check(v1, refs, ids):
    """v1 scored directly and through the adapter: every page's score rows must be equal."""
    direct = score.page_rows({p: {"gutters": v1[p]} for p in ids}, refs)
    via = score.page_rows({p: {"gutters": refit.scoring_gutters(refit.v1_lines(v1[p]))} for p in ids}, refs)
    same_pieces = all(refit.check_adapter(v1[p]) for p in ids)
    same_rows = all(direct[p] == via[p] for p in ids)
    return {"pages": len(ids), "pieces_identical": same_pieces, "score_rows_identical": same_rows,
            "pass": bool(same_pieces and same_rows)}


def score_split(a):
    is_test = guard(a.split_dir, a.measure_run)
    out_path = os.path.join(a.split_dir, "results.json")
    if is_test:
        assert not os.path.exists(out_path), "test already scored"
    refs = json.load(open(os.path.join(a.split_dir, "refs.json")))["pages"]
    ids = read_ids(os.path.join(a.split_dir, "page-ids.txt"))
    saved = json.load(open(os.path.join(a.split_dir, "v1v2.json")))["pages"]
    assert set(saved) == set(ids)
    v1 = {p: saved[p]["v1"] for p in ids}
    v2 = {p: refit.scoring_gutters(saved[p]["v2_lines"]) for p in ids}
    res = {"pages": len(ids), "test_split": is_test, "adapter_check_v1": adapter_check(v1, refs, ids)}
    assert res["adapter_check_v1"]["pass"], "adapter changes v1 scores"
    vec = {}
    for name, g in (("v1", v1), ("v2", v2)):
        rows = score.page_rows({p: {"gutters": g[p]} for p in ids}, refs)
        v = score._vec([rows[p] for p in ids], "primary")
        vec[name] = v
        p, r = score.set_pr(v)
        res[name] = {"precision": p, "recall": r, "f1": 2 * p * r / (p + r) if p + r else 0.0, "det_height": float(v[:, 0].sum())}
    res["paired_bootstrap_v2_minus_v1"] = paired(vec["v1"], vec["v2"])
    res["diff"] = {k: res["v2"][k] - res["v1"][k] for k in ("precision", "recall", "f1")}
    res["rule_1_f1_diff_lower_above_-0.01"] = bool(res["paired_bootstrap_v2_minus_v1"]["f1_diff"][0] > F1_FLOOR)
    tc = [refit.touch_counts(saved[p]["v2_lines"]) for p in ids]
    tot = {k: sum(t[k] for t in tc) for k in tc[0]} if tc else {}
    res["descriptive_touch_print"] = dict(tot, v1_share=tot["v1_gt5"] / tot["pieces"] if tot.get("pieces") else None,
                                          v2_share=tot["v2_gt5"] / tot["pieces"] if tot.get("pieces") else None)
    mode = "x" if is_test else "w"
    with open(out_path, mode) as f:
        json.dump(res, f, indent=1, sort_keys=True)
    print(json.dumps(res, indent=1, sort_keys=True))
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sp = ap.add_subparsers(dest="cmd", required=True)
    s = sp.add_parser("select")
    s.add_argument("--refs-dir", required=True)
    s.add_argument("--combo-ids", required=True)
    s.add_argument("--thr-dir", required=True, help="thr2107/work (dev-liberte, test-liberte page lists)")
    s.add_argument("--out-dir", required=True)
    e = sp.add_parser("extract")
    e.add_argument("--split-dir", required=True)
    e.add_argument("--parquet", required=True)
    e.add_argument("--img-dir", required=True)
    r = sp.add_parser("run")
    r.add_argument("--split-dir", required=True)
    r.add_argument("--img-dir", required=True)
    r.add_argument("--workers", type=int, default=3)
    r.add_argument("--measure-run", action="store_true")
    c = sp.add_parser("score")
    c.add_argument("--split-dir", required=True)
    c.add_argument("--measure-run", action="store_true")
    a = ap.parse_args(argv)
    return {"select": select, "extract": extract, "run": run, "score": score_split}[a.cmd](a)


if __name__ == "__main__":
    sys.exit(main())
