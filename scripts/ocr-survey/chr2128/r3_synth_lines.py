#!/usr/bin/env python3
"""R3's synthetic lines (#2128): round two's `chr_synth_lines.py` (ocr/models/chr-phoenix/recipe), with
  - the text from `r3_corpus.py` (the NT without the excluded verses, plus ChrEn);
  - line-end augmentation: each line's left and right margins are drawn at random, from --margin-min to
    --margin-max body heights (round two always left one body height), and a glyph sliver may be pasted
    into a margin (--edge-rate, as round two) where that margin is at least --sliver-room body heights wide,
    so a sliver never touches the text;
  - a last check: a line whose text holds a run of excluded-verse letters (`exclusion.py`; possible only
    where the word stream crosses from one corpus unit to the next) is skipped and counted, and the script
    asserts on its own output that no written line holds one.

Rendering, degradation, marks and the glyph bank are round two's functions, called unchanged.

    r3_synth_lines.py --bank BANK --text corpus.txt --exclude-manifest exclude.json --nt-text cnts.raw.txt
                      --exclude-verses verses-any-aligned.txt --out DIR --lines 20000 --seed 2128
                      [--marks-share 0.4 --punct-rate 0.15 --quote-rate 0.08 --edge-rate 0.3]
                      [--margin-min 0.05 --margin-max 1.5 --sliver-room 0.6]
"""
import argparse
import collections
import json
import os
import random
import sys

import numpy as np

import common
import exclusion as ex

sys.path.insert(0, str(common.REPO / "ocr" / "models" / "chr-phoenix" / "recipe"))
import chr_synth_lines as cs  # noqa: E402


def random_margins(arr, body, rng, lo, hi):
    """The line with its ink kept and new left and right margins; returns (array, left, right) in pixels."""
    cols = np.nonzero((arr < 128).any(axis=0))[0]
    if len(cols) == 0:
        return arr, body, body
    x0, x1 = cols[0], cols[-1] + 1
    left, right = int(rng.uniform(lo, hi) * body), int(rng.uniform(lo, hi) * body)
    out = np.full((arr.shape[0], left + (x1 - x0) + right), 255.0, np.float32)
    out[:, left:left + (x1 - x0)] = arr[:, x0:x1]
    return out, left, right


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bank", required=True)
    ap.add_argument("--text", required=True)
    ap.add_argument("--exclude-manifest", required=True)
    ap.add_argument("--nt-text", required=True)
    ap.add_argument("--exclude-verses", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--lines", type=int, default=20000)
    ap.add_argument("--seed", type=int, default=2128)
    ap.add_argument("--body", type=int, default=36)
    ap.add_argument("--marks-share", type=float, default=0.4)
    ap.add_argument("--punct-rate", type=float, default=0.15)
    ap.add_argument("--quote-rate", type=float, default=0.08)
    ap.add_argument("--edge-rate", type=float, default=0.3)
    ap.add_argument("--margin-min", type=float, default=0.05)
    ap.add_argument("--margin-max", type=float, default=1.5)
    ap.add_argument("--sliver-room", type=float, default=0.6)
    ap.add_argument("--max-misses", type=int, default=100000)
    a = ap.parse_args(argv)
    E = ex.Exclusion(a.nt_text, a.exclude_verses)
    rng = random.Random(a.seed)
    exclude = cs.excluded_pages(a.exclude_manifest)
    pools = cs.load_bank(a.bank, exclude)
    cs.refuse_used_out(a.out)
    os.makedirs(a.out, exist_ok=True)
    missing, cache = collections.Counter(), {}
    made = misses = skipped_excluded = slivers = 0
    with open(os.path.join(a.out, "lines.jsonl"), "w", encoding="utf-8") as idx:
        for words in cs.sentences(a.text, rng):
            if made >= a.lines:
                break
            if E.text_excluded(" ".join(words)):
                skipped_excluded += 1
                continue
            words = cs.add_marks(words, rng, a.marks_share, a.punct_rate, a.quote_rate)
            arr, miss = cs.render(words, pools, rng, a.body + rng.randint(-4, 4), cache)
            if arr is None:
                missing[miss] += 1
                misses += 1
                if misses >= a.max_misses:
                    print(f"gave up after {misses} unrenderable lines in a row", file=sys.stderr)
                    break
                continue
            misses = 0
            body = int(round(arr.shape[0] / 1.9))
            arr, left, right = random_margins(arr, body, rng, a.margin_min, a.margin_max)
            for is_right, room in ((False, left), (True, right)):
                if rng.random() < a.edge_rate and room >= a.sliver_room * body:
                    cs.edge_sliver(arr, pools, rng, body, cache, is_right)
                    slivers += 1
            name = f"chr-synth-{a.seed}-{made:06d}"
            cs.degrade(arr, rng).save(os.path.join(a.out, name + ".png"))
            text = " ".join(words)
            open(os.path.join(a.out, name + ".gt.txt"), "w", encoding="utf-8").write(text + "\n")
            idx.write(json.dumps({"name": name, "text": text, "margins": [left, right]}, ensure_ascii=False) + "\n")
            made += 1
    for line in open(os.path.join(a.out, "lines.jsonl"), encoding="utf-8"):
        assert not E.text_excluded(json.loads(line)["text"]), line
    summary = {"lines": made, "excluded_pages": len(exclude), "letters_with_samples": len(pools),
               "has_nah_samples": bool(pools.get("Ꮐ")), "slivers": slivers,
               "skipped_for_excluded_text": skipped_excluded, "skipped_for_missing_letter": dict(missing.most_common()),
               "asserted_no_excluded_text": True, "seed": a.seed}
    json.dump(summary, open(os.path.join(a.out, "summary.json"), "w"), ensure_ascii=False, indent=1)
    print(json.dumps(summary, ensure_ascii=False))
    return 0 if made >= a.lines else 1


if __name__ == "__main__":
    sys.exit(main())
