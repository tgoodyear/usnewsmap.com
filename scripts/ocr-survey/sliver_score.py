#!/usr/bin/env python3
"""Score the sliver filter (usnm_alto::slivers) on references: each line's text before and after.

Reads the JSON lines `cargo run -p usnm-alto --example slivers` writes, matches them to reference
lines and reports character and word error before and after the filter.

    sliver_score.py cherokee --measured out.jsonl --lines crops/lines.jsonl --alto-root DIR [--tier silver]
    sliver_score.py crops --measured out.jsonl --crops crops.jsonl --alto-root DIR --work DIR [--scorer PATH]

`cherokee` takes test set v2's lines.jsonl: a row per LoC TextLine with `page`, `loc_text` and
`reference`. A measured line matches the row on its page whose `loc_text` is its text before the
filter. Error is counted on syllabary letters only, as `cherokee_testset.py score` counts it; word
error on the syllabary words.

`crops` takes gold crops (`page_id` as `<lccn>/<date>/ed-<n>/seq-<m>`, `box` in the page image's
pixels, `text` the transcription): a crop's hypothesis is the measured lines whose centre is in its
box, in order. They are scored by `usnm-ocr-survey score` (the gold set's CER and WER: NFC, line-end
hyphens rejoined, whitespace runs as one space).

The measured `file` must end in `<alto-root>/<lccn>/<yyyy>/<mm>/<dd>/ed-<n>/seq-<m>/ocr.xml`.
"""
from __future__ import annotations

import argparse
import collections
import csv
import json
import os
import re
import subprocess
import sys

SYLLABARY = re.compile("[Ꭰ-Ᏽ]+")


def edit(a, b) -> int:
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1]


def page_of(path: str, root: str) -> str:
    rel = path[len(root):].strip("/") if path.startswith(root) else path
    parts = rel.split("/")
    lccn, y, m, d, ed, seq = parts[-7:-1]
    return f"{lccn}/{y}-{m}-{d}/{ed}/{seq}"


def measured(path: str, root: str):
    lines, words, pages = collections.defaultdict(list), [], []
    for row in map(json.loads, open(path, encoding="utf-8")):
        if row["kind"] == "line":
            lines[page_of(row["file"], root)].append(row)
        elif row["kind"] == "word":
            words.append(row)
        elif row["kind"] == "page":
            pages.append(row)
    return lines, words, pages


def strip_hyphen(s: str) -> str:
    return s[:-1] if s.endswith(("-", "¬", "⸗")) else s


def cmd_cherokee(a) -> dict:
    lines, _, _ = measured(a.measured, a.alto_root)
    rows = [json.loads(x) for x in open(a.lines, encoding="utf-8")]
    if a.tier:
        rows = [r for r in rows if r["tier"] == a.tier]
    by_text = {}
    for page, ls in lines.items():
        for l in ls:
            by_text.setdefault((page, strip_hyphen(l["before"])), l)
            by_text.setdefault((page, l["before"]), l)
    tot = collections.Counter()
    changed = []
    for r in rows:
        l = by_text.get((r["page"], r["loc_text"])) or by_text.get((r["page"], strip_hyphen(r["loc_text"])))
        if l is None:
            tot["unmatched"] += 1
            continue
        tot["lines"] += 1
        gold = "".join(SYLLABARY.findall(r["reference"]))
        gw = SYLLABARY.findall(r["reference"])
        for side in ("before", "after"):
            hyp = l[side]
            tot[f"{side}_cerr"] += edit(gold, "".join(SYLLABARY.findall(hyp)))
            tot[f"{side}_werr"] += edit(gw, SYLLABARY.findall(hyp))
        tot["letters"] += len(gold)
        tot["words"] += len(gw)
        if l["before"] != l["after"]:
            tot["lines_changed"] += 1
            changed.append({"crop": r["crop"], "tier": r["tier"], "reference": r["reference"],
                            "before": l["before"], "after": l["after"]})
    return summarize(tot, changed, a.tier or "all")


def cmd_crops(a) -> dict:
    """Writes the crops and the before and after texts in `usnm-ocr-survey score`'s form to --work
    and scores them with it (Myers' bit-parallel edit distance in Rust)."""
    lines, _, _ = measured(a.measured, a.alto_root)
    os.makedirs(a.work, exist_ok=True)
    crops_path = os.path.join(a.work, "crops.jsonl")
    hyp_paths = {side: os.path.join(a.work, f"{side}.jsonl") for side in ("before", "after")}
    tot = collections.Counter()
    changed = []
    with open(crops_path, "w", encoding="utf-8") as cf, open(hyp_paths["before"], "w", encoding="utf-8") as bf, \
            open(hyp_paths["after"], "w", encoding="utf-8") as af:
        for c in map(json.loads, open(a.crops, encoding="utf-8")):
            page = c["page_id"]
            if page not in lines:
                tot["unmatched"] += 1
                continue
            x0, y0, x1, y1 = c["box"]
            inside = [l for l in lines[page]
                      if x0 <= (l["box"][0] + l["box"][2]) / 2 <= x1 and y0 <= (l["box"][1] + l["box"][3]) / 2 <= y1]
            tot["lines"] += 1
            cf.write(json.dumps({"crop_id": c["crop_id"], "group": page, "transcription": c["text"],
                                 "transcriber": "published ground truth", "reviewer": "not applicable"},
                                ensure_ascii=False) + "\n")
            texts = {side: "\n".join(l[side] for l in inside) for side in ("before", "after")}
            bf.write(json.dumps({"crop_id": c["crop_id"], "text": texts["before"]}, ensure_ascii=False) + "\n")
            af.write(json.dumps({"crop_id": c["crop_id"], "text": texts["after"]}, ensure_ascii=False) + "\n")
            if texts["before"] != texts["after"]:
                tot["lines_changed"] += 1
                changed.append({"crop": c["crop_id"], "dropped": [
                    (l["before"], l["after"]) for l in inside if l["before"] != l["after"]]})
    out_dir = os.path.join(a.work, "scores")
    subprocess.run([a.scorer, "score", "--crops", crops_path, "--hyp", f"before={hyp_paths['before']}",
                    "--hyp", f"after={hyp_paths['after']}", "--out", out_dir], check=True, stdout=subprocess.DEVNULL)
    out = {"set": "crops", "lines": tot["lines"], "unmatched": tot["unmatched"], "lines_changed": tot["lines_changed"]}
    for r in csv.DictReader(open(os.path.join(out_dir, "cer-summary.csv"), encoding="utf-8")):
        if r["group"] == "all":
            out["letters"] = int(r["ref_chars"])
            out[f"cer_{r['engine']}"] = float(r["cer"])
            out[f"wer_{r['engine']}"] = float(r["wer"])
    out["changed"] = changed
    return out


def summarize(tot, changed, label) -> dict:
    out = {"set": label, **{k: tot[k] for k in ("lines", "unmatched", "lines_changed", "letters", "words")}}
    for side in ("before", "after"):
        out[f"cer_{side}"] = round(tot[f"{side}_cerr"] / tot["letters"], 5) if tot["letters"] else None
        out[f"wer_{side}"] = round(tot[f"{side}_werr"] / tot["words"], 5) if tot["words"] else None
        out[f"char_errors_{side}"] = tot[f"{side}_cerr"]
        out[f"word_errors_{side}"] = tot[f"{side}_werr"]
    out["changed"] = changed
    return out


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    ch = sub.add_parser("cherokee")
    ch.add_argument("--measured", required=True)
    ch.add_argument("--lines", required=True)
    ch.add_argument("--alto-root", required=True)
    ch.add_argument("--tier")
    cr = sub.add_parser("crops")
    cr.add_argument("--measured", required=True)
    cr.add_argument("--crops", required=True)
    cr.add_argument("--alto-root", required=True)
    cr.add_argument("--work", required=True, help="where the scorer's inputs and its CSV files go")
    cr.add_argument("--scorer", default="usnm-ocr-survey", help="the usnm-ocr-survey binary")
    a = p.parse_args(argv)
    out = cmd_cherokee(a) if a.cmd == "cherokee" else cmd_crops(a)
    json.dump(out, sys.stdout, ensure_ascii=False)
    print()
    return 0


if __name__ == "__main__":
    sys.exit(main())
