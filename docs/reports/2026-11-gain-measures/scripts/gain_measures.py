#!/usr/bin/env python3
"""Step 1 gap check: which gain measure tracks real CER drops between two readings of the same page?

    gain_measures.py --metrics-dir metrics --damage damage-truth.jsonl --out gain-measures.json [--boot 1000]

Inputs are the qe study's outputs (#1925, run_all.sh): metrics/truth-*.jsonl (per crop and reader: cer,
ref_chars, words, oov, charlm_bpc, lm_oov) and damage-truth.jsonl (hyp_rate, hyp_known, hyp_words).
Pages pool their crops (CER weighted by reference chars, measures by words), as validate.py does.
For every pair of readers on the same page and every measure (damage, unknown, oov, lm_oov):
  - Spearman rho between the measure's drop and the CER drop, with a 95% page bootstrap;
  - a threshold for the measure that matches damage's 0.02 in selectivity (the same share of pairs passes),
    and the share of passing pairs whose CER really fell, plus their mean CER drop.
Only numbers are written, never text. Higher measure = worse.
"""
import argparse, glob, json, random
from collections import defaultdict
import numpy as np

ap = argparse.ArgumentParser()
ap.add_argument("--metrics-dir", required=True); ap.add_argument("--damage", required=True)
ap.add_argument("--out", required=True); ap.add_argument("--boot", type=int, default=1000)
ap.add_argument("--seed", type=int, default=1)
a = ap.parse_args()
MEAS = ["damage", "unknown", "oov", "lm_oov"]

rows = []
for p in sorted(glob.glob(f"{a.metrics_dir}/truth-*.jsonl")):
    rows += [json.loads(l) for l in open(p, encoding="utf-8")]
dmg = {}
for l in open(a.damage, encoding="utf-8"):
    d = json.loads(l); dmg[d["id"]] = d
for r in rows:
    d = dmg.get(r["reader"] + "|" + r["unit"], {})
    r["damage"] = d.get("hyp_rate")
    r["unknown"] = (1 - d["hyp_known"] / d["hyp_words"]) if d.get("hyp_words") else None
    if r.get("lm_oov") is None and r.get("charlm_bpc") is not None and r.get("oov") is not None:
        r["lm_oov"] = r["charlm_bpc"] / 1.185 + r["oov"] / 0.117  # qe-v1's fixed pooled-truth scaling
rows = [r for r in rows if r.get("cer") is not None and r["reader"] != "as"]

# page level
g = defaultdict(list)
for r in rows:
    g[(r["lang"], r["page"], r["reader"])].append(r)
pages = {}
for (lang, pg, rd), xs in g.items():
    rc = sum(max(x["ref_chars"], 1) for x in xs)
    o = {"cer": sum(x["cer"] * max(x["ref_chars"], 1) for x in xs) / rc}
    for m in MEAS:
        v = [(x[m], max(x["words"], 1)) for x in xs if x.get(m) is not None]
        o[m] = sum(a_ * w for a_, w in v) / sum(w for _, w in v) if v else None
    pages[(lang, pg, rd)] = o

pairs = defaultdict(list)  # lang -> list of (page, dCER, {m: dm}); d = first minus second (positive = second better)
byp = defaultdict(dict)
for (lang, pg, rd), o in pages.items():
    byp[(lang, pg)][rd] = o
for (lang, pg), rs in byp.items():
    names = sorted(rs)
    for i in range(len(names)):
        for j in range(len(names)):
            if i == j: continue
            x, y = rs[names[i]], rs[names[j]]
            dm = {m: (x[m] - y[m]) if x[m] is not None and y[m] is not None else None for m in MEAS}
            pairs[lang].append((pg, x["cer"] - y["cer"], dm, names[i], names[j]))


def rank(v):
    v = np.asarray(v, float); o = np.argsort(v, kind="mergesort"); r = np.empty(len(v)); r[o] = np.arange(len(v))
    s = v[o]; i = 0
    while i < len(s):
        j = i
        while j + 1 < len(s) and s[j + 1] == s[i]: j += 1
        r[o[i:j + 1]] = (i + j) / 2; i = j + 1
    return r


def rho(x, y):
    if len(x) < 5: return None
    rx, ry = rank(x), rank(y)
    if rx.std() == 0 or ry.std() == 0: return None
    return float(np.corrcoef(rx, ry)[0, 1])


rnd = random.Random(a.seed)
out = {"note": "page-level reader pairs on the #968 truth sets; d = first reader minus second", "langs": {}}
for lang in sorted(pairs) + ["all"]:
    P = [p for l in pairs for p in pairs[l]] if lang == "all" else pairs[lang]
    pg_ids = sorted({(p[0]) for p in P})
    res = {"pairs": len(P) // 2, "pages": len(pg_ids), "measures": {}}
    # damage selectivity at 0.02
    dsel = [p for p in P if p[2]["damage"] is not None]
    share = sum(1 for p in dsel if p[2]["damage"] >= 0.02) / len(dsel) if dsel else None
    res["damage_pass_share_at_0.02"] = share
    for m in MEAS:
        Q = [p for p in P if p[2][m] is not None]
        if len(Q) < 10: continue
        r0 = rho([p[2][m] for p in Q], [p[1] for p in Q])
        bys = defaultdict(list)
        for p in Q: bys[p[0]].append(p)
        keys = list(bys); bs = []
        for _ in range(a.boot):
            S = [p for k in (rnd.choice(keys) for _ in keys) for p in bys[k]]
            v = rho([p[2][m] for p in S], [p[1] for p in S])
            if v is not None: bs.append(v)
        lo, hi = (float(np.percentile(bs, 2.5)), float(np.percentile(bs, 97.5))) if bs else (None, None)
        e = {"rho": r0, "rho_lo": lo, "rho_hi": hi}
        if share:
            thr = 0.02 if m == "damage" else float(np.quantile([p[2][m] for p in Q], 1 - share))
            sel = [p for p in Q if (p[2][m] >= thr if thr > 0 else p[2][m] > thr)]
            if sel:
                e.update({"threshold": thr, "passing_pairs": len(sel),
                          "cer_fell_share": sum(1 for p in sel if p[1] > 0) / len(sel),
                          "mean_cer_drop_pt": 100 * float(np.mean([p[1] for p in sel]))})
        res["measures"][m] = e
    out["langs"][lang] = res
json.dump(out, open(a.out, "w"), indent=1)
for lang, r in out["langs"].items():
    print(lang, r["pages"], "pages", r["pairs"], "pairs")
    for m, e in r["measures"].items():
        f = lambda k, n=2: "-" if e.get(k) is None else f"{e[k]:.{n}f}"
        print(f"  {m:8s} rho {f('rho')} ({f('rho_lo')}..{f('rho_hi')})  thr {f('threshold', 4)}"
              f"  pass {e.get('passing_pairs', 0)}  fell {f('cer_fell_share')}  drop {f('mean_cer_drop_pt')}pt")
