"""Compare how people answered words that differ only in a combining mark with words that don't (#1939).

Two readings that differ only in a vowel point or a dagesh on the same base letter (`mark_only` in the task data,
lookalike.py) are not measured for look-alikes: a mark is a few percent of the ink. Whether people can tell them apart
is a question for the answers. From exports (`GET /api/export?lang=...`) this reports, per language, per mark and per
font group, for the mark-only words and for the words flagged by neither the mark rule nor the overlap measure:

- the can't-tell rate: answers `unsure` over all answers;
- the pairwise agreement: over every pair of answers to the same word, how many chose the same thing.

Only `counts` are used (answers set aside for a `confusable_display` word are not counted). A word with several marks
that differ counts under each of them. A word that has both a mark-only pair and an overlap-flagged pair is left out of
both groups. A row with fewer than MIN_ANSWERS answers prints "too few answers" and no rates.

Font groups: `font_counts` says what the visitor's page reported about the bundled font, per word. Its values are
counted per word, not per choice, so a word goes in the group `loaded`, `not_loaded`, `none` or `unknown` when all its
answers carry that value, `mixed` when they carry several, and `unknown` when the export has no `font_counts`.

  python3 mark_report.py --export check-yi.jsonl [--export ...] [--font SCRIPT=PATH ...] [--min-answers 20] [--json]
"""
import argparse
import collections
import json
import os
import sys
import unicodedata

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lookalike as la  # noqa: E402

MIN_ANSWERS = 20
CHOICES = ("a", "b", "neither", "unsure", "notext")


def font_group(task: dict) -> str:
    fc = task.get("font_counts")
    if not fc:
        return "unknown"
    used = [k for k, n in fc.items() if n]
    return used[0] if len(used) == 1 else ("mixed" if used else "unknown")


def pairs_of(n: int) -> int:
    return n * (n - 1) // 2


class Tally:
    def __init__(self):
        self.words = 0
        self.answers = 0
        self.unsure = 0
        self.agree = 0
        self.pairs = 0

    def add(self, counts: dict) -> None:
        n = sum(counts.get(c, 0) for c in CHOICES)
        self.words += 1
        self.answers += n
        self.unsure += counts.get("unsure", 0)
        self.pairs += pairs_of(n)
        self.agree += sum(pairs_of(counts.get(c, 0)) for c in CHOICES)

    def row(self, min_answers: int) -> dict:
        out = {"words": self.words, "answers": self.answers}
        if self.answers < min_answers:
            out["note"] = "too few answers"
            return out
        out.update(unsure=self.unsure, cant_tell_rate=round(self.unsure / self.answers, 3), agree_pairs=self.agree, pairs=self.pairs,
                   agreement=round(self.agree / self.pairs, 3) if self.pairs else None)
        return out


def mark_label(ch: str) -> str:
    name = unicodedata.name(ch, "").lower().removeprefix("hebrew point ").removeprefix("hebrew ")
    return f"U+{ord(ch):04X} {name}".strip()


def report(tasks: list, fonts, languages_dir: str, threshold: float, min_answers: int) -> dict:
    """tasks: export lines. Returns {lang: {"marks": {mark: {font group or "all": row}}, "baseline": {...}}}."""
    cache, tallies = {}, {}
    for t in tasks:
        lang = t["id"].split("/")[0]
        eq = cache.setdefault(lang, la.language_equivalents(lang, languages_dir))
        alike, marked = la.analyse(t["a"], t["b"], eq, fonts, threshold)
        counts = t.get("counts") or {}
        if marked and not alike:
            groups = sorted({m for pair in marked for m in la.differing_marks(pair)})
            keys = [mark_label(m) for m in groups] or ["(same marks, other order)"]
        elif not marked and not alike:
            keys = ["unflagged"]
        else:
            continue
        for key in keys:
            for g in ("all", font_group(t)):
                tallies.setdefault(lang, {}).setdefault(key, {}).setdefault(g, Tally()).add(counts)
    out = {}
    for lang, by in sorted(tallies.items()):
        rows = {k: {g: tl.row(min_answers) for g, tl in sorted(gs.items(), key=lambda x: (x[0] != "all", x[0]))} for k, gs in sorted(by.items())}
        out[lang] = {"baseline": rows.pop("unflagged", {}), "marks": rows}
    return out


def show(rep: dict, min_answers: int) -> None:
    def line(name, g, r):
        if "note" in r:
            return f"  {name:<34} {g:<10} {r['words']:>4} words {r['answers']:>5} answers  too few answers (under {min_answers})"
        ag = "none" if r["agreement"] is None else f"{r['agree_pairs']} of {r['pairs']} ({100 * r['agreement']:.0f}%)"
        return (f"  {name:<34} {g:<10} {r['words']:>4} words {r['answers']:>5} answers  can't tell {r['unsure']} "
                f"({100 * r['cant_tell_rate']:.0f}%)  agreement {ag}")
    for lang, r in rep.items():
        print(f"{lang}: words flagged by neither the mark rule nor the overlap measure, then mark-only words per mark")
        for g, row in r["baseline"].items():
            print(line("unflagged", g, row))
        for mark, gs in r["marks"].items():
            for g, row in gs.items():
                print(line(mark, g, row))


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--export", action="append", required=True, help="an export (`/api/export?lang=...`); repeatable")
    ap.add_argument("--font", action="append", help="SCRIPT=PATH: the reference font for the overlap check; repeatable")
    ap.add_argument("--threshold", type=float, default=la.THRESHOLD)
    ap.add_argument("--min-answers", type=int, default=MIN_ANSWERS, help="rows with fewer answers print 'too few answers'")
    ap.add_argument("--languages-dir", default=os.path.join(la.HERE, "..", "languages"))
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args()
    tasks = []
    for path in a.export:
        with open(path, encoding="utf-8") as f:
            tasks.extend(json.loads(raw) for raw in f if raw.strip())
    rep = report(tasks, la.Fonts(la.parse_fonts(a.font)), a.languages_dir, a.threshold, a.min_answers)
    if a.json:
        print(json.dumps(rep, ensure_ascii=False, indent=1, sort_keys=True))
    else:
        show(rep, a.min_answers)


if __name__ == "__main__":
    main()
