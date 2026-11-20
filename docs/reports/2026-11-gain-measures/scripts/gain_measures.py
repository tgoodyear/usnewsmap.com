#!/usr/bin/env python3
"""Step 1 gap check: which gain measure tracks real CER drops between two readings of the same page?

    gain_measures.py --metrics-dir metrics --damage damage-truth.jsonl --out gain-measures.json [--boot 1000]

Inputs are the qe study's outputs (#1925, run_all.sh): metrics/truth-*.jsonl (per crop and reader: cer,
ref_chars, words, oov, charlm_bpc, lm_oov) and damage-truth.jsonl (hyp_rate, hyp_known, hyp_words).
Each reader pair is pooled over the crops both readers read (crops only one read are counted
and left out), as the production measures pool: CER over reference chars; damage as
sum(near) / sum(head + near) (usnm_core::damage); the unknown share as 1 - sum(known) / sum(words);
oov over words and charlm_bpc over characters, and lm_oov from those two pooled values.
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
    r["d_head"], r["d_near"] = d.get("hyp_head"), d.get("hyp_near")
    r["d_words"], r["d_known"] = d.get("hyp_words"), d.get("hyp_known")
rows = [r for r in rows if r.get("cer") is not None and r["reader"] != "as"]

# page level
g = defaultdict(list)
for r in rows:
    g[(r["lang"], r["page"], r["reader"])].append(r)
def pooled(xs, key, weight):
    v = [(x[key], x[weight]) for x in xs if x.get(key) is not None and x.get(weight)]
    tw = sum(w for _, w in v)
    return sum(a_ * w for a_, w in v) / tw if tw else None


def page_scores(xs):
    """One reader's page scores, pooled over the crops `xs`."""
    rc = sum(max(x["ref_chars"], 1) for x in xs)
    o = {"cer": sum(x["cer"] * max(x["ref_chars"], 1) for x in xs) / rc}
    hn = [(x["d_head"], x["d_near"]) for x in xs if x.get("d_head") is not None and x.get("d_near") is not None]
    den = sum(h + n for h, n in hn)
    o["damage"] = sum(n for _, n in hn) / den if den else None
    kw = [(x["d_known"], x["d_words"]) for x in xs if x.get("d_words")]
    tw = sum(w for _, w in kw)
    o["unknown"] = 1 - sum(k for k, _ in kw) / tw if tw else None
    o["oov"] = pooled(xs, "oov", "words")
    bpc = pooled(xs, "charlm_bpc", "chars")
    # qe-v1's fixed pooled-truth scaling (ocr/qe/README.md)
    o["lm_oov"] = bpc / 1.185 + o["oov"] / 0.117 if bpc is not None and o["oov"] is not None else None
    return o


# Each pair of readers is pooled over the crops both read, so a crop one reader lacks can't
# read as a gain or a loss. The crops left out are counted per language.
pairs = defaultdict(list)  # lang -> list of (page, dCER, {m: dm}); d = first minus second (positive = second better)
excluded = defaultdict(lambda: {"pair_crops_left_out": 0, "pairs_with_no_common_crop": 0})
byp = defaultdict(dict)
for (lang, pg, rd), xs in g.items():
    byp[(lang, pg)][rd] = {x["unit"]: x for x in xs}
for (lang, pg), rs in byp.items():
    names = sorted(rs)
    for i in range(len(names)):
        for j in range(len(names)):
            if i == j: continue
            ux, uy = rs[names[i]], rs[names[j]]
            common = sorted(set(ux) & set(uy))
            if i < j:
                excluded[lang]["pair_crops_left_out"] += len(set(ux) ^ set(uy))
            if not common:
                if i < j:
                    excluded[lang]["pairs_with_no_common_crop"] += 1
                continue
            x, y = page_scores([ux[u] for u in common]), page_scores([uy[u] for u in common])
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
    ex = ({k: sum(e[k] for e in excluded.values()) for k in ("pair_crops_left_out", "pairs_with_no_common_crop")}
          if lang == "all" else dict(excluded[lang]))
    res = {"pairs": len(P) // 2, "pages": len(pg_ids), "excluded": ex, "measures": {}}
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
    print(lang, r["pages"], "pages", r["pairs"], "pairs", "crops left out of pairs:", r["excluded"]["pair_crops_left_out"],
          "pairs with no common crop:", r["excluded"]["pairs_with_no_common_crop"])
    for m, e in r["measures"].items():
        f = lambda k, n=2: "-" if e.get(k) is None else f"{e[k]:.{n}f}"
        print(f"  {m:8s} rho {f('rho')} ({f('rho_lo')}..{f('rho_hi')})  thr {f('threshold', 4)}"
              f"  pass {e.get('passing_pairs', 0)}  fell {f('cer_fell_share')}  drop {f('mean_cer_drop_pt')}pt")
