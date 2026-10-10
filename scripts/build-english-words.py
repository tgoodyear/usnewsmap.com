#!/usr/bin/env python3
"""Build the word lists that make a query English for its relative rate's
baseline (#237, 06 §6.3.3): crates/usnm-core/data/english-words.txt and
english-shared-words.txt.

    scripts/build-english-words.py [--top 100000] [--shared-ratio 10] [--out-dir path] [--check]

Needs wordfreq 3.1.1 (`pip install wordfreq==3.1.1`, the version the OCR
audit pins in ja-ocr/requirements.txt).

Words are folded as the index folds a page's words (lowercase, accents
dropped: `usnm_core::text::fold`), and a word's frequency in a language is
summed over the words that fold to it there. Of English's `--top` most
frequent words in wordfreq that fold to letters a to z only:

- **english-words.txt** lists those at least as frequent in English as in
  each other Latin-script language the corpus has a wordfreq list for
  (OTHERS). A search for one finds mostly English pages: "railroad",
  "cotton", "korea" and "the" are listed; "november" (German), "television"
  (French "télévision") and "influenza" (Italian) are not.
- **english-shared-words.txt** lists the rest that no such language uses more
  than `--shared-ratio` times as often as English: "november", "television",
  "civil", "john". They don't make a query English on their own, but they
  don't stop a phrase or an AND with an English word from being English.
  Words another language uses far more ("der", "la", "guerra", "influenza",
  and "war", German for "was") are in neither.

`usnm_core::query_language` reads a query as English from these lists. Each
is sorted, one word per line. `--check` compares them with the committed
files instead of writing them (exit 1 when they differ). A change to the
lists changes which baselines searches get, so it goes with a bump of
`RESPONSE_FORMAT` in crates/usnm-api/src/routes/mod.rs.
"""

import argparse
import re
import sys
import unicodedata
from pathlib import Path

OUT_DIR = Path(__file__).resolve().parent.parent / "crates" / "usnm-core" / "data"
ENGLISH, SHARED = "english-words.txt", "english-shared-words.txt"

# wordfreq codes of the corpus's Latin-script languages other than English
# (docs/reports/2026-10-language-analysis): German, Spanish, French, Polish,
# Italian, Danish, Czech, Norwegian, Serbo-Croatian (Serbian and Croatian),
# Finnish, Swedish, Lithuanian, Slovenian, Hungarian, Romanian, Slovak and
# Icelandic. Yiddish, Russian and Japanese are in other scripts, which an
# English word can't match; Hawaiian titles are English papers, and the
# indigenous languages have no list.
OTHERS = ["de", "es", "fr", "pl", "it", "da", "cs", "nb", "sh", "fi", "sv", "lt", "sl", "hu", "ro", "sk", "is"]

# Letters with no decomposition, transliterated as `push_folded` does.
SPECIAL = {"ſ": "s", "æ": "ae", "œ": "oe", "ø": "o", "ł": "l", "ß": "ss", "đ": "d", "ð": "d", "þ": "th",
           "ı": "i", "ŋ": "ng", "ħ": "h"}
LETTERS = re.compile(r"[a-z]+")


def fold(word: str) -> str:
    """`usnm_core::text::fold` for Latin-script words."""
    out = []
    for c in unicodedata.normalize("NFKD", word):
        if unicodedata.combining(c):
            continue
        for lc in c.lower():
            out.append(SPECIAL.get(lc, lc))
    return "".join(out)


def folded_frequencies(lang: str) -> dict[str, float]:
    """Each folded word's frequency in `lang`: the sum over the words that fold to it."""
    from wordfreq import get_frequency_dict

    out: dict[str, float] = {}
    for word, freq in get_frequency_dict(lang, wordlist="best").items():
        f = fold(word)
        if LETTERS.fullmatch(f):
            out[f] = out.get(f, 0.0) + freq
    return out


def build(top: int, shared_ratio: float) -> tuple[list[str], list[str]]:
    """The English words and the shared words (see the module docs)."""
    from wordfreq import top_n_list

    english = folded_frequencies("en")
    candidates = {f for w in top_n_list("en", top, wordlist="best") if LETTERS.fullmatch(f := fold(w))}
    others = [folded_frequencies(lang) for lang in OTHERS]
    most_elsewhere = {w: max(o.get(w, 0.0) for o in others) for w in candidates}
    words = sorted(w for w in candidates if english[w] >= most_elsewhere[w])
    shared = sorted(w for w in candidates if most_elsewhere[w] > english[w] and english[w] * shared_ratio >= most_elsewhere[w])
    return words, shared


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--top", type=int, default=100000, help="English words considered, most frequent first")
    ap.add_argument("--shared-ratio", type=float, default=10.0,
                    help="most times as frequent in another language as in English for a shared word")
    ap.add_argument("--out-dir", type=Path, default=OUT_DIR, help="where to write the lists")
    ap.add_argument("--check", action="store_true", help="compare with the files in --out-dir instead of writing them")
    a = ap.parse_args()
    words, shared = build(a.top, a.shared_ratio)
    differs = False
    for name, ws in ((ENGLISH, words), (SHARED, shared)):
        path = a.out_dir / name
        text = "".join(w + "\n" for w in ws)
        if a.check:
            same = path.exists() and path.read_text() == text
            differs |= not same
            print(f"{path}: {'up to date' if same else 'differs'}")
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
            print(f"{path}: {len(ws)} words")
    return 1 if differs else 0


if __name__ == "__main__":
    sys.exit(main())
