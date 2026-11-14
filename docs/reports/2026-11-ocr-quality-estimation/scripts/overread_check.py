#!/usr/bin/env python3
"""The en-op crops where LoC beats a VLM reader, and an exploratory over-read guard: prefer LoC when its
crop is clean (out-of-lexicon rate <= --loc-clean) and the VLM reading is more than --ratio times longer.
Fitted and scored on the same crops (in-sample), for both VLM readers.

    overread_check.py --metrics metrics/truth-eng.jsonl --verdicts data/verdicts.json --out data/overread-enop.json
"""
import argparse, json, statistics

ap = argparse.ArgumentParser()
ap.add_argument("--metrics", required=True)
ap.add_argument("--verdicts", required=True, help="apply.py output; its pooled truth-set standard deviations scale lm_oov")
ap.add_argument("--out", required=True)
a = ap.parse_args()
_sd = json.load(open(a.verdicts))["sd"]
a.sd_bpc, a.sd_oov = _sd["charlm_bpc"], _sd["oov"]
m = {}
for l in open(a.metrics, encoding="utf-8"):
    r = json.loads(l); m[(r["unit"], r["reader"])] = r
lo = lambda r: r["charlm_bpc"] / a.sd_bpc + r["oov"] / a.sd_oov
res = {}
for vlm in ("qwen8b-local", "xiaomi-local"):
    P = [(m[(u, vlm)], m[(u, "loc")]) for (u, rd) in m if rd == vlm and (u, "loc") in m]
    P = [(q, l) for q, l in P if abs(q["cer"] - l["cer"]) >= 0.01 and q["oov"] is not None and l["oov"] is not None]
    lost = [(q, l) for q, l in P if q["cer"] > l["cer"]]
    out = {"pairs_gap1": len(P), "vlm_loses": len(lost),
           "median_vlm_chars_over_ref_when_losing": statistics.median(q["chars"] / max(1, q["ref_chars"]) for q, l in lost) if lost else None,
           "rules": []}
    for clean, ratio in [(None, None), (0.03, 1.1), (0.05, 1.05), (0.05, 1.1), (0.08, 1.1)]:
        def pick_vlm(q, l):
            if clean is not None and l["oov"] <= clean and q["chars"] > ratio * l["chars"]:
                return False
            return lo(q) < lo(l)
        vb = [pick_vlm(q, l) for q, l in P if q["cer"] < l["cer"]]
        lb = [not pick_vlm(q, l) for q, l in P if q["cer"] > l["cer"]]
        out["rules"].append({"loc_clean": clean, "ratio": ratio, "accuracy": (sum(vb) + sum(lb)) / len(P),
                             "acc_when_vlm_better": sum(vb) / len(vb), "acc_when_loc_better": sum(lb) / len(lb) if lb else None,
                             "balanced": (sum(vb) / len(vb) + sum(lb) / len(lb)) / 2 if lb else None})
    res[vlm] = out
json.dump(res, open(a.out, "w"), indent=1)
print(json.dumps(res, indent=1))
