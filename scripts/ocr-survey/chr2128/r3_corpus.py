#!/usr/bin/env python3
"""R3's synthetic-line text (#2128): the 1860 New Testament without the excluded verses, plus ChrEn's
Cherokee sentences (Zhang, Frey and Bansal, EMNLP 2020; research use), with no excluded verse's text.

Units are NT verses (the words `cherokee_testset.nt_stream` reads, grouped by `nt_verses.py`'s verse ids)
and ChrEn sentences: the Cherokee side of the 2020 parallel splits (train, dev, test, out_dev, out_test)
and the monolingual Cherokee file. Only syllabary words are kept, as `chr_synth_lines.py` reads them.

A unit is dropped when it is an excluded verse, or when it holds a run of 20 syllabary letters found in an
excluded verse (`exclusion.py`). ChrEn sentences are deduplicated by their letters, and so are NT units
already given by ChrEn (so the NT is not counted twice). The units are shuffled (--seed) and written one a
line, so the line writer's word stream mixes the sources. The script then asserts, on the file it wrote,
that no unit is an excluded verse and no line holds an excluded run.

    r3_corpus.py --nt-text cnts.raw.txt --exclude-verses verses-any-aligned.txt --chren-dir DIR
                 --out corpus.txt --summary corpus-summary.json [--seed 2128]
"""
import argparse
import json
import random
from pathlib import Path

import common
import exclusion as ex

CHREN_FILES = ["parallel_train.chr", "parallel_dev.chr", "parallel_test.chr", "parallel_out_dev.chr",
               "parallel_out_test.chr", "monolingual_chr"]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--nt-text", required=True)
    ap.add_argument("--exclude-verses", required=True)
    ap.add_argument("--chren-dir", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--summary", required=True)
    ap.add_argument("--seed", type=int, default=2128)
    a = ap.parse_args()
    E = ex.Exclusion(a.nt_text, a.exclude_verses)
    stats = {"excluded_verses": len(E.ids), "excluded_letters": E.excluded_letters, "run_letters": E.run,
             "nt_units": 0, "nt_dropped_excluded_verse": 0, "nt_dropped_excluded_text": 0,
             "chren_sentences": 0, "chren_dropped_excluded_text": 0, "chren_dropped_duplicate": 0,
             "chren_dropped_no_syllabary": 0, "nt_dropped_already_in_chren": 0, "chren_files": {}}
    seen, chren = set(), []
    for name in CHREN_FILES:
        p = Path(a.chren_dir) / name
        stats["chren_files"][name] = common.sha256_file(p)
        for line in open(p, encoding="utf-8"):
            words = ex.ct.SYLLABARY.findall(line)
            stats["chren_sentences"] += 1
            if not words:
                stats["chren_dropped_no_syllabary"] += 1
                continue
            key = "".join(words)
            if key in seen:
                stats["chren_dropped_duplicate"] += 1
                continue
            seen.add(key)
            if E.text_excluded(key):
                stats["chren_dropped_excluded_text"] += 1
                continue
            chren.append((f"chren:{name}", words))
    nt = []
    for vid, words, _ in ex.nt_verse_words(a.nt_text, E.vm):
        stats["nt_units"] += 1
        if vid in E.ids:
            stats["nt_dropped_excluded_verse"] += 1
            continue
        key = "".join(words)
        if E.text_excluded(key):
            stats["nt_dropped_excluded_text"] += 1
            continue
        if key in seen:
            stats["nt_dropped_already_in_chren"] += 1
            continue
        seen.add(key)
        nt.append((f"nt:{vid}", words))
    units = nt + chren
    random.Random(a.seed).shuffle(units)
    with open(a.out, "w", encoding="utf-8") as f:
        for _, words in units:
            f.write(" ".join(words) + "\n")
    # the asserts, on what was written
    assert not any(src[3:] in E.ids for src, _ in units if src.startswith("nt:")), "an excluded verse is in the corpus"
    for line in open(a.out, encoding="utf-8"):
        assert not E.text_excluded(line), f"excluded text in the corpus: {line[:60]}"
    stats.update({"units_written": len(units), "nt_units_kept": len(nt), "chren_units_kept": len(chren),
                  "nt_words": sum(len(w) for s, w in units if s.startswith("nt:")),
                  "chren_words": sum(len(w) for s, w in units if s.startswith("chren:")),
                  "lines_with_nah": sum("Ꮐ" in "".join(w) for _, w in units),
                  "corpus_sha256": common.sha256_file(a.out), "seed": a.seed, "asserted": True})
    json.dump(stats, open(a.summary, "w"), indent=1, sort_keys=True, ensure_ascii=False)
    print(json.dumps(stats, indent=1, sort_keys=True, ensure_ascii=False))


if __name__ == "__main__":
    main()
