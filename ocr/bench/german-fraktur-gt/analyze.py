#!/usr/bin/env python3
"""Pool score.py CSVs, bootstrap CIs, apply decision rule A (German truth protocol).

Rows are limited to the year window (--years, default 1850-1920; undated rows
are dropped). A run is INCOMPLETE, and never prints LIFT, unless every
expected (set, page, arm) row from score.py's sidecar (<csv>.meta.json) is
present, both decision-pool sets have pages, and no --limit was used.

Pooled CER = sum(edits) / sum(ref_chars). Page bootstrap resamples pages;
title-clustered bootstrap resamples titles (all their pages). 10,000
resamples, seed 20261121. Lift the BOINC hold only if the upper 95% page-
bootstrap limit of pooled folded CER on the decision pool (Chronicling
Germany 1850-1920 plus Reichsanzeiger pages not in the GPU study) is
<= 0.179. Any set whose point estimate is > 0.235 is flagged (hold and
investigate). Reads CSV rows only; no page text is ever involved.

Titles are compared with runs of '-', '_' and spaces made one '_' (Chronicling
Germany names one newspaper both Weisseritz-Zeitung and Weisseritz_Zeitung).

--rule A2 is for line_score.py CSVs (line-aligned CER, reading order taken out).
It lifts only if every one of these holds on the decision pool: upper 95%
page-bootstrap limit <= 0.179, upper 95% title-bootstrap limit <= 0.235, no
set's point estimate above 0.235, and lost text (reference characters on truth
lines that got no engine words) at most 5%. --without-title adds a report-only
pool row without the named title(s).
"""
import argparse, csv, glob, json, os, random, re, sys
from collections import defaultdict

LIFT_UPPER, FLAG_POINT = 0.179, 0.235
A2_TITLE_UPPER, A2_MAX_LOST = 0.235, 0.05

def title_key(t):
    """One spelling per title: '-', '_' and whitespace runs become one '_'."""
    return re.sub(r"[-_\s]+", "_", (t or "").strip())
N_BOOT, SEED = 10000, 20261121

def expand(paths):
    out = []
    for pat in paths:
        for p in sorted(glob.glob(pat)) or [pat]:
            if p.endswith(".meta.json"):
                continue
            rp = os.path.realpath(p)
            if rp not in {os.path.realpath(q) for q in out}:
                out.append(p)
    return out

def load(paths):
    rows, seen, dup = [], {}, []
    for p in paths:
        with open(p, newline="", encoding="utf-8") as f:
            for r in csv.DictReader(f):
                k = (r["set"], r["page_id"], r["arm"])
                if k in seen:
                    dup.append("%s (in %s and %s)" % ("/".join(k), seen[k], p))
                    continue
                seen[k] = p
                for c in ("ref_chars", "edits_raw", "edits_folded"):
                    r[c] = int(r[c])
                for c in ("word_f1", "cpu_s"):
                    r[c] = float(r[c])
                r["gpu_study_flag"] = int(r["gpu_study_flag"] or 0)
                r["title"] = title_key(r.get("title"))
                if r.get("lost_chars") not in (None, ""):
                    r["lost_chars"] = int(r["lost_chars"])
                rows.append(r)
    if dup:
        raise SystemExit("duplicate (set, page_id, arm) rows, refusing to guess "
                         "which is right: " + "; ".join(dup[:5])
                         + (" ... (%d total)" % len(dup) if len(dup) > 5 else ""))
    return rows

def year_of(r):
    try:
        return int(r["year"])
    except (KeyError, TypeError, ValueError):
        return None

def in_window(rows, y0, y1):
    """Rows dated inside y0..y1; returns (kept, n_undated, n_outside)."""
    kept, undated, outside = [], 0, 0
    for r in rows:
        y = year_of(r)
        if y is None:
            undated += 1
        elif not y0 <= y <= y1:
            outside += 1
        else:
            kept.append(r)
    return kept, undated, outside

PROVENANCE = (("tesseract_version", "tesseract version"), ("cmd", "command"),
              ("gpu_study_sha256", "GPU-study exclusion list"))

def provenance_problems(metas, arms):
    """Sidecars of several CSVs must come from the same engine and models."""
    out = []
    if len(metas) < 2:
        return out
    seen = {}                      # arm -> (path, sha) of the first sidecar naming it
    for p, m in metas:
        for arm in arms:
            o = m.get("arms") or {}
            if arm not in o:
                continue
            if arm in seen and seen[arm][1] != o[arm]:
                out.append("model %s sha256 differs between %s and %s" % (
                    arm, seen[arm][0], p))
            seen.setdefault(arm, (p, o[arm]))
    p0, m0 = metas[0]
    for p, m in metas[1:]:
        for k, label in PROVENANCE:
            if m0.get(k) != m.get(k):
                out.append("%s differs between %s and %s" % (label, p0, p))
    return out

def completeness(csv_paths, rows, arms, pool_sets, all_rows=None):
    """Reasons the run cannot support a decision (empty list: complete).

    rows: the rows kept by the year filter. all_rows: every CSV row; a page
    whose row exists but fell outside the window is not expected, a page with
    no row at all is missing."""
    problems, expected, metas = [], {}, []
    all_rows = rows if all_rows is None else all_rows
    for p in csv_paths:
        try:
            with open(p + ".meta.json", encoding="utf-8") as f:
                meta = json.load(f)
        except (OSError, ValueError) as e:
            problems.append("no readable sidecar %s.meta.json (%s)" % (p, e))
            continue
        if not isinstance(meta.get("expected"), dict):
            problems.append("%s.meta.json has no expected-pages list" % p)
            continue
        metas.append((p, meta))
        if meta.get("limit"):
            problems.append("%s was run with --limit %s" % (p, meta["limit"]))
        gone = meta.get("missing_images")
        if gone:
            n = sum(len(v) for v in gone.values()) if isinstance(gone, dict) else len(gone)
            problems.append("%s: %d eligible pages have no image "
                            "(--allow-missing-images)" % (p, n))
        for arm, sets in meta["expected"].items():
            for s, ids in sets.items():
                expected.setdefault(arm, {}).setdefault(s, set()).update(ids)
    problems += provenance_problems(metas, arms)
    have = {(r["set"], r["page_id"], r["arm"]) for r in rows}
    seen = {(r["set"], r["page_id"], r["arm"]) for r in all_rows}
    for arm in arms:
        if arm not in expected:
            problems.append("no expected pages recorded for arm %s" % arm)
            continue
        for s, ids in sorted(expected[arm].items()):
            miss = sorted(i for i in ids if (s, i, arm) not in seen)
            if miss:
                problems.append("%s/%s: %d of %d expected pages missing (e.g. %s)" % (
                    arm, s, len(miss), len(ids), ", ".join(miss[:3])))
    for s in sorted(pool_sets):
        if not expected.get(arms[0], {}).get(s):
            problems.append("pool set %s has no expected pages" % s)
        if not any(r["set"] == s and r["arm"] == arms[0] and not r["gpu_study_flag"]
                   for r in rows):
            problems.append("pool set %s has no retained non-GPU-study pages "
                            "in the year window" % s)
    return problems

def pooled(rows, col):
    n = sum(r["ref_chars"] for r in rows)
    return sum(r[col] for r in rows) / n if n else float("nan")

def ci(units, col, n_boot, rng):
    """units: list of lists of rows (a resampling unit each)."""
    agg = [(sum(r[col] for r in u), sum(r["ref_chars"] for r in u)) for u in units]
    k, stats = len(agg), []
    for _ in range(n_boot):
        e = c = 0
        for i in rng.choices(range(k), k=k):
            e += agg[i][0]; c += agg[i][1]
        stats.append(e / c if c else float("nan"))
    stats.sort()
    return stats[int(0.025 * n_boot)], stats[min(n_boot - 1, int(0.975 * n_boot))]

def by(rows, key):
    d = defaultdict(list)
    for r in rows:
        d[r[key]].append(r)
    return list(d.values())

def summarize(name, rows, n_boot, seed):
    pages = [[r] for r in rows]
    titles = by(rows, "title")
    out = {"name": name, "pages": len(rows), "titles": len(titles),
           "ref_chars": sum(r["ref_chars"] for r in rows),
           "cer_f": pooled(rows, "edits_folded"), "cer_r": pooled(rows, "edits_raw"),
           "f1": sum(r["word_f1"] for r in rows) / len(rows),
           "cpu": sum(r["cpu_s"] for r in rows) / len(rows)}
    if all(isinstance(r.get("lost_chars"), int) for r in rows):
        out["lost"] = sum(r["lost_chars"] for r in rows) / out["ref_chars"] if out["ref_chars"] else 0.0
    out["page_ci"] = ci(pages, "edits_folded", n_boot, random.Random(seed))
    out["title_ci"] = ci(titles, "edits_folded", n_boot, random.Random(seed + 1))
    return out

def table(items):
    lost = all("lost" in o for o in items)
    h = ("| group | pages | titles | ref chars | CER folded | 95% page CI | "
         "95% title CI | CER raw | word F1 | CPU s/page |" + (" lost text |" if lost else ""))
    print(h); print("|" + "---|" * (11 if lost else 10))
    for o in items:
        print("| %s | %d | %d | %d | %.4f | %.4f-%.4f | %.4f-%.4f | %.4f | %.4f | %.1f |" % (
            o["name"], o["pages"], o["titles"], o["ref_chars"], o["cer_f"],
            *o["page_ci"], *o["title_ci"], o["cer_r"], o["f1"], o["cpu"])
            + (" %.4f |" % o["lost"] if lost else ""))

def rule_a2(pool_o, flags):
    """Rule A v2 (line-aligned): (verdict, reasons it failed)."""
    why = []
    if pool_o["page_ci"][1] > LIFT_UPPER:
        why.append("upper page limit %.4f > %.3f" % (pool_o["page_ci"][1], LIFT_UPPER))
    if pool_o["title_ci"][1] > A2_TITLE_UPPER:
        why.append("upper title limit %.4f > %.3f" % (pool_o["title_ci"][1], A2_TITLE_UPPER))
    if flags:
        why.append("set(s) above %.3f: %s" % (FLAG_POINT, ", ".join(flags)))
    if "lost" not in pool_o:
        why.append("no lost_chars column (use line_score.py CSVs)")
    elif pool_o["lost"] > A2_MAX_LOST:
        why.append("lost text %.4f > %.2f" % (pool_o["lost"], A2_MAX_LOST))
    return ("HOLD" if why else "LIFT"), why

def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--csv", nargs="+", required=True, help="score.py output CSVs (globs ok)")
    ap.add_argument("--arm", default="frak2021", help="deployed arm (decides)")
    ap.add_argument("--ref-arm", default="deu_latf", help="reference arm (reported)")
    ap.add_argument("--pool-sets", default="chronicling,reichsanzeiger",
                    help="sets in the decision pool (GPU-study pages excluded)")
    ap.add_argument("--years", default="1850-1920",
                    help="year window enforced on CSV rows (undated rows dropped)")
    ap.add_argument("--rule", choices=("A", "A2"), default="A",
                    help="A: page CER (score.py); A2: line-aligned CER (line_score.py)")
    ap.add_argument("--without-title", action="append", default=[],
                    help="also report the decision pool without this title (repeatable)")
    ap.add_argument("--n-boot", type=int, default=N_BOOT)
    ap.add_argument("--seed", type=int, default=SEED)
    a = ap.parse_args(argv)
    y0, y1 = (int(x) for x in a.years.split("-"))
    csv_paths = expand(a.csv)
    all_rows = load(csv_paths)
    pool_sets = set(a.pool_sets.split(","))
    rows, undated, outside = in_window(all_rows, y0, y1)
    problems = completeness(csv_paths, rows, [a.arm, a.ref_arm], pool_sets, all_rows)
    if undated or outside:
        print("Year window %d-%d: dropped %d undated and %d out-of-window rows."
              % (y0, y1, undated, outside))
    verdict = None
    for arm in (a.arm, a.ref_arm):
        rs = [r for r in rows if r["arm"] == arm]
        if not rs:
            print("\n(no rows for arm %s)" % arm); continue
        items = []
        for s in sorted({r["set"] for r in rs}):
            sub = [r for r in rs if r["set"] == s]
            items.append(summarize(s, sub, a.n_boot, a.seed))
            for st in sorted({r["stratum"] for r in sub}):
                sst = [r for r in sub if r["stratum"] == st]
                if len({r["stratum"] for r in sub}) > 1:
                    items.append(summarize("%s / %s" % (s, st), sst, a.n_boot, a.seed))
            g = [r for r in sub if r["gpu_study_flag"]]
            if g:
                items.append(summarize("%s / GPU-study pages" % s, g, a.n_boot, a.seed))
                ng = [r for r in sub if not r["gpu_study_flag"]]
                if ng:
                    items.append(summarize("%s / not GPU-study" % s, ng, a.n_boot, a.seed))
        pool = [r for r in rs if r["set"] in pool_sets and not r["gpu_study_flag"]]
        pool_o = summarize("DECISION POOL", pool, a.n_boot, a.seed) if pool else None
        if pool_o:
            items.append(pool_o)
            for t in a.without_title:
                rest = [r for r in pool if r["title"] != title_key(t)]
                if rest and len(rest) < len(pool):
                    items.append(summarize("DECISION POOL without %s" % title_key(t), rest,
                                           a.n_boot, a.seed))
        print("\n### Arm %s%s\n" % (arm, "" if arm == a.arm else " (reference, no decision)"))
        table(items)
        if arm == a.arm:
            flags = [o["name"] for o in items if " / " not in o["name"]
                     and not o["name"].startswith("DECISION POOL") and o["cer_f"] > FLAG_POINT]
            if pool_o and a.rule == "A2":
                verdict, why = rule_a2(pool_o, flags)
                if problems:
                    verdict = "INCOMPLETE"
                print("\nDecision rule A2 (line-aligned): page upper %.4f (<= %.3f), title upper "
                      "%.4f (<= %.3f), lost text %s (<= %.2f) -> **%s**%s" % (
                          pool_o["page_ci"][1], LIFT_UPPER, pool_o["title_ci"][1], A2_TITLE_UPPER,
                          "%.4f" % pool_o["lost"] if "lost" in pool_o else "n/a", A2_MAX_LOST,
                          verdict if not problems else "no decision, incomplete",
                          (" (" + "; ".join(why) + ")") if why and not problems else ""))
            elif pool_o:
                up = pool_o["page_ci"][1]
                verdict = "LIFT" if up <= LIFT_UPPER else "HOLD"
                why = ""
                if flags:               # a flagged set overrides a lift
                    verdict = "HOLD"
                    why = " (flagged set: %s)" % ", ".join(flags)
                if problems:
                    verdict = "INCOMPLETE"
                print("\nDecision rule A: upper 95%% page-bootstrap limit of pooled folded CER "
                      "= %.4f vs %.3f -> **%s**%s" % (
                          up, LIFT_UPPER, verdict if not problems else "no decision, incomplete",
                          why if not problems else ""))
                tu = pool_o["title_ci"][1]
                if verdict == "LIFT" and tu > LIFT_UPPER:
                    print("Note: title-clustered upper limit %.4f exceeds %.3f; "
                          "the rule is on the page bootstrap, but look at it." % (tu, LIFT_UPPER))
            else:
                print("\nNo decision-pool pages scored yet.")
            if flags:
                print("FLAG: set(s) with point estimate > %.3f: %s -> hold and investigate"
                      % (FLAG_POINT, ", ".join(flags)))
    if problems:
        print("\nINCOMPLETE: no LIFT verdict is possible.")
        for p in problems:
            print("  - " + p)
        return 3
    return 0

if __name__ == "__main__":
    sys.exit(main())
