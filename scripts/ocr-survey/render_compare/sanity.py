#!/usr/bin/env python3
"""Step 5 of #2103: a synthetic sanity check. SANITY CHECK ONLY: it is no evidence that the distances separate real errors.

    sanity.py --words words.jsonl --alto-dir LOC --out sanity.json [--rows-out swaps.jsonl] [--seed 2103] [--limit N]
              [--min-overlap 0.6] [--font latin=PATH]

Take the words LoC read correctly (label 1), swap one letter of each for a look-alike, render the swapped text
against the same scan, and check the distances rise. The look-alike pairs are the Latin lower-case pairs that
`edge/check/tools/lookalike.py` measures as drawing alike in the rendering font (`lookalike.measure`: intersection
over union of the two glyphs' ink, the best of a one-pixel shift), kept when at least --min-overlap (n/u, h/n, i/l,
b/h, c/e, e/o ... in Old Standard TT). A word of three or more letters with a letter in a pair is swapped at one
such letter, chosen by --seed.

The report gives, for each distance, the mean for the original and the swapped text, the mean paired rise, the
share of words whose distance rose, a Wilcoxon signed-rank p value (10 or more words), and the AUC of telling the
swapped from the original rendering by that distance; and the same split by pair overlap (above and below 0.7).
A rise is expected, since the original is the true text; the sizes say how much of the swap the distances see.
Real-word errors the engine made are not these swaps: they come with the full run.
"""

from __future__ import annotations

import argparse
import itertools
import json
import random
import string
import sys
from pathlib import Path

import numpy as np

import paths  # noqa: F401
import compare
import distances
import fonts
import gold
import lookalike
import render

METRICS = ["chamfer", "pixel", "hog_cos"]
TIER = 0.7


def lookalike_pairs(font_path: str | Path, min_overlap: float = 0.6) -> list[tuple[str, str, float]]:
    """Lower-case Latin letter pairs that `lookalike.measure` finds drawn alike in this font, most alike first."""
    f = lookalike.Fonts({"LATIN": str(font_path)})
    out = []
    for a, b in itertools.combinations(string.ascii_lowercase, 2):
        v = lookalike.measure(f, a, b)
        if v is not None and v >= min_overlap:
            out.append((a, b, float(v)))
    return sorted(out, key=lambda t: -t[2])


def swap_word(word: str, pairs: list[tuple[str, str, float]], rng: random.Random):
    """(swapped word, (position, letter, look-alike, overlap)) or None when the word can't be swapped: it needs
    three or more letters, all lower-case Latin, and one of them in a pair."""
    if len(word) < 3 or not all(c in string.ascii_lowercase for c in word):
        return None
    options = []
    for i, c in enumerate(word):
        for a, b, ov in pairs:
            if c == a:
                options.append((i, c, b, ov))
            elif c == b:
                options.append((i, c, a, ov))
    if not options:
        return None
    i, c, other, ov = rng.choice(options)
    return word[:i] + other + word[i + 1:], (i, c, other, ov)


def summarise(orig: dict[str, list], swapped: dict[str, list]) -> dict:
    out = {}
    for m in METRICS:
        o, s = np.array(orig[m]), np.array(swapped[m])
        if len(o) == 0:
            continue
        d = s - o
        y = np.concatenate([np.zeros(len(o), int), np.ones(len(s), int)])
        entry = {"mean_original": float(o.mean()), "mean_swapped": float(s.mean()), "mean_rise": float(d.mean()),
                 "share_rose": float((d > 0).mean()), "auc_swapped_vs_original": compare.auc(y, np.concatenate([o, s]))}
        if len(o) >= 10 and np.any(d != 0):
            from scipy.stats import wilcoxon

            entry["wilcoxon_p_rise"] = float(wilcoxon(d, alternative="greater").pvalue)
        out[m] = entry
    return out


def run(rows: list[dict], alto_dir: Path, font: fonts.FontInfo, pairs, seed: int = 2103, limit: int | None = None) -> tuple[dict, list[dict]]:
    from PIL import Image

    Image.MAX_IMAGE_PIXELS = None
    rng = random.Random(seed)
    right = sorted((r for r in rows if r["label"] == 1 and r["lang"] == "eng"), key=lambda r: (r["page_id"], r["crop_id"], r["word_index"]))
    pages: dict[str, object] = {}
    out_rows, skipped = [], 0
    for r in right:
        if limit and len(out_rows) >= limit:
            break
        sw = swap_word(r["ocr_word"], pairs, rng)
        if sw is None or not render.has_glyphs(sw[0], font.path):
            skipped += 1
            continue
        stem = r["source_image"].rsplit(".", 1)[0]
        if stem not in pages:
            pages = {stem: Image.open(alto_dir / f"{stem}.jpg").convert("L")}
        a = distances.word_distances(r["ocr_word"], pages[stem], r["box"], font.path)
        b = distances.word_distances(sw[0], pages[stem], r["box"], font.path)
        if a["status"] != "ok" or b["status"] != "ok":
            skipped += 1
            continue
        out_rows.append({"page_id": r["page_id"], "crop_id": r["crop_id"], "word_index": r["word_index"], "word": r["ocr_word"],
                         "swapped": sw[0], "position": sw[1][0], "letter": sw[1][1], "lookalike": sw[1][2],
                         "overlap": sw[1][3], **{f"{m}_original": a[m] for m in METRICS}, **{f"{m}_swapped": b[m] for m in METRICS}})

    def collect(sel):
        return ({m: [x[f"{m}_original"] for x in sel] for m in METRICS}, {m: [x[f"{m}_swapped"] for x in sel] for m in METRICS})

    report = {"sanity_check_only": True, "seed": seed, "font": font.record(), "pairs": len(pairs),
              "swapped_words": len(out_rows), "skipped_words": skipped, "all": summarise(*collect(out_rows)),
              "overlap_ge_%.1f" % TIER: summarise(*collect([x for x in out_rows if x["overlap"] >= TIER])),
              "overlap_lt_%.1f" % TIER: summarise(*collect([x for x in out_rows if x["overlap"] < TIER]))}
    return report, out_rows


def format_report(rep: dict) -> str:
    lines = [f"SANITY CHECK ONLY. {rep['swapped_words']} correct words with one look-alike letter swapped "
             f"({rep['skipped_words']} skipped; {rep['pairs']} pairs, font {rep['font']['font_family']})"]
    for split in ("all", "overlap_ge_%.1f" % TIER, "overlap_lt_%.1f" % TIER):
        for m, e in rep[split].items():
            p = f", p={e['wilcoxon_p_rise']:.3g}" if "wilcoxon_p_rise" in e else ""
            auc = e["auc_swapped_vs_original"]
            lines.append(f"  {split:<14} {m:<8} {e['mean_original']:.4f} -> {e['mean_swapped']:.4f} "
                         f"(rise {e['mean_rise']:+.4f}, {e['share_rose']:.0%} rose, AUC {auc:.3f}{p})")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--words", type=Path, required=True, help="loader.py output")
    ap.add_argument("--alto-dir", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True, help="the report (JSON)")
    ap.add_argument("--rows-out", type=Path, help="one row per swapped word (JSON lines)")
    ap.add_argument("--seed", type=int, default=2103)
    ap.add_argument("--limit", type=int)
    ap.add_argument("--min-overlap", type=float, default=0.6)
    ap.add_argument("--font", action="append", metavar="SCRIPT=PATH")
    ap.add_argument("--font-cache")
    a = ap.parse_args(argv)
    fs = fonts.resolve("latin", fonts.parse_overrides(a.font), a.font_cache)
    if not fs:
        print(f"latin: {fonts.unavailable_reason('latin')}", file=sys.stderr)
        return 1
    pairs = lookalike_pairs(fs[0].path, a.min_overlap)
    rep, rows = run(gold.read_jsonl(a.words), a.alto_dir, fs[0], pairs, a.seed, a.limit)
    rep["pair_list"] = [[x, y, round(v, 3)] for x, y, v in pairs]
    a.out.parent.mkdir(parents=True, exist_ok=True)
    a.out.write_text(json.dumps(rep, indent=1, sort_keys=True) + "\n", encoding="utf-8")
    if a.rows_out:
        gold.write_jsonl(a.rows_out, rows)
    print(format_report(rep))
    return 0


if __name__ == "__main__":
    sys.exit(main())
