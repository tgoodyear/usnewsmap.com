"""Xiaomi's win rate against LoC from check.usnewsmap.com's settled picks (#1867).

  curl -s 'https://check.usnewsmap.com/api/export?lang=en' > check-en.jsonl
  python3 en_pick_analysis.py --export check-en.jsonl [--set xiaomi-loc-v1] [--calibration-set cal-v1] [--reps 2000] [--seed 1]

Reads /api/export rows (a_is, b_is and truth are there once a task settles). A settled task whose outcome is `a` or `b`
is a win for the reading that side is; `neither`, `unsure`, `notext` and `disputed` are counted and left out of the win rate.
The win rate is Xiaomi wins / (Xiaomi wins + LoC wins), with a page-clustered bootstrap interval (pages are
resampled with replacement, --reps times), overall and by decade and by damage band. With --calibration-set, the
tasks of that set carry a human truth (`xiaomi`, `loc` or `neither`) and the report gives how often the verdict
matches it, and the win rate the truth gives on the same tasks.
"""
import argparse
import json
import random
from collections import Counter, defaultdict


def verdict(row: dict):
    """'xiaomi', 'loc', or another outcome word, for a settled row; None for an unsettled one."""
    if not row.get("settled"):
        return None
    out = row.get("outcome")
    if out in ("a", "b"):
        return row.get("a_is") if out == "a" else row.get("b_is")
    return out


def win_rate(rows: list, key=verdict):
    x = sum(1 for r in rows if key(r) == "xiaomi")
    l = sum(1 for r in rows if key(r) == "loc")
    return (x / (x + l) if x + l else None), x, l


def bootstrap(rows: list, reps: int, seed, key=verdict):
    """95% interval of the win rate, resampling whole pages."""
    pages = defaultdict(list)
    for r in rows:
        pages[r["page"]].append(r)
    keys = sorted(pages)
    if len(keys) < 2:
        return None
    rng = random.Random(seed)
    rates = []
    for _ in range(reps):
        pick = [rng.choice(keys) for _ in keys]
        x = sum(1 for p in pick for r in pages[p] if key(r) == "xiaomi")
        l = sum(1 for p in pick for r in pages[p] if key(r) == "loc")
        if x + l:
            rates.append(x / (x + l))
    if not rates:
        return None
    rates.sort()
    return [round(rates[int(0.025 * len(rates))], 4), round(rates[min(len(rates) - 1, int(0.975 * len(rates)))], 4)]


def summary(rows: list, reps: int, seed, key=verdict) -> dict:
    rate, x, l = win_rate(rows, key)
    return {"tasks": len(rows), "xiaomi": x, "loc": l, "xiaomi_win_rate": None if rate is None else round(rate, 4),
            "ci95_page_bootstrap": bootstrap(rows, reps, seed, key), "pages": len({r["page"] for r in rows}),
            "other": dict(sorted(Counter(key(r) for r in rows if key(r) not in ("xiaomi", "loc")).items(), key=str))}


def truth_key(row: dict):
    return row.get("truth")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--export", required=True)
    ap.add_argument("--set", help="only this set's tasks (the pick set)")
    ap.add_argument("--calibration-set")
    ap.add_argument("--reps", type=int, default=2000)
    ap.add_argument("--seed", default="1")
    a = ap.parse_args()
    rows = [json.loads(l) for l in open(a.export, encoding="utf-8") if l.strip()]
    settled = [r for r in rows if r.get("settled")]
    # A row with a truth is a calibration item (the export has `truth` for those only): never in the pilot figures.
    main_rows = [r for r in settled if (a.set is None or r["set"] == a.set) and r["set"] != a.calibration_set and not r.get("truth")]
    report = {"exported": len(rows), "settled": len(settled), "overall": summary(main_rows, a.reps, a.seed)}
    for name, get in (("decade", lambda r: (r.get("strata") or {}).get("decade")),
                      ("damage_band", lambda r: (r.get("strata") or {}).get("damage_band"))):
        groups = defaultdict(list)
        for r in main_rows:
            groups[get(r)].append(r)
        report["by_" + name] = {str(k): summary(v, a.reps, a.seed) for k, v in sorted(groups.items(), key=lambda kv: str(kv[0]))}
    if a.calibration_set:
        cal = [r for r in settled if r["set"] == a.calibration_set and r.get("truth")]
        agree = Counter()
        for r in cal:
            agree[(r["truth"], verdict(r))] += 1
        match = sum(v for (t, o), v in agree.items() if t == o)
        report["calibration"] = {
            "settled_with_truth": len(cal),
            "verdict_matches_truth": round(match / len(cal), 4) if cal else None,
            "truth_by_verdict": {f"{t}->{o}": v for (t, o), v in sorted(agree.items(), key=str)},
            "xiaomi_win_rate_from_truth": summary(cal, a.reps, a.seed, truth_key),
            "xiaomi_win_rate_from_picks": summary(cal, a.reps, a.seed)}
    print(json.dumps(report, indent=1, ensure_ascii=False))


if __name__ == "__main__":
    main()
