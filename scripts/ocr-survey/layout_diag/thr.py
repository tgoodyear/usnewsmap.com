#!/usr/bin/env python3
"""Does the confidence threshold explain the trained detector's low column recall? (#2107, diagnostic)

Plan frozen before any run (issue #2107, comment 7105573766). Pages none of which is in inventory.py's used
set, the La Liberte combination test (500, comment 7102798228) or the German test (300, comment 7103363654):
  La Liberte: unused train pages with a non-empty scoring reference (row_reference gutters after the 3% gap
              cap, as combo_eval.py), ranked by sha256("2107:thr:"+id): first 300 dev, next 500 test.
  Chronicling Germany: the 315 unused qualifying pages (cg_refs.py rule), same key: first 100 dev, rest test.
Gutter rule for both models: combo_eval.py's (`02f74ea59`) diag.gutters_generic on `article` boxes at
confidence >= c. Trained model: eval_infer.py as in the first evaluation (imgsz 1280, Ultralytics defaults
otherwise, grayscale to three channels, candidate floor 0.01, so one run covers every c >= 0.01). AS:
asmodel.Detector as in combo_eval.py, at 0.01. c* is chosen on dev only from C_GRID by best pool F1.
Scoring: layout_columns/score.py score_sets (primary metric; per set pooled over pages; pool = equal-weight
mean of the sets, as the first evaluation). Intervals: page bootstrap, 1,000 draws, seed 2107.

Sequence (each step refuses to run out of order):
    thr.py select   --refs-dir R --cg-dir CG/66b50f53 --combo-ids F --cg-used-ids F --out-dir D [--path-from A --path-to B]
    thr.py extract  --split-dirs D/dev-liberte D/test-liberte --parquet P --img-dir I   (per parquet; bytes only)
    thr.py local-paths --split-dirs D/* --img-dir I
    (pagecache.py + eval_infer.py on D/dev-*)
    thr.py dev      --out-dir D
    thr.py freeze   --out-dir D            writes D/frozen.json (c*, test page-list sha256s) before any test run
    (pagecache.py + eval_infer.py + thr.py as-boxes on D/test-*)
    thr.py as-boxes --split-dir D/test-x --model M [--shard-index I --shard-count N --threads T]
    thr.py test     --out-dir D            scores once; refuses if D/test-results.json exists

Data: Teklia/Newspapers-finlam-La-Liberte (MIT); Chronicling Germany (CC BY-NC 4.0, University of Bonn,
commit 66b50f53); detector: Ultralytics YOLOv8 (AGPL-3.0); American Stories (Dell et al., arXiv:2308.12477).
"""
import argparse
import glob
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
import refs as refsmod  # noqa: E402
import score  # noqa: E402

PREFIX = "2107:thr:"
C_GRID = (0.01, 0.05, 0.10, 0.25)
AS_CONF = 0.01
LL_DEV, LL_TEST = 300, 500
CG_DEV, CG_UNUSED = 100, 315
COMBO_SHA = "eddf61bedddaefa714d000446a0043f7d10b62f79e93679038c51251e47bb523"
CG_SHA = "e67d4a615e3cc2007f7f2c64c35e2197f6087934ee2fdbc5164628964b9d1bda"
TRAINED_SHA = "7d75048e1192c22e501ffc1f649d74d80fa4218c824455fbc699a66e5005b62f"
AS_SHA = "045b2e5588e53c700490730244bdc3e8ff21e903aff1c2af9b169dcdb1d9155e"
SETS = ("liberte", "cg")


def key(prefix, pid):
    return hashlib.sha256((prefix + str(pid)).encode()).hexdigest()


def file_sha(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def read_ids(path):
    return [ln.strip() for ln in open(path) if ln.strip()]


def write_split(d, ids, refs, paths, for_pub, name):
    os.makedirs(d, exist_ok=True)
    listing = "\n".join(ids) + "\n"
    open(os.path.join(d, "page-ids.txt"), "w").write(listing)
    json.dump({"for_publication": for_pub, "pages": {p: refs[p] for p in ids}, "set": name},
              open(os.path.join(d, "refs.json"), "w"), sort_keys=True)
    json.dump({p: paths[p] for p in ids}, open(os.path.join(d, "paths.json"), "w"), sort_keys=True)
    return hashlib.sha256(listing.encode()).hexdigest()


def select(a):
    import cg_refs
    assert file_sha(a.combo_ids) == COMBO_SHA, "combo page list"
    assert file_sha(a.cg_used_ids) == CG_SHA, "German test page list"
    combo, cg_used = set(read_ids(a.combo_ids)), set(read_ids(a.cg_used_ids))
    fix = (lambda p: a.path_to + p[len(a.path_from):] if a.path_from and p.startswith(a.path_from) else p)
    # La Liberte: inventory.py's used set (train: ll 500 + tr 2,000; val and test wholly used), then combo.
    rows = list(refsmod.read_rows(os.path.join(a.refs_dir, refsmod.FILES["liberte-train"])))
    ids = sorted({r["page_id"] for r in rows})
    assert len(ids) == len(rows), "duplicate page ids"
    ll_inv = {r["page_id"] for r in sorted(rows, key=lambda r: key("2107:ll:", r["page_id"]))[:500]}
    ll_combo = set(sorted(ids, key=lambda p: (key("2107:ll:", p), p))[:500])
    assert ll_inv == ll_combo
    tr = set(sorted(ids, key=lambda p: (key("2107:tr:", p), p))[:2000])
    fresh = [p for p in ids if p not in ll_inv | tr]
    assert combo <= set(fresh), "combo pages outside inventory's fresh set"
    unused = [p for p in fresh if p not in combo]
    by = {r["page_id"]: r for r in rows}
    ll_refs, ll_paths, no_gut = {}, {}, 0
    for p in unused:
        ref = refsmod.row_reference(by[p])
        if not ref["gutters"]:
            no_gut += 1
            continue
        ll_refs[p], ll_paths[p] = ref, fix(by[p]["image_path"])
    ll_order = sorted(ll_refs, key=lambda p: (key(PREFIX, p), p))
    assert len(ll_order) >= LL_DEV + LL_TEST
    # Chronicling Germany: cg_refs.py's qualifying rule on the non-Test pages, minus the German test's 300.
    ann = os.path.join(a.cg_dir, "data", "annotations")
    test_split = cg_refs.read_split(a.cg_dir)
    stems = sorted(f[:-4] for f in os.listdir(ann) if f.endswith(".xml"))
    cg_refs_, cg_paths, qual = {}, {}, 0
    for p in stems:
        if p in test_split:
            continue
        regs, w, h = cg_refs.parse_page(os.path.join(ann, p + ".xml"))
        c = cg_refs.counts(regs)
        if c["paragraph"] < 1 or c["separator_vertical"] < 1:
            continue
        qual += 1
        if p in cg_used:
            continue
        cg_refs_[p] = cg_refs.reference(regs, w, h)
        cg_paths[p] = fix(os.path.abspath(os.path.join(a.cg_dir, "data", "images", p + ".jpg")))
    assert cg_used <= set(stems) and len(cg_refs_) == CG_UNUSED, len(cg_refs_)
    cg_order = sorted(cg_refs_, key=lambda p: (key(PREFIX, p), p))
    out = a.out_dir
    sh = {
        "dev-liberte": write_split(os.path.join(out, "dev-liberte"), ll_order[:LL_DEV], ll_refs, ll_paths, True, "liberte-thr-dev"),
        "test-liberte": write_split(os.path.join(out, "test-liberte"), ll_order[LL_DEV:LL_DEV + LL_TEST], ll_refs, ll_paths, True, "liberte-thr-test"),
        "dev-cg": write_split(os.path.join(out, "dev-cg"), cg_order[:CG_DEV], cg_refs_, cg_paths, False, "cg-thr-dev"),
        "test-cg": write_split(os.path.join(out, "test-cg"), cg_order[CG_DEV:], cg_refs_, cg_paths, False, "cg-thr-test"),
    }
    info = {"liberte": {"train_pages": len(ids), "used_inventory": len(ll_inv | tr), "fresh": len(fresh), "combo_test": len(combo),
                        "unused": len(unused), "unused_without_reference_gutter": no_gut, "unused_with_reference_gutter": len(ll_order),
                        "dev": LL_DEV, "test": LL_TEST},
            "cg": {"non_test": len([p for p in stems if p not in test_split]), "qualifying": qual, "german_test": len(cg_used),
                   "unused_qualifying": len(cg_order), "dev": CG_DEV, "test": len(cg_order) - CG_DEV},
            "page_ids_sha256": sh}
    json.dump(info, open(os.path.join(out, "selection.json"), "w"), indent=1, sort_keys=True)
    print(json.dumps(info, indent=1, sort_keys=True))
    return 0


def extract(a):
    """Copy the image bytes of the split pages held in one parquet to img-dir/<page id>.img, unchanged (disk
    space: parquets are copied in one at a time). pagecache.py decodes a file path and bytes alike."""
    import pyarrow.parquet as pq
    want = {}
    for sd in a.split_dirs:
        for pid, p in json.load(open(os.path.join(sd, "paths.json"))).items():
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


def local_paths(a):
    """paths-local.json: La Liberte pages read from img-dir, the rest as in paths.json."""
    for sd in a.split_dirs:
        paths = json.load(open(os.path.join(sd, "paths.json")))
        loc = {p: (os.path.join(a.img_dir, p + ".img") if "#row=" in v else v) for p, v in paths.items()}
        assert all(os.path.exists(v) for v in loc.values()), sd
        json.dump(loc, open(os.path.join(sd, "paths-local.json"), "w"), sort_keys=True)
    return 0


def load_boxes(split_dir, stem):
    pages, meta = {}, []
    for f in sorted(glob.glob(os.path.join(split_dir, stem + "-*.json"))):
        d = json.load(open(f))
        meta.append(d.get("_meta"))
        pages.update(d["pages"])
    ids = read_ids(os.path.join(split_dir, "page-ids.txt"))
    assert set(pages) == set(ids), "%s: boxes for %d of %d pages" % (split_dir, len(set(pages) & set(ids)), len(ids))
    return pages, meta


def gutters(boxes, refs, conf):
    import diag
    return {p: {"gutters": diag.gutters_generic(boxes[p], (refs[p]["width"], refs[p]["height"]), conf, diag.ARTICLE, diag.OBSTACLE)}
            for p in refs}


def score_cond(sets):
    """sets: name -> (detections, refs). score.py's several-set scoring, primary metric."""
    res = score.score_sets({n: {"detections": d, "refs": r, "for_publication": False} for n, (d, r) in sets.items()}, per_page=False)
    out = {"pool": {"precision": res["precision"], "recall": res["recall"], "f1": res["f1"], "interval_95": res["interval_95"]}}
    for n in sets:
        e = res["per_set"][n]
        out[n] = {"precision": e["precision"], "recall": e["recall"], "f1": e["f1"], "interval_95": e["interval_95"], "pages": e["pages"]}
    return out


def vecs(sets):
    rows = {n: score.page_rows(d, r) for n, (d, r) in sorted(sets.items())}
    return {n: score._vec([rows[n][p] for p in sorted(rows[n])], "primary") for n in rows}


def paired_pool_bootstrap(va, vb):
    """b minus a on the pool (and per set): pages resampled within each set, the same draws for both."""
    rng = np.random.default_rng(score.SEED)
    names = sorted(va)
    res = []
    for _ in range(score.DRAWS):
        idx = {k: rng.integers(0, len(va[k]), len(va[k])) for k in names}
        pa, ra, fa, prs_a = score.pool({k: va[k][idx[k]] for k in names})
        pb, rb, fb, prs_b = score.pool({k: vb[k][idx[k]] for k in names})
        row = [pb - pa, rb - ra, fb - fa]
        for k in names:
            row.append(prs_b[k][1] - prs_a[k][1])
        res.append(row)
    res = np.array(res)
    out = {}
    for i, k in enumerate(["pool_precision", "pool_recall", "pool_f1"] + ["recall_" + k for k in names]):
        lo, hi = np.percentile(res[:, i], [2.5, 97.5])
        out[k] = [round(float(lo), 4), round(float(hi), 4)]
    return out


def split_data(out, split):
    d = {}
    for n in SETS:
        sd = os.path.join(out, "%s-%s" % (split, n))
        d[n] = (sd, json.load(open(os.path.join(sd, "refs.json")))["pages"])
    return d


def dev(a):
    assert not os.path.exists(os.path.join(a.out_dir, "frozen.json")), "already frozen"
    d = split_data(a.out_dir, "dev")
    boxes, meta = {}, {}
    for n, (sd, refs) in d.items():
        boxes[n], meta[n] = load_boxes(sd, "trained-boxes")
        assert all(m["checkpoint_sha256"] == TRAINED_SHA for m in meta[n])
    table = {}
    for c in C_GRID:
        table["%.2f" % c] = score_cond({n: (gutters(boxes[n], refs, c), refs) for n, (sd, refs) in d.items()})
    best = max(C_GRID, key=lambda c: (table["%.2f" % c]["pool"]["f1"], c))  # a tie goes to the higher c
    res = {"c_grid": list(C_GRID), "c_star": best, "table": table, "infer_meta": meta,
           "pages": {n: len(refs) for n, (sd, refs) in d.items()}}
    json.dump(res, open(os.path.join(a.out_dir, "dev-results.json"), "w"), indent=1, sort_keys=True)
    for c in C_GRID:
        t = table["%.2f" % c]
        print("c=%.2f  pool P %.4f R %.4f F1 %.4f  | LL F1 %.4f  CG F1 %.4f" % (c, t["pool"]["precision"], t["pool"]["recall"],
                                                                              t["pool"]["f1"], t["liberte"]["f1"], t["cg"]["f1"]))
    print("c* =", best)
    return 0


def freeze(a):
    fz = os.path.join(a.out_dir, "frozen.json")
    assert not os.path.exists(fz), "already frozen"
    for n in SETS:
        assert not glob.glob(os.path.join(a.out_dir, "test-" + n, "*boxes*")), "test outputs exist before freezing"
        assert not os.path.exists(os.path.join(a.out_dir, "test-" + n, "cache")), "test pages cached before freezing"
    dv = json.load(open(os.path.join(a.out_dir, "dev-results.json")))
    sel = json.load(open(os.path.join(a.out_dir, "selection.json")))
    sh = {n: file_sha(os.path.join(a.out_dir, "test-" + n, "page-ids.txt")) for n in SETS}
    assert all(sh[n] == sel["page_ids_sha256"]["test-" + n] for n in SETS)
    fr = {"c_star": dv["c_star"], "as_conf": AS_CONF, "test_page_ids_sha256": sh,
          "dev_page_ids_sha256": {n: sel["page_ids_sha256"]["dev-" + n] for n in SETS},
          "dev_pool_f1": {c: v["pool"]["f1"] for c, v in dv["table"].items()},
          "trained_sha256": TRAINED_SHA, "as_sha256": AS_SHA, "frozen_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
    with open(fz, "x") as f:
        json.dump(fr, f, indent=1, sort_keys=True)
    print(json.dumps(fr, indent=1, sort_keys=True))
    return 0


def as_boxes(a):
    import asmodel
    import columns
    assert os.path.exists(os.path.join(os.path.dirname(os.path.abspath(a.split_dir)), "frozen.json")), "AS runs on test pages only, after freezing"
    assert file_sha(a.model) == AS_SHA, "AS model sha256"
    ids = read_ids(os.path.join(a.split_dir, "page-ids.txt"))[a.shard_index::a.shard_count]
    det = asmodel.Detector(a.model, a.threads)
    boxes, t0 = {}, time.time()
    for n, pid in enumerate(ids, 1):  # as combo_eval.py run
        gray, size = columns.load_cached(os.path.join(a.split_dir, "cache"), pid)
        dets = det.boxes(gray)
        sx, sy = size[0] / gray.shape[1], size[1] / gray.shape[0]
        boxes[pid] = [[round(d[0] * sx), round(d[1] * sy), round(d[2] * sx), round(d[3] * sy), round(d[4], 3), asmodel.NAMES[d[5]]] for d in dets]
        print("pages %d/%d %.0fs" % (n, len(ids), time.time() - t0), flush=True)
    meta = {"as_sha256": AS_SHA, "shard_index": a.shard_index, "shard_count": a.shard_count, "pages": len(ids)}
    json.dump({"_meta": meta, "pages": boxes}, open(os.path.join(a.split_dir, "as-boxes-%d.json" % a.shard_index), "w"), sort_keys=True)
    return 0


def test(a):
    fr = json.load(open(os.path.join(a.out_dir, "frozen.json")))
    out = os.path.join(a.out_dir, "test-results.json")
    assert not os.path.exists(out), "test already scored"
    for n in SETS:
        assert file_sha(os.path.join(a.out_dir, "test-" + n, "page-ids.txt")) == fr["test_page_ids_sha256"][n]
    d = split_data(a.out_dir, "test")
    tb, ab, meta = {}, {}, {}
    for n, (sd, refs) in d.items():
        tb[n], m1 = load_boxes(sd, "trained-boxes")
        ab[n], m2 = load_boxes(sd, "as-boxes")
        assert all(m["checkpoint_sha256"] == TRAINED_SHA for m in m1) and all(m["as_sha256"] == AS_SHA for m in m2)
        meta[n] = {"trained": m1, "as": m2}
    cs = fr["c_star"]
    conds = {"trained_c_star": (tb, cs), "trained_0.25": (tb, 0.25), "as_0.01": (ab, AS_CONF)}
    sets = {k: {n: (gutters(bx[n], refs, c), refs) for n, (sd, refs) in d.items()} for k, (bx, c) in conds.items()}
    res = {"c_star": cs, "frozen": fr, "pages": {n: len(refs) for n, (sd, refs) in d.items()}, "infer_meta": meta}
    res["conditions"] = {k: score_cond(s) for k, s in sets.items()}
    v = {k: vecs(s) for k, s in sets.items()}
    res["paired_c_star_minus_0.25"] = paired_pool_bootstrap(v["trained_0.25"], v["trained_c_star"])
    res["paired_c_star_minus_as_descriptive"] = paired_pool_bootstrap(v["as_0.01"], v["trained_c_star"])
    c = res["conditions"]
    res["recall_gain_pool"] = c["trained_c_star"]["pool"]["recall"] - c["trained_0.25"]["pool"]["recall"]
    gap = c["as_0.01"]["pool"]["f1"] - c["trained_c_star"]["pool"]["f1"]
    res["as_minus_trained_c_star_pool_f1"] = gap
    res["reading"] = 1 if gap <= 0.02 else 2
    with open(out, "x") as f:
        json.dump(res, f, indent=1, sort_keys=True)
    for k in conds:
        for n in ("liberte", "cg", "pool"):
            e = c[k][n]
            print("%-15s %-7s P %.4f R %.4f F1 %.4f  CI %s" % (k, n, e["precision"], e["recall"], e["f1"], e["interval_95"]))
    print("R(c*)-R(0.25) pool %.4f CI %s" % (res["recall_gain_pool"], res["paired_c_star_minus_0.25"]["pool_recall"]))
    print("AS - trained@c* pool F1 %.4f -> reading %d" % (gap, res["reading"]))
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sp = ap.add_subparsers(dest="cmd", required=True)
    s = sp.add_parser("select")
    s.add_argument("--refs-dir", required=True)
    s.add_argument("--cg-dir", required=True)
    s.add_argument("--combo-ids", required=True)
    s.add_argument("--cg-used-ids", required=True)
    s.add_argument("--out-dir", required=True)
    s.add_argument("--path-from")
    s.add_argument("--path-to")
    for name in ("dev", "freeze", "test"):
        sp.add_parser(name).add_argument("--out-dir", required=True)
    b = sp.add_parser("as-boxes")
    b.add_argument("--split-dir", required=True)
    b.add_argument("--model", required=True)
    b.add_argument("--shard-index", type=int, default=0)
    b.add_argument("--shard-count", type=int, default=1)
    b.add_argument("--threads", type=int, default=4)
    e = sp.add_parser("extract")
    e.add_argument("--split-dirs", nargs="+", required=True)
    e.add_argument("--parquet", required=True)
    e.add_argument("--img-dir", required=True)
    lp = sp.add_parser("local-paths")
    lp.add_argument("--split-dirs", nargs="+", required=True)
    lp.add_argument("--img-dir", required=True)
    a = ap.parse_args(argv)
    return {"select": select, "dev": dev, "freeze": freeze, "as-boxes": as_boxes, "test": test,
            "extract": extract, "local-paths": local_paths}[a.cmd](a)


if __name__ == "__main__":
    sys.exit(main())
