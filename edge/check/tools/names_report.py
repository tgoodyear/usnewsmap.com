"""Compare how people answered with letter names showing and without (the "Show letter names" option).

Each answer records whether the page was showing letter names (`names_shown`: shown, not_shown, unknown). The export gives,
per word, `names_counts` with the answers counted by that value. From exports (`GET /api/export?lang=...`) this reports,
per language and per names group, for all words and for the words with look-alike pairs (`lookalikes`) and with mark-only
pairs (`mark_only`):

- the can't-tell rate: answers `unsure` over all answers;
- the pairwise agreement: over every pair of answers to the same word, how many chose the same thing;
- the agreement with the reference, for words whose export says which reading is the reference (`a_is` or `b_is` is
  "reference"): of the answers that picked a reading (a or b), the share that picked the reference one.

Names groups are per word: `shown` when all its answers carry `shown`, `not_shown` when all carry `not_shown`, `unknown` when
all are unknown (or the export has no `names_counts`: an export from before the option), and `mixed` otherwise. A word counts
in "all", in "lookalikes" if it has `lookalikes`, and in "mark_only" if it has `mark_only`. Only `counts` are used: answers set
aside for a `confusable_display` word are not counted. A row with fewer than MIN_ANSWERS answers prints "too few answers".

Words are not assigned to a group: visitors turn the option on themselves, so a difference between groups may come from who
turns it on, as well as from the names.

  python3 names_report.py --export check-chr.jsonl [--export ...] [--min-answers 20] [--json]
"""
import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mark_report import MIN_ANSWERS, Tally  # noqa: E402

GROUPS = ("shown", "not_shown", "mixed", "unknown")
SUBSETS = ("all", "lookalikes", "mark_only")


def names_group(task: dict) -> str:
    nc = task.get("names_counts")
    used = [k for k, n in (nc or {}).items() if n]
    if len(used) == 1:
        return used[0] if used[0] in GROUPS else "unknown"
    return "mixed" if used else "unknown"


def reference_side(task: dict):
    for side in ("a", "b"):
        if task.get(side + "_is") == "reference":
            return side
    return None


class RefTally(Tally):
    def __init__(self):
        super().__init__()
        self.picks = 0
        self.ref = 0

    def add_task(self, task: dict) -> None:
        counts = task.get("counts") or {}
        self.add(counts)
        side = reference_side(task)
        if side:
            self.picks += counts.get("a", 0) + counts.get("b", 0)
            self.ref += counts.get(side, 0)

    def row(self, min_answers: int) -> dict:
        out = super().row(min_answers)
        if "note" not in out:
            out.update(reference_picks=self.ref, reference_answers=self.picks,
                       reference_agreement=round(self.ref / self.picks, 3) if self.picks else None)
        return out


def report(tasks: list, min_answers: int) -> dict:
    """Returns {lang: {subset: {names group: row}}}."""
    tallies = {}
    for t in tasks:
        lang = t["id"].split("/")[0]
        g = names_group(t)
        subs = ["all"] + (["lookalikes"] if t.get("lookalikes") else []) + (["mark_only"] if t.get("mark_only") else [])
        for sub in subs:
            tallies.setdefault(lang, {}).setdefault(sub, {}).setdefault(g, RefTally()).add_task(t)
    return {lang: {sub: {g: subs[sub][g].row(min_answers) for g in GROUPS if g in subs[sub]} for sub in SUBSETS if sub in subs}
            for lang, subs in sorted(tallies.items())}


def show(rep: dict, min_answers: int) -> None:
    for lang, subs in rep.items():
        print(f"{lang}: words by whether letter names were showing when they were answered")
        for sub, groups in subs.items():
            for g, r in groups.items():
                head = f"  {sub:<10} {g:<9} {r['words']:>4} words {r['answers']:>5} answers  "
                if "note" in r:
                    print(head + f"too few answers (under {min_answers})")
                    continue
                ag = "none" if r["agreement"] is None else f"{r['agree_pairs']} of {r['pairs']} ({100 * r['agreement']:.0f}%)"
                ref = "no reference" if r["reference_agreement"] is None else f"{r['reference_picks']} of {r['reference_answers']} ({100 * r['reference_agreement']:.0f}%)"
                print(head + f"can't tell {r['unsure']} ({100 * r['cant_tell_rate']:.0f}%)  agreement {ag}  picked the reference {ref}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--export", action="append", required=True, help="an export (`/api/export?lang=...`); repeatable")
    ap.add_argument("--min-answers", type=int, default=MIN_ANSWERS, help="rows with fewer answers print 'too few answers'")
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args()
    tasks = []
    for path in a.export:
        with open(path, encoding="utf-8") as f:
            tasks.extend(json.loads(raw) for raw in f if raw.strip())
    rep = report(tasks, a.min_answers)
    if a.json:
        print(json.dumps(rep, ensure_ascii=False, indent=1, sort_keys=True))
    else:
        show(rep, a.min_answers)


if __name__ == "__main__":
    main()
