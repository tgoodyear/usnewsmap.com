#!/usr/bin/env python3
"""Prints the report's tables from analyze.py's results.json.   tables.py --results results.json"""
import argparse, json
ap = argparse.ArgumentParser(); ap.add_argument("--results", required=True); a = ap.parse_args()
r = json.load(open(a.results))
f = lambda x, d=2: "-" if x is None else f"{x:.{d}f}"
p1 = lambda x: "-" if x is None else f"{x*100:.0f}%"
pt = lambda x: "-" if x is None else f"{x*100:+.1f}"
NAME = {"eng": "English", "deu": "German", "fra": "French", "fin": "Finnish", "swe": "Swedish"}
ORDER = ["eng", "fra", "swe", "fin", "deu"]
for proxy in ("damage", "unknown"):
    for level in ("crop", "page", "chunk"):
        for tgt in ("cer", "folded"):
            print(f"\n### {proxy} / {level} / {tgt}")
            print("| lang | units | rho (95%) | CER drop at 0.02 pt (95%) | up n | up: CER better | up: CER >=1pt | missed gain | down n | down: CER worse |")
            print("|---|---|---|---|---|---|---|---|---|---|")
            for L in ORDER:
                b = r[L][f"{proxy}_{level}_{tgt}"]; p = b["point"]
                rc, ic = b.get("rho_ci", [None, None]), b.get("implied_ci", [None, None])
                print(f"| {NAME[L]} | {p['n']} | {f(p['rho'])} ({f(rc[0])} to {f(rc[1])}) | {pt(p['implied_cer_at_t'])} ({pt(ic[0])} to {pt(ic[1])}) | {p['n_up']} | {p1(p['up_cer_improves'])} | {p1(p['up_cer_step'])} | {p1(p['missed_gain'])} | {p['n_down']} | {p1(p['down_cer_worsens'])} |")
