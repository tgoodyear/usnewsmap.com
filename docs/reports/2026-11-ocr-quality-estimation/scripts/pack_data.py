#!/usr/bin/env python3
"""Copies the study's numbers (no corpus text) into the report's data/ directory.

    pack_data.py --work /root/work/ocr-qe --out data
"""
import argparse, glob, gzip, json, os, shutil

ap = argparse.ArgumentParser()
ap.add_argument("--work", required=True)
ap.add_argument("--out", required=True)
a = ap.parse_args()
os.makedirs(a.out, exist_ok=True)
for f in ("validation.json", "verdicts.json", "aggregate-check.json", "overread-enop.json", "mlm-same-sample.json"):
    shutil.copy(os.path.join(a.work, "out", f), os.path.join(a.out, f))
conf = json.load(open(os.path.join(a.work, "confusions.json"), encoding="utf-8"))
top = {l: {r: {"chars": v["chars"], "errors": sum(v["ops"].values()), "ops": dict(list(v["ops"].items())[:40])} for r, v in m.items() if r != "as"} for l, m in conf.items()}
json.dump(top, open(os.path.join(a.out, "confusions-top.json"), "w", encoding="utf-8"), ensure_ascii=False, indent=1, sort_keys=True)
keep = ["set", "lang", "unit", "page", "reader", "chars", "words", "known3", "ws_tokens", "lex_hit", "oov", "nearmiss_any", "nearmiss_conf30",
        "nearmiss_conf50", "nearmiss_conf80", "novel", "garbage", "script_mix", "charlm_bpc", "cer", "cer_raw", "ref_chars"]
with gzip.open(os.path.join(a.out, "truth-metrics.jsonl.gz"), "wt", encoding="utf-8") as out:
    for p in sorted(glob.glob(os.path.join(a.work, "metrics", "truth-*.jsonl"))):
        for l in open(p, encoding="utf-8"):
            r = json.loads(l)
            out.write(json.dumps({k: r.get(k) for k in keep}) + "\n")
shutil.copy(os.path.join(a.work, "data", "qe-resources", "SOURCES.tsv"), os.path.join(a.out, "resources.tsv"))
shutil.copy(os.path.join(a.work, "data", "qe-resources", "gutenberg", "books.tsv"), os.path.join(a.out, "gutenberg-books.tsv"))
