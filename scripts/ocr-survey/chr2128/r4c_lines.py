#!/usr/bin/env python3
"""R4c's real GHN lines (#2128, Amendment 1, change 2), from S0's Cherokee lines on the 489 GHN training
pages, written as <name>.png + <name>.gt.txt for training.

Consensus lines: stock `chr` and `chr_phoenix` read the crop to the same text (exactly, `ghn_words.py`),
with at least 10 syllabary letters, and every word in the #1621 dictionary test's model B word list (stock
`chr` plus the 1860 New Testament's forms, 29,477 words). A word is a space-separated token with leading
and trailing punctuation stripped; it must then be all syllabary and in the list. The text must be writable
by the start model (no 4, ? or straight quotes, as `chr_real_lines.py`). The label is the agreed reading.

Aligned lines: the 51 lines phase 1's builder kept on the training pages (`r4-candidates/lines.jsonl`),
labelled with their NT reference. Where an aligned line is also a consensus line, the aligned reference wins.

Dropped: any line that aligns to one of the excluded verses (the builder's aligned rows on the training
pages, kept or dropped, whose verses touch the list), and any line whose label holds a run of an excluded
verse's letters (`exclusion.py`). The script asserts both on what it writes.

    r4c_lines.py --words words.jsonl --wordlist B_stock_nt.txt --aligned-rows work/r4/aligned.jsonl
                 --aligned-lines r4-candidates/lines.jsonl --crops-dir DIR --nt-text cnts.raw.txt
                 --exclude-verses verses-any-aligned.txt --out DIR
"""
import argparse
import json
import os
import re
import shutil

import common
import exclusion as ex

PUNCT = ".,;:!?\"'“”‘’()[]-—"
UNWRITABLE = re.compile("[?\"'‘4]")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--words", required=True)
    ap.add_argument("--wordlist", required=True)
    ap.add_argument("--aligned-rows", required=True)
    ap.add_argument("--aligned-lines", required=True)
    ap.add_argument("--crops-dir", required=True)
    ap.add_argument("--nt-text", required=True)
    ap.add_argument("--exclude-verses", required=True)
    ap.add_argument("--min-letters", type=int, default=10)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    E = ex.Exclusion(a.nt_text, a.exclude_verses)
    vocab = {l.strip() for l in open(a.wordlist, encoding="utf-8") if l.strip()}
    touch = set()
    for r in common.read_jsonl(a.aligned_rows):
        if set(E.vm.verses(r["nt_offset"], r["nt_end"])) & E.ids:
            touch.add(r["line_id"])
    stats = {"wordlist_words": len(vocab), "candidates": 0, "consensus": 0, "aligned": 0,
             "dropped_aligned_to_excluded_verse": 0, "dropped_excluded_text": 0, "not_same": 0,
             "too_short": 0, "word_not_in_list": 0, "unwritable": 0}
    labels = {}
    for r in common.read_jsonl(a.words):
        stats["candidates"] += 1
        t = r["chr_phoenix"].strip()
        if t != r["chr"].strip():
            stats["not_same"] += 1
            continue
        if len(ex.letters(t)) < a.min_letters:
            stats["too_short"] += 1
            continue
        if UNWRITABLE.search(t):
            stats["unwritable"] += 1
            continue
        cores = [w.strip(PUNCT) for w in t.split()]
        if not all(c and all(ex.ct.SYLLABARY.fullmatch(ch) for ch in c) and c in vocab for c in cores):
            stats["word_not_in_list"] += 1
            continue
        labels[r["line_id"]] = (t, r["crop"], "consensus")
    for r in common.read_jsonl(a.aligned_lines):
        labels[r["line_id"]] = (r["reference"], r["crop"], "aligned")
    os.makedirs(a.out, exist_ok=True)
    with open(os.path.join(a.out, "lines.jsonl"), "w", encoding="utf-8") as idx:
        for lid, (text, crop, kind) in sorted(labels.items()):
            if lid in touch:
                stats["dropped_aligned_to_excluded_verse"] += 1
                continue
            if E.text_excluded(text):
                stats["dropped_excluded_text"] += 1
                continue
            stats[kind] += 1
            name = "ghn_" + lid.replace("/", "_").replace("#", "_")
            shutil.copyfile(os.path.join(a.crops_dir, crop), os.path.join(a.out, name + ".png"))
            open(os.path.join(a.out, name + ".gt.txt"), "w", encoding="utf-8").write(text + "\n")
            idx.write(json.dumps({"name": name, "line_id": lid, "text": text, "kind": kind}, ensure_ascii=False) + "\n")
    for line in open(os.path.join(a.out, "lines.jsonl"), encoding="utf-8"):
        r = json.loads(line)
        assert r["line_id"] not in touch and not E.text_excluded(r["text"]), r
    stats["written"] = stats["consensus"] + stats["aligned"]
    stats["asserted"] = True
    json.dump(stats, open(os.path.join(a.out, "summary.json"), "w"), indent=1)
    print(json.dumps(stats))


if __name__ == "__main__":
    main()
