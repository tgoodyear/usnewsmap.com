"""What the per-word script route did to readings of test set v2 lines (#1666).

Reads one or more prediction files (JSON lines with `crop` and `text`, and, from
`usnm-boinc line-crops`, `fallback`) and reports for each:

- the Latin words it wrote: words with a Latin letter and no syllabary letter. Every test-set
  line is New Testament syllabary, so on these lines each one is wrong;
- the digit-only words it wrote (verse numbers are in the references' lines, but the scores
  count syllabary letters only);
- the share of lines that would keep LoC's text (`fallback` set), by reason;
- the syllabary-letter error rate on the silver tier, on all lines and on the checked lines,
  as `cherokee_testset.py score` counts it.

Run from the repository root with named arguments; it prints JSON.

    python3 docs/reports/2026-11-cherokee/scripts/chr_word_route_score.py \
      --lines $D/testset-v2/crops/lines.jsonl --checked checked.json \
      --pred r2=$D/readings/phoenix-round2-psm13.jsonl --pred routed=routed.jsonl

`checked.json` is `chr_checked_score.py --out`'s list of the 70 fully checked lines.
"""
import argparse
import collections
import importlib.util
import json
import os
import re

ROOT = os.path.dirname(os.path.abspath(__file__))
SPEC = importlib.util.spec_from_file_location(
    "cherokee_testset", os.path.join(ROOT, "..", "..", "..", "..", "scripts", "ocr-survey", "cherokee_testset.py")
)
T = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(T)

LATIN_LETTER = re.compile("[A-Za-z]")


def is_latin_word(word):
    return bool(LATIN_LETTER.search(word)) and not T.SYLLABARY.search(word)


def is_digit_word(word):
    stripped = word.strip(".,;:!?()[]\"'")
    return bool(stripped) and stripped.isdigit()


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--lines", required=True, help="the test set's crops/lines.jsonl")
    p.add_argument("--checked", help="chr_checked_score.py --out: the hand-checked lines")
    p.add_argument("--pred", action="append", required=True, help="NAME=PATH, one JSON line per crop")
    a = p.parse_args()
    lines = [json.loads(l) for l in open(a.lines, encoding="utf-8")]
    page_of = {r["crop"]: r["page"] for r in lines}
    sets = {
        "silver": [(r["crop"], r["reference"]) for r in lines if r["tier"] == "silver"],
        "all": [(r["crop"], r["reference"]) for r in lines],
    }
    if a.checked:
        sets["checked"] = [(c["crop"], c["checked"]) for c in json.load(open(a.checked, encoding="utf-8"))]
    out = {}
    for spec in a.pred:
        name, _, path = spec.partition("=")
        rows = [json.loads(l) for l in open(path, encoding="utf-8")]
        preds = {r["crop"]: r["text"] for r in rows}
        res = {"lines": len(preds)}
        words = [w for r in rows for w in r["text"].split()]
        res["words"] = len(words)
        res["latin_words"] = sum(is_latin_word(w) for w in words)
        res["latin_lines"] = sum(any(is_latin_word(w) for w in r["text"].split()) for r in rows)
        res["digit_words"] = sum(is_digit_word(w) for w in words)
        if any("fallback" in r for r in rows):
            fb = collections.Counter(r["fallback"] for r in rows if r.get("fallback"))
            res["fallback_lines"] = sum(fb.values())
            res["fallback_share"] = round(sum(fb.values()) / len(rows), 4)
            res["fallback_by_reason"] = dict(sorted(fb.items()))
        for sname, items in sets.items():
            err = ref = 0
            for crop, gold in items:
                g = "".join(T.SYLLABARY.findall(gold))
                err += T.letter_edit_distance(g, "".join(T.SYLLABARY.findall(preds.get(crop) or "")))
                ref += len(g)
            res[f"cer_{sname}"] = round(err / max(1, ref), 4)
        out[name] = res
    print(json.dumps(out, indent=1, ensure_ascii=False))


if __name__ == "__main__":
    main()
