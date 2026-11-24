#!/usr/bin/env python3
"""Score arms A, B and C once on the 115 held-out pages and apply #2127's decision rule.

Reading. Every held-out line crop (cut_lines.py --heldout-out) is read by each arm with Tesseract
5.5.0 at PSM 13, OEM 1 (frak_score.py): A is frak2021 0.905, B and C the fine-tuned models; each
model's sha256 is checked first. Reference and reading are normalised with frak_norm.

Scoring (the issue's "Scoring"). CER is edit distance over reference length, pooled over lines.
Intervals come from a bootstrap that resamples titles with replacement (BOOT draws, seed SEED) and
recomputes each arm's pooled CER on the drawn titles' lines; the 95% interval of a difference is
its 2.5th and 97.5th percentiles. A title is the page name's part before its date, with "-" read
as "_" (Weisseritz-Zeitung and Weisseritz_Zeitung are one paper), giving the issue's 18 titles.

Decision rule (the issue's rules 1 to 3):
  1. B passes if CER_B <= 0.90 * CER_A and the interval of B - A lies below 0.
  2. C passes if CER_C <= 0.95 * CER_B and the interval of C - B lies below 0.
     If C fails, the synthetic arm closes and B is the candidate.
  3. The candidate is worse than A by more than 1 point of CER on no more than 3 of the 18 titles.
The candidate is C if rule 2 passes and rule 1 passes; B if rule 1 passes and rule 2 fails; none
if rule 1 fails (the issue builds C's test on B; a C that beats a failing B is reported, not taken).

Descriptive: the DONE and GT pages (Transkribus status in their PAGE XML) on their own, per-title
CER, and the lines and characters whose reference has a character outside frak2021's set.

    score_heldout.py --lines-heldout lines-heldout --annotations cg/annotations \\
        --heldout heldout-pages.txt --model-a frak2021-0.905.traineddata \\
        --model-b frak2127_b.traineddata --model-c frak2127_c.traineddata --out heldout-scores
"""
import argparse, collections, hashlib, json, os, re, shutil, sys

import numpy as np

import frak_score

SHA = {"A": "1da2384254fa8462c776faf3b43307fe19ce51be0931c623b2fdd560f96e299a",
       "B": "f1dc87ebc3a1caea9ab6f62bd4601b11bf407350a4de5b5b8725e3f720088f60",
       "C": "327573d37eafa65566368301e1c81771a464ae783268a2fbe36be943daa2232f"}
HELDOUT_SHA256 = "6b0c03e714d1d254e7bf144c8d93d3413fc1a2eae6710cdabd5d694c89e97ce5"
BOOT, SEED = 2000, 2127
TITLE = re.compile(r"^(.+?)_\d{4}(?:-\d{2}(?:-\d{2})?)?_")


def title_of(page):
    return TITLE.match(page).group(1).replace("-", "_")


def sha256(p):
    return hashlib.sha256(open(p, "rb").read()).hexdigest()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    for k in ("lines-heldout", "annotations", "heldout", "model-a", "model-b", "model-c", "out"):
        ap.add_argument("--" + k, required=True)
    ap.add_argument("--jobs", type=int, default=20)
    a = ap.parse_args()
    raw = open(a.heldout, "rb").read()
    assert hashlib.sha256(raw).hexdigest() == HELDOUT_SHA256
    heldout = set(raw.decode().split())
    models = {"A": a.model_a, "B": a.model_b, "C": a.model_c}
    for arm, p in models.items():
        assert sha256(p) == SHA[arm], f"arm {arm}: {p} is not the frozen model"
    os.makedirs(a.out, exist_ok=False)
    tessdata = os.path.join(a.out, "tessdata")
    os.makedirs(tessdata)
    os.symlink("/usr/share/tesseract-ocr/5/tessdata/configs", os.path.join(tessdata, "configs"))
    for arm, p in models.items():
        shutil.copy(p, os.path.join(tessdata, f"arm{arm}.traineddata"))
    meta = {}
    for line in open(os.path.join(a.lines_heldout, "lines.jsonl"), encoding="utf-8"):
        r = json.loads(line)
        meta[r["name"]] = r
    assert {r["page"] for r in meta.values()} == heldout
    titles = sorted({title_of(p) for p in heldout})
    assert len(titles) == 18, titles
    status = {}
    for p in heldout:
        m = re.search(r'status="([A-Z_]+)"', open(os.path.join(a.annotations, p + ".xml"), encoding="utf-8").read())
        status[p] = m.group(1) if m else ""
    # Read and score.
    rows = {}
    for arm in "ABC":
        res, rr = frak_score.score(os.path.join(a.lines_heldout, "heldout"), tessdata, f"arm{arm}", a.jobs,
                                   heldout_ok=True, oem=1)
        rows[arm] = {name: (ref, hyp, e) for name, ref, hyp, e in rr}
        with open(os.path.join(a.out, f"read_{arm}.jsonl"), "w", encoding="utf-8") as f:
            for name, ref, hyp, e in rr:
                f.write(json.dumps({"name": name, "ref": ref, "hyp": hyp, "edits": e}, ensure_ascii=False) + "\n")
        print(f"arm {arm}: {json.dumps(res)}", flush=True)
    names = sorted(rows["A"])
    assert set(names) == set(rows["B"]) == set(rows["C"]) == set(meta)
    # Per-title sums: edits per arm and reference characters.
    t_index = {t: i for i, t in enumerate(titles)}
    E = np.zeros((3, len(titles))); N = np.zeros(len(titles))
    for n in names:
        i = t_index[title_of(meta[n]["page"])]
        N[i] += len(rows["A"][n][0])
        for k, arm in enumerate("ABC"):
            E[k, i] += rows[arm][n][2]
    cer = {arm: E[k].sum() / N.sum() for k, arm in enumerate("ABC")}
    rng = np.random.default_rng(SEED)
    draws = rng.integers(0, len(titles), size=(BOOT, len(titles)))
    counts = np.stack([np.bincount(d, minlength=len(titles)) for d in draws])  # BOOT x titles
    boot = (counts @ E.T) / (counts @ N)[:, None]  # BOOT x arms
    def interval(x):
        return [float(np.percentile(x, 2.5)), float(np.percentile(x, 97.5))]
    d_ba, d_cb = boot[:, 1] - boot[:, 0], boot[:, 2] - boot[:, 1]
    iv_ba, iv_cb = interval(d_ba), interval(d_cb)
    rule1 = bool(cer["B"] <= 0.90 * cer["A"] and iv_ba[1] < 0)
    rule2 = bool(cer["C"] <= 0.95 * cer["B"] and iv_cb[1] < 0)
    candidate = ("C" if rule2 else "B") if rule1 else None
    per_title = {t: {arm: float(E[k, i] / N[i]) for k, arm in enumerate("ABC")} | {"ref_chars": int(N[i])}
                 for t, i in t_index.items()}
    def worse_titles(arm):
        return sorted(t for t, v in per_title.items() if v[arm] - v["A"] > 0.01)
    rule3 = {arm: {"titles_worse_than_A_by_over_1_point": worse_titles(arm), "passes": len(worse_titles(arm)) <= 3}
             for arm in "BC"}
    # Descriptive.
    done = [n for n in names if status[meta[n]["page"]] in ("DONE", "GT")]
    sub = {arm: sum(rows[arm][n][2] for n in done) / sum(len(rows["A"][n][0]) for n in done) for arm in "ABC"}
    unenc = [n for n in names if meta[n]["unencodable"]]
    out = {
        "cer": cer, "lines": len(names), "ref_chars": int(N.sum()), "titles": len(titles),
        "bootstrap": {"draws": BOOT, "seed": SEED, "unit": "title"},
        "B_minus_A": {"diff": cer["B"] - cer["A"], "relative": cer["B"] / cer["A"] - 1, "ci95": iv_ba},
        "C_minus_B": {"diff": cer["C"] - cer["B"], "relative": cer["C"] / cer["B"] - 1, "ci95": iv_cb},
        "rule1_fine_tuning_passes": rule1, "rule2_glyph_bank_passes": rule2,
        "candidate": candidate, "rule3": rule3,
        "rule3_candidate_passes": rule3[candidate]["passes"] if candidate else None,
        "descriptive": {
            "done_gt_subset": {"pages": sum(1 for p in heldout if status[p] in ("DONE", "GT")), "lines": len(done), "cer": sub},
            "per_title": per_title,
            "lines_with_chars_outside_frak2021": len(unenc),
            "chars_in_those_lines": sum(len(rows["A"][n][0]) for n in unenc),
        },
        "models_sha256": SHA,
    }
    json.dump(out, open(os.path.join(a.out, "scores.json"), "w"), indent=1)
    print(json.dumps({k: out[k] for k in ("cer", "B_minus_A", "C_minus_B", "rule1_fine_tuning_passes",
                                           "rule2_glyph_bank_passes", "candidate", "rule3_candidate_passes")}, indent=1))


if __name__ == "__main__":
    sys.exit(main())
