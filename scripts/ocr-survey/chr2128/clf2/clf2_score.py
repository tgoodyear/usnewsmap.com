#!/usr/bin/env python3
"""Score the classifier v2 gate (#2128 amendments 3 and 3.1): clf_score.py (307d960a) plus a per-page table.
Only the not-held stratum is labelled under 3.1; held and faded count as failed. Stdlib only.

From clf_score.py:

Final label per crop: A and B if they agree, else the majority of A, B and C, else X (can't tell).
Truth: C or M is Cherokee, L is Latin, X is left out (counted).
Calibration: per labeller (A, B) agreement with known script on the 400 calibration lines, overall
and on the lookalike subset, plus the can't-tell share. Thresholds 0.97, 0.95, 0.03.
Sample measures, per stratum (not_held, held, faded) and overall (held + not_held pages):
  recall      = sum(w * chr * called) / sum(w * chr)            (Horvitz-Thompson, w = 1 for called)
  latin_share = sum(latin * called) / sum(called, labelled)      (unweighted: every call is sampled)
  latin_fpr   = sum(w * latin * called) / sum(w * latin)         (reported only)
Bootstrap: pages with replacement, then boxes within page x {called, not called}, same weights;
2,000 draws, seed 2128; 95% percentile intervals. Gates per amendment 2 and 2.1: a gate passes only
if the point estimate and the bound both pass (recall >= 0.90 and lower >= 0.80; latin_share <= 0.05
and upper <= 0.10). Also reports A/B agreement on the faded stratum.
Usage: clf_score.py --key KEY.jsonl --labels-a A.tsv --labels-b B.tsv --labels-c C.tsv --out OUT.json
"""
import argparse, collections, json, random

def read_tsv(p):
    return dict(l.rstrip("\n").split("\t") for l in open(p) if l.strip())

def final(a, b, c, i):
    if a[i] == b[i]:
        return a[i]
    v = [a[i], b[i], c.get(i)]
    m = collections.Counter(x for x in v if x).most_common(1)[0]
    return m[0] if m[1] >= 2 else "X"

def measures(boxes):
    num = den = ls_num = ls_den = fp_num = fp_den = 0.0
    for b in boxes:
        t, w, called = b["truth"], b["w"], b["called"]
        if t == "chr":
            den += w; num += w * called
        elif t == "lat":
            fp_den += w; fp_num += w * called
        if called and t in ("chr", "lat"):
            ls_den += 1; ls_num += (t == "lat")
    f = lambda n, d: n / d if d else None
    return {"recall": f(num, den), "latin_share": f(ls_num, ls_den), "latin_fpr": f(fp_num, fp_den)}

def boot(pages, rng, draws):
    ids = sorted(pages); out = collections.defaultdict(list)
    for _ in range(draws):
        boxes = []
        for p in (rng.choice(ids) for _ in ids):
            for grp in (pages[p]["called"], pages[p]["notcalled"]):
                boxes += [rng.choice(grp) for _ in grp]
        for k, v in measures(boxes).items():
            if v is not None: out[k].append(v)
    q = lambda xs, a: sorted(xs)[min(len(xs) - 1, int(a * len(xs)))]
    return {k: [q(v, 0.025), q(v, 0.975)] for k, v in out.items()}

def gate(m, ci):
    r = m["recall"] is not None and m["recall"] >= 0.90 and ci.get("recall", [0])[0] >= 0.80
    l = m["latin_share"] is not None and m["latin_share"] <= 0.05 and ci.get("latin_share", [0, 1])[1] <= 0.10
    return {"recall": r, "latin_share": l, "pass": r and l}

def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    for k in ("key", "labels-a", "labels-b", "labels-c", "out"): ap.add_argument("--" + k, required=True)
    ap.add_argument("--draws", type=int, default=2000); ap.add_argument("--seed", type=int, default=2128)
    a_ = ap.parse_args()
    A, B, Cc = read_tsv(a_.labels_a), read_tsv(a_.labels_b), read_tsv(a_.labels_c)
    key = [json.loads(l) for l in open(a_.key)]
    assert {k["crop_id"] for k in key} == set(A) == set(B), "label files don't cover the key"
    res = {"n_crops": len(key)}
    # Calibration
    cal = [k for k in key if "known_script" in k]
    for name, L in (("A", A), ("B", B)):
        def acc(rows):
            ok = sum(1 for k in rows if (L[k["crop_id"]] in ("C", "M")) == (k["known_script"] == "cherokee") and L[k["crop_id"]] != "X")
            return ok / len(rows)
        look = [k for k in cal if k["lookalike"]]
        ct = sum(L[k["crop_id"]] == "X" for k in cal) / len(cal)
        r = {"overall": acc(cal), "lookalike": acc(look), "cant_tell": ct, "n": len(cal), "n_lookalike": len(look)}
        r["pass"] = r["overall"] >= 0.97 and r["lookalike"] >= 0.95 and ct <= 0.03
        res.setdefault("calibration", {})[name] = r
    # Sample
    samp = [k for k in key if "strata" in k]
    lab = collections.Counter(); faded_agree = [0, 0]
    pages = collections.defaultdict(lambda: {"called": [], "notcalled": [], "strata": set()})
    for k in samp:
        i = k["crop_id"]; f = final(A, B, Cc, i); lab[f] += 1
        if "faded" in k["strata"]: faded_agree[0] += A[i] == B[i]; faded_agree[1] += 1
        t = {"C": "chr", "M": "chr", "L": "lat"}.get(f, "x")
        b = {"truth": t, "called": bool(k["called"]), "w": 1.0 if k["called"] else float(k["weight"])}
        p = pages[k["page_id"]]; p["strata"].update(k["strata"])
        (p["called"] if k["called"] else p["notcalled"]).append(b)
    res["final_labels"] = dict(lab)
    res["faded_ab_agreement"] = faded_agree[0] / faded_agree[1] if faded_agree[1] else None
    rng = random.Random(a_.seed)
    for name, pred in (("not_held", lambda s: "not_held" in s), ("held", lambda s: "held" in s),
                       ("faded", lambda s: "faded" in s), ("overall", lambda s: "held" in s or "not_held" in s)):
        sub = {p: v for p, v in pages.items() if pred(v["strata"])}
        boxes = [b for v in sub.values() for b in v["called"] + v["notcalled"]]
        m = measures(boxes); ci = boot(sub, rng, a_.draws)
        res.setdefault("strata", {})[name] = {"pages": len(sub), "boxes": len(boxes), **m, "ci": ci,
                                             "gate": gate(m, ci) if name in ("not_held", "held") else None}
    per = {}
    for p, v in sorted(pages.items()):
        m = measures(v["called"] + v["notcalled"])
        per[p] = {**m, "called": len(v["called"]), "notcalled_sampled": len(v["notcalled"]),
                  "chr_labelled": sum(b["truth"] == "chr" for b in v["called"] + v["notcalled"])}
    res["per_page"] = per
    res["note"] = "amendment 3.1: held and faded strata not labelled; they count as failed"
    json.dump(res, open(a_.out, "w"), indent=1, sort_keys=True); print(json.dumps(res, indent=1, sort_keys=True))

if __name__ == "__main__":
    main()
