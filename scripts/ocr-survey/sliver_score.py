#!/usr/bin/env python3
"""Score the sliver filter (usnm_alto::slivers) on references: each line's text before and after.

Reads the JSON lines `cargo run -p usnm-alto --example slivers` writes, matches them to reference
lines and reports character and word error before and after the filter.

    sliver_score.py cherokee --measured out.jsonl --lines crops/lines.jsonl --alto-root DIR [--tier silver]
    sliver_score.py crops --measured out.jsonl --crops crops.jsonl --alto-root DIR

`cherokee` takes test set v2's lines.jsonl: a row per LoC TextLine with `page`, `loc_text` and
`reference`. A measured line matches the row on its page whose `loc_text` is its text before the
filter. Error is counted on syllabary letters only, as `cherokee_testset.py score` counts it; word
error on the syllabary words.

`crops` takes gold crops (`page_id` as `<lccn>/<date>/ed-<n>/seq-<m>`, `box` in the page image's
pixels, `text` the transcription): a crop's hypothesis is the measured lines whose centre is in its
box, in order. Character error is on all characters with whitespace runs as one space, word error
on whitespace tokens.

The measured `file` must end in `<alto-root>/<lccn>/<yyyy>/<mm>/<dd>/ed-<n>/seq-<m>/ocr.xml`.
"""
from __future__ import annotations

import argparse
import collections
import json
import re
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
    lines, _, _ = measured(a.measured, a.alto_root)
    tot = collections.Counter()
    changed = []
    for c in map(json.loads, open(a.crops, encoding="utf-8")):
        page = c["page_id"]
        if page not in lines:
            tot["unmatched"] += 1
            continue
        x0, y0, x1, y1 = c["box"]
        inside = [l for l in lines[page]
                  if x0 <= (l["box"][0] + l["box"][2]) / 2 <= x1 and y0 <= (l["box"][1] + l["box"][3]) / 2 <= y1]
        gold = " ".join(c["text"].split())
        tot["lines"] += 1
        hyps = {}
        for side in ("before", "after"):
            hyp = " ".join(" ".join(l[side] for l in inside).split())
            hyps[side] = hyp
            tot[f"{side}_cerr"] += edit(gold, hyp)
            tot[f"{side}_werr"] += edit(gold.split(), hyp.split())
        tot["letters"] += len(gold)
        tot["words"] += len(gold.split())
        if hyps["before"] != hyps["after"]:
            tot["lines_changed"] += 1
            changed.append({"crop": c.get("crop_id"), "dropped": [
                (l["before"], l["after"]) for l in inside if l["before"] != l["after"]]})
    return summarize(tot, changed, "crops")


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
    a = p.parse_args(argv)
    out = cmd_cherokee(a) if a.cmd == "cherokee" else cmd_crops(a)
    json.dump(out, sys.stdout, ensure_ascii=False)
    print()
    return 0


if __name__ == "__main__":
    sys.exit(main())
