#!/usr/bin/env python3
"""Pages for the German keep/revoke human truth, by damage band (#835, #968).

    german_band_draw.py --live LIVE.tsv --out selection.jsonl [--per-band 32] [--tag reader-cmp-202611]

LIVE.tsv is the decoded list of the pages whose search text is our frak2021 reading (release
pages-v20261026-1): doc_id, engine, batch_index, lang, decade, loc_damage, band. Only lang `ger`.
Bands are set by loc_damage (quality.damage's damage_rate on LoC's text) at --edges (default 0.10, 0.20,
0.25, 0.32: five bands, named by their range). The list's own `band` column is not used.
In each band: one page per issue; titles (LCCNs) in order of sha256(TAG:de-band:lccn); the pages of
the titles taken round robin, each title's pages in order of sha256(TAG:de-band-page:doc_id), until
--per-band pages. Output rows are gold.py `select` rows: lccn, date, edition, seq, page_id, group,
lang, era, stratum, plus band and loc_damage.
"""
import argparse, csv, hashlib, json, re
from collections import defaultdict
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("--live", type=Path, required=True)
ap.add_argument("--out", type=Path, required=True)
ap.add_argument("--per-band", type=int, default=32)
ap.add_argument("--tag", default="reader-cmp-202611")
ap.add_argument("--edges", default="0.10,0.20,0.25,0.32", help="band edges on loc_damage")
a = ap.parse_args()
h = lambda s: hashlib.sha256(s.encode()).hexdigest()
EDGES = [float(x) for x in a.edges.split(",")]
BAND = [f"<{EDGES[0]:.2f}"] + [f"{lo:.2f}-{hi:.2f}" for lo, hi in zip(EDGES, EDGES[1:])] + [f">={EDGES[-1]:.2f}"]
band_of = lambda d: BAND[sum(d >= e for e in EDGES)]
rows = [r for r in csv.DictReader(a.live.open(), delimiter="\t") if r["lang"] == "ger"]
out = []
for band in BAND:
    issues = {}
    for r in rows:
        if band_of(float(r["loc_damage"])) != band:
            continue
        m = re.fullmatch(r"(\w+)_(\d{4}-\d{2}-\d{2})_ed-(\d+)_seq-(\d+)", r["doc_id"])
        if not m:
            continue
        key = (m[1], m[2], m[3])
        if key not in issues or h(f"{a.tag}:de-band-page:{r['doc_id']}") < h(f"{a.tag}:de-band-page:{issues[key]['doc_id']}"):
            issues[key] = {**r, "lccn": m[1], "date": m[2], "ed": int(m[3]), "seq": int(m[4])}
    by_title = defaultdict(list)
    for v in issues.values():
        by_title[v["lccn"]].append(v)
    titles = sorted(by_title, key=lambda t: h(f"{a.tag}:de-band:{t}"))
    for t in titles:
        by_title[t].sort(key=lambda v: h(f"{a.tag}:de-band-page:{v['doc_id']}"))
    picked, rank = [], 0
    while len(picked) < a.per_band and any(len(by_title[t]) > rank for t in titles):
        for t in titles:
            if len(by_title[t]) > rank and len(picked) < a.per_band:
                picked.append(by_title[t][rank])
        rank += 1
    for v in picked:
        out.append({"lccn": v["lccn"], "date": v["date"], "edition": v["ed"], "seq": v["seq"], "page_id": v["doc_id"],
                    "group": f"de-{band}", "lang": "ger", "era": f"{v['decade']}s", "stratum": f"de-{band}",
                    "band": band, "loc_damage": float(v["loc_damage"])})
a.out.write_text("".join(json.dumps(r) + "\n" for r in out), encoding="utf-8")
from collections import Counter
print("coverage of the list:", Counter(band_of(float(r["loc_damage"])) for r in rows), "sample:", Counter(r["band"] for r in out), "titles per band:", {b: len({r['lccn'] for r in out if r['band']==b}) for b in BAND})
