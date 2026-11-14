#!/usr/bin/env python3
"""Does the proxy's per-language aggregate verdict match the truth? For each truth language and reader
pair: pooled true CER of each reader, the share of 1-point pages and crops where each is truly better,
and the share where lm_oov (with the known-word guard) prefers the first reader, with page-bootstrap
intervals. The unlabelled verdicts are this aggregate, so this is the check that matches their use.

    aggregate_check.py --metrics-dir metrics --verdicts data/verdicts.json --out data/aggregate-check.json
"""
import argparse, glob, json, random
from collections import defaultdict

ap = argparse.ArgumentParser()
ap.add_argument("--metrics-dir", required=True)
ap.add_argument("--verdicts", required=True, help="apply.py output; its pooled truth-set standard deviations scale lm_oov")
ap.add_argument("--out", required=True)
ap.add_argument("--boot", type=int, default=1000)
a = ap.parse_args()
_sd = json.load(open(a.verdicts))["sd"]
a.sd_bpc, a.sd_oov = _sd["charlm_bpc"], _sd["oov"]
rows = []
for p in glob.glob(f"{a.metrics_dir}/truth-*.jsonl"):
    rows += [json.loads(l) for l in open(p, encoding="utf-8")]
rows = [r for r in rows if r["reader"] != "as" and r["cer"] is not None and r["charlm_bpc"] is not None and r["oov"] is not None]
for r in rows:
    r["lm_oov"] = r["charlm_bpc"] / a.sd_bpc + r["oov"] / a.sd_oov
    r["known"] = r.get("known3") or 0


def pick_first(x, y):
    if x["known"] < 0.8 * y["known"]:
        return 0.0
    if y["known"] < 0.8 * x["known"]:
        return 1.0
    return 1.0 if x["lm_oov"] < y["lm_oov"] else (0.0 if x["lm_oov"] > y["lm_oov"] else 0.5)


by = defaultdict(dict)
for r in rows:
    by[(r["lang"], r["page"], r["unit"])][r["reader"]] = r
res = {}
rnd = random.Random(1)
for lang in sorted({r["lang"] for r in rows}):
    readers = sorted({r["reader"] for r in rows if r["lang"] == lang})
    for i, A in enumerate(readers):
        for B in readers[i + 1:]:
            pg = defaultdict(list)
            for (lg, page, u), m in by.items():
                if lg == lang and A in m and B in m:
                    pg[page].append((m[A], m[B]))
            P = sorted(pg)
            if not P:
                continue

            def stat(ps):
                cr = [(x, y) for p in ps for x, y in pg[p]]
                ref = sum(max(x["ref_chars"], 1) for x, y in cr)
                ca = sum(x["cer"] * max(x["ref_chars"], 1) for x, y in cr) / ref
                cb = sum(y["cer"] * max(y["ref_chars"], 1) for x, y in cr) / ref
                big = [(x, y) for x, y in cr if abs(x["cer"] - y["cer"]) >= 0.01]
                truth = sum(x["cer"] < y["cer"] for x, y in big) / len(big) if big else None
                proxy = sum(pick_first(x, y) for x, y in big) / len(big) if big else None
                return ca, cb, truth, proxy, len(big)
            ca, cb, t, pr, nb = stat(P)
            bs = [stat([rnd.choice(P) for _ in P]) for _ in range(a.boot)]
            ci = lambda k: sorted(b[k] for b in bs if b[k] is not None)
            q = lambda v: [v[int(0.025 * len(v))], v[int(0.975 * len(v)) - 1]] if v else None
            res[f"{lang}:{A}~{B}"] = {"pages": len(P), "pairs_gap1": nb, "cer_first": ca, "cer_second": cb, "truth_first_better_crops": t, "truth_ci": q(ci(2)),
                                       "proxy_first_preferred_crops": pr, "proxy_ci": q(ci(3)),
                                       "verdict_agrees": (t is not None and pr is not None and (t > 0.5) == (pr > 0.5))}
json.dump(res, open(a.out, "w"), indent=1)
for k, e in res.items():
    if "tess3" in k and "tess6" in k:
        continue
    f = lambda v: "–" if v is None else f"{v:.2f}"
    print(f"{k:32s} pages {e['pages']:4d} CER {e['cer_first']:.3f} vs {e['cer_second']:.3f} | truth first better {f(e['truth_first_better_crops'])} {e['truth_ci']} | proxy {f(e['proxy_first_preferred_crops'])} {e['proxy_ci']} | agrees {e['verdict_agrees']}")
