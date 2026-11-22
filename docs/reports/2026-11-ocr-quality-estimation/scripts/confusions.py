#!/usr/bin/env python3
"""Character confusion counts from the truth sets: each reading is aligned to its reference
(both folded and lower-cased, whitespace collapsed) and every edit is counted as (expected, read).

    confusions.py --truth rows/truth.jsonl --out confusions.json

Out: {"<lang>": {"<reader>": {"chars": N, "ops": {"<expected>\\t<read>": count}}}}.
"""
import argparse, json
from collections import Counter, defaultdict
from qe_lib import cer_text, edit_ops, fold

ap = argparse.ArgumentParser()
ap.add_argument("--truth", required=True)
ap.add_argument("--out", required=True)
a = ap.parse_args()
ops = defaultdict(lambda: defaultdict(Counter))
chars = defaultdict(Counter)
for l in open(a.truth, encoding="utf-8"):
    r = json.loads(l)
    ref, hyp = fold(cer_text(r["ref"], True), r["lang"]), fold(cer_text(r["hyp"], True), r["lang"])
    if not ref:
        continue
    chars[r["lang"]][r["reader"]] += len(ref)
    for e, g in edit_ops(ref, hyp):
        ops[r["lang"]][r["reader"]][f"{e}\t{g}"] += 1
out = {lang: {rd: {"chars": chars[lang][rd], "ops": dict(c.most_common())} for rd, c in m.items()} for lang, m in ops.items()}
json.dump(out, open(a.out, "w", encoding="utf-8"), ensure_ascii=False, indent=1, sort_keys=True)
for lang, m in sorted(out.items()):
    for rd, v in sorted(m.items()):
        top = list(v["ops"].items())[:8]
        print(lang, rd, v["chars"], sum(v["ops"].values()), [(k.replace("\t", "→") or "?", n) for k, n in top])
