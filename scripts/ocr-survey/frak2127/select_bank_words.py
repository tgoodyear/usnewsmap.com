#!/usr/bin/env python3
"""Which trusted words go into arm C's glyph bank (#2127), so the bank fits the dev CT's disk.

The 250 pages give far more trusted words than a bank needs for its common letters (each glyph is a
file). Words are taken in the order of sha256("2127:bank:word:" + id), and a word is taken while
any of its letters has fewer than CAP_PER_LETTER glyphs from the words taken so far. So every letter
gets up to CAP_PER_LETTER samples (rare letters get all their words) and common letters get more,
from the same words. The crops of words not taken are deleted (--delete-rest).

    select_bank_words.py --words trusted/words.jsonl --out trusted/words-bank.jsonl --delete-rest
"""
import argparse, collections, hashlib, json, os, sys

CAP_PER_LETTER = 3000


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--words", required=True); ap.add_argument("--out", required=True)
    ap.add_argument("--delete-rest", action="store_true")
    a = ap.parse_args()
    rows = [json.loads(l) for l in open(a.words, encoding="utf-8")]
    rows.sort(key=lambda r: hashlib.sha256(("2127:bank:word:" + r["id"]).encode()).hexdigest())
    have, taken, rest = collections.Counter(), [], []
    for r in rows:
        if any(have[c] < CAP_PER_LETTER for c in r["label"]):
            have.update(r["label"])
            taken.append(r)
        else:
            rest.append(r)
    base = os.path.dirname(a.words)
    with open(a.out, "w", encoding="utf-8") as f:
        for r in taken:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    if a.delete_rest:
        for r in rest:
            os.remove(os.path.join(base, r["crop"]))
    summary = {"cap_per_letter": CAP_PER_LETTER, "trusted_words": len(rows), "taken": len(taken),
               "letters_taken": sum(have.values()), "per_letter": dict(sorted(have.items()))}
    json.dump(summary, open(a.out + ".summary.json", "w"), ensure_ascii=False, indent=1)
    print(json.dumps({k: summary[k] for k in ("trusted_words", "taken", "letters_taken")}))


if __name__ == "__main__":
    sys.exit(main())
