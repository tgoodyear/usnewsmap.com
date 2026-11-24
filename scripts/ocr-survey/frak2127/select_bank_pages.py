#!/usr/bin/env python3
"""Freeze the US German Chronicling America pages for arm C's glyph bank (#2127, amendment 1).

Frame. A page is a candidate when all of these hold:
  - its title (LCCN) is a German title: one of the LCCNs of the German sample of the Fraktur pilot
    (ocr/fraktur-pilot/pages.jsonl, sample "ger"; each of those pages' language was detected as German);
  - its scan is in the scan host's keep set (--keep-listing: <batch>/<lccn>/<date>/<ed>/<seq>.jp2|jpg
    with sizes); a page with both a JP2 and a JPEG counts once, as the JP2;
  - LoC's ALTO for it is in the bulk store: the batch's archive is in --locbulk-listing and the
    batch's locstats table (--locstats-dir, one parquet per archive) has the page with words > 0;
  - where the batch has page-level language decisions (--langs-dir, qeloc), the page's is "ger";
    a batch without them keeps its German title's pages;
  - it is in no US German evaluation artefact (--exclude, files or folders: every page id found in
    their file names or contents), and not one of #835's twelve 1910 iahi_feebas pages, whose ids are
    not listed anywhere we hold, so every 1910 sn84027101 page of iahi_feebas is left out.

Selection. Pages are ranked by sha256("2127:bank:" + page_id). Each title's pages are queued
decade by decade: its best-ranked page of each decade in chronological order, then each decade's
second, and so on. Titles are visited in the order of sha256("2127:bank:title:" + lccn), taking the
next queued page of each in turn, until --n pages are chosen; no title gives more than --cap.

Outputs in --out: pages.txt (sorted ids, one per line, trailing newline), pages.jsonl (one record a
page), exclusion-manifest.json (the pages future US evaluations must leave out, with their sha256),
summary.json (counts by title, decade and exclusion reason; inputs' sha256).

    select_bank_pages.py --pilot-pages ocr/fraktur-pilot/pages.jsonl --keep-listing keep.tsv \
        --locbulk-listing locbulk.txt --locstats-dir locstats --langs-dir langs \
        --exclude de968 --exclude check-de --exclude calib --exclude ocr/fraktur-pilot/pages.jsonl \
        --n 250 --cap 15 --out bank-pages
"""
import argparse, collections, glob, hashlib, json, os, re, sys

import pyarrow.parquet as pq

PAGE_RE = re.compile(r"([a-z]{0,3}\d{8,10})[_/](\d{4}-\d{2}-\d{2})[_/]ed-(\d+)[_/]seq-(\d+)")
SCAN_RE = re.compile(r"^([a-z0-9_]+)/([a-z]{0,3}\d{8,10})/(\d{4}-\d{2}-\d{2})/(\d+)/(\d+)\.(jp2|jpg)$")
# #835's first quality check: 12 published iahi_feebas pages of sn84027101 from 1910, ids not kept.
RULE_835 = {"batch": "iahi_feebas", "lccn": "sn84027101", "year": "1910"}


def key(s):
    return hashlib.sha256(s.encode()).hexdigest()


def sha256_file(p):
    return hashlib.sha256(open(p, "rb").read()).hexdigest()


def ids_in(path):
    """Every page id in a file's name or text, or in a folder's files' names and texts."""
    out = set()
    files = [path] if os.path.isfile(path) else [os.path.join(d, f) for d, _, fs in os.walk(path) for f in fs]
    for f in files:
        texts = [os.path.basename(f)]
        if f.endswith((".json", ".jsonl", ".txt", ".tsv", ".csv")):
            texts.append(open(f, encoding="utf-8", errors="replace").read())
        for t in texts:
            for m in PAGE_RE.finditer(t):
                out.add(f"{m.group(1)}_{m.group(2)}_ed-{int(m.group(3))}_seq-{int(m.group(4))}")
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--pilot-pages", required=True); ap.add_argument("--keep-listing", required=True)
    ap.add_argument("--locbulk-listing", required=True); ap.add_argument("--locstats-dir", required=True)
    ap.add_argument("--langs-dir", required=True)
    ap.add_argument("--exclude", action="append", required=True)
    ap.add_argument("--n", type=int, default=250); ap.add_argument("--cap", type=int, default=15)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()

    titles = sorted({r["lccn"] for r in map(json.loads, open(a.pilot_pages)) if r["sample"] == "ger"})
    tset = set(titles)
    # Scans.
    scans, bad = {}, 0
    for line in open(a.keep_listing):
        path, size = line.rstrip("\n").split("\t")
        m = SCAN_RE.match(path)
        if not m:
            bad += 1
            continue
        batch, lccn, date, ed, seq, fmt = m.groups()
        if lccn not in tset:
            continue
        pid = f"{lccn}_{date}_ed-{int(ed)}_seq-{int(seq)}"
        rec = {"page_id": pid, "lccn": lccn, "batch": batch, "date": date, "decade": int(date[:3]) * 10,
               "scan_path": "keep/" + path, "scan_format": fmt, "scan_bytes": int(size)}
        if pid not in scans or (fmt == "jp2" and scans[pid]["scan_format"] != "jp2"):
            scans[pid] = rec
    counts = collections.Counter(candidates_scanned=len(scans), unparsed_scan_paths=bad)
    # LoC ALTO.
    archives = {}
    for name in open(a.locbulk_listing).read().split():
        m = re.match(r"(.+)_ver(\d+)\.tar\.bz2$", name)
        if m:
            archives.setdefault(m.group(1), []).append(name)
    alto, langs = {}, {}
    for p in sorted(glob.glob(os.path.join(a.locstats_dir, "*.parquet"))):
        t = pq.read_table(p, columns=["batch", "doc_id", "words", "archive"]).to_pylist()
        for r in t:
            if r["words"] > 0:
                alto[(r["batch"], r["doc_id"])] = r["archive"]
    for p in sorted(glob.glob(os.path.join(a.langs_dir, "*.tsv"))):
        batch = re.match(r"(.+)_ver\d+\.tar\.bz2\.tsv$", os.path.basename(p)).group(1)
        for line in open(p):
            f = line.rstrip("\n").split("\t")
            if len(f) >= 2:
                langs[(batch, f[0])] = f[1]
    lang_batches = {b for b, _ in langs}
    # Exclusions.
    excl_by_source = {}
    for e in a.exclude:
        excl_by_source[e] = ids_in(e)
    excluded = set().union(*excl_by_source.values())
    reasons = collections.Counter()
    frame = []
    for pid, r in sorted(scans.items()):
        arch = alto.get((r["batch"], pid))
        if r["batch"] not in archives or arch is None or arch not in archives[r["batch"]]:
            reasons["no_loc_alto"] += 1
            continue
        if r["batch"] in lang_batches and langs.get((r["batch"], pid)) != "ger":
            reasons["page_language_not_ger"] += 1
            continue
        if pid in excluded:
            reasons["in_evaluation_artefact"] += 1
            continue
        if r["batch"] == RULE_835["batch"] and r["lccn"] == RULE_835["lccn"] and r["date"].startswith(RULE_835["year"]):
            reasons["issue_835_rule"] += 1
            continue
        r["archive"] = arch
        r["rank_key"] = key("2127:bank:" + pid)
        frame.append(r)
    # Selection.
    by_title = collections.defaultdict(lambda: collections.defaultdict(list))
    for r in frame:
        by_title[r["lccn"]][r["decade"]].append(r)
    queues = {}
    for lccn, decs in by_title.items():
        for d in decs.values():
            d.sort(key=lambda r: r["rank_key"])
        q, i = [], 0
        while any(len(decs[d]) > i for d in decs):
            q.extend(decs[d][i] for d in sorted(decs) if len(decs[d]) > i)
            i += 1
        queues[lccn] = q[:a.cap]
    order = sorted(queues, key=lambda l: key("2127:bank:title:" + l))
    chosen, pos = [], {l: 0 for l in order}
    while len(chosen) < a.n and any(pos[l] < len(queues[l]) for l in order):
        for l in order:
            if len(chosen) < a.n and pos[l] < len(queues[l]):
                chosen.append(queues[l][pos[l]])
                pos[l] += 1
    assert len(chosen) == a.n, f"only {len(chosen)} pages available"
    ids = sorted(r["page_id"] for r in chosen)
    assert len(set(ids)) == len(ids) and not (set(ids) & excluded)
    os.makedirs(a.out, exist_ok=True)
    txt = "".join(i + "\n" for i in ids)
    open(os.path.join(a.out, "pages.txt"), "w").write(txt)
    digest = hashlib.sha256(txt.encode()).hexdigest()
    with open(os.path.join(a.out, "pages.jsonl"), "w") as f:
        for r in sorted(chosen, key=lambda r: r["page_id"]):
            f.write(json.dumps(r, sort_keys=True) + "\n")
    json.dump({"format": "usnm-exclusion-manifest/1", "issue": 2127,
               "why": "Pages arm C's glyph bank (#2127) takes letter images from. Leave them out of every later US German evaluation.",
               "pages_sha256": digest, "count": len(ids), "pages": ids},
              open(os.path.join(a.out, "exclusion-manifest.json"), "w"), indent=1)
    summary = {
        "pages": len(ids), "pages_sha256": digest, "n": a.n, "cap": a.cap,
        "titles_in_frame_list": len(titles), "titles_with_candidates": len(by_title), "titles_chosen": len({r["lccn"] for r in chosen}),
        "frame_pages": len(frame), "counts": dict(counts), "left_out": dict(reasons),
        "excluded_ids_by_source": {k: len(v) for k, v in excl_by_source.items()},
        "batches_with_page_language": sorted(lang_batches),
        "by_title": dict(sorted(collections.Counter(r["lccn"] for r in chosen).items())),
        "by_decade": dict(sorted(collections.Counter(r["decade"] for r in chosen).items())),
        "frame_by_decade": dict(sorted(collections.Counter(r["decade"] for r in frame).items())),
        "scan_formats": dict(collections.Counter(r["scan_format"] for r in chosen)),
        "inputs_sha256": {"pilot_pages": sha256_file(a.pilot_pages), "keep_listing": sha256_file(a.keep_listing),
                          "locbulk_listing": sha256_file(a.locbulk_listing)},
    }
    json.dump(summary, open(os.path.join(a.out, "summary.json"), "w"), indent=1)
    print(json.dumps({k: summary[k] for k in ("pages", "pages_sha256", "titles_with_candidates", "titles_chosen",
                                              "frame_pages", "left_out", "excluded_ids_by_source", "by_decade")}, indent=1))


if __name__ == "__main__":
    sys.exit(main())
