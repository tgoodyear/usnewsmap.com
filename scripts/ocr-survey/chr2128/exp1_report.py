#!/usr/bin/env python3
"""Experiment 1's results file (#2128) from `score_layout.py`'s output: each arm's pooled Cherokee and
all-line recall, precision, their 95% page-bootstrap intervals, the difference from S0 with its interval,
the issue's rule, and per-page outliers (it scores nothing itself).

Outliers, per arm other than the baseline: the pages where its Cherokee recall moved most against S0 (both
ways; pages with at least 5 Cherokee truth lines), and for every arm the pages with the lowest Cherokee and
all-line recall.

    exp1_report.py --score score.json [--score score-s1s2.json] --commit SHA --out results.json [--top 8]
"""
import argparse
import json


def rate(a, b):
    return round(a / b, 4) if b else None


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--score", action="append", required=True)
    ap.add_argument("--commit", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--top", type=int, default=8)
    a = ap.parse_args()
    out = {"issue": 2128, "experiment": 1, "code_commit": a.commit, "arms": {}, "outliers": {}, "scores": a.score}
    for path in a.score:
        s = json.load(open(path))
        out.setdefault("pages", s["pages"])
        out.setdefault("pages_sha256", s["pages_sha256"])
        out.update({"iou": s["iou"], "draws": s["draws"], "seed": s["seed"], "baseline": s["baseline"]})
        base = s["arms"][s["baseline"]]["per_page"]
        for name, r in s["arms"].items():
            out["arms"][name] = {k: v for k, v in r.items() if k != "per_page"}
            pp = r["per_page"]
            low_chr = sorted((p for p in pp if pp[p]["chr_truth"] >= 5), key=lambda p: pp[p]["chr_hit"] / pp[p]["chr_truth"])
            low_all = sorted(pp, key=lambda p: pp[p]["truth_hit"] / max(pp[p]["truth"], 1))
            o = {"lowest_chr_recall": [{"page": p, "chr_recall": rate(pp[p]["chr_hit"], pp[p]["chr_truth"]),
                                        "chr_truth": pp[p]["chr_truth"]} for p in low_chr[:a.top]],
                 "lowest_recall": [{"page": p, "recall": rate(pp[p]["truth_hit"], pp[p]["truth"])} for p in low_all[:a.top]]}
            if name != s["baseline"]:
                d = {p: pp[p]["chr_hit"] / pp[p]["chr_truth"] - base[p]["chr_hit"] / base[p]["chr_truth"]
                     for p in pp if pp[p]["chr_truth"] >= 5}
                order = sorted(d, key=d.get)
                o["chr_recall_minus_baseline_worst"] = [{"page": p, "diff": round(d[p], 4)} for p in order[:a.top]]
                o["chr_recall_minus_baseline_best"] = [{"page": p, "diff": round(d[p], 4)} for p in order[::-1][:a.top]]
                o["pages_better_same_worse"] = [sum(v > 0 for v in d.values()), sum(v == 0 for v in d.values()),
                                                sum(v < 0 for v in d.values())]
            out["outliers"][name] = o
    rules = {n: r["rule"] for n, r in out["arms"].items() if "rule" in r}
    out["verdict"] = {n: ("passes" if r["passes"] else "fails") for n, r in rules.items()}
    json.dump(out, open(a.out, "w"), indent=1, sort_keys=True)
    print(json.dumps({n: {k: r.get(k) for k in ("chr_recall", "recall", "precision", "ci95", "minus_baseline",
                                                  "minus_baseline_ci95", "rule")} for n, r in out["arms"].items()}, indent=1))


if __name__ == "__main__":
    main()
