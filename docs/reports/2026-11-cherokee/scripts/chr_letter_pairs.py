"""Which syllabary letters LoC's OCR reads as which, from its misreadings of known words.

A token counts when it is not a New Testament form (chr-attested.txt) and is one letter substitution
from exactly one of the NT's 5,000 most frequent forms (chr.txt); the pair (known letter, OCR letter)
is counted once per occurrence. Writes JSON to standard output.

Usage: --tar LOC_BATCH.tar.bz2 [--tar ...] --wordlists ocr/ja-ocr/wordlists
"""
import argparse
import collections
import json
import os
import re
import tarfile

ap = argparse.ArgumentParser()
ap.add_argument("--tar", action="append", required=True)
ap.add_argument("--wordlists", required=True)
a = ap.parse_args()

top = [w for w in open(os.path.join(a.wordlists, "chr.txt"), encoding="utf-8").read().split() if w]
attested = set(open(os.path.join(a.wordlists, "chr-attested.txt"), encoding="utf-8").read().split())
known = set(top) | attested
# Index the top 5,000 by each one-letter wildcard so a token's neighbours are a lookup.
by_mask = collections.defaultdict(set)
for w in top:
    for i in range(len(w)):
        by_mask[w[:i] + "\0" + w[i + 1:]].add(w)

tokens = collections.Counter()
for t in a.tar:
    with tarfile.open(t, "r:bz2") as tf:
        for m in tf:
            if m.name.endswith("ocr.txt"):
                tokens.update(re.findall(r"[Ꭰ-Ᏽ]+", tf.extractfile(m).read().decode("utf-8", "replace")))

pairs = collections.Counter()
examples = collections.defaultdict(collections.Counter)
misread = 0
for tok, n in tokens.items():
    if tok in known:
        continue
    hits = {w for i in range(len(tok)) for w in by_mask.get(tok[:i] + "\0" + tok[i + 1:], ())}
    if len(hits) != 1:
        continue
    (w,) = hits
    i = next(i for i, (x, y) in enumerate(zip(w, tok)) if x != y)
    pairs[(w[i], tok[i])] += n
    examples[(w[i], tok[i])][f"{w}>{tok}"] += n
    misread += n

total = sum(tokens.values())
print(json.dumps({
    "syllabary_tokens": total,
    "one_letter_misreadings": misread,
    "pairs": [{"known": k, "ocr": o, "occurrences": n,
               "examples": [e for e, _ in examples[(k, o)].most_common(3)]}
              for (k, o), n in pairs.most_common(40)],
}, ensure_ascii=False, indent=1))
