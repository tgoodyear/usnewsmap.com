"""Score the production re-read against LoC on the held-out lines checked on check.usnewsmap.com (#1620, #1621).

Reads the check site's export of the `chr-lines` language (`GET /api/export?lang=chr-lines`, CC0) and the manifest of the
lines it was loaded with (`check-lines-manifest.jsonl`, next to the report). Each loaded line has exactly one word where
`chr_phoenix`'s production reading and LoC's differ, so a line is fully resolved once that word has settled on one of the
two readings. The checked text of a resolved line is LoC's words with that word replaced by the settled reading.

Reports, as JSON on standard output:

- `progress`: resolved lines against `--target`, lines still waiting for answers or settled without a winner ("neither",
  "can't tell", "not text" or "disputed"), answers so far and per resolved line, and how many of the loaded lines are
  expected to resolve if the share resolved so far holds.
- `words`: how the settled words went: the re-read right, LoC right, neither, can't tell, not text, disputed.
- `score`: syllabary-letter error of each reading against the checked text, pooled over the letters of the resolved lines,
  and the paired difference (re-read minus LoC), each with a 95% interval from a page bootstrap (pages drawn with
  replacement). Also the same figures with the lines whose only problem is a "neither" added, each such word counted as
  one letter wrong for both readings, as a bound on what leaving them out hides.
- `by_kind`: the same counts for news lines and lines that reprint the New Testament.

Only lines where the readings differ are loaded. Each reading can differ from the checked text only in the disputed word, and
errors the readings share elsewhere on the page are not seen. The figures are not each reading's error on the page.
The difference between them is what the disputes decide.

  python3 chr_disputes_score.py --export check-chr-lines.jsonl --manifest check-lines-manifest.jsonl [--target 300]
"""
import argparse
import collections
import json
import math
import random
import re

SY = re.compile("[Ꭰ-Ᏽ]")
SETTLED_OTHER = ("neither", "unsure", "notext", "disputed")


def letters(s: str) -> str:
    return "".join(SY.findall(s))


def edit_distance(x: str, y: str) -> int:
    prev = list(range(len(y) + 1))
    for i, cx in enumerate(x, 1):
        cur = [i]
        for j, cy in enumerate(y, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (cx != cy)))
        prev = cur
    return prev[-1]


def task_key(task_id: str) -> tuple:
    """`chr-lines/disputes-v1/<stem>~<k>` -> (stem, k)."""
    stem, _, k = task_id.split("/")[-1].rpartition("~")
    return stem, int(k)


def line_states(manifest: list, export: list) -> list:
    """One record per manifest line: its state (`resolved`, `open` when a word settled without a winner,
    `pending` while a word waits for answers or has none) and, when resolved, the checked words and each reading's errors."""
    tasks = {task_key(r["id"]): r for r in export}
    out = []
    for m in manifest:
        rows = [tasks.get((m["id"], k)) for k in m["words"]]
        answers = sum(sum(r["counts"].values()) for r in rows if r)
        rec = {"id": m["id"], "page": m["page"], "kind": m["kind"], "answers": answers, "words": []}
        if any(r is None or not r["settled"] for r in rows):
            rec["state"] = "pending"
        elif any(r["outcome"] in SETTLED_OTHER for r in rows):
            rec["state"] = "open"
            rec["why"] = [r["outcome"] for r in rows if r["outcome"] in SETTLED_OTHER]
            rec["letters"] = len("".join(m["ours"]))
        else:
            rec["state"] = "resolved"
            checked = list(m["loc"])
            for k, r in zip(m["words"], rows):
                checked[k] = letters(r["a"] if r["outcome"] == "a" else r["b"])
                rec["words"].append({"winner": r["a_is"] if r["outcome"] == "a" else r["b_is"]})
            text = "".join(checked)
            rec["letters"] = len(text)
            rec["errors"] = {"chr_phoenix": edit_distance("".join(m["ours"]), text), "loc": edit_distance("".join(m["loc"]), text)}
        out.append(rec)
    return out


def score(recs: list, neither_as_error: bool = False) -> dict:
    """Pooled error of each reading over the lines (resolved ones, and with `neither_as_error` the open ones
    whose only problem is a "neither": each such word counts one letter wrong for both readings)."""
    e = collections.Counter()
    for r in recs:
        if r["state"] == "resolved":
            e["letters"] += r["letters"]
            e["chr_phoenix"] += r["errors"]["chr_phoenix"]
            e["loc"] += r["errors"]["loc"]
            e["lines"] += 1
        elif neither_as_error and r["state"] == "open" and set(r["why"]) == {"neither"}:
            e["letters"] += r["letters"]
            e["chr_phoenix"] += len(r["why"])
            e["loc"] += len(r["why"])
            e["lines"] += 1
    n = e["letters"]
    ours, loc = (e["chr_phoenix"] / n, e["loc"] / n) if n else (None, None)
    return {"lines": e["lines"], "letters": n, "chr_phoenix": ours, "loc": loc, "difference": None if n == 0 else ours - loc}


def percentile(values: list, q: float):
    if not values:
        return None
    values = sorted(values)
    k = (len(values) - 1) * q
    lo, hi = math.floor(k), math.ceil(k)
    return values[lo] + (values[hi] - values[lo]) * (k - lo)


def bootstrap(recs: list, draws: int, seed: int, neither_as_error: bool = False) -> dict:
    """95% intervals of the scores, drawing whole pages with replacement. Pages without a counted line
    stay in the draw as empty, so the page count that a draw sees varies as it does in the data."""
    by_page = collections.defaultdict(list)
    for r in recs:
        by_page[r["page"]].append(r)
    pages = sorted(by_page)
    rng = random.Random(seed)
    stats = {"chr_phoenix": [], "loc": [], "difference": []}
    for _ in range(draws):
        sample = [r for p in (rng.choice(pages) for _ in pages) for r in by_page[p]]
        s = score(sample, neither_as_error)
        if s["letters"]:
            for k in stats:
                stats[k].append(s[k])
    return {k: [percentile(v, 0.025), percentile(v, 0.975)] if v else None for k, v in stats.items()}


def report(manifest: list, export: list, target: int, draws: int, seed: int) -> dict:
    recs = line_states(manifest, export)
    states = collections.Counter(r["state"] for r in recs)
    answers = sum(r["answers"] for r in recs)
    resolved = states["resolved"]
    settled_lines = resolved + states["open"]
    yield_ = resolved / settled_lines if settled_lines else None
    expected = resolved + states["pending"] * yield_ if yield_ is not None else None
    per_line = answers / resolved if resolved else None
    out = {
        "progress": {
            "target_lines": target, "resolved_lines": resolved, "still_needed": max(0, target - resolved),
            "share_of_target": round(resolved / target, 3), "lines_loaded": len(recs),
            "lines_open": states["open"], "lines_pending": states["pending"], "answers": answers,
            "answers_per_resolved_line": None if per_line is None else round(per_line, 2),
            "resolved_share_of_settled_lines": None if yield_ is None else round(yield_, 3),
            "expected_resolved_if_all_loaded_lines_settle": None if expected is None else round(expected),
            "loaded_lines_enough": None if expected is None else expected >= target,
            "answers_still_needed": None if per_line is None else math.ceil(max(0, target - resolved) * per_line),
        },
    }
    win = collections.Counter(w["winner"] for r in recs if r["state"] == "resolved" for w in r["words"])
    other = collections.Counter(w for r in recs if r["state"] == "open" for w in r["why"])
    out["words"] = {"chr_phoenix_right": win["chr_phoenix"], "loc_right": win["loc"], **{k: other[k] for k in SETTLED_OTHER}}
    out["score"] = {"resolved": {**score(recs), "interval_95": bootstrap(recs, draws, seed)},
                    "with_neither_lines": {**score(recs, True), "interval_95": bootstrap(recs, draws, seed, True)}}
    out["by_kind"] = {}
    for kind in sorted({r["kind"] for r in recs}):
        sub = [r for r in recs if r["kind"] == kind]
        c = collections.Counter(r["state"] for r in sub)
        out["by_kind"][kind] = {"loaded": len(sub), "resolved": c["resolved"], "open": c["open"], "pending": c["pending"], **{
            k: v for k, v in score(sub).items() if k in ("chr_phoenix", "loc", "difference")}}
    pages = {r["page"] for r in recs if r["state"] == "resolved"}
    out["pages_with_resolved_lines"] = len(pages)
    out["summary"] = (f"{resolved} of {target} held-out lines resolved ({100 * resolved / target:.0f}%) from {answers} answers; "
                      f"{states['open']} lines settled without a winner, {states['pending']} still waiting.")
    return out


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--export", required=True, help="the chr-lines export, JSON lines")
    ap.add_argument("--manifest", required=True, help="check-lines-manifest.jsonl")
    ap.add_argument("--target", type=int, default=300, help="fully resolved held-out lines to reach")
    ap.add_argument("--draws", type=int, default=10000, help="page bootstrap draws")
    ap.add_argument("--seed", type=int, default=1)
    a = ap.parse_args()
    with open(a.manifest, encoding="utf-8") as f:
        manifest = [json.loads(x) for x in f if x.strip()]
    with open(a.export, encoding="utf-8") as f:
        export = [json.loads(x) for x in f if x.strip()]
    print(json.dumps(report(manifest, export, a.target, a.draws, a.seed), ensure_ascii=False, indent=1))


if __name__ == "__main__":
    main()
