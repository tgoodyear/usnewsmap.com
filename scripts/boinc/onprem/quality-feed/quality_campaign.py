#!/usr/bin/env python3
"""Builds a worst-first re-OCR list (#1415) from usnm_quality, in the feed's own formats.

  quality_campaign.py --campaign NAME --seed TEXT --out-dir DIR --ocr-state DIR
      (--dsn-file FILE | --input FILE)
      [--index FILE] [--campaign-exclusions [--exclude-list FILE ...]]
      [--tier-width 0.05] [--cell-pages 400] [--manifest-pages 2000]

It only reads the database and writes under --out-dir: nothing is staged, queued or shipped.

Two lists come out, each its own campaign, so their results stay apart in
quality.unit_pages.campaign (and in its group, which the shipper sets to the campaign):

  <campaign>           Every page worth re-OCRing, as the damage ranking report defines it:
                       damage rate over 0.25 from at least 50 word occurrences (head + near),
                       no output of our OCR. Worst first.
  <campaign>-moderate  An OCR-then-compare sample of the band from over 0.10 to 0.25, with
                       the same evidence and OCR conditions.

Both are lists of pages to OCR. A usnm_quality unit compares the texts a page already has and
reads nothing itself, so neither list shows any gain until its pages have quality.ocr_outputs
rows to compare with LoC's damage score.

The band tests run in SQL, with the report's expressions: damage_rate is a real, so the
page Postgres stores as 0.1 is above the literal 0.1 there and would not be in Python.

Order of the worst pages. Pages fall in tiers --tier-width wide, from the top. A tier goes
out whole before the next starts. Inside a tier, batches come worst mean damage first, and
a batch's pages in issue order, so its scans fetch together and an issue's METS and its
batch's manifest are asked for once. The default width is under the score's own noise (about
0.06 at a rate of 0.25 and 50 occurrences), so the grouping reorders only pages the score
can't tell apart: no page ever sits ahead of one more than a tier width worse.

The moderate sample. Each cell of language and decade gets up to --cell-pages pages. Where a
cell has no more titles than that, its titles take turns, so one title can't fill it. Where
it has more, not every title can have a page, and picking titles evenly would misstate a cell
whose large titles differ from its small ones, so the picks fall at equal steps along the
cell's pages, with the titles in hash order: every page has the same chance, and titles get
pages in proportion to their size. A title's pages are the ones with the lowest hash, not its
worst ones. Every row carries stratum_pages (eligible pages of its language, decade and
title) and weight (the inverse of its chance, so a cell's weights sum to its eligible pages),
so a result can be carried back to the band. The list is in hash order, so any prefix of it
is mixed. The hash is the sampler's (crates/usnm-boinc assess::sample):
SHA-256 of the seed, the stream and the doc id. The same input and seed give the same bytes.

Exclusions: only pages our OCR has done or is about to do. That is a row in
quality.ocr_outputs (live, where page_summary's ocr_runs can be six hours old), or in
--ocr-state, the state directory of ocr_manifest.py build: a page still outstanding by its
queued/ and outcomes/ files, or one with an imported outcome not yet loaded. A failed page
(dead-lettered) stays a candidate. Being in a quality campaign doesn't exclude a page.

--campaign-exclusions, off by default, also leaves out pages with a row in quality.unit_pages
or quality.staging, and any page named in an --exclude-list file (first tab-separated field,
so a feeder's done.txt and skipped.txt work as they are). --ocr-state and the lists are part
of the input and keep growing, so a rebuild needs copies from the first run; summary.json
records their digests.

Files, under --out-dir/<campaign>/:

  plan.jsonl    every page in order: quality_feed.py select's fields (bytes, scan and sha256
                null until the scan is on the scan server), plus rank, band, decade, decision,
                page_lang (the report's language), and tier or stratum_pages and weight.
  pages.jsonl   the pages --index has a scan for, in order: what quality_feed.py run reads.
                Like select without --scan-server, scan assumes the keep set.
  manifests/m00001.parquet ...  the other pages, usnm-prefetch-manifest/1, cut at issue
                boundaries. As in ocr_manifest.py build-quality, rows say `eng`, so the fetcher
                files the scans in its buffer set; plan.jsonl has the page's language.

summary.json holds the counts by band, language and decade, and is printed too. --dsn-file
keeps the export it read as input.csv in --out-dir; --input replays such a file.
"""
import argparse
import collections
import csv
import glob
import hashlib
import heapq
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
# Installed side by side; in a checkout ocr_manifest.py is in ../ocr-feed.
sys.path[:0] = [HERE, os.path.join(HERE, "..", "ocr-feed")]
import ocr_manifest  # noqa: E402
import quality_feed  # noqa: E402

COLUMNS = ["doc_id", "batch", "batch_version", "lccn", "date", "decade", "lang", "decision",
           "damage_rate", "band", "in_campaign", "has_ocr_output"]
EXPORT_SQL = """
BEGIN TRANSACTION READ ONLY;
COPY (
  SELECT s.doc_id, s.batch, s.batch_version, s.lccn, s.date, s.decade,
         coalesce(s.lang, 'none') AS lang, coalesce(s.decision, 'detected') AS decision, s.damage_rate,
         CASE WHEN s.damage_rate > 0.25 THEN 'worst' ELSE 'moderate' END AS band,
         EXISTS (SELECT FROM quality.unit_pages u WHERE u.doc_id = s.doc_id)
           OR EXISTS (SELECT FROM quality.staging g WHERE g.doc_id = s.doc_id) AS in_campaign,
         EXISTS (SELECT FROM quality.ocr_outputs o WHERE o.doc_id = s.doc_id) AS has_ocr_output
  FROM quality.page_summary s JOIN quality.damage d USING (doc_id)
  WHERE s.damage_rate > 0.1 AND d.head + d.near >= 50 AND s.ocr_runs = 0
) TO STDOUT WITH CSV HEADER;
COMMIT;
"""
INDEX_FIELDS = ("bytes", "fetched_at", "scan", "sha256")


def export(dsn_file, path):
    """The query's result to `path`. The read-only transaction is the SQL's; the session
    default is set too, so a mistake in the SQL can't write whichever role the DSN names."""
    with open(dsn_file) as f:
        dsn = f.read().strip()
    env = {**os.environ, "PGOPTIONS": "-c default_transaction_read_only=on",
           "PGAPPNAME": "usnm-quality-campaign", "LC_ALL": "C.UTF-8"}
    tmp = path + ".tmp"
    with open(tmp, "wb") as out:
        p = subprocess.run(["psql", dsn, "-X", "-q", "-v", "ON_ERROR_STOP=1"], input=EXPORT_SQL.encode(),
                           stdout=out, stderr=subprocess.PIPE, env=env, timeout=3600)
    if p.returncode:
        os.unlink(tmp)
        sys.exit(f"the export failed: {p.stderr.decode(errors='replace').strip()[-300:]}")
    os.replace(tmp, path)


def read_rows(path):
    """Rows as dicts of the export's columns, with their types."""
    with open(path, newline="") as f:
        r = csv.reader(f)
        head = next(r, None)
        if head != COLUMNS:
            sys.exit(f"{path}: expected the columns {','.join(COLUMNS)}")
        for doc, batch, bv, lccn, date, decade, lang, decision, rate, band, in_campaign, has_ocr in r:
            yield {"doc_id": doc, "batch": batch, "batch_version": int(bv), "lccn": lccn, "date": date,
                   "decade": int(decade), "page_lang": lang, "decision": decision,
                   "damage_rate": float(rate), "band": band, "in_campaign": in_campaign == "t",
                   "has_ocr_output": has_ocr == "t"}


def read_index(path):
    """(doc_id, batch, version) -> the fields a page takes from its index line, full-size
    scans only. The key has the batch: a scan of another batch version isn't this page's."""
    out = {}
    if not path:
        return out
    with open(path) as f:
        for line in f:
            row = json.loads(line)
            if row.get("resolution", "full") == "full" and quality_feed.DOC_RE.match(row.get("doc_id") or ""):
                key = (row["doc_id"], row.get("batch"), int(row.get("batch_version") or 1))
                out[key] = {"bytes": int(row["bytes"]), "fetched_at": row.get("fetched_at", ""),
                            "scan": quality_feed.scan_path(row), "sha256": row["sha256"]}
    return out


def parquet_rows(path, columns):
    import pyarrow.parquet as pq

    return pq.read_table(path, columns=columns).to_pylist()


def ocr_pages(state):
    """Pages OCR holds in --ocr-state: outstanding by ocr_manifest.pending, or imported."""
    def rows(kind, columns):
        return [r for path in sorted(glob.glob(os.path.join(state, kind, "*.parquet")))
                for r in parquet_rows(path, columns)]

    queued = rows("queued", ["doc_id", "reason", "queued_at"])
    outcomes = rows("outcomes", ["doc_id", "outcome", "cause", "at"])
    reasons = sorted({q["reason"] for q in queued if q.get("reason")})
    return ({q["doc_id"] for q in ocr_manifest.pending(queued, outcomes, reasons)}
            | {o["doc_id"] for o in outcomes if o.get("outcome") == "imported"})


def scan_key(r):
    return r["doc_id"], r["batch"], r["batch_version"]


def order(seed, doc_id):
    """The sampler's random order (assess::sample::order, stream ocr)."""
    return hashlib.sha256(f"{seed}\0ocr\0{doc_id}".encode()).hexdigest()


def tier_of(rate, width):
    return int((1.0 - rate) / width)


def order_worst(rows, width):
    """Tiers from the top; inside one, batches by mean damage, then issue order."""
    rows = sorted(rows, key=lambda r: (tier_of(r["damage_rate"], width), r["batch"], r["batch_version"],
                                       r["lccn"], r["date"], *edition_seq(r), r["doc_id"]))
    pages = collections.defaultdict(list)
    for r in rows:
        pages[(tier_of(r["damage_rate"], width), r["batch"], r["batch_version"])].append(r)
    keys = sorted(pages, key=lambda k: (k[0], -sum(p["damage_rate"] for p in pages[k]) / len(pages[k]), k[1], k[2]))
    return [p for k in keys for p in pages[k]]


def edition_seq(r):
    m = quality_feed.DOC_RE.match(r["doc_id"])
    return int(m["ed"]), int(m["seq"])


def allocate(sizes, cell_pages, seed):
    """How many pages each stratum (language, decade, title) gets: {stratum: n}. Titles in
    hash order; turns where each can have a page, equal steps along the pages where not."""
    cells = collections.defaultdict(list)
    for (lang, decade, lccn), size in sizes.items():
        cells[(lang, decade)].append((order(seed, lccn), lccn, size))
    take = {}
    for (lang, decade), titles in sorted(cells.items()):
        titles.sort()
        pages = sum(size for _, _, size in titles)
        got = collections.Counter()
        if len(titles) > cell_pages:
            start = int(order(seed, f"cell\0{lang}\0{decade}"), 16) % pages
            picks = iter(sorted((start + i * pages) // cell_pages % pages for i in range(cell_pages)))
            pick = next(picks)
            end = 0
            for _, lccn, size in titles:
                end += size
                while pick is not None and pick < end:
                    got[lccn] += 1
                    pick = next(picks, None)
        else:
            left = min(cell_pages, pages)
            while left:
                for _, lccn, size in titles:
                    if left and got[lccn] < size:
                        got[lccn] += 1
                        left -= 1
        for lccn, n in got.items():
            take[(lang, decade, lccn)] = n
    return take


def draw(path, eligible, take, seed):
    """The sample, from a second read of the input: each stratum's lowest hashes, in hash
    order overall. Holds no more rows than the sample has."""
    heaps = collections.defaultdict(list)
    for r in read_rows(path):
        key = (r["page_lang"], r["decade"], r["lccn"])
        n = take.get(key)
        if n is None or not eligible(r):
            continue
        h = order(seed, r["doc_id"])
        item = (-int(h, 16), h, r["doc_id"], r)
        if len(heaps[key]) < n:
            heapq.heappush(heaps[key], item)
        elif item[0] > heaps[key][0][0]:
            heapq.heapreplace(heaps[key], item)
    return [(h, r) for rows in heaps.values() for _, h, _, r in rows]


def weigh(picked, sizes, take, cell_pages):
    """stratum_pages and weight on each picked row, in hash order: a cell's pages over its
    picks where every page had the same chance, else a title's pages over its picks."""
    cell_all = collections.Counter()
    titles = collections.Counter()
    for (lang, decade, _), size in sizes.items():
        cell_all[(lang, decade)] += size
        titles[(lang, decade)] += 1
    out = []
    for _, r in sorted(picked, key=lambda x: x[0]):
        key = (r["page_lang"], r["decade"], r["lccn"])
        cell = key[:2]
        weight = cell_all[cell] / cell_pages if titles[cell] > cell_pages else sizes[key] / take[key]
        out.append({**r, "stratum_pages": sizes[key], "weight": weight})
    return out


def feed_row(r, scans):
    """A page in the shape quality_feed.py select writes."""
    edition, seq = edition_seq(r)
    scan = scans.get(scan_key(r), dict.fromkeys(INDEX_FIELDS))
    return {"batch": r["batch"], "batch_version": r["batch_version"], "bytes": scan["bytes"],
            "damage_rate": r["damage_rate"], "date": r["date"], "doc_id": r["doc_id"], "edition": edition,
            "fetched_at": scan["fetched_at"] or "", "lang": quality_feed.lang_of(r["page_lang"]),
            "lccn": r["lccn"], "scan": scan["scan"], "seq": seq, "sha256": scan["sha256"]}


def manifest_row(r):
    """The fetcher's row, as ocr_manifest.py build-quality makes it: `eng` for the buffer set."""
    return ocr_manifest.row_of(
        {"doc_id": r["doc_id"], "batch": r["batch"], "batch_version": r["batch_version"], "language": "eng"},
        {r["doc_id"]: {"lang": "eng", "decision": r["decision"], "damage_rate": r["damage_rate"]}})


def manifest_slices(rows, pages):
    """Consecutive slices of about `pages` rows. A slice ends only where an issue does, and
    always where a tier does: a manifest is written in issue order, so one that crossed a
    tier could put a page ahead of one more than a tier width worse."""
    out, cur, last, tier = [], [], None, None
    for r in rows:
        issue = ocr_manifest.issue_of(r["doc_id"])
        if cur and (r.get("tier") != tier or (len(cur) >= pages and issue != last)):
            out.append(cur)
            cur = []
        cur.append(r)
        last, tier = issue, r.get("tier")
    return out + [cur] if cur else out


def write_atomic(path, body):
    with open(path + ".tmp", "wb") as f:
        f.write(body)
    os.replace(path + ".tmp", path)


def write_lines(path, rows):
    with open(path + ".tmp", "w") as f:
        for row in rows:
            f.write(json.dumps(row, sort_keys=True) + "\n")
    os.replace(path + ".tmp", path)


def write_campaign(out_dir, name, rows, extras, scans, manifest_pages):
    """plan.jsonl, pages.jsonl and the manifests of one campaign; how many pages went where."""
    camp = os.path.join(out_dir, name)
    man_dir = os.path.join(camp, "manifests")
    os.makedirs(man_dir, exist_ok=True)
    write_lines(os.path.join(camp, "plan.jsonl"),
                ({**feed_row(r, scans), "band": r["band"], "decade": r["decade"], "decision": r["decision"],
                  "page_lang": r["page_lang"], "rank": i, **{k: r[k] for k in extras}}
                 for i, r in enumerate(rows, 1)))
    write_lines(os.path.join(camp, "pages.jsonl"), (feed_row(r, scans) for r in rows if scan_key(r) in scans))
    slices = manifest_slices([r for r in rows if scan_key(r) not in scans], manifest_pages)
    for i, chunk in enumerate(slices, 1):
        write_atomic(os.path.join(man_dir, f"m{i:05d}.parquet"),
                     ocr_manifest.manifest_bytes([manifest_row(r) for r in chunk]))
    return {"manifests": len(slices), "on_scan_server": sum(1 for r in rows if scan_key(r) in scans),
            "to_fetch": sum(len(s) for s in slices)}


def tally(rows):
    """Counts of a list of pages, sorted so the summary reads the same every run."""
    lang = collections.Counter(r["page_lang"] for r in rows)
    decade = collections.Counter(r["decade"] for r in rows)
    return {"batches": len({(r["batch"], r["batch_version"]) for r in rows}),
            "by_decade": {str(k): decade[k] for k in sorted(decade)},
            "by_language": dict(sorted(lang.items(), key=lambda kv: (-kv[1], kv[0]))),
            "issues": len({ocr_manifest.issue_of(r["doc_id"]) for r in rows}),
            "pages": len(rows), "titles": len({r["lccn"] for r in rows})}


def input_digest(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def build(a):
    names = (a.campaign, f"{a.campaign}-moderate")
    if not all(quality_feed.CAMPAIGN_RE.match(n) for n in names):
        sys.exit("--campaign: 1-32 of [a-z0-9-], so that <campaign>-moderate is a campaign name too")
    dirs = [os.path.join(a.out_dir, n) for n in names]
    for path in (os.path.abspath(a.out_dir), os.path.realpath(a.out_dir)):
        if "outbox" in path.split(os.sep):
            sys.exit("--out-dir: this tool writes no outbox; give it a scratch directory")
    if any(os.path.lexists(d) for d in dirs):
        # An old manifest left beside a new plan could fetch a page the plan no longer has.
        sys.exit("--out-dir already holds this campaign; give it a new directory")
    os.makedirs(a.out_dir, exist_ok=True)
    source = a.input
    if a.dsn_file:
        source = os.path.join(a.out_dir, "input.csv")
        export(a.dsn_file, source)
    if not os.path.isdir(a.ocr_state):
        sys.exit(f"--ocr-state {a.ocr_state}: no such directory")
    for path in a.exclude_list:
        if not os.path.exists(path):
            sys.exit(f"--exclude-list {path}: no such file")
    lists = {path: quality_feed.read_lines(path) for path in a.exclude_list}
    scans = read_index(a.index)
    ocr_held = ocr_pages(a.ocr_state)

    def excluded_by(r):
        if r["has_ocr_output"]:
            return "quality.ocr_outputs"
        if r["doc_id"] in ocr_held:
            return "OCR queue or import"
        if not a.campaign_exclusions:
            return None
        if r["in_campaign"]:
            return "quality.unit_pages or quality.staging"
        return next((path for path, docs in lists.items() if r["doc_id"] in docs), None)

    def eligible(r):
        return r["band"] == "moderate" and quality_feed.DOC_RE.match(r["doc_id"]) and not excluded_by(r)

    # The first read: the worst pages, and the size of each moderate stratum.
    population = {"worst": collections.Counter(), "moderate": collections.Counter()}
    excluded = {"worst": collections.Counter(), "moderate": collections.Counter()}
    bad_ids = 0
    worst = []
    sizes = collections.Counter()
    rows_read = 0
    pop_decade = {"worst": collections.Counter(), "moderate": collections.Counter()}
    in_a_campaign = collections.Counter()
    for r in read_rows(source):
        rows_read += 1
        population[r["band"]][r["page_lang"]] += 1
        pop_decade[r["band"]][r["decade"]] += 1
        in_a_campaign[r["band"]] += r["in_campaign"]
        if not quality_feed.DOC_RE.match(r["doc_id"]):
            bad_ids += 1
            continue
        why = excluded_by(r)
        if why:
            excluded[r["band"]][why] += 1
        elif r["band"] == "worst":
            worst.append(r)
        else:
            sizes[(r["page_lang"], r["decade"], r["lccn"])] += 1

    take = allocate(sizes, a.cell_pages, a.seed)
    moderate = weigh(draw(source, eligible, take, a.seed), sizes, take, a.cell_pages)
    worst = order_worst(worst, a.tier_width)
    for r in worst:
        r["tier"] = tier_of(r["damage_rate"], a.tier_width)
    wrote = {
        "worst": write_campaign(a.out_dir, names[0], worst, ("tier",), scans, a.manifest_pages),
        "moderate": write_campaign(a.out_dir, names[1], moderate, ("stratum_pages", "weight"), scans,
                                   a.manifest_pages),
    }
    cells = collections.Counter()
    drawn = collections.Counter()
    per_title = max(take.values(), default=0)
    for (lang, decade, _), size in sizes.items():
        cells[(lang, decade)] += size
    for r in moderate:
        drawn[(r["page_lang"], r["decade"])] += 1
    tiers = collections.Counter(r["tier"] for r in worst)
    bins = collections.Counter(min(int((r["damage_rate"] - 0.1) / 0.05), 2) for r in moderate)

    def popular(band):
        lang, decade = population[band], pop_decade[band]
        return {"by_decade": {str(k): decade[k] for k in sorted(decade)},
                "by_language": dict(sorted(lang.items(), key=lambda kv: (-kv[1], kv[0]))),
                "excluded": dict(sorted(excluded[band].items())), "in_a_campaign": in_a_campaign[band],
                "pages": sum(lang.values())}

    summary = {
        "bad_doc_ids": bad_ids,
        "campaign_exclusions": a.campaign_exclusions,
        "campaigns": {"moderate": names[1], "worst": names[0]},
        "exclude_lists": {path: {"docs": len(lists[path]), "sha256": input_digest(path)} for path in sorted(lists)},
        "input": {"rows": rows_read, "sha256": input_digest(source)},
        "ocr_state": {"docs": len(ocr_held), "sha256": hashlib.sha256("\n".join(sorted(ocr_held)).encode()).hexdigest()},
        "moderate": {
            **tally(moderate), **wrote["moderate"],
            "by_damage_bin": {("0.10-0.15", "0.15-0.20", "0.20-0.25")[k]: bins[k] for k in sorted(bins)},
            "cell_pages": a.cell_pages, "cells": len(cells),
            "cells_at_cell_pages": sum(1 for c in cells if drawn[c] >= a.cell_pages),
            "most_pages_from_a_title_in_a_cell": per_title,
            "weights_sum": round(sum(r["weight"] for r in moderate), 3),
        },
        "population": {band: popular(band) for band in ("moderate", "worst")},
        "seed": a.seed,
        "worst": {
            **tally(worst), **wrote["worst"],
            "by_tier": {f"{1.0 - (k + 1) * a.tier_width:.2f}-{1.0 - k * a.tier_width:.2f}": tiers[k]
                        for k in sorted(tiers)},
        },
    }
    write_atomic(os.path.join(a.out_dir, "summary.json"), (json.dumps(summary, indent=1, sort_keys=True) + "\n").encode())
    print(json.dumps(summary, indent=1, sort_keys=True), flush=True)
    return summary


def parser():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter,
                                 allow_abbrev=False)
    ap.add_argument("--campaign", required=True, help="the worst list's name; the sample is <campaign>-moderate")
    ap.add_argument("--seed", required=True, help="the moderate sample's seed")
    ap.add_argument("--out-dir", required=True, help="a scratch directory; never an outbox")
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("--dsn-file", help="usnm_quality's libpq DSN: read with a read-only transaction")
    src.add_argument("--input", help="an input.csv from an earlier run, to rebuild from")
    ap.add_argument("--ocr-state", required=True, help="ocr_manifest.py build's --state directory")
    ap.add_argument("--campaign-exclusions", action="store_true",
                    help="also leave out pages in a quality campaign, and pages in --exclude-list files")
    ap.add_argument("--exclude-list", action="append", default=[],
                    help="doc ids to leave out, with --campaign-exclusions; may be repeated")
    ap.add_argument("--index", help="the scan server's index/full.jsonl, or a copy")
    ap.add_argument("--tier-width", type=float, default=0.05)
    ap.add_argument("--cell-pages", type=int, default=400, help="moderate pages per language and decade")
    ap.add_argument("--manifest-pages", type=int, default=2000)
    return ap


def main(argv=None):
    ap = parser()
    a = ap.parse_args(argv)
    if not 0 < a.tier_width <= 1 or a.cell_pages < 1 or a.manifest_pages < 1:
        ap.error("--tier-width over 0 to 1, --cell-pages and --manifest-pages positive")
    if not a.seed:
        ap.error("--seed must not be empty")
    if a.exclude_list and not a.campaign_exclusions:
        ap.error("--exclude-list needs --campaign-exclusions")
    build(a)


if __name__ == "__main__":
    main()
