#!/usr/bin/env python3
"""The Experiment 1 scorer of #2128: line finders against LoC's ALTO lines.

Truth is `chr_layout_eval.py truth`'s file (LoC's ALTO text lines in scan pixels, with `chr` as the line
job decides it). A truth line counts as found when one box has IoU >= 0.5 with it. Recall is counted on
Cherokee truth lines (primary) and on all truth lines; precision is the share of boxes with IoU >= 0.5 with
some truth line (a lower bound: LoC's ALTO misses some printed lines and joins others, #1978). Boxes under
8 px either way are dropped first, as `chr_layout_eval.py score` does. Figures are pooled over the pages.

Intervals: 2,000 page-bootstrap draws (pages drawn with replacement, the same draws for every arm), 95%
percentile intervals for each arm's figures and for each arm minus the baseline. The issue's rule is
evaluated for each arm against the baseline: Cherokee recall at least 3 points higher with the interval of
the difference above 0, and precision no more than 2 points lower.

    score_layout.py --truth truth.jsonl --pages pages.txt --found NAME=found.jsonl [--found ...]
                    --baseline NAME --out score.json [--draws 2000] [--seed 2128]

Every page of --pages must be in the truth and in every found file.
"""
import argparse
import json

import numpy as np

import common

THR = 0.5


def iou_matrix(t, f):
    t = np.asarray(t, float).reshape(-1, 4)
    f = np.asarray(f, float).reshape(-1, 4)
    w = np.clip(np.minimum(t[:, None, 2], f[None, :, 2]) - np.maximum(t[:, None, 0], f[None, :, 0]), 0, None)
    h = np.clip(np.minimum(t[:, None, 3], f[None, :, 3]) - np.maximum(t[:, None, 1], f[None, :, 1]), 0, None)
    x = w * h
    at = ((t[:, 2] - t[:, 0]).clip(0) * (t[:, 3] - t[:, 1]).clip(0))[:, None]
    af = ((f[:, 2] - f[:, 0]).clip(0) * (f[:, 3] - f[:, 1]).clip(0))[None, :]
    with np.errstate(divide="ignore", invalid="ignore"):
        return np.where(x > 0, x / (at + af - x), 0.0)


def page_counts(truth, found):
    t = [l["box"] for l in truth]
    is_chr = np.array([bool(l["chr"]) for l in truth], bool)
    f = [l["box"] for l in found if l["box"][2] - l["box"][0] >= 8 and l["box"][3] - l["box"][1] >= 8]
    m = iou_matrix(t, f) >= THR
    t_hit = m.any(1) if len(f) else np.zeros(len(t), bool)
    f_hit = m.any(0) if len(t) else np.zeros(len(f), bool)
    return np.array([len(t), is_chr.sum(), t_hit.sum(), (t_hit & is_chr).sum(), len(f), f_hit.sum()], float)


def metrics(c):
    """c: summed [truth, chr_truth, truth_hit, chr_hit, found, found_hit]."""
    return {"recall": c[2] / c[0] if c[0] else float("nan"), "chr_recall": c[3] / c[1] if c[1] else float("nan"),
            "precision": c[5] / c[4] if c[4] else float("nan")}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--truth", required=True)
    ap.add_argument("--pages", required=True)
    ap.add_argument("--found", action="append", required=True, help="NAME=found.jsonl")
    ap.add_argument("--baseline", required=True)
    ap.add_argument("--draws", type=int, default=2000)
    ap.add_argument("--seed", type=int, default=2128)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    pages = common.read_pages(a.pages)
    truth = {r["page"]: r for r in common.read_jsonl(a.truth)}
    arms = {}
    for spec in a.found:
        name, _, path = spec.partition("=")
        arms[name] = {r["page"]: r for r in common.read_jsonl(path)}
    if a.baseline not in arms:
        raise SystemExit(f"--baseline {a.baseline} is not one of the --found names")
    for p in pages:
        if p not in truth:
            raise SystemExit(f"{p}: not in the truth file")
        for name, f in arms.items():
            if p not in f:
                raise SystemExit(f"{p}: not in {name}'s found file")
    counts = {name: np.stack([page_counts(truth[p]["lines"], f[p]["lines"]) for p in pages]) for name, f in arms.items()}
    rng = np.random.default_rng(a.seed)
    draws = rng.integers(0, len(pages), size=(a.draws, len(pages)))
    keys = ("recall", "chr_recall", "precision")
    boot = {name: {k: [] for k in keys} for name in arms}
    for d in draws:
        for name, c in counts.items():
            m = metrics(c[d].sum(0))
            for k in keys:
                boot[name][k].append(m[k])
    out = {"pages": len(pages), "pages_sha256": common.sha256_list(pages), "iou": THR, "draws": a.draws,
           "seed": a.seed, "baseline": a.baseline, "arms": {}}
    for name, c in counts.items():
        tot = c.sum(0)
        m = metrics(tot)
        row = {"truth_lines": int(tot[0]), "chr_truth_lines": int(tot[1]), "boxes": int(tot[4]),
               **{k: round(float(m[k]), 4) for k in keys},
               "ci95": {k: [round(float(np.nanpercentile(boot[name][k], q)), 4) for q in (2.5, 97.5)] for k in keys}}
        if name != a.baseline:
            diff = {k: np.array(boot[name][k]) - np.array(boot[a.baseline][k]) for k in keys}
            mb = metrics(counts[a.baseline].sum(0))
            row["minus_baseline"] = {k: round(float(m[k] - mb[k]), 4) for k in keys}
            row["minus_baseline_ci95"] = {k: [round(float(np.nanpercentile(diff[k], q)), 4) for q in (2.5, 97.5)] for k in keys}
            row["rule"] = {
                "chr_recall_up_3_points": bool(m["chr_recall"] - mb["chr_recall"] >= 0.03),
                "chr_recall_interval_above_0": bool(row["minus_baseline_ci95"]["chr_recall"][0] > 0),
                "precision_down_at_most_2_points": bool(m["precision"] - mb["precision"] >= -0.02),
            }
            row["rule"]["passes"] = all(row["rule"].values())
        row["per_page"] = {p: dict(zip(("truth", "chr_truth", "truth_hit", "chr_hit", "boxes", "boxes_hit"),
                                       map(int, c[i]))) for i, p in enumerate(pages)}
        out["arms"][name] = row
    json.dump(out, open(a.out, "w"), indent=1, sort_keys=True)
    print(json.dumps({n: {k: v for k, v in r.items() if k != "per_page"} for n, r in out["arms"].items()}, indent=1))


if __name__ == "__main__":
    main()
