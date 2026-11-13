#!/usr/bin/env python3
"""Applies the validated measures to the unlabelled Qwen 3-VL 8B and LoC readings of the same crops.

    apply.py --metrics-dir metrics --damage damage-unlabelled.jsonl --truth-validation data/validation.json \
        [--mlm mlm-unlabelled.jsonl] --out data/verdicts.json [--boot 2000 --seed 1]

Per language (Serbian split by the script of the Qwen reading): for each crop, which reading each
measure prefers (lower is better; the omission guard makes a reading under 80% of the other's length
lose); the same per page, with measures averaged over the page's crops weighted by words. Shares of
units where Qwen is preferred, the mean Qwen-minus-LoC difference of each measure, and 95% percentile
bootstrap intervals over pages. lm_oov uses the standard deviations of the pooled truth sets.
Also the share of crops where the margin rule decides (|difference| >= the margin chosen on the truth
sets) and Qwen's share among those. This is proxy evidence, not measured accuracy.
"""
import argparse, glob, json, random
from collections import defaultdict
import numpy as np

ap = argparse.ArgumentParser()
ap.add_argument("--metrics-dir", required=True)
ap.add_argument("--damage", required=True)
ap.add_argument("--truth-validation", required=True)
ap.add_argument("--mlm", default=None)
ap.add_argument("--out", required=True)
ap.add_argument("--boot", type=int, default=2000)
ap.add_argument("--seed", type=int, default=1)
a = ap.parse_args()

V = json.load(open(a.truth_validation))
truth = []
for p in glob.glob(f"{a.metrics_dir}/truth-*.jsonl"):
    truth += [json.loads(l) for l in open(p, encoding="utf-8")]
truth = [r for r in truth if r["reader"] != "as"]
SD = {f: float(np.std([r[f] for r in truth if r.get(f) is not None])) for f in ("charlm_bpc", "oov")}
# the lm_oov margin chosen on the truth languages other than German (validate.py, part c): with German
# in the pool no margin reaches 90% there, so the rule is the one that held on eng, fra, fin and swe
MARGIN = V["lolo_margins"]["lm_oov"]["all"]["margin"] or V["lolo_margins_without_deu"]["lm_oov"]["all_but_deu"]["margin"]

rows = []
for p in sorted(glob.glob(f"{a.metrics_dir}/unlabelled-*.jsonl")):
    rows += [json.loads(l) for l in open(p, encoding="utf-8")]
dmg = {}
for l in open(a.damage, encoding="utf-8"):
    d = json.loads(l); dmg[d["id"]] = d
mlm = {}
if a.mlm:
    for l in open(a.mlm, encoding="utf-8"):
        d = json.loads(l); mlm[d["reader"] + "|" + d["unit"]] = d.get("pll_per_token")
for r in rows:
    d = dmg.get(r["reader"] + "|" + r["unit"], {})
    r["damage"] = d.get("hyp_rate")
    r["unknown"] = (1 - d["hyp_known"] / d["hyp_words"]) if d.get("hyp_words") else None
    r["lm_oov"] = (r["charlm_bpc"] / SD["charlm_bpc"] + r["oov"] / SD["oov"]) if r.get("charlm_bpc") is not None and r.get("oov") is not None else None
    r["mlm_pppl"] = mlm.get(r["reader"] + "|" + r["unit"])

crops = defaultdict(dict)
for r in rows:
    crops[r["unit"]][r["reader"]] = r
FE = ["lm_oov", "oov", "charlm_bpc", "unknown", "damage", "nearmiss_any", "garbage", "script_mix", "mlm_pppl"]


def group(q):
    lang = q["lang"]
    if lang == "srp":
        s = q.get("srp_cyrillic_share")
        return "srp-cyrl" if (s or 0) >= 0.5 else "srp-latn"
    return lang


def known(r):
    """In-lexicon words of three or more letters: the omission guard's count. Shorter tokens are left out
    because LoC's fragments ("a", "y", "S") are often in the lexicon."""
    return r.get("known3") or 0


def overread(q, l):
    """Exploratory, fitted on en-op only: LoC's crop is clean (out-of-lexicon rate <= 0.05) and Qwen's
    reading is over 10% longer, the pattern of the en-op crops Qwen lost (it read past the crop)."""
    return l.get("oov") is not None and l["oov"] <= 0.05 and q["chars"] > 1.1 * l["chars"]


def pick(q, l, f, guard):
    if guard == "overread" and overread(q, l):
        return 0.0
    if guard:  # a reading with under 80% of the other's in-lexicon words of 3+ letters loses (validate.py's "known" guard)
        if known(q) < 0.8 * known(l):
            return 0.0
        if known(l) < 0.8 * known(q):
            return 1.0
    if q.get(f) is None or l.get(f) is None:
        return None
    return 1.0 if q[f] < l[f] else (0.0 if q[f] > l[f] else 0.5)


def page_agg(units):
    out = {}
    for rd in ("qwen", "loc"):
        xs = [u[rd] for u in units]
        w = [max(x["words"], 1) for x in xs]
        agg = {"chars": sum(x["chars"] for x in xs), "words": sum(x["words"] for x in xs), "known3": sum(x.get("known3") or 0 for x in xs),
               "lex_hit": (sum(known(x) for x in xs) / max(1, sum(x["words"] for x in xs)))}
        for f in FE:
            v = [(x[f], wi) for x, wi in zip(xs, w) if x.get(f) is not None]
            agg[f] = sum(a_ * b for a_, b in v) / sum(b for _, b in v) if v else None
        out[rd] = agg
    return out


rnd = random.Random(a.seed)
res = {"sd": SD, "lm_oov_margin": MARGIN, "groups": {}}
G = defaultdict(lambda: defaultdict(list))
for u, m in crops.items():
    if "qwen" in m and "loc" in m:
        G[group(m["qwen"])][m["qwen"]["page"]].append(m)
for g, pages in sorted(G.items()):
    P = sorted(pages)
    units = [u for p in P for u in pages[p]]
    e = {"pages": len(P), "crops": len(units), "titles": len({p.split("_")[0] for p in P})}
    ratio = [u["qwen"]["chars"] / u["loc"]["chars"] for u in units if u["loc"]["chars"]]
    e["qwen_loc_length_ratio_median"] = float(np.median(ratio)) if ratio else None
    e["qwen_shorter_80pct_share"] = float(np.mean([x < 0.8 for x in ratio])) if ratio else None
    e["loc_empty_share"] = float(np.mean([u["loc"]["chars"] == 0 for u in units]))
    # Qwen readings with no word at all (empty, or only the "[?]" illegible marker)
    e["overread_share"] = float(np.mean([overread(u["qwen"], u["loc"]) for u in units]))
    e["qwen_no_words_share"] = float(np.mean([u["qwen"]["words"] == 0 for u in units]))
    e["loc_no_words_share"] = float(np.mean([u["loc"]["words"] == 0 for u in units]))
    kr = [known(u["qwen"]) / known(u["loc"]) for u in units if known(u["loc"]) > 0]
    e["qwen_loc_known_words_ratio_median"] = float(np.median(kr)) if kr else None
    PA = {p: page_agg(pages[p]) for p in P}
    for f in FE:
        for guard in ((False, True, "overread") if f == "lm_oov" else (False, True)):
            # per page: crop picks, the page-level pick and the crop differences, computed once
            per = {}
            for p in P:
                cs = [c for c in (pick(u["qwen"], u["loc"], f, guard) for u in pages[p]) if c is not None]
                pp = pick(PA[p]["qwen"], PA[p]["loc"], f, guard)
                d = [u["qwen"][f] - u["loc"][f] for u in pages[p] if u["qwen"].get(f) is not None and u["loc"].get(f) is not None]
                per[p] = (cs, pp, d)

            def stat(pg):
                cs = [c for p in pg for c in per[p][0]]
                ps = [per[p][1] for p in pg if per[p][1] is not None]
                d = [x for p in pg for x in per[p][2]]
                return (float(np.mean(cs)) if cs else None, float(np.mean(ps)) if ps else None, float(np.mean(d)) if d else None, len(cs))
            c0, p0, d0, n0 = stat(P)
            if n0 == 0 or d0 is None:
                continue
            bs = [stat([rnd.choice(P) for _ in P]) for _ in range(a.boot)]

            def ci(k):
                v = [b[k] for b in bs if b[k] is not None]
                return [float(np.percentile(v, 2.5)), float(np.percentile(v, 97.5))] if v else None
            e[f"{f}{'_guard_overread' if guard == 'overread' else ('_guard' if guard else '')}"] = {"crops_n": n0, "pages_n": len([p for p in P if per[p][2]]), "qwen_preferred_crops": c0, "ci_crops": ci(0),
                                                    "qwen_preferred_pages": p0, "ci_pages": ci(1),
                                                    "mean_diff_qwen_minus_loc": d0, "ci_diff": ci(2)}
    if MARGIN:
        dec = [(u["qwen"]["lm_oov"] - u["loc"]["lm_oov"]) for u in units if u["qwen"].get("lm_oov") is not None and u["loc"].get("lm_oov") is not None]
        big = [d for d in dec if abs(d) >= MARGIN]
        e["margin_rule"] = {"decided_share": len(big) / len(dec) if dec else None, "qwen_preferred_among_decided": (sum(d < 0 for d in big) / len(big)) if big else None}
    res["groups"][g] = e
json.dump(res, open(a.out, "w"), indent=1, sort_keys=True)
print(f"SD {SD} margin {MARGIN}")
for g, e in res["groups"].items():
    x = e.get("lm_oov_guard", {}); y = e.get("lm_oov", {}); m = e.get("mlm_pppl", {})
    print(f"{g:9s} pages {e['pages']:5d} crops {e['crops']:6d} len_ratio {e['qwen_loc_length_ratio_median']:.2f} known_ratio {e['qwen_loc_known_words_ratio_median']} q_nowords {e['qwen_no_words_share']:.2f} | "
          f"lm_oov crops {y.get('qwen_preferred_crops', 0):.2f} pages {y.get('qwen_preferred_pages', 0):.2f} {y.get('ci_pages')} | guard crops {x.get('qwen_preferred_crops', 0):.2f} {x.get('ci_crops')} pages {x.get('qwen_preferred_pages', 0):.2f} | "
          f"mlm {m.get('qwen_preferred_crops')} n {m.get('crops_n')} | margin {e.get('margin_rule')}")
