"""Compare Tesseract chr models that differ only in their word list (#1621).

For each model directory under --out-dir (holding nt/<page>.txt from full-page OCR of the New Testament
sample and phoenix/<page>.json sidecars from cherokee_hybrid.py), reports: on the New Testament pages,
the share of the reference's syllabary words the OCR has, and the share of the OCR's syllabary words in
the reference, counted as multisets (the reference text is in pdftotext's layout order, with the two
columns interleaved, so an order-dependent character error isn't meaningful); on the Phoenix lines, the ᎾᏍᎩ family counts,
edit-distance agreement with LoC's text, the share of tokens in the NT top 5,000 and in the NT (both
circular once the word list holds those words), and, against --baseline, how the changed tokens fall:
a known word replacing a non-word or the reverse, a one-letter change along a measured confusion pair
(towards or away from a known word), or other.

Usage: --out-dir DIR --refs NT_REF_DIR --wordlists ocr/ja-ocr/wordlists --baseline MODEL
"""
import argparse
import collections
import glob
import json
import os
import re

SYL = re.compile("[Ꭰ-Ᏽ]+")
PAIRS = {frozenset(p) for p in [("Ꮅ", "Ꮲ"), ("Ꮧ", "Ꮨ"), ("Ꮩ", "Ꭺ"), ("Ꮎ", "Ᏻ"), ("Ꮎ", "Ꮹ"), ("Ꮎ", "Ꭷ"),
                                ("Ꮎ", "Ꮾ"), ("Ꮎ", "Ꮳ"), ("Ꮈ", "Ꮔ"), ("Ꮣ", "Ꮦ"), ("Ꮓ", "Ꭲ"), ("Ᏻ", "Ꮕ"),
                                ("Ꮯ", "Ꮆ")]}
NASGI = ["ᎾᏍᎩ", "ᏩᏍᎩ", "ᏣᏍᎩ", "ᏳᏍᎩ", "ᎧᏍᎩ", "ᏮᏍᎩ"]


def edit_distance(a, b):
    """Levenshtein distance, Myers' bit-parallel algorithm on Python integers."""
    if not a:
        return len(b)
    peq = collections.defaultdict(int)
    for i, c in enumerate(a):
        peq[c] |= 1 << i
    m, pv, mv, score, high = len(a), -1, 0, len(a), 1 << (len(a) - 1)
    mask = (1 << m) - 1
    pv &= mask
    for c in b:
        eq = peq.get(c, 0)
        xv = eq | mv
        xh = (((eq & pv) + pv) ^ pv) | eq
        ph = mv | ~(xh | pv) & mask
        mh = pv & xh
        if ph & high:
            score += 1
        elif mh & high:
            score -= 1
        ph = (ph << 1) | 1
        mh <<= 1
        pv = (mh | ~(xv | ph)) & mask
        mv = ph & xv & mask
    return score


def tokens(s):
    return [t for t in re.findall(r"[^\W_]+", s) if SYL.fullmatch(t)]


ap = argparse.ArgumentParser()
ap.add_argument("--out-dir", required=True)
ap.add_argument("--refs", required=True)
ap.add_argument("--wordlists", required=True)
ap.add_argument("--baseline", required=True)
a = ap.parse_args()
top = set(open(os.path.join(a.wordlists, "chr.txt"), encoding="utf-8").read().split())
nt = top | set(open(os.path.join(a.wordlists, "chr-attested.txt"), encoding="utf-8").read().split())

lines = {}
result = {}
for model in sorted(os.listdir(a.out_dir)):
    d = os.path.join(a.out_dir, model)
    if not os.path.isdir(os.path.join(d, "nt")):
        continue
    bag = collections.Counter()
    for ref in sorted(glob.glob(os.path.join(a.refs, "*.txt"))):
        hyp = os.path.join(d, "nt", os.path.basename(ref))
        r = collections.Counter(tokens(open(ref, encoding="utf-8").read()))
        h = collections.Counter(tokens(open(hyp, encoding="utf-8").read())) if os.path.exists(hyp) else collections.Counter()
        bag["matched"] += sum((r & h).values())
        bag["ref"] += sum(r.values())
        bag["hyp"] += sum(h.values())
    toks = collections.Counter()
    agree = collections.Counter()
    lines[model] = {}
    for f in sorted(glob.glob(os.path.join(d, "phoenix", "*.json"))):
        page = os.path.basename(f)[:-5]
        for b in json.load(open(f, encoding="utf-8"))["blocks"]:
            for ln in b["lines"]:
                t = ln.get("tesseract_text")
                if t is None:
                    continue
                lines[model][(page, ln["line_id"])] = t
                toks.update(tokens(t))
                loc = "".join(SYL.findall(ln["source_text"] or ""))
                agree["err"] += edit_distance(loc, "".join(SYL.findall(t)))
                agree["len"] += len(loc)
    total = sum(toks.values())
    result[model] = {
        "nt_word_recall": round(bag["matched"] / bag["ref"], 4),
        "nt_word_precision": round(bag["matched"] / bag["hyp"], 4),
        "phoenix_lines": len(lines[model]),
        "phoenix_tokens": total,
        "phoenix_in_top5000": round(sum(n for w, n in toks.items() if w in top) / total, 4),
        "phoenix_in_nt": round(sum(n for w, n in toks.items() if w in nt) / total, 4),
        "phoenix_edit_distance_to_loc": round(agree["err"] / agree["len"], 4),
        "nasgi_family": {w: toks[w] for w in NASGI},
    }

base = lines[a.baseline]
for model, ls in lines.items():
    if model == a.baseline:
        continue
    kinds = collections.Counter()
    examples = collections.defaultdict(collections.Counter)
    for key, t in ls.items():
        b_toks, m_toks = tokens(base.get(key, "")), tokens(t)
        if b_toks == m_toks:
            continue
        if len(b_toks) != len(m_toks):
            kinds["token_count_changed_lines"] += 1
            continue
        for x, y in zip(b_toks, m_toks):
            if x == y:
                continue
            diff = [(p, q) for p, q in zip(x, y) if p != q] if len(x) == len(y) else None
            along = diff is not None and len(diff) == 1 and frozenset(diff[0]) in PAIRS
            if along and y in nt and x not in nt:
                k = "pair_letter_to_a_known_word"
            elif along and x in nt and y not in nt:
                k = "pair_letter_away_from_a_known_word"
            elif along and x in nt and y in nt:
                k = "pair_letter_both_known"
            elif along:
                k = "pair_letter_neither_known"
            elif y in nt and x not in nt:
                k = "known_word_replaced_a_non_word"
            elif x in nt and y not in nt:
                k = "non_word_replaced_a_known_word"
            else:
                k = "other"
            kinds[k] += 1
            examples[k][f"{x}>{y}"] += 1
    result[model]["changed_vs_" + a.baseline] = dict(kinds)
    result[model]["examples"] = {k: [e for e, _ in c.most_common(5)] for k, c in examples.items()}
print(json.dumps(result, ensure_ascii=False, indent=1))
