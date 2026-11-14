#!/usr/bin/env python3
"""Side-by-side numbers for the character-model training text: Gutenberg only, Gutenberg topped up from
Wikisource, and Wikisource only. Reads each variant's validation.json, verdicts.json and
aggregate-check.json.

    compare_lm_sources.py --variant gutenberg=out --variant gutenberg+wikisource=out-gw --variant wikisource=out-w --out data/lm-sources.json
"""
import argparse, json, os

ap = argparse.ArgumentParser()
ap.add_argument("--variant", action="append", required=True, help="NAME=DIR")
ap.add_argument("--out", required=True)
a = ap.parse_args()
res = {}
for spec in a.variant:
    name, d = spec.split("=", 1)
    V = json.load(open(os.path.join(d, "validation.json")))
    R = json.load(open(os.path.join(d, "verdicts.json")))
    A = json.load(open(os.path.join(d, "aggregate-check.json")))
    e = {"truth": {}, "unlabelled": {}, "aggregate_agree": sum(x["verdict_agrees"] for x in A.values()), "aggregate_cases": len(A)}
    for lang, L in V["languages"].items():
        e["truth"][lang] = {f: {"rho_crop": L["corr_crop"].get(f, {}).get("rho"), "rho_page": L["corr_page"].get(f, {}).get("rho"),
                                "paired_crop": L["paired_crop"][f]["kguard_gap1"], "paired_page": L["paired_page"][f]["kguard_gap1"]}
                            for f in ("charlm_bpc", "lm_oov")}
    e["lolo"] = {k: v for k, v in V["lolo_margins_without_deu"]["lm_oov"].items()}
    for g, x in R["groups"].items():
        y = x.get("lm_oov_guard", {}); z = x.get("charlm_bpc_guard", {})
        e["unlabelled"][g] = {"lm_oov_qwen_preferred_crops": y.get("qwen_preferred_crops"), "ci": y.get("ci_crops"),
                              "charlm_qwen_preferred_crops": z.get("qwen_preferred_crops"),
                              "charlm_mean_diff": x.get("charlm_bpc", {}).get("mean_diff_qwen_minus_loc")}
    res[name] = e
json.dump(res, open(a.out, "w"), indent=1)
names = list(res)
f = lambda v: "  –  " if v is None else f"{v:5.2f}"
print("truth: rho crop / paired crop (lm_oov, charlm_bpc) per variant")
for lang in res[names[0]]["truth"]:
    print(f"{lang:4s}", " || ".join(" ".join(f(res[n]["truth"][lang][m][k]) for m in ("lm_oov", "charlm_bpc") for k in ("rho_crop", "paired_crop")) for n in names))
print("lolo held-out accuracy", {n: {k: (v.get("heldout_acc_gap1") and round(v["heldout_acc_gap1"], 2)) for k, v in res[n]["lolo"].items()} for n in names})
print("aggregate agree", {n: (res[n]["aggregate_agree"], res[n]["aggregate_cases"]) for n in names})
for g in res[names[0]]["unlabelled"]:
    print(f"{g:9s}", " || ".join(f(res[n]["unlabelled"][g]["lm_oov_qwen_preferred_crops"]) + " " + f(res[n]["unlabelled"][g]["charlm_qwen_preferred_crops"]) + " " + f(res[n]["unlabelled"][g]["charlm_mean_diff"]) for n in names))
