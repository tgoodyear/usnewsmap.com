#!/usr/bin/env python3
"""Writes data/tables.md from validation.json, verdicts.json and confusions.json.

    tables.py --validation data/validation.json --verdicts data/verdicts.json --confusions data/confusions-top.json --out data/tables.md
"""
import argparse, json

ap = argparse.ArgumentParser()
ap.add_argument("--validation", required=True)
ap.add_argument("--verdicts", required=True)
ap.add_argument("--confusions", required=True)
ap.add_argument("--out", required=True)
a = ap.parse_args()
V = json.load(open(a.validation)); R = json.load(open(a.verdicts)); C = json.load(open(a.confusions))
f2 = lambda v: "–" if v is None else f"{v:.2f}"
L = []
feats = [f for f in ["lm_oov", "charlm_bpc", "oov", "unknown", "nearmiss_any", "nearmiss_conf30", "nearmiss_conf50", "nearmiss_conf80", "novel",
                     "damage", "garbage", "script_mix", "combo_lolo", "mlm_pppl"] if f in V["languages"]["all"]["paired_crop"]]
langs = [l for l in V["languages"] if l != "all"] + ["all"]
for level in ("crop", "page"):
    L.append(f"## Spearman's rho with CER, {level} level (95% page bootstrap)\n")
    L.append("| Measure | " + " | ".join(langs) + " |")
    L.append("|---|" + "---|" * len(langs))
    for f in feats:
        cells = []
        for l in langs:
            c = V["languages"][l][f"corr_{level}"].get(f)
            cells.append("–" if not c or c["rho"] is None else f"{c['rho']:.2f} ({f2(c['lo'])} to {f2(c['hi'])})")
        L.append(f"| {f} | " + " | ".join(cells) + " |")
    L.append("")
    L.append(f"## Paired decision accuracy, {level} level: all pairs / pairs with a CER gap of 1 point or more / the same with the known-word omission guard\n")
    L.append("| Measure | " + " | ".join(langs) + " |")
    L.append("|---|" + "---|" * len(langs))
    for f in feats:
        cells = []
        for l in langs:
            q = V["languages"][l][f"paired_{level}"][f]
            cells.append(f"{f2(q['all'])} / {f2(q['gap1'])} / {f2(q['kguard_gap1'])}")
        L.append(f"| {f} | " + " | ".join(cells) + " |")
    L.append("| pairs (1 pt) | " + " | ".join(str(V["languages"][l][f"paired_{level}"]["oov"]["n_gap1"]) for l in langs) + " |")
    L.append("")
L.append("## Reader pairs, crop level, pairs with a CER gap of 1 point or more (guard on)\n")
L.append("Accuracy / balanced accuracy (mean of the accuracies when the first reader is better and when the second is). `first better` is the truth.\n")
L.append("| Language | Pair | Pairs | First better | lm_oov | charlm_bpc | oov | unknown | damage |" + (" mlm_pppl |" if "mlm_pppl" in feats else ""))
L.append("|---|---|---|---|---|---|---|---|---|" + ("---|" if "mlm_pppl" in feats else ""))
for l in langs[:-1]:
    for k, e in V["languages"][l]["by_pair_crop"].items():
        if "tess3" in k and "tess6" in k:
            continue
        ms = ["lm_oov", "charlm_bpc", "oov", "unknown", "damage"] + (["mlm_pppl"] if "mlm_pppl" in feats else [])
        L.append(f"| {l} | {k.replace('~', ' vs ')} | {e['n_gap1']} | {f2(e['first_better_gap1'])} | " + " | ".join(f"{f2(e.get(m))} / {f2(e.get(m + '_balanced'))}" for m in ms) + " |")
L.append("")
L.append("## Leave-one-language-out decision margins (guard on, 1-point pairs)\n")
L.append("The margin is chosen on the other languages: the smallest gap at which every one of them reaches 90% accuracy with at least 5% of pairs decided. `–`: no margin met that on the training languages.\n")
L.append("| Measure | Held out | Margin | Held-out accuracy | Held-out share decided |")
L.append("|---|---|---|---|---|")
for f in ["lm_oov", "oov", "charlm_bpc", "unknown", "damage", "combo_lolo"] + (["mlm_pppl"] if "mlm_pppl" in feats else []):
    for l, m in V["lolo_margins"][f].items():
        L.append(f"| {f} | {l} | {f2(m.get('margin'))} | {f2(m.get('heldout_acc_gap1'))} | {f2(m.get('heldout_coverage_gap1'))} |")
L.append("")
L.append("German kept out of every training pool:\n")
L.append("| Measure | Held out | Margin | Held-out accuracy | Held-out share decided |")
L.append("|---|---|---|---|---|")
for f in ["lm_oov", "oov", "charlm_bpc", "unknown", "damage"] + (["mlm_pppl"] if "mlm_pppl" in feats else []):
    for l, m in V["lolo_margins_without_deu"][f].items():
        L.append(f"| {f} | {l} | {f2(m.get('margin'))} | {f2(m.get('heldout_acc_gap1'))} | {f2(m.get('heldout_coverage_gap1'))} |")
L.append("")
L.append("## Unlabelled crops: Qwen 3-VL 8B against LoC (proxy evidence)\n")
L.append(f"Measure: lm_oov (character-model bits per character / {R['sd']['charlm_bpc']:.3f} + out-of-lexicon rate / {R['sd']['oov']:.3f}); guard: in-lexicon word count. Margin {f2(R['lm_oov_margin'])}. Intervals: 95% page bootstrap.\n")
L.append("| Group | Titles | Pages | Crops | Qwen no words | Length Qwen/LoC (median) | Known words Qwen/LoC (median) | Qwen preferred, crops (guard) | Qwen preferred, pages (guard) | Mean lm_oov Qwen − LoC | Qwen preferred, crops, no guard | Over-read flag share | Qwen preferred, crops (guard + over-read) |")
L.append("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
for g, e in sorted(R["groups"].items()):
    x = e.get("lm_oov_guard", {}); y = e.get("lm_oov", {})
    ci = lambda v: "–" if not v else f"{v[0]:.2f} to {v[1]:.2f}"
    L.append(f"| {g} | {e.get('titles')} | {e['pages']} | {e['crops']} | {f2(e.get('qwen_no_words_share'))} | {f2(e['qwen_loc_length_ratio_median'])} | {f2(e.get('qwen_loc_known_words_ratio_median'))} | "
             f"{f2(x.get('qwen_preferred_crops'))} ({ci(x.get('ci_crops'))}) | {f2(x.get('qwen_preferred_pages'))} ({ci(x.get('ci_pages'))}) | "
             f"{f2(y.get('mean_diff_qwen_minus_loc'))} ({ci(y.get('ci_diff'))}) | {f2(y.get('qwen_preferred_crops'))} | {f2(e.get('overread_share'))} | "
             f"{f2(e.get('lm_oov_guard_overread', {}).get('qwen_preferred_crops'))} ({ci(e.get('lm_oov_guard_overread', {}).get('ci_crops'))}) |")
L.append("")
if any("mlm_pppl" in e for e in R["groups"].values()):
    L.append("## Unlabelled crops: masked-LM pseudo-perplexity sample (XLM-R base)\n")
    L.append("| Group | Crops scored | Qwen preferred (no guard) | Mean NLL/token Qwen − LoC |")
    L.append("|---|---|---|---|")
    for g, e in sorted(R["groups"].items()):
        m = e.get("mlm_pppl")
        if m:
            L.append(f"| {g} | {m['crops_n']} | {f2(m['qwen_preferred_crops'])} | {f2(m['mean_diff_qwen_minus_loc'])} ({m['ci_diff'][0]:.2f} to {m['ci_diff'][1]:.2f}) |")
    L.append("")
L.append("## Commonest character edits per reader (truth sets, folded, lower-cased; expected → read)\n")
for lang, m in sorted(C.items()):
    for rd, v in sorted(m.items()):
        ops = ", ".join(f"`{k.split(chr(9))[0] or '∅'}→{k.split(chr(9))[1] or '∅'}` {n}" for k, n in list(v["ops"].items())[:10])
        L.append(f"- {lang} {rd} ({v['chars']} reference characters, {v['errors']} edits): {ops}")
open(a.out, "w").write("\n".join(L) + "\n")
