#!/usr/bin/env python3
"""lm_oov and XLM-R pseudo-perplexity on the same masked-LM sample of truth crops: Spearman's rho with
CER and paired accuracy (1-point pairs, omission guard), so the two are compared on identical pairs.

    mlm_same_sample.py --metrics-dir metrics --mlm mlm-truth.jsonl --verdicts data/verdicts.json --out data/mlm-same-sample.json
"""
import argparse, glob, json
from collections import defaultdict
import numpy as np

ap = argparse.ArgumentParser()
ap.add_argument("--metrics-dir", required=True)
ap.add_argument("--mlm", required=True)
ap.add_argument("--verdicts", required=True)
ap.add_argument("--out", required=True)
a = ap.parse_args()
sd = json.load(open(a.verdicts))["sd"]
mlm = {}
for l in open(a.mlm, encoding="utf-8"):
    d = json.loads(l)
    if d.get("pll_per_token") is not None:
        mlm[(d["reader"], d["unit"])] = d["pll_per_token"]
rows = []
for p in glob.glob(f"{a.metrics_dir}/truth-*.jsonl"):
    for l in open(p, encoding="utf-8"):
        r = json.loads(l)
        k = (r["reader"], r["unit"])
        if k in mlm and r["cer"] is not None and r["charlm_bpc"] is not None and r["oov"] is not None:
            r["mlm"] = mlm[k]
            r["lm_oov"] = r["charlm_bpc"] / sd["charlm_bpc"] + r["oov"] / sd["oov"]
            rows.append(r)


def rho(x, y):
    rx = np.argsort(np.argsort(x)); ry = np.argsort(np.argsort(y))
    return float(np.corrcoef(rx, ry)[0, 1])


def pick(x, y, f):
    kx, ky = x.get("known3") or 0, y.get("known3") or 0
    if kx < 0.8 * ky:
        return -1
    if ky < 0.8 * kx:
        return 1
    return (x[f] < y[f]) - (x[f] > y[f])


res = {}
for lang in sorted({r["lang"] for r in rows}) + ["all"]:
    rs = [r for r in rows if lang == "all" or r["lang"] == lang]
    g = defaultdict(dict)
    for r in rs:
        g[r["unit"]][r["reader"]] = r
    P = [(m[a_], m[b]) for m in g.values() for i, a_ in enumerate(sorted(m)) for b in sorted(m)[i + 1:]]
    P = [(x, y) for x, y in P if abs(x["cer"] - y["cer"]) >= 0.01]
    e = {"readings": len(rs), "pairs_gap1": len(P)}
    for f in ("lm_oov", "mlm"):
        e[f"rho_{f}"] = rho([r[f] for r in rs], [r["cer"] for r in rs])
        ok = [pick(x, y, f) == (1 if x["cer"] < y["cer"] else -1) for x, y in P]
        e[f"acc_{f}"] = sum(ok) / len(ok) if ok else None
    lq = [(x, y) for x, y in P if {x["reader"], y["reader"]} == {"loc", "qwen8b-local"}]
    if lq:
        for f in ("lm_oov", "mlm"):
            qb = [pick(x, y, f) == (1 if x["cer"] < y["cer"] else -1) for x, y in lq if min(x, y, key=lambda r: r["cer"])["reader"] == "qwen8b-local"]
            lb = [pick(x, y, f) == (1 if x["cer"] < y["cer"] else -1) for x, y in lq if min(x, y, key=lambda r: r["cer"])["reader"] == "loc"]
            e[f"loc_qwen_{f}"] = {"pairs": len(lq), "loc_better": len(lb), "acc_when_qwen_better": sum(qb) / len(qb) if qb else None, "acc_when_loc_better": sum(lb) / len(lb) if lb else None}
    res[lang] = e
json.dump(res, open(a.out, "w"), indent=1)
print(json.dumps(res, indent=1))
