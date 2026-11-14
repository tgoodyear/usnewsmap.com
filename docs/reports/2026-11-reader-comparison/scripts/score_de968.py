#!/usr/bin/env python3
"""Ours (live frak2021 ALTO) against LoC per damage band on the German band crops (#835, #968).

Parked: the crops have no human transcriptions and no truth source is planned (12 November 2026).

    score_de968.py --dir DE968_DIR --gpu-study BIN --out OUT_DIR [--live LIVE.tsv]

Reads DIR/crops/crops.jsonl (filled in with human transcriptions, validated), DIR/crops/manifest.jsonl,
and DIR/readers/{ours-live-alto,loc-alto}.jsonl. Crops without a transcription are left out and counted
(never scored against anything else). Refuses to run if no crop has a transcription. Runs
`gpu-study score` with the band as `source`, ours as the baseline, then applies the rule:

  re-add ("keep") ours in a band when the paired CER difference ours - LoC has 95% limits below zero;
  "revoke" (stay out) when the limits are above zero; "undecided" when they include zero or the band
  has under 5 pages (protocol rule 1). Undecided bands stay out.

All German frak2021 text is being revoked at the ~19 Nov rebuild. This script says which bands earn their
way back at a later rebuild: with --live (the decoded pages-v20261026-1 list) it writes readd-<band>.txt for
each band where ours beats LoC (verdict keep): every live German page whose loc_damage falls in the band
(page ids only). The write-up must say that the three lowest bands are one title (sn84027101) and that
new titles stay out by default.
"""
import argparse, csv, json, subprocess, sys
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("--dir", type=Path, required=True)
ap.add_argument("--gpu-study", required=True)
ap.add_argument("--out", type=Path, required=True)
ap.add_argument("--live", type=Path)
a = ap.parse_args()
a.out.mkdir(parents=True, exist_ok=True)
man = {r["crop_id"]: r for r in map(json.loads, (a.dir / "crops/manifest.jsonl").read_text().splitlines())}
refs, left = [], 0
for t in map(json.loads, (a.dir / "crops/crops.jsonl").read_text().splitlines()):
    if not (t.get("transcription") or "").strip():
        left += 1
        continue
    m = man[t["crop_id"]]
    refs.append({"id": t["crop_id"], "page": m["page_id"], "ref": t["transcription"], "source": m["band"], "lang": "ger"})
if not refs:
    sys.exit("no crop has a human transcription yet")
(a.out / "refs.jsonl").write_text("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in refs), encoding="utf-8")
subprocess.run([a.gpu_study, "score", "--refs", str(a.out / "refs.jsonl"),
                "--hyp", f"ours={a.dir / 'readers/ours-live-alto.jsonl'}", "--hyp", f"loc={a.dir / 'readers/loc-alto.jsonl'}",
                "--hyp", f"ours-psm3-crop={a.dir / 'readers/ours-tesseract-frak2021-psm3.jsonl'}",
                "--baseline", "loc", "--out", str(a.out / "score.jsonl")], check=True)
rows = [json.loads(x) for x in (a.out / "score.jsonl").read_text().splitlines()]
rule = {}
print(f"{left} crops without a transcription left out")
for r in rows:
    if not r["group"].startswith("source:") or r["reader"] != "ours":
        continue
    band = r["group"][7:]
    lo, hi, d = r.get("cer_diff_lo"), r.get("cer_diff_hi"), r.get("cer_diff")
    if r["pages"] < 5 or lo is None:
        verdict = "undecided"
    else:
        verdict = "keep" if hi < 0 else "revoke" if lo > 0 else "undecided"
    rule[band] = verdict
    loc = next(x for x in rows if x["group"] == r["group"] and x["reader"] == "loc")
    print(f"{band}: {r['items']} crops, {r['pages']} pages; ours {r['cer']*100:.2f}% ({r['cer_lo']*100:.2f} to {r['cer_hi']*100:.2f}), "
          f"LoC {loc['cer']*100:.2f}%; ours minus LoC {d*100:+.2f} ({lo*100:.2f} to {hi*100:.2f}) -> {verdict}")
(a.out / "rule.json").write_text(json.dumps(rule, indent=1) + "\n")
if a.live:
    edges = [0.10, 0.20, 0.25, 0.32]
    names = ["<0.10", "0.10-0.20", "0.20-0.25", "0.25-0.32", ">=0.32"]
    pages = {n: [] for n in names}
    for r in csv.DictReader(a.live.open(), delimiter="\t"):
        if r["lang"] == "ger":
            pages[names[sum(float(r["loc_damage"]) >= e for e in edges)]].append(r["doc_id"])
    for n in names:
        readd = rule.get(n) == "keep"
        if readd:
            (a.out / f"readd-{n.replace('<', 'lt').replace('>=', 'ge')}.txt").write_text("".join(p + "\n" for p in sorted(pages[n])))
        print(f"band {n}: {len(pages[n])} live pages; {'re-add' if readd else 'stay out'}")
