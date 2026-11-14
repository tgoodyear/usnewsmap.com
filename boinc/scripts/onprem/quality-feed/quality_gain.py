#!/usr/bin/env python3
"""Measures what re-reading gained, per cell of language and decade, against LoC (#1415).

  quality_gain.py --out-dir DIR (--dsn-file FILE | --input FILE)
      [--readers ours-tesseract] [--min-gain 0.02] [--cell-pages 374]
      [--worst-above 0.25] [--moderate-above 0.1] [--min-occurrences 50] [--plan FILE ...]

It reads the database read-only (use the quality_reader DSN, which points at the replica) and
writes only under --out-dir.

The measure is the one the threshold was set on: for each page with a re-read text, LoC's
damage rate (quality.page_summary) minus the re-read text's (quality.ocr_outputs, the newest
output of that reader), page by page. A cell is (reader, band, language, decade); band is the
campaign builder's, from LoC's rate and the page's word occurrences (head + near). Where both
texts have an estimated CER from a usnm_quality unit (quality.reader_scores_effective, LoC as `loc` or
`loc-hist`), the cell also gets the mean drop in estimated CER, as a second measure. It
decides nothing.

Each cell gets one decision:

  stop       the gain is below --min-gain: the 95% interval's upper end is under it (from 30
             pages), or the cell has --cell-pages pages and its mean is under it.
  go         the gain is at least --min-gain: the interval's lower end is at or over it (from 30
             pages), or the cell has --cell-pages pages and its mean is at or over it.
  measuring  neither yet.

--cell-pages defaults to 374, the pages a paired test needs to see a mean drop of 0.02 at 80%
power and two-sided 5% with SD 0.138 (#1416). The interval is the normal one, mean +/- 1.96 SE.

Readers. --readers names quality.readers rows whose output is page text in quality.ocr_outputs:
`ours-tesseract` today. A new reader (Xiaomi-OCR-0, once usnm_qwen 1.20 ships it) is measured
here by name once it has a quality.readers row and loaded outputs; nothing else changes. An
unknown name stops the run, so a typo isn't read as "no pages".

--plan restricts the pages to those in one or more of quality_campaign.py's plan.jsonl files,
so a campaign's own gain is measured apart from earlier re-reads. A moderate sample's rows
carry `weight`, and with --plan the cell's mean and interval are weighted by it, so a cell
estimates its band's gain rather than the sample's.

A cell is stopped (in stop.json) only when every requested reader stopped there; a reader with
no pages in the cell is still measuring it. stop.json records the band filters it was made
with, and quality_campaign.py refuses it under other filters.

The CER pair comes from one unit: the page's unit chosen as quality.page_signals chooses it
(hat version 2 first, then the newest, leaving out campaigns in quality.excluded_estimates),
LoC's estimate and the reader's both from that unit.

Files, under --out-dir:

  pages.csv   the export: one row per page and reader.
  input.json  the export's filters and SHA-256, so --input can replay it.
  cells.csv   one row per cell: n, mean drop, SD, the interval, the CER pair, the decision.
  stop.json   the stopped cells (band, language, decade), which quality_campaign.py
              --stop-cells leaves out of that band's list.
  summary.json
"""
import argparse
import collections
import csv
import hashlib
import json
import math
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path[:0] = [HERE, os.path.join(HERE, "..", "ocr-feed")]
import quality_campaign  # noqa: E402

COLUMNS = ["doc_id", "reader", "lang", "decade", "occurrences", "loc_damage", "ours_damage", "loc_est_cer",
           "ours_est_cer"]
EXPORT_SQL = """
BEGIN TRANSACTION READ ONLY;
COPY (
  WITH wanted(reader) AS (VALUES {readers}),
  latest AS (
    -- The newest output of each reader, scored or not: an unscored newest run isn't
    -- replaced by an older scored one.
    SELECT DISTINCT ON (doc_id, reader) doc_id, reader, damage_rate
    FROM quality.ocr_outputs
    WHERE reader IN (SELECT reader FROM wanted)
    ORDER BY doc_id, reader, ocred_at DESC
  ), o AS (
    SELECT * FROM latest WHERE damage_rate IS NOT NULL
  ), hat AS (
    -- The page's unit, as quality.page_signals picks it: hat version 2 first, then the
    -- newest, among campaigns whose estimates aren't excluded.
    SELECT DISTINCT ON (u.doc_id) u.campaign, u.doc_id
    FROM quality.unit_pages u JOIN quality.campaigns c USING (campaign)
    WHERE u.outcome = 'ok' AND u.doc_id IN (SELECT doc_id FROM o)
      AND NOT EXISTS (SELECT FROM quality.excluded_estimates x WHERE x.campaign = u.campaign)
    ORDER BY u.doc_id, u.hat_version DESC, c.loaded_at DESC
  ), l AS (
    SELECT DISTINCT ON (r.doc_id) r.doc_id, r.est_cer
    FROM hat JOIN quality.reader_scores_effective r USING (campaign, doc_id)
    WHERE r.reader IN ('loc', 'loc-hist') AND r.est_cer IS NOT NULL
    ORDER BY r.doc_id, (r.reader = 'loc-hist') DESC
  ), r AS (
    -- The reader's estimate from the same unit as LoC's, so the two are a pair.
    SELECT r.doc_id, r.reader, r.est_cer
    FROM hat JOIN quality.reader_scores_effective r USING (campaign, doc_id)
    WHERE r.est_cer IS NOT NULL AND r.reader IN (SELECT reader FROM wanted)
  )
  SELECT o.doc_id, o.reader, coalesce(s.lang, 'none') AS lang, s.decade, d.head + d.near AS occurrences,
         s.damage_rate AS loc_damage, o.damage_rate AS ours_damage, l.est_cer AS loc_est_cer,
         r.est_cer AS ours_est_cer
  FROM o JOIN quality.page_summary s USING (doc_id) JOIN quality.damage d USING (doc_id)
  LEFT JOIN l USING (doc_id) LEFT JOIN r ON r.doc_id = o.doc_id AND r.reader = o.reader
  WHERE s.damage_rate IS NOT NULL AND s.decade IS NOT NULL
) TO STDOUT WITH CSV HEADER;
COMMIT;
"""
CHECK_SQL = """
BEGIN TRANSACTION READ ONLY;
COPY (SELECT reader FROM quality.readers) TO STDOUT;
COMMIT;
"""
Z95 = 1.96
MIN_INTERVAL_PAGES = 30


def export_sql(readers):
    return EXPORT_SQL.format(readers=", ".join("(" + quote(r) + ")" for r in readers))


def quote(text):
    return "'" + text.replace("'", "''") + "'"


def psql(dsn_file, sql, out):
    """Runs `sql` read-only with the DSN in libpq's environment, its output to the file `out`."""
    import subprocess
    with open(dsn_file) as f:
        dsn = f.read().strip()
    env = {**os.environ, **quality_campaign.dsn_env(dsn), "PGOPTIONS": "-c default_transaction_read_only=on",
           "PGAPPNAME": "usnm-quality-gain", "LC_ALL": "C.UTF-8"}
    p = subprocess.run(["psql", "-X", "-q", "-v", "ON_ERROR_STOP=1"], input=sql.encode(), stdout=out,
                       stderr=subprocess.PIPE, env=env, timeout=quality_campaign.EXPORT_TIMEOUT)
    if p.returncode:
        sys.exit(f"the query failed: {p.stderr.decode(errors='replace').strip()[-300:]}")


def check_readers(dsn_file, readers, tmp):
    with open(tmp, "wb") as out:
        psql(dsn_file, CHECK_SQL, out)
    with open(tmp) as f:
        known = {line.strip() for line in f}
    os.unlink(tmp)
    unknown = sorted(set(readers) - known)
    if unknown:
        sys.exit(f"--readers: not in quality.readers: {', '.join(unknown)}")


def export(dsn_file, readers, path):
    check_readers(dsn_file, readers, path + ".readers")
    tmp = path + ".tmp"
    with open(tmp, "wb") as out:
        psql(dsn_file, export_sql(readers), out)
    os.replace(tmp, path)


def read_pages(path):
    with open(path, newline="") as f:
        r = csv.reader(f)
        if next(r, None) != COLUMNS:
            sys.exit(f"{path}: expected the columns {','.join(COLUMNS)}")
        for doc, reader, lang, decade, occ, loc, ours, loc_cer, ours_cer in r:
            yield {"doc_id": doc, "reader": reader, "lang": lang, "decade": int(decade), "occurrences": int(occ),
                   "loc_damage": float(loc), "ours_damage": float(ours),
                   "loc_est_cer": float(loc_cer) if loc_cer else None,
                   "ours_est_cer": float(ours_cer) if ours_cer else None}


def plan_docs(paths):
    """doc_id -> weight: the moderate sample's `weight` (the inverse of the page's chance of
    being drawn), 1 for a page of the worst list, which takes every page."""
    docs = {}
    for path in paths:
        with open(path) as f:
            for line in f:
                row = json.loads(line)
                docs[row["doc_id"]] = float(row.get("weight", 1.0))
    return docs


def weighted(xs, ws):
    """The weighted mean and its standard error (the ratio estimator's linearised variance),
    with an effective SD so the decision rule reads it as it reads an unweighted cell."""
    n = len(xs)
    total = math.fsum(ws)
    m = math.fsum(w * x for x, w in zip(xs, ws)) / total
    if n < 2:
        return m, None
    var = n / (n - 1) * math.fsum((w * (x - m)) ** 2 for x, w in zip(xs, ws)) / total ** 2
    return m, math.sqrt(var * n)


def mean_sd(xs):
    n = len(xs)
    if not n:
        return None, None
    m = math.fsum(xs) / n
    if n < 2:
        return m, None
    return m, math.sqrt(math.fsum((x - m) ** 2 for x in xs) / (n - 1))


def decide(n, mean, sd, min_gain, cell_pages):
    """stop, go or measuring, by the rule in the header."""
    if n >= MIN_INTERVAL_PAGES and sd is not None:
        half = Z95 * sd / math.sqrt(n)
        if mean + half < min_gain:
            return "stop"
        if mean - half >= min_gain:
            return "go"
    if n >= cell_pages:
        return "go" if mean >= min_gain else "stop"
    return "measuring"


def cells_of(pages, filters, weights=None):
    cells = collections.defaultdict(lambda: {"drop": [], "cer": [], "w": []})
    for p in pages:
        band = quality_campaign.band_of(p["loc_damage"], p["occurrences"], filters) or "low"
        c = cells[(p["reader"], band, p["lang"], p["decade"])]
        c["drop"].append(p["loc_damage"] - p["ours_damage"])
        c["w"].append(weights.get(p["doc_id"], 1.0) if weights else 1.0)
        if p["loc_est_cer"] is not None and p["ours_est_cer"] is not None:
            c["cer"].append(p["loc_est_cer"] - p["ours_est_cer"])
    return cells


def band_summary(pages, filters, weights, band):
    """A band's page count and mean drop, weighted by the plans' weights as the cells are."""
    xs, ws = [], []
    for p in pages:
        if (quality_campaign.band_of(p["loc_damage"], p["occurrences"], filters) or "low") == band:
            xs.append(p["loc_damage"] - p["ours_damage"])
            ws.append(weights.get(p["doc_id"], 1.0) if weights else 1.0)
    if not xs:
        return {"mean_drop": None, "n": 0}
    return {"mean_drop": r4(weighted(xs, ws)[0]), "n": len(xs)}


def r4(x):
    return None if x is None else round(x, 4)


def measure(a):
    os.makedirs(a.out_dir, exist_ok=True)
    readers = sorted(set(a.readers.split(",")))
    source = a.input
    if a.dsn_file:
        source = os.path.join(a.out_dir, "pages.csv")
        export(a.dsn_file, readers, source)
        quality_campaign.write_atomic(os.path.join(a.out_dir, "input.json"), (json.dumps(
            {"columns": COLUMNS, "readers": readers, "sha256": quality_campaign.input_digest(source)},
            indent=1, sort_keys=True) + "\n").encode())
    filters = quality_campaign.Filters(a.worst_above, a.moderate_above, a.min_occurrences)
    only = plan_docs(a.plan) if a.plan else None
    pages = [p for p in read_pages(source) if p["reader"] in readers and (only is None or p["doc_id"] in only)]
    rows = []
    for (reader, band, lang, decade), c in sorted(cells_of(pages, filters, only).items()):
        n = len(c["drop"])
        mean, sd = weighted(c["drop"], c["w"]) if only else mean_sd(c["drop"])
        half = Z95 * sd / math.sqrt(n) if sd is not None else None
        cer_mean, _ = mean_sd(c["cer"])
        rows.append({"reader": reader, "band": band, "lang": lang, "decade": decade, "n": n,
                     "mean_drop": r4(mean), "sd": r4(sd),
                     "low95": r4(mean - half) if half is not None else None,
                     "high95": r4(mean + half) if half is not None else None,
                     "cer_pairs": len(c["cer"]), "mean_cer_drop": r4(cer_mean),
                     "decision": decide(n, mean, sd, a.min_gain, a.cell_pages)})
    with open(os.path.join(a.out_dir, "cells.csv.tmp"), "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=list(rows[0]) if rows else ["reader"], lineterminator="\n")
        w.writeheader()
        w.writerows(rows)
    os.replace(os.path.join(a.out_dir, "cells.csv.tmp"), os.path.join(a.out_dir, "cells.csv"))
    # A cell of a band is stopped when every reader measured there stopped; one reader still
    # measuring or going keeps it open.
    by_cell = collections.defaultdict(set)
    for r in rows:
        if r["band"] in a.stop_bands:
            by_cell[(r["band"], r["lang"], r["decade"])].add(r["decision"])
    seen = collections.defaultdict(set)
    for r in rows:
        if r["band"] in a.stop_bands and r["decision"] == "stop":
            seen[(r["band"], r["lang"], r["decade"])].add(r["reader"])
    # Every requested reader must have stopped there: a reader with no pages in the cell is
    # still measuring it.
    stopped = sorted(k for k, d in by_cell.items() if d == {"stop"} and seen[k] == set(readers))
    stop = {"cells": [{"band": b, "decade": d, "lang": l} for b, l, d in stopped], "min_gain": a.min_gain,
            "readers": readers, "bands": sorted(a.stop_bands),
            "filters": {**quality_campaign.cut_of(filters), "worst_above": quality_campaign.literal(filters.worst_above)}}
    quality_campaign.write_atomic(os.path.join(a.out_dir, "stop.json"),
                                  (json.dumps(stop, indent=1, sort_keys=True) + "\n").encode())
    decisions = collections.Counter((r["band"], r["decision"]) for r in rows)
    summary = {
        "cell_pages": a.cell_pages, "cells": len(rows), "min_gain": a.min_gain, "pages": len(pages),
        "plans": a.plan, "readers": readers, "stopped_cells": len(stopped),
        "decisions": {f"{b}/{d}": decisions[(b, d)] for b, d in sorted(decisions)},
        "input_sha256": quality_campaign.input_digest(source),
        "by_band": {b: band_summary(pages, filters, only, b) for b in ("worst", "moderate", "low")},
    }
    quality_campaign.write_atomic(os.path.join(a.out_dir, "summary.json"),
                                  (json.dumps(summary, indent=1, sort_keys=True) + "\n").encode())
    print(json.dumps(summary, indent=1, sort_keys=True), flush=True)
    return rows, stop


def parser():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter,
                                 allow_abbrev=False)
    ap.add_argument("--out-dir", required=True)
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("--dsn-file", help="the quality_reader DSN (the replica); read with a read-only transaction")
    src.add_argument("--input", help="a pages.csv from an earlier run")
    ap.add_argument("--readers", default="ours-tesseract", help="quality.readers names, comma-separated")
    ap.add_argument("--min-gain", type=float, default=0.02, help="the smallest gain worth acting on (#1415)")
    ap.add_argument("--cell-pages", type=int, default=374, help="pages a cell needs for a decision on its mean")
    ap.add_argument("--stop-bands", default="worst,moderate",
                    help="bands whose stopped cells go in stop.json (default %(default)s)")
    ap.add_argument("--worst-above", type=quality_campaign.threshold,
                    default=quality_campaign.DEFAULT_FILTERS.worst_above)
    ap.add_argument("--moderate-above", type=quality_campaign.threshold,
                    default=quality_campaign.DEFAULT_FILTERS.moderate_above)
    ap.add_argument("--min-occurrences", type=int, default=quality_campaign.DEFAULT_FILTERS.min_occurrences)
    ap.add_argument("--plan", action="append", default=[], help="a campaign's plan.jsonl; may be repeated")
    return ap


def main(argv=None):
    ap = parser()
    a = ap.parse_args(argv)
    if not 0 < a.min_gain < 1 or a.cell_pages < 2:
        ap.error("--min-gain over 0 and under 1, --cell-pages at least 2")
    if not all(r and all(ch.isalnum() or ch in "-_" for ch in r) for r in a.readers.split(",")):
        ap.error("--readers: names of [A-Za-z0-9_-], comma-separated")
    a.stop_bands = set(a.stop_bands.split(","))
    if not a.stop_bands <= {"worst", "moderate", "low"}:
        ap.error("--stop-bands: worst, moderate or low")
    if not 0 <= a.moderate_above < a.worst_above <= 1:
        ap.error("0 <= --moderate-above < --worst-above <= 1")
    return measure(a)


if __name__ == "__main__":
    main()
