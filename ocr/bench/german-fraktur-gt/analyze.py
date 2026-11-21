#!/usr/bin/env python3
"""Pool score.py CSVs, bootstrap CIs, apply decision rule A (German truth protocol).

Pooled CER = sum(edits) / sum(ref_chars). Page bootstrap resamples pages;
title-clustered bootstrap resamples titles (all their pages). 10,000
resamples, seed 20261121. Lift the BOINC hold only if the upper 95% page-
bootstrap limit of pooled folded CER on the decision pool (Chronicling
Germany 1850-1920 plus Reichsanzeiger pages not in the GPU study) is
<= 0.179. Any set whose point estimate is > 0.235 is flagged (hold and
investigate). Reads CSV rows only; no page text is ever involved.
"""
import argparse, csv, glob, random, sys
from collections import defaultdict

LIFT_UPPER, FLAG_POINT = 0.179, 0.235
N_BOOT, SEED = 10000, 20261121

def load(paths):
    rows, seen = [], set()
    for pat in paths:
        for p in sorted(glob.glob(pat)) or [pat]:
            with open(p, newline="", encoding="utf-8") as f:
                for r in csv.DictReader(f):
                    k = (r["set"], r["page_id"], r["arm"])
                    if k in seen:
                        continue
                    seen.add(k)
                    for c in ("ref_chars", "edits_raw", "edits_folded"):
                        r[c] = int(r[c])
                    for c in ("word_f1", "cpu_s"):
                        r[c] = float(r[c])
                    r["gpu_study_flag"] = int(r["gpu_study_flag"] or 0)
                    rows.append(r)
    return rows

def pooled(rows, col):
    n = sum(r["ref_chars"] for r in rows)
    return sum(r[col] for r in rows) / n if n else float("nan")

def ci(units, col, n_boot, rng):
    """units: list of lists of rows (a resampling unit each)."""
    agg = [(sum(r[col] for r in u), sum(r["ref_chars"] for r in u)) for u in units]
    k, stats = len(agg), []
    for _ in range(n_boot):
        e = c = 0
        for i in rng.choices(range(k), k=k):
            e += agg[i][0]; c += agg[i][1]
        stats.append(e / c if c else float("nan"))
    stats.sort()
    return stats[int(0.025 * n_boot)], stats[min(n_boot - 1, int(0.975 * n_boot))]

def by(rows, key):
    d = defaultdict(list)
    for r in rows:
        d[r[key]].append(r)
    return list(d.values())

def summarize(name, rows, n_boot, seed):
    pages = [[r] for r in rows]
    titles = by(rows, "title")
    out = {"name": name, "pages": len(rows), "titles": len(titles),
           "ref_chars": sum(r["ref_chars"] for r in rows),
           "cer_f": pooled(rows, "edits_folded"), "cer_r": pooled(rows, "edits_raw"),
           "f1": sum(r["word_f1"] for r in rows) / len(rows),
           "cpu": sum(r["cpu_s"] for r in rows) / len(rows)}
    out["page_ci"] = ci(pages, "edits_folded", n_boot, random.Random(seed))
    out["title_ci"] = ci(titles, "edits_folded", n_boot, random.Random(seed + 1))
    return out

def table(items):
    h = ("| group | pages | titles | ref chars | CER folded | 95% page CI | "
         "95% title CI | CER raw | word F1 | CPU s/page |")
    print(h); print("|" + "---|" * 10)
    for o in items:
        print("| %s | %d | %d | %d | %.4f | %.4f-%.4f | %.4f-%.4f | %.4f | %.4f | %.1f |" % (
            o["name"], o["pages"], o["titles"], o["ref_chars"], o["cer_f"],
            *o["page_ci"], *o["title_ci"], o["cer_r"], o["f1"], o["cpu"]))

def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--csv", nargs="+", required=True, help="score.py output CSVs (globs ok)")
    ap.add_argument("--arm", default="frak2021", help="deployed arm (decides)")
    ap.add_argument("--ref-arm", default="deu_latf", help="reference arm (reported)")
    ap.add_argument("--pool-sets", default="chronicling,reichsanzeiger",
                    help="sets in the decision pool (GPU-study pages excluded)")
    ap.add_argument("--n-boot", type=int, default=N_BOOT)
    ap.add_argument("--seed", type=int, default=SEED)
    a = ap.parse_args(argv)
    rows = load(a.csv)
    pool_sets = set(a.pool_sets.split(","))
    verdict = None
    for arm in (a.arm, a.ref_arm):
        rs = [r for r in rows if r["arm"] == arm]
        if not rs:
            print("\n(no rows for arm %s)" % arm); continue
        items = []
        for s in sorted({r["set"] for r in rs}):
            sub = [r for r in rs if r["set"] == s]
            items.append(summarize(s, sub, a.n_boot, a.seed))
            for st in sorted({r["stratum"] for r in sub}):
                sst = [r for r in sub if r["stratum"] == st]
                if len({r["stratum"] for r in sub}) > 1:
                    items.append(summarize("%s / %s" % (s, st), sst, a.n_boot, a.seed))
            g = [r for r in sub if r["gpu_study_flag"]]
            if g:
                items.append(summarize("%s / GPU-study pages" % s, g, a.n_boot, a.seed))
        pool = [r for r in rs if r["set"] in pool_sets and not r["gpu_study_flag"]]
        pool_o = summarize("DECISION POOL", pool, a.n_boot, a.seed) if pool else None
        if pool_o:
            items.append(pool_o)
        print("\n### Arm %s%s\n" % (arm, "" if arm == a.arm else " (reference, no decision)"))
        table(items)
        if arm == a.arm:
            flags = [o["name"] for o in items if " / " not in o["name"]
                     and o["name"] != "DECISION POOL" and o["cer_f"] > FLAG_POINT]
            if pool_o:
                up = pool_o["page_ci"][1]
                verdict = "LIFT" if up <= LIFT_UPPER else "HOLD"
                print("\nDecision rule A: upper 95%% page-bootstrap limit of pooled folded CER "
                      "= %.4f vs %.3f -> **%s**" % (up, LIFT_UPPER, verdict))
                tu = pool_o["title_ci"][1]
                if verdict == "LIFT" and tu > LIFT_UPPER:
                    print("Note: title-clustered upper limit %.4f exceeds %.3f; "
                          "the rule is on the page bootstrap, but look at it." % (tu, LIFT_UPPER))
            else:
                print("\nNo decision-pool pages scored yet.")
            if flags:
                print("FLAG: set(s) with point estimate > %.3f: %s -> hold and investigate"
                      % (FLAG_POINT, ", ".join(flags)))
            if verdict == "LIFT" and flags:
                print("A flagged set overrides: HOLD.")
    return 0

if __name__ == "__main__":
    sys.exit(main())
