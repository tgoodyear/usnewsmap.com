"""Pages found for the New Testament's most frequent Cherokee words, exact and with `~ocr`'s Cherokee
spellings (crates/usnm-core/src/ocr_variants.rs, CHEROKEE_CONFUSIONS), on LoC's OCR of the Phoenix.

An added page is "misreading" when every variant it matched is a non-word, "attested" when every one is
itself a New Testament form (the page may hold that other word), and "mixed" when it matched both kinds.
"attested" and "mixed" pages are possible false hits. Writes JSON to standard output.

Usage: --tar LOC_BATCH.tar.bz2 [--tar ...] --wordlists ocr/ja-ocr/wordlists [--top 200]
"""
import argparse
import collections
import json
import os
import re
import tarfile

PAIRS = [("Ꮅ", "Ꮲ"), ("Ꮧ", "Ꮨ"), ("Ꮩ", "Ꭺ"), ("Ꮎ", "Ᏻ"), ("Ꮎ", "Ꮹ"), ("Ꮎ", "Ꭷ"), ("Ꮎ", "Ꮾ"),
         ("Ꮎ", "Ꮳ"), ("Ꮈ", "Ꮔ"), ("Ꮣ", "Ꮦ"), ("Ꮓ", "Ꭲ"), ("Ᏻ", "Ꮕ"), ("Ꮯ", "Ꮆ")]


def variants(word):
    out = {word}
    for a, b in PAIRS:
        for f, t in ((a, b), (b, a)):
            for i, c in enumerate(word):
                if c == f:
                    out.add(word[:i] + t + word[i + 1:])
    return out


# Tokens as the index keeps them: whole runs of letters and digits (usnm_core::text::index_tokens), so
# `5ᏂᎥ` is one token that no syllabary word matches. Only all-syllabary tokens count.
SYLLABARY = re.compile("[\u13A0-\u13F5]+")


def syllabary_tokens(text):
    return [t for t in re.findall(r"[^\W_]+", text) if SYLLABARY.fullmatch(t)]


ap = argparse.ArgumentParser()
ap.add_argument("--tar", action="append", required=True)
ap.add_argument("--wordlists", required=True)
ap.add_argument("--top", type=int, default=200)
a = ap.parse_args()
top = [w for w in open(os.path.join(a.wordlists, "chr.txt"), encoding="utf-8").read().split() if w]
attested = set(top) | set(open(os.path.join(a.wordlists, "chr-attested.txt"), encoding="utf-8").read().split())

pages = []
for t in a.tar:
    with tarfile.open(t, "r:bz2") as tf:
        for m in tf:
            if m.name.endswith("ocr.txt"):
                pages.append(set(syllabary_tokens(tf.extractfile(m).read().decode("utf-8", "replace"))))

rows = []
for w in top[: a.top]:
    vs = variants(w) - {w}
    exact = {i for i, p in enumerate(pages) if w in p}
    added = collections.Counter()
    for i, p in enumerate(pages):
        if i in exact:
            continue
        hit = vs & p
        if hit:
            known_hit = hit & attested
            added["attested" if known_hit == hit else "mixed" if known_hit else "misreading"] += 1
    rows.append({"word": w, "variants": len(vs) + 1, "exact_pages": len(exact),
                 "added_misreading": added["misreading"], "added_attested": added["attested"],
                 "added_mixed": added["mixed"]})
tot = lambda k: sum(r[k] for r in rows)
def gain(r):
    return r["added_misreading"] + r["added_attested"] + r["added_mixed"]


improved = [r for r in rows if gain(r)]
print(json.dumps({
    "words": len(rows), "words_with_variants": sum(r["variants"] > 1 for r in rows),
    "words_with_added_pages": len(improved),
    "page_hits_exact": tot("exact_pages"),
    "page_hits_added_misreading": tot("added_misreading"),
    "page_hits_added_attested": tot("added_attested"),
    "page_hits_added_mixed": tot("added_mixed"),
    "largest_gains": sorted(rows, key=lambda r: -gain(r))[:15],
}, ensure_ascii=False, indent=1))
