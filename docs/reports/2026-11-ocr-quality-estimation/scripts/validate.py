#!/usr/bin/env python3
"""Validates the reference-free measures against true CER on the truth sets.

    validate.py --metrics-dir metrics --damage damage-truth.jsonl [--mlm mlm-truth.jsonl] --out data/validation.json

For each language (and pooled), at crop and page level:
  (a) Spearman's rho between each measure and CER, with a 95% page-bootstrap interval;
  (b) paired decision accuracy: for two readers of the same unit, how often the reading with the lower
      measure has the lower CER (ties in the measure count half), over all pairs and over pairs whose
      CER gap is at least 1 point; also with an omission guard: a reading under 80% of the other's
      length loses ("chars"), or a reading with under 80% of the other's in-lexicon words of 3+ letters loses ("known",
      the guard used for the decisions below and in apply.py);
  (c) leave-one-language-out: a linear combination of measures is fitted on the other languages and
      scored on the held-out one, and a decision margin is chosen on the other languages (the smallest
      gap in the measure giving 90% accuracy on 1-point pairs there) and applied to the held-out one.
Every measure is oriented so that higher means worse.
"""
import argparse, glob, json, math, random
from collections import defaultdict
import numpy as np

ap = argparse.ArgumentParser()
ap.add_argument("--metrics-dir", required=True)
ap.add_argument("--damage", required=True)
ap.add_argument("--mlm", default=None)
ap.add_argument("--out", required=True)
ap.add_argument("--boot", type=int, default=1000)
ap.add_argument("--seed", type=int, default=1)
a = ap.parse_args()

MEASURES = ["oov", "nearmiss_any", "nearmiss_conf30", "nearmiss_conf50", "nearmiss_conf80", "novel", "garbage",
            "script_mix", "charlm_bpc", "damage", "unknown"]
COMBO = ["oov", "nearmiss_conf50", "garbage", "charlm_bpc", "unknown"]

rows = []
for p in sorted(glob.glob(f"{a.metrics_dir}/truth-*.jsonl")):
    rows += [json.loads(l) for l in open(p, encoding="utf-8")]
dmg = {}
for l in open(a.damage, encoding="utf-8"):
    d = json.loads(l)
    dmg[d["id"]] = d
mlm = {}
if a.mlm:
    for l in open(a.mlm, encoding="utf-8"):
        d = json.loads(l)
        mlm[d["reader"] + "|" + d["unit"]] = d
for r in rows:
    d = dmg.get(r["reader"] + "|" + r["unit"], {})
    r["damage"] = d.get("hyp_rate")
    r["unknown"] = (1 - d["hyp_known"] / d["hyp_words"]) if d.get("hyp_words") else None
    m = mlm.get(r["reader"] + "|" + r["unit"])
    r["mlm_pppl"] = m["pll_per_token"] if m else None
if mlm:
    MEASURES.append("mlm_pppl")
# American Stories rows on en-op are article text, mostly not the crop's text: left out.
rows = [r for r in rows if r["cer"] is not None and r["reader"] != "as"]
LANGS = sorted({r["lang"] for r in rows})


def rank(x):
    x = np.asarray(x, float)
    o = np.argsort(x, kind="mergesort")
    rk = np.empty(len(x)); rk[o] = np.arange(len(x))
    # average ties
    xs = x[o]
    i = 0
    while i < len(xs):
        j = i
        while j + 1 < len(xs) and xs[j + 1] == xs[i]:
            j += 1
        rk[o[i:j + 1]] = (i + j) / 2
        i = j + 1
    return rk


def spearman(x, y):
    if len(x) < 5:
        return None
    rx, ry = rank(x), rank(y)
    if rx.std() == 0 or ry.std() == 0:
        return None
    return float(np.corrcoef(rx, ry)[0, 1])


def page_level(rs, feats):
    g = defaultdict(list)
    for r in rs:
        g[(r["page"], r["reader"])].append(r)
    out = []
    for (pg, rd), xs in g.items():
        w = [max(x["words"], 1) for x in xs]
        o = {"page": pg, "unit": pg, "reader": rd, "lang": xs[0]["lang"], "set": xs[0]["set"],
             "chars": sum(x["chars"] for x in xs), "ref_chars": sum(x["ref_chars"] for x in xs),
             "words": sum(x["words"] for x in xs), "known3": sum(x.get("known3") or 0 for x in xs),
             "lex_hit": sum((x["words"] or 0) * (x["lex_hit"] or 0) for x in xs) / max(1, sum(x["words"] for x in xs))}
        rc = sum(len_ref(x) for x in xs)
        o["cer"] = sum(x["cer"] * len_ref(x) for x in xs) / rc if rc else None
        for f in feats:
            vals = [(x[f], wi) for x, wi in zip(xs, w) if x.get(f) is not None]
            o[f] = sum(v * wi for v, wi in vals) / sum(wi for _, wi in vals) if vals else None
        out.append(o)
    return out


def len_ref(x):
    return max(x["ref_chars"], 1)


def corr_block(rs, feats, boot):
    res = {}
    pages = sorted({r["page"] for r in rs})
    bypage = defaultdict(list)
    for r in rs:
        bypage[r["page"]].append(r)
    rnd = random.Random(a.seed)
    for f in feats:
        xs = [(r[f], r["cer"]) for r in rs if r.get(f) is not None]
        if len(xs) < 5:
            continue
        rho = spearman([x for x, _ in xs], [y for _, y in xs])
        bs = []
        for _ in range(boot):
            samp = [r for p in (rnd.choice(pages) for _ in pages) for r in bypage[p] if r.get(f) is not None]
            v = spearman([r[f] for r in samp], [r["cer"] for r in samp])
            if v is not None:
                bs.append(v)
        bs.sort()
        res[f] = {"rho": rho, "lo": bs[int(0.025 * len(bs))] if bs else None, "hi": bs[int(0.975 * len(bs)) - 1] if bs else None, "n": len(xs)}
    return res


def pairs(rs):
    g = defaultdict(dict)
    for r in rs:
        g[r["unit"]][r["reader"]] = r
    out = []
    for u, m in g.items():
        rd = sorted(m)
        for i in range(len(rd)):
            for j in range(i + 1, len(rd)):
                out.append((m[rd[i]], m[rd[j]]))
    return out


def known(r):
    """In-lexicon words of three or more letters: the omission guard's count. Shorter tokens are left out
    because LoC's fragments ("a", "y", "S") are often in the lexicon."""
    return r.get("known3") or 0


def decide(x, y, f, guard):
    """+1 if the measure prefers x, -1 if y, 0 for a tie or no value. guard "chars": a reading under 80%
    of the other's length loses; "known": a reading with under 80% of the other's in-lexicon words of 3+ letters loses."""
    if guard:
        k = (lambda r: r["chars"]) if guard == "chars" else known
        if k(x) < 0.8 * k(y):
            return -1
        if k(y) < 0.8 * k(x):
            return 1
    vx, vy = x.get(f), y.get(f)
    if vx is None or vy is None:
        return 0
    return (vx < vy) - (vx > vy)


def accuracy(prs, f, guard, min_gap=0.0, margin=0.0):
    ok = n = 0.0
    decided = 0
    for x, y in prs:
        gap = y["cer"] - x["cer"]
        if abs(gap) < max(min_gap, 1e-12):
            continue
        if x.get(f) is None or y.get(f) is None:
            continue  # pairs the measure has no value for (e.g. outside the masked-LM sample) are not counted
        n += 1
        if margin > 0:
            vx, vy = x.get(f), y.get(f)
            if vx is None or vy is None or abs(vx - vy) < margin:
                continue
        d = decide(x, y, f, guard)
        if margin > 0:
            decided += 1
            ok += (d == (1 if gap > 0 else -1))
        else:
            ok += 1 if d == (1 if gap > 0 else -1) else (0.5 if d == 0 else 0)
    if margin > 0:
        return (ok / decided if decided else None), (decided / n if n else None), int(n)
    return (ok / n if n else None), int(n)


def fit(rs, feats):
    X = np.array([[r[f] for f in feats] + [1.0] for r in rs if all(r.get(f) is not None for f in feats)])
    y = np.array([r["cer"] for r in rs if all(r.get(f) is not None for f in feats)])
    lam = 1e-3 * np.eye(X.shape[1]); lam[-1, -1] = 0
    return np.linalg.solve(X.T @ X + lam, X.T @ y)


def apply_fit(rs, feats, w, name):
    for r in rs:
        r[name] = float(np.dot([r[f] for f in feats] + [1.0], w)) if all(r.get(f) is not None for f in feats) else None


out = {"measures": MEASURES, "combo": COMBO, "languages": {}}
# leave-one-language-out combination, fitted at crop level
for lang in LANGS:
    w = fit([r for r in rows if r["lang"] != lang], COMBO)
    apply_fit([r for r in rows if r["lang"] == lang], COMBO, w, "combo_lolo")
    out.setdefault("combo_weights", {})[lang] = dict(zip(COMBO + ["const"], map(float, w)))
for lang in LANGS:
    tr = [r for r in rows if r["lang"] != lang]
    sd = {f: float(np.std([r[f] for r in tr if r.get(f) is not None])) for f in ("charlm_bpc", "oov")}
    out.setdefault("lm_oov_sd", {})[lang] = sd
    for r in rows:
        if r["lang"] == lang:
            r["lm_oov"] = (r["charlm_bpc"] / sd["charlm_bpc"] + r["oov"] / sd["oov"]) if r.get("charlm_bpc") is not None and r.get("oov") is not None else None
w_all = fit(rows, COMBO)
out["combo_weights"]["all"] = dict(zip(COMBO + ["const"], map(float, w_all)))
feats = MEASURES + ["combo_lolo", "lm_oov"]
KEY = ["oov", "charlm_bpc", "lm_oov", "unknown", "damage", "nearmiss_any", "garbage", "combo_lolo"] + (["mlm_pppl"] if mlm else [])

for lang in LANGS + ["all"]:
    rs = [r for r in rows if lang == "all" or r["lang"] == lang]
    res = {"sets": sorted({r["set"] for r in rs}), "readers": sorted({r["reader"] for r in rs}),
           "crops": len({r["unit"] for r in rs}), "pages": len({r["page"] for r in rs})}
    pr = page_level(rs, feats)
    res["corr_crop"] = corr_block(rs, feats, a.boot if lang != "all" else 200)
    res["corr_page"] = corr_block(pr, feats, a.boot if lang != "all" else 200)
    for level, data in (("crop", rs), ("page", pr)):
        P = pairs(data)
        acc = {}
        for f in feats:
            a0, n0 = accuracy(P, f, False)
            a1, n1 = accuracy(P, f, False, 0.01)
            g0, _ = accuracy(P, f, "chars")
            g1, _ = accuracy(P, f, "chars", 0.01)
            k1, _ = accuracy(P, f, "known", 0.01)
            acc[f] = {"all": a0, "n_all": n0, "gap1": a1, "n_gap1": n1, "guard_all": g0, "guard_gap1": g1, "kguard_gap1": k1}
        acc["shorter_wins"] = accuracy(P, "chars", False)[0]
        res[f"paired_{level}"] = acc
        # by reader pair: truth share for the first reader, and each key measure's accuracy (1-point pairs, guard on)
        bp = defaultdict(list)
        for x, y in P:
            bp[f"{x['reader']}~{y['reader']}"].append((x, y))
        res[f"by_pair_{level}"] = {}
        for k, pl in sorted(bp.items()):
            big = [(x, y) for x, y in pl if abs(x["cer"] - y["cer"]) >= 0.01]
            e = {"n": len(pl), "n_gap1": len(big), "first_better_gap1": (sum(x["cer"] < y["cer"] for x, y in big) / len(big)) if big else None}
            for f in KEY:
                e[f] = accuracy(pl, f, "known", 0.01)[0]
                e[f + "_noguard"] = accuracy(pl, f, False, 0.01)[0]
                fb_ = [(x, y) for x, y in big if x["cer"] < y["cer"]]
                sb_ = [(x, y) for x, y in big if x["cer"] > y["cer"]]
                ab = [accuracy(fb_, f, "known", 0.01)[0], accuracy(sb_, f, "known", 0.01)[0]] if fb_ and sb_ else [None]
                e[f + "_balanced"] = (ab[0] + ab[1]) / 2 if None not in ab else None
                e[f + "_picks_first"] = (sum(decide(x, y, f, "known") == 1 for x, y in big) / len(big)) if big else None
            res[f"by_pair_{level}"][k] = e
    out["languages"][lang] = res

# (c) decision margins: the smallest gap in the measure at which EVERY training language (with 20 or
# more 1-point pairs) reaches 90% accuracy on its 1-point pairs with at least 5% of them decided (guard
# on); chosen on the other languages and applied to the held-out one, and once on all languages.
def choose_margin(f, train_langs):
    per = {l: pairs([r for r in rows if r["lang"] == l]) for l in train_langs}
    per = {l: P for l, P in per.items() if sum(abs(x["cer"] - y["cer"]) >= 0.01 for x, y in P) >= 20}
    vals = sorted({abs(x[f] - y[f]) for P in per.values() for x, y in P if x.get(f) is not None and y.get(f) is not None})
    if not vals:
        return None
    for m in np.quantile(vals, np.linspace(0, 0.98, 50)):
        good = True
        for P in per.values():
            acc_, cov_, _ = accuracy(P, f, "known", 0.01, max(float(m), 1e-12))
            if acc_ is None or acc_ < 0.9 or not cov_ or cov_ < 0.05:
                good = False
                break
        if good:
            return float(m)
    return None


mg = {}
for f in feats:
    mg[f] = {}
    for lang in LANGS:
        chosen = choose_margin(f, [l for l in LANGS if l != lang])
        if chosen is None:
            mg[f][lang] = {"margin": None}
            continue
        acc_, cov_, n_ = accuracy(pairs([r for r in rows if r["lang"] == lang]), f, "known", 0.01, max(chosen, 1e-12))
        mg[f][lang] = {"margin": chosen, "heldout_acc_gap1": acc_, "heldout_coverage_gap1": cov_, "n_gap1": n_}
    mg[f]["all"] = {"margin": choose_margin(f, LANGS)}
out["lolo_margins"] = mg
# the same with German kept out of every training pool (it fails the 90% bar on its own)
mg2 = {}
for f in feats:
    mg2[f] = {}
    for lang in LANGS:
        chosen = choose_margin(f, [l for l in LANGS if l != lang and l != "deu"])
        if chosen is None:
            mg2[f][lang] = {"margin": None}
            continue
        acc_, cov_, n_ = accuracy(pairs([r for r in rows if r["lang"] == lang]), f, "known", 0.01, max(chosen, 1e-12))
        mg2[f][lang] = {"margin": chosen, "heldout_acc_gap1": acc_, "heldout_coverage_gap1": cov_, "n_gap1": n_}
    mg2[f]["all_but_deu"] = {"margin": choose_margin(f, [l for l in LANGS if l != "deu"])}
out["lolo_margins_without_deu"] = mg2
out["lolo_margins"] = mg
json.dump(out, open(a.out, "w"), indent=1, sort_keys=True)

# short console summary
for lang in LANGS + ["all"]:
    r = out["languages"][lang]
    print(f"\n== {lang} crops={r['crops']} pages={r['pages']} readers={r['readers']} shorter_wins={r['paired_crop']['shorter_wins']:.2f}")
    for k, e in r["by_pair_crop"].items():
        fb = e["first_better_gap1"]
        print(f"  pair {k:28s} n_gap1 {e['n_gap1']:5d} first_better {fb if fb is None else round(fb, 2)} " + " ".join(f"{f}={e[f]:.2f}/{e[f + '_picks_first']:.2f}" for f in KEY if e[f] is not None and e[f + '_picks_first'] is not None))
    for f in feats:
        c = r["corr_crop"].get(f, {}); p = r["corr_page"].get(f, {}); q = r["paired_crop"][f]; qp = r["paired_page"][f]
        fmt = lambda v: "  -  " if v is None else f"{v:5.2f}"
        print(f"{f:16s} rho_crop {fmt(c.get('rho'))} rho_page {fmt(p.get('rho'))} | pair {fmt(q['all'])} gap1 {fmt(q['gap1'])} guard_gap1 {fmt(q['guard_gap1'])} kguard {fmt(q['kguard_gap1'])} (n {q['n_gap1']}) | page pair gap1 {fmt(qp['gap1'])} guard {fmt(qp['guard_gap1'])}")
