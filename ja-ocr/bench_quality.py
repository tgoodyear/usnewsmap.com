"""Time quality.py's per-page scoring (score_v2) on synthetic pages, to compare changes to it.

    python3 bench_quality.py [--pages 300] [--words 4000] [--profile]

Pages are drawn from wordfreq's English and German top 3,000 words, half from the top 200, with 8% of
words given an OCR-like slip ("h" read as "b", or a stray "i"); a tenth are mixed German and English.
Real pages repeat fewer tokens than these, so the token cache helps them less.
"""

import argparse
import cProfile
import pstats
import random
import time

import quality


def pages(n: int, words: int, seed: int = 1) -> list[tuple[str, tuple]]:
    from wordfreq import top_n_list

    rng = random.Random(seed)
    en, de = top_n_list("en", 3000), top_n_list("de", 3000)

    def page(vocab: list[str]) -> str:
        out = []
        for _ in range(words):
            w = rng.choice(vocab[:200] if rng.random() < 0.5 else vocab)
            if rng.random() < 0.08:
                w = w.replace("h", "b", 1) if "h" in w else w + "i"
            out.append(w)
        return " ".join(out)

    kinds = [(en, ("eng",))] * 6 + [(de, ("ger", "eng"))] * 3 + [(en + de, ("ger", "eng"))]
    return [(page(v), langs) for v, langs in (kinds[i % len(kinds)] for i in range(n))]


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--pages", type=int, default=300)
    ap.add_argument("--words", type=int, default=4000)
    ap.add_argument("--profile", action="store_true", help="print the 20 costliest calls")
    a = ap.parse_args()
    models = quality.Models(None)
    ps = pages(a.pages, a.words)
    quality.score_v2(*ps[0], models)  # loads the word lists
    t = time.perf_counter()
    for text, langs in ps:
        quality.score_v2(text, langs, models)
    print(f"{(time.perf_counter() - t) / len(ps) * 1000:.2f} ms per page ({len(ps)} pages of {a.words} words)")
    if a.profile:
        prof = cProfile.Profile()
        prof.runcall(lambda: [quality.score_v2(text, langs, models) for text, langs in ps])
        pstats.Stats(prof).sort_stats("cumtime").print_stats(20)


if __name__ == "__main__":
    main()
