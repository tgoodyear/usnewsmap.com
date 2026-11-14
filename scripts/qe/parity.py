#!/usr/bin/env python3
"""Checks usnm_core::qe against the study's Python numbers
(docs/reports/2026-11-ocr-quality-estimation, `metrics.py --lm-sources gutenberg+wikisource`).

    parity.py --study-metrics DIR --scores FILE [--scores FILE ...] --out FILE

--study-metrics holds the study's truth-<lang>.jsonl and unlabelled-<lang>.jsonl; each --scores file
is `qe --command score` output over the same rows (keyed by reader and unit). For every reading the study
scored it compares oov, charlm_bpc and the guard count, and per language it compares the study's
decision measures with ours:

- truth sets: paired accuracy on crop pairs whose CER differs by at least 1 point, with the
  omission guard, for lm_oov (the pooled SDs 1.185 and 0.117), charlm_bpc and oov;
- unlabelled crops: the share of crops on which Qwen is preferred (lm_oov with the guard), and the
  share of crop pairs on which the two decisions differ.
"""
import argparse, glob, json, os
from collections import defaultdict

BPC_SD, OOV_SD = 1.185, 0.117


def lm_oov(r):
    if r.get("charlm_bpc") is None or r.get("oov") is None:
        return None
    return r["charlm_bpc"] / BPC_SD + r["oov"] / OOV_SD


def decide(x, y, f):
    """The study's `validate.decide` with the "known" guard: +1 prefers x, -1 y, 0 a tie or no value."""
    kx, ky = x.get("known3") or 0, y.get("known3") or 0
    if kx < 0.8 * ky:
        return -1
    if ky < 0.8 * kx:
        return 1
    vx, vy = x.get(f), y.get(f)
    if vx is None or vy is None:
        return 0
    return (vx < vy) - (vx > vy)


def pairs(rows):
    g = defaultdict(dict)
    for r in rows:
        g[r["unit"]][r["reader"]] = r
    out = []
    for m in g.values():
        rd = sorted(m)
        for i in range(len(rd)):
            for j in range(i + 1, len(rd)):
                out.append((m[rd[i]], m[rd[j]]))
    return out


def accuracy(prs, f):
    """The study's `validate.accuracy(P, f, "known", 0.01)`."""
    ok = n = 0.0
    for x, y in prs:
        gap = y["cer"] - x["cer"]
        if abs(gap) < 0.01 or x.get(f) is None or y.get(f) is None:
            continue
        n += 1
        d = decide(x, y, f)
        ok += 1 if d == (1 if gap > 0 else -1) else (0.5 if d == 0 else 0)
    return (ok / n if n else None), int(n)


def ours(r, s):
    """A copy of the study's row with our measures in place of its own."""
    q = s["qe"]
    o = dict(r)
    o.update({"oov": q["oov"], "charlm_bpc": q["charlm_bpc"], "known3": q["known3"], "words": q["words"]})
    o["lm_oov"] = lm_oov(o)
    return o


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--study-metrics", required=True)
    ap.add_argument("--scores", action="append", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args(argv)
    scores = {}
    for p in a.scores:
        for line in open(p, encoding="utf-8"):
            s = json.loads(line)
            if "qe" in s:
                scores[(s["reader"], s["unit"])] = s
    res = {"truth": {}, "unlabelled": {}}
    for path in sorted(glob.glob(os.path.join(a.study_metrics, "*.jsonl"))):
        kind, lang = os.path.basename(path)[:-6].split("-")
        study = [json.loads(line) for line in open(path, encoding="utf-8")]
        if kind == "truth":
            study = [r for r in study if r.get("cer") is not None and r["reader"] != "as"]
        for r in study:
            r["lm_oov"] = lm_oov(r)
        mine = [ours(r, scores[(r["reader"], r["unit"])]) for r in study if (r["reader"], r["unit"]) in scores]
        missing = len(study) - len(mine)
        st = {(r["reader"], r["unit"]): r for r in study}
        d_oov = d_bpc = 0.0
        same_known3 = 0
        for m in mine:
            s = st[(m["reader"], m["unit"])]
            if (s["oov"] is None) != (m["oov"] is None) or (s["charlm_bpc"] is None) != (m["charlm_bpc"] is None):
                d_oov = d_bpc = float("inf")
                continue
            if s["oov"] is not None:
                d_oov = max(d_oov, abs(s["oov"] - m["oov"]))
            if s["charlm_bpc"] is not None:
                d_bpc = max(d_bpc, abs(s["charlm_bpc"] - m["charlm_bpc"]))
            same_known3 += s["known3"] == m["known3"]
        e = {"readings": len(study), "missing": missing, "max_abs_diff_oov": d_oov, "max_abs_diff_bpc": d_bpc,
             "known3_equal": same_known3 / len(mine) if mine else None,
             "mean_oov_study": sum(r["oov"] for r in study if r["oov"] is not None) / max(1, sum(r["oov"] is not None for r in study)),
             "mean_oov_ours": sum(r["oov"] for r in mine if r["oov"] is not None) / max(1, sum(r["oov"] is not None for r in mine))}
        if kind == "truth":
            ps, pm = pairs(study), pairs(mine)
            for f in ("lm_oov", "charlm_bpc", "oov"):
                (a_s, n_s), (a_m, n_m) = accuracy(ps, f), accuracy(pm, f)
                e[f"paired_{f}"] = {"study": a_s, "ours": a_m, "n_study": n_s, "n_ours": n_m,
                                    "diff_pts": None if a_s is None or a_m is None else 100 * (a_m - a_s)}
        else:
            prefer = {}
            for name, rows in (("study", study), ("ours", mine)):
                pr = [(x, y) for x, y in pairs(rows) if {x["reader"], y["reader"]} == {"loc", "qwen"}]
                prefer[name] = {(x["unit"]): (decide(x, y, "lm_oov") if x["reader"] == "qwen" else -decide(x, y, "lm_oov")) for x, y in pr}
                n = len(pr)
                e[f"qwen_preferred_{name}"] = sum(v == 1 for v in prefer[name].values()) / n if n else None
                e["crop_pairs"] = n
            common = set(prefer["study"]) & set(prefer["ours"])
            e["decisions_differ"] = sum(prefer["study"][u] != prefer["ours"][u] for u in common) / len(common) if common else None
        res[kind][lang] = e
        print(kind, lang, json.dumps(e), flush=True)
    json.dump(res, open(a.out, "w"), indent=1, sort_keys=True)


if __name__ == "__main__":
    main()
