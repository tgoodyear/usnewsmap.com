#!/usr/bin/env python3
"""Apply a trained combined segmenter to feature rows and write gutters in columns.py's format (#2107).

The output ({page id: {gutters, height, width}, "_meta"}) is what layout_columns/score.py --sets-json reads.
Pages listed by --pages-from but with no kept atom get an empty gutter list, so every page is scorable.

    infer.py --features feats.jsonl --model model.pkl --out detections.json [--threshold T] [--pages-from refs.json]
"""
import argparse
import hashlib
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "layout_columns"))
import gbdt  # noqa: E402


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--features", nargs="+", required=True)
    ap.add_argument("--model", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--threshold", type=float, help="default: the threshold stored with the model")
    ap.add_argument("--pages-from")
    a = ap.parse_args(argv)
    blob = gbdt.load(a.model)
    t = a.threshold if a.threshold is not None else blob["info"]["threshold"]
    rows = []
    for p in a.features:
        rows += [json.loads(ln) for ln in open(p) if ln.strip()]
    probs = gbdt.predict(blob["model"], rows)
    g = gbdt.gutters(rows, probs, t)
    sizes = {r["page_id"]: (r["width"], r["height"]) for r in rows}
    if a.pages_from:
        refs = json.load(open(a.pages_from))["pages"]
        for pid, ref in refs.items():
            sizes.setdefault(pid, (ref["width"], ref["height"]))
    out = {pid: {"gutters": g.get(pid, []), "height": sizes[pid][1], "width": sizes[pid][0]} for pid in sorted(sizes)}
    out["_meta"] = {"method": "combo-seg-gbdt", "threshold": t, "model_sha256": hashlib.sha256(open(a.model, "rb").read()).hexdigest(),
                    "atoms": len(rows), "kept": int((probs >= t).sum()), "pages": len(sizes)}
    with open(a.out, "w") as f:
        json.dump(out, f, sort_keys=True)
    print(json.dumps(out["_meta"], sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
