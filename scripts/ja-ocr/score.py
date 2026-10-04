#!/usr/bin/env python3
"""Score the OCR engines' output on a sample (#128).

    scripts/ja-ocr/score.py SAMPLE_DIR [--engines ndlocr-lite azure tesseract-jpn_vert]

Needs rapidfuzz (pip install rapidfuzz). Prints markdown:

- Per engine: pages with output, median seconds per page, characters per
  page, and the share that are Japanese script (kana and kanji). A page
  where LoC found no text is Japanese, so a low share means noise.
- Agreement: character error rate between each pair of engines on the
  Japanese characters only (punctuation, spaces and Latin dropped). Two good
  engines agree; this doesn't say which is right.
- Accuracy, when SAMPLE_DIR/refs/<page_id>.txt exist (transcriptions of
  crops; see crops.py): character error rate of each engine against them.
"""

import argparse
import csv
import re
import statistics
from pathlib import Path

from rapidfuzz.distance import Levenshtein

JA = re.compile(r"[぀-ヿ㐀-䶿一-鿿豈-﫿々〆ヶ]")


def ja_only(s: str) -> str:
    return "".join(JA.findall(s))


def cer(hyp: str, ref: str) -> float:
    return Levenshtein.distance(hyp, ref) / max(len(ref), 1)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("sample", type=Path)
    ap.add_argument("--engines", nargs="+", default=["ndlocr-lite", "azure", "tesseract-jpn_vert"])
    a = ap.parse_args()
    rows = list(csv.DictReader((a.sample / "sample.csv").open()))
    engines = [e for e in a.engines if (a.sample / "out" / e).is_dir()]
    text = {e: {} for e in engines}
    secs = {e: {} for e in engines}
    for e in engines:
        d = a.sample / "out" / e
        for r in rows:
            f = d / f"{r['page_id']}.txt"
            if f.exists():
                text[e][r["page_id"]] = f.read_text()
        t = d / "times.csv"
        if t.exists():
            secs[e] = {x["page_id"]: float(x["seconds"]) for x in csv.DictReader(t.open())}

    print(f"## {a.sample.name}: {len(rows)} pages\n")
    print("| engine | pages | s/page (median) | chars/page (median) | Japanese share (median) |")
    print("|---|---|---|---|---|")
    for e in engines:
        t = text[e]
        if not t:
            continue
        chars = [len(re.sub(r"\s", "", s)) for s in t.values()]
        share = [len(ja_only(s)) / max(len(re.sub(r"\s", "", s)), 1) for s in t.values()]
        s = statistics.median(secs[e].values()) if secs[e] else float("nan")
        print(f"| {e} | {len(t)} | {s:.1f} | {statistics.median(chars):.0f} | {statistics.median(share):.0%} |")

    print("\n### Agreement (CER between engines, Japanese characters only; lower is closer)\n")
    print("| pair | pages | median | mean |")
    print("|---|---|---|---|")
    for i, x in enumerate(engines):
        for y in engines[i + 1:]:
            both = [p for p in text[x] if p in text[y]]
            if not both:
                continue
            v = [cer(ja_only(text[x][p]), ja_only(text[y][p])) for p in both]
            print(f"| {x} vs {y} | {len(both)} | {statistics.median(v):.1%} | {statistics.mean(v):.1%} |")

    refs = a.sample / "refs"
    if refs.is_dir():
        print("\n### Accuracy against reference transcriptions (CER, Japanese characters only)\n")
        print("| engine | crops | median | mean | by group |")
        print("|---|---|---|---|---|")
        group = {r["page_id"]: r.get("group", "") for r in rows}
        for e in engines:
            v, by = [], {}
            for f in sorted(refs.glob("*.txt")):
                pid = f.stem
                if pid not in text[e]:
                    continue
                c = cer(ja_only(text[e][pid]), ja_only(f.read_text()))
                v.append(c)
                by.setdefault(group.get(pid, ""), []).append(c)
            if v:
                g = ", ".join(f"{k or '-'} {statistics.mean(x):.1%} ({len(x)})" for k, x in sorted(by.items()))
                print(f"| {e} | {len(v)} | {statistics.median(v):.1%} | {statistics.mean(v):.1%} | {g} |")


if __name__ == "__main__":
    main()
