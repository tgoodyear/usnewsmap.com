#!/usr/bin/env python3
"""Weak English column labels from LoC ALTO for Chronicling America pages we hold (#2107, model v2).

No American Stories output is used anywhere: the labels come only from LoC's own ALTO (TextLine and
TextBlock geometry) and the column rule in colgeom.py, and a page is kept only when that geometry
looks clean (colgeom.FILTERS, recorded in every manifest row).

Two steps, both on a dev container:

  select  pick candidate pages. Eligible pool: scans we hold (the scans index, or --held-list listings
          of the scan store made where it lives), whose batch has an archive in locbulk, whose title
          is in --title-lang only (--titles-json, the search release's title catalogue), minus every
          reserved test page. The pool is split by decade; decades take turns (round robin), and each
          turn takes the decade's next page in sha256(PREFIX + doc_id) order that keeps its title
          under --per-title and its batch under --per-batch, until --pages. The page must be in the
          batch's locstats parquet (which archive holds its ALTO). With --batch-count N (batch-first, to cut
          streaming): first N batches in sha256(BATCH_PREFIX + batch) order, at most --batches-per-title
          per title and at most --max-decade-share of N per decade (a batch's decade is the most common
          decade of its eligible pages), then up to --per-batch pages per batch in sha256 page order. Writes pages.tsv, a manifest and
          <out>.summary.json (held, eligible and selected counts by title, batch and decade).
      alto_weak.py select --held-list keep.txt --held-list buffer.txt --scans-root /cold/ocr-scans
                          --titles-json titles.json --locstats-dir /data/locstats --locbulk-dir /data/locbulk/ocr
                          --exclude en2107=pages-300.txt --exclude bwval=bw-val.txt ...
                          --pages 500 --per-title 10 --out pages.tsv --manifest-out select-manifest.tsv

  build   stream each needed archive once (tar r|bz2; never a whole-locbulk scan), take the wanted
          pages' ocr.xml, derive column regions and the filter metrics, write labels.
      alto_weak.py build --pages pages.tsv --locbulk-dir /data/locbulk/ocr --out-dir OUT --workers 4

Exclusion lists (--exclude NAME=FILE, repeatable): one page id per line (first tab or comma field;
a header line is skipped), or JSONL with a page_id / doc_id field. A page is excluded when its id is
listed or, with --exclude-issues (default on), when any page of the same issue (LCCN, date, edition)
is listed. The manifest names the list that excluded it.

--lang ger builds German labels from the German-language Chronicling America titles the same way.

Outputs of build: OUT/labels.jsonl (accepted pages: page_id, source "loc-alto", lang, licence, image_path,
width, height, columns [[x0, y0, x1, y1]] in image pixels, gutters, metrics), OUT/manifest.tsv (every
page: status accepted | rejected | no_alto | no_image | bad_alto, failed filters, metrics),
OUT/alto/<doc_id>.xml (the ALTO read, so no archive is streamed twice), OUT/summary.json.
Progress lines: `archives i/N`.

Data: Chronicling America (Library of Congress / NEH NDNP); page images and OCR are public domain
or no known restrictions (loc.gov "Rights and Access" for Chronicling America).
"""
import argparse
import csv
import glob
import hashlib
import json
import multiprocessing as mp
import os
import re
import sys
import tarfile
import time
import xml.etree.ElementTree as ET

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import colgeom  # noqa: E402

PREFIX = "2107:colv2:"
BATCH_PREFIX = "2107:colv2:batch:"
LINE_VGAP = 2.5   # times the median line height
DOC_RE = re.compile(r"^(?P<lccn>[a-z0-9]+)_(?P<date>\d{4}-\d{2}-\d{2})_ed-(?P<ed>\d+)_seq-(?P<seq>\d+)$")


def sha_key(pid, prefix=PREFIX):
    return hashlib.sha256((prefix + pid).encode()).hexdigest()


def issue_of(doc_id):
    m = DOC_RE.match(doc_id)
    return (m.group("lccn"), m.group("date"), int(m.group("ed"))) if m else None


def member_name(doc_id):
    m = DOC_RE.match(doc_id)
    y, mo, d = m.group("date").split("-")
    return f"{m.group('lccn')}/{y}/{mo}/{d}/ed-{int(m.group('ed'))}/seq-{int(m.group('seq'))}/ocr.xml"


def doc_of_member(name):
    p = name.split("/")
    if len(p) != 7 or p[6] != "ocr.xml" or not p[4].startswith("ed-") or not p[5].startswith("seq-"):
        return None
    try:
        return f"{p[0]}_{p[1]}-{p[2]}-{p[3]}_ed-{int(p[4][3:])}_seq-{int(p[5][4:])}"
    except ValueError:
        return None


def image_rel(doc_id):
    """Path of a held scan under the scans root's <batch>/ directory: <lccn>/<date>/<ed>/<seq>.jpg."""
    m = DOC_RE.match(doc_id)
    return f"{m.group('lccn')}/{m.group('date')}/{int(m.group('ed'))}/{int(m.group('seq'))}.jpg"


# ---------------------------------------------------------------- exclusions

def read_id_list(path):
    ids = []
    with open(path) as f:
        for ln in f:
            ln = ln.strip()
            if not ln:
                continue
            if ln.startswith("{"):
                d = json.loads(ln)
                v = d.get("page_id") or d.get("doc_id")
                if v:
                    ids.append(str(v))
                continue
            v = re.split(r"[\t,]", ln)[0].strip()
            if v in ("page_id", "doc_id", "id"):
                continue
            ids.append(v)
    return ids


class Exclusions:
    def __init__(self, specs, issues=True):
        self.pages, self.issues, self.counts = {}, {}, {}
        for spec in specs or []:
            name, path = spec.split("=", 1)
            ids = read_id_list(path)
            self.counts[name] = len(ids)
            for pid in ids:
                self.pages.setdefault(pid, name)
                iss = issue_of(pid)
                if issues and iss:
                    self.issues.setdefault(iss, name)

    def reason(self, doc_id):
        if doc_id in self.pages:
            return "excluded:" + self.pages[doc_id]
        iss = issue_of(doc_id)
        if iss in self.issues:
            return "excluded_issue:" + self.issues[iss]
        return None


# ---------------------------------------------------------------- ALTO

def _local(tag):
    return tag.rsplit("}", 1)[-1]


def _num(el, *keys):
    try:
        return [float(el.get(k)) for k in keys]
    except (TypeError, ValueError):
        return None


def parse_alto(xml, image_size):
    """Lines, blocks (boxes [x0, y0, x1, y1] in image pixels), words (text) and the ALTO page size.

    Coordinates are mapped by proportion (image size / ALTO Page WIDTH, HEIGHT), as #2103 and #2107
    step 2 (layout_alto/altolayout.py) do. A line's box is its own attributes, else the union of its
    String boxes."""
    root = ET.fromstring(xml)
    page = next((e for e in root.iter() if _local(e.tag) == "Page"), None)
    d = _num(page, "WIDTH", "HEIGHT") if page is not None else None
    if not d or d[0] <= 0 or d[1] <= 0:
        raise ValueError("ALTO has no usable Page WIDTH/HEIGHT")
    sx, sy = image_size[0] / d[0], image_size[1] / d[1]

    def box(el):
        v = _num(el, "HPOS", "VPOS", "WIDTH", "HEIGHT")
        if v is None or v[2] <= 0 or v[3] <= 0:
            return None
        return [v[0] * sx, v[1] * sy, (v[0] + v[2]) * sx, (v[1] + v[3]) * sy]

    lines, blocks, words, langs = [], [], [], {}
    block_lang = None
    for el in root.iter():
        t = _local(el.tag)
        if t == "TextBlock":
            b = box(el)
            if b:
                blocks.append(b)
            block_lang = colgeom.norm_lang(el.get("language") or el.get("LANG") or el.get("lang"))
        elif t == "TextLine":
            b = box(el)
            sb = []
            for s in el:
                if _local(s.tag) == "String":
                    if s.get("CONTENT"):
                        words.append(s.get("CONTENT"))
                    x = box(s)
                    if x:
                        sb.append(x)
            if b is None and sb:
                b = [min(x[0] for x in sb), min(x[1] for x in sb), max(x[2] for x in sb), max(x[3] for x in sb)]
            if b:
                lines.append(b)
                if block_lang:
                    langs[block_lang] = langs.get(block_lang, 0) + 1
    return {"lines": lines, "blocks": blocks, "words": words, "alto_size": (d[0], d[1]), "langs": langs}


def label_page(parsed, width, height, lang="eng"):
    """Column regions, gutters and filter metrics for one parsed ALTO page."""
    lines = parsed["lines"]
    if lines:
        hs = sorted(b[3] - b[1] for b in lines)
        body_h = hs[len(hs) // 2]
    else:
        body_h = 0.0
    vgap = LINE_VGAP * body_h
    regions = colgeom.columnize(lines, width, height, max_vgap=vgap, min_members=3)
    gutters = colgeom.gutters_from_regions(regions, width, height)
    aw, ah = parsed["alto_size"]
    langs = parsed.get("langs") or {}
    alto_lang = max(sorted(langs), key=lambda k: langs[k]) if langs else None
    share = colgeom.lang_share(parsed["words"], lang)
    metrics = {
        "aspect_err": abs((aw / ah) / (width / height) - 1.0),
        "block_align": colgeom.block_align(parsed["blocks"], regions),
        "columns": len(regions),
        "coverage": colgeom.coverage(lines, regions),
        "cross_share": colgeom.cross_share(lines, gutters, width, bridge=vgap, body_h=body_h),
        "alto_lang": alto_lang,
        "lang_ok": colgeom.lang_ok(alto_lang, share, lang),
        "lang_share": share,
        "gutters": len(gutters),
        "lines": len(lines),
        "width_consistency": colgeom.width_consistency(regions),
    }
    ok, failed = colgeom.judge(metrics)
    return {"columns": regions, "gutters": gutters, "metrics": metrics, "passed": ok, "failed": failed}


def image_size(path):
    from PIL import Image
    with Image.open(path) as im:
        return im.size


# ---------------------------------------------------------------- select

def _locstats_rows(locstats_dir, batches):
    import pyarrow.parquet as pq
    out = {}
    for b in sorted(batches):
        for p in sorted(glob.glob(os.path.join(locstats_dir, f"{b}_ver*.parquet"))):
            t = pq.read_table(p, columns=["batch", "batch_version", "doc_id", "archive", "words"])
            for r in t.to_pylist():
                out.setdefault((r["batch"], r["doc_id"]), []).append(r)
    return out


def held_entry(rel):
    """(doc_id, batch) of a held-scan path [keep/|buffer/]<batch>/<lccn>/<yyyy-mm-dd>/<ed>/<seq>.jpg|.jp2, else (None, None)."""
    p = rel.strip().strip("/").split("/")
    if p and p[0] in ("keep", "buffer"):
        p = p[1:]
    if len(p) != 5 or not (p[4].endswith(".jpg") or p[4].endswith(".jp2")):
        return None, None
    try:
        did = "%s_%s_ed-%d_seq-%d" % (p[1], p[2], int(p[3]), int(p[4][:-4]))
    except ValueError:
        return None, None
    return (did, p[0]) if DOC_RE.match(did) else (None, None)


def decade_of(doc_id):
    return int(DOC_RE.match(doc_id).group("date")[:3]) * 10


def read_titles(path):
    """lccn -> list of language codes from a titles.json (the search release's title catalogue)."""
    with open(path) as f:
        return {t["lccn"]: [colgeom.norm_lang(x) for x in t.get("languages") or []] for t in json.load(f)}


def _archives(locbulk_dir):
    out = {}
    for fn in os.listdir(locbulk_dir):
        m = re.match(r"^(.+)_ver(\d+)\.tar\.bz2$", fn)
        if m:
            out.setdefault(m.group(1), []).append(fn)
    return out


def cmd_select(a):
    """Eligible pool -> sha256-ordered, decade-balanced, per-title and per-batch capped selection."""
    excl = Exclusions(a.exclude, issues=a.exclude_issues)
    held = {}   # batch -> {doc_id: path relative to --scans-root, or None (scans index form)}
    if a.held_list:
        for path in a.held_list:
            with open(path) as f:
                for ln in f:
                    did, b = held_entry(ln)
                    if did:
                        held.setdefault(b, {}).setdefault(did, ln.strip().strip("/"))
    else:
        with open(a.scans_index) as f:
            for ln in f:
                if not ln.strip():
                    continue
                r = json.loads(ln)
                did, b = r.get("doc_id"), r.get("batch")
                if did and b and DOC_RE.match(did):
                    held.setdefault(b, {})[did] = None
    if a.only_batches:
        keep = set(read_id_list(a.only_batches))
        held = {b: v for b, v in held.items() if b in keep}
    titles = read_titles(a.titles_json) if a.titles_json else None
    archives = _archives(a.locbulk_dir)
    stats = {"held_pages": 0, "held_batches": len(held), "held_titles": set(), "no_archive_pages": 0,
             "no_archive_batches": 0, "not_target_language_pages": 0, "unknown_title_pages": 0, "excluded": {},
             "eligible_pages": 0, "eligible_titles": set(), "eligible_batches": set(), "eligible_by_decade": {},
             "eligible_titles_by_decade": {}}
    pool = []   # (decade, sha key, doc_id, batch, rel)
    manifest = []
    for b in sorted(held):
        has_arch = b in archives
        if not has_arch:
            stats["no_archive_batches"] += 1
        for did, rel in held[b].items():
            stats["held_pages"] += 1
            lccn = DOC_RE.match(did).group("lccn")
            stats["held_titles"].add(lccn)
            if not has_arch:
                stats["no_archive_pages"] += 1
                manifest.append({"doc_id": did, "batch": b, "status": "no_locbulk_archive"})
                continue
            if titles is not None:
                langs = titles.get(lccn)
                if langs is None:
                    stats["unknown_title_pages"] += 1
                    manifest.append({"doc_id": did, "batch": b, "status": "title_not_in_catalogue"})
                    continue
                if langs != [a.title_lang]:
                    stats["not_target_language_pages"] += 1
                    manifest.append({"doc_id": did, "batch": b, "status": "title_language:" + "+".join(map(str, langs))})
                    continue
            why = excl.reason(did)
            if why:
                stats["excluded"][why] = stats["excluded"].get(why, 0) + 1
                manifest.append({"doc_id": did, "batch": b, "status": why})
                continue
            dec = decade_of(did)
            pool.append((dec, sha_key(did), did, b, rel))
            stats["eligible_pages"] += 1
            stats["eligible_titles"].add(lccn)
            stats["eligible_batches"].add(b)
            stats["eligible_by_decade"][dec] = stats["eligible_by_decade"].get(dec, 0) + 1
            stats["eligible_titles_by_decade"].setdefault(dec, set()).add(lccn)
    stat_cache = {}
    per_title, per_batch, chosen = {}, {}, []

    def take(did, b, rel, d):
        """Add a page if it is in its batch's locstats; returns True when taken."""
        if b not in stat_cache:
            stat_cache[b] = _locstats_rows(a.locstats_dir, [b])
        rows = stat_cache[b].get((b, did), [])
        if not rows:
            manifest.append({"doc_id": did, "batch": b, "status": "not_in_locstats"})
            return False
        lccn = DOC_RE.match(did).group("lccn")
        arch = sorted(rows, key=lambda r: -r["batch_version"])[0]["archive"]
        img = os.path.join(a.scans_root, rel) if rel else os.path.join(a.scans_root, b, image_rel(did))
        chosen.append({"doc_id": did, "batch": b, "archive": arch, "image_path": img, "lccn": lccn, "decade": d,
                       "source_rel": rel or ""})
        manifest.append({"doc_id": did, "batch": b, "status": "selected"})
        per_title[lccn] = per_title.get(lccn, 0) + 1
        per_batch[b] = per_batch.get(b, 0) + 1
        return True

    batch_pick = None
    if a.batch_count:
        # batch-first: batches in sha256(BATCH_PREFIX + batch) order; a batch is taken while every eligible title in
        # it has fewer than --batches-per-title batches and its decade (the most common decade of its eligible
        # pages) has fewer than --max-decade-share of --batch-count; then up to --per-batch pages from each taken
        # batch in sha256 page order, under --per-title, until --pages.
        pages_of = {}
        for p in pool:
            pages_of.setdefault(p[3], []).append(p)
        dec_of = {}
        for bb, ps in pages_of.items():
            cnt = {}
            for p in ps:
                cnt[p[0]] = cnt.get(p[0], 0) + 1
            dec_of[bb] = max(sorted(cnt), key=lambda d: cnt[d])
        dec_cap = max(1, int(a.max_decade_share * a.batch_count))
        title_batches, dec_batches, batch_pick = {}, {}, []
        for bb in sorted(pages_of, key=lambda x: (hashlib.sha256((BATCH_PREFIX + x).encode()).hexdigest(), x)):
            if len(batch_pick) >= a.batch_count:
                break
            ts = {DOC_RE.match(p[2]).group("lccn") for p in pages_of[bb]}
            if dec_batches.get(dec_of[bb], 0) >= dec_cap or any(title_batches.get(t, 0) >= a.batches_per_title for t in ts):
                continue
            batch_pick.append(bb)
            dec_batches[dec_of[bb]] = dec_batches.get(dec_of[bb], 0) + 1
            for t in ts:
                title_batches[t] = title_batches.get(t, 0) + 1
        for bb in batch_pick:
            n = 0
            for d, _, did, _, rel in sorted(pages_of[bb], key=lambda p: (p[1], p[2])):
                if len(chosen) >= a.pages or n >= a.per_batch:
                    break
                if per_title.get(DOC_RE.match(did).group("lccn"), 0) >= a.per_title:
                    continue
                n += take(did, bb, rel, d)
            print("batches %d/%d pages %d/%d" % (batch_pick.index(bb) + 1, len(batch_pick), len(chosen), a.pages), flush=True)
    else:
        by_dec = {}
        for p in sorted(pool):
            by_dec.setdefault(p[0], []).append(p)
        decs = sorted(by_dec)
        pos = {d: 0 for d in decs}
        # round robin over decades; within a decade, the next page in sha256 order that the caps allow
        while len(chosen) < a.pages and any(pos[d] < len(by_dec[d]) for d in decs):
            for d in decs:
                if len(chosen) >= a.pages:
                    break
                lst = by_dec[d]
                while pos[d] < len(lst):
                    _, _, did, b, rel = lst[pos[d]]
                    pos[d] += 1
                    lccn = DOC_RE.match(did).group("lccn")
                    if per_title.get(lccn, 0) >= a.per_title or per_batch.get(b, 0) >= a.per_batch:
                        continue
                    if take(did, b, rel, d):
                        break
            print("selected %d/%d" % (len(chosen), a.pages), flush=True)
    with open(a.out, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=["doc_id", "batch", "archive", "image_path", "lccn", "decade", "source_rel"],
                           delimiter="\t")
        w.writeheader()
        w.writerows(chosen)
    with open(a.manifest_out, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=["doc_id", "batch", "status"], delimiter="\t")
        w.writeheader()
        w.writerows(sorted(manifest, key=lambda m: m["doc_id"]))
    picked = {m["doc_id"] for m in manifest}
    manifest += [{"doc_id": p[2], "batch": p[3], "status": "eligible_not_drawn"} for p in pool if p[2] not in picked]
    summ = {k: (len(v) if isinstance(v, set) else v) for k, v in stats.items() if k != "eligible_titles_by_decade"}
    summ["eligible_titles_by_decade"] = {d: len(v) for d, v in sorted(stats["eligible_titles_by_decade"].items())}
    summ.update({"exclusion_lists": excl.counts, "pages": len(chosen), "prefix": PREFIX, "per_title": a.per_title,
                 "per_batch": a.per_batch, "title_lang": a.title_lang if titles is not None else None,
                 "selected_titles": len(per_title), "selected_batches": len(per_batch),
                 "selected_by_decade": _count(c["decade"] for c in chosen),
                 "batch_first": bool(a.batch_count), "batches_picked": batch_pick,
                 "archives_bytes": sum(os.path.getsize(os.path.join(a.locbulk_dir, x))
                                       for x in sorted({c["archive"] for c in chosen}))})
    with open(a.out + ".summary.json", "w") as f:
        json.dump(summ, f, indent=1, sort_keys=True)
    print(json.dumps(summ, sort_keys=True))
    return 0


def _count(it):
    out = {}
    for x in it:
        out[x] = out.get(x, 0) + 1
    return dict(sorted(out.items()))


# ---------------------------------------------------------------- build

def scan_archive(job):
    """Stream one archive; return {doc_id: ALTO bytes} for the wanted pages (members need not be adjacent)."""
    tar_path, wanted, alto_dir = job
    want = set(wanted)
    got = {}
    for did in list(want):
        p = os.path.join(alto_dir, did + ".xml")
        if os.path.exists(p):
            got[did] = p
            want.discard(did)
    if want:
        with tarfile.open(tar_path, "r|bz2") as tf:
            for m in tf:
                did = doc_of_member(m.name)
                if did is None or did not in want:
                    continue
                data = tf.extractfile(m).read()
                p = os.path.join(alto_dir, did + ".xml")
                with open(p + ".part", "wb") as f:
                    f.write(data)
                os.replace(p + ".part", p)
                got[did] = p
                want.discard(did)
                if not want:
                    break
    return tar_path, got


def _metric_cells(m):
    return {k: ("" if v is None else (round(v, 4) if isinstance(v, float) else v)) for k, v in m.items()}


def cmd_build(a):
    os.makedirs(os.path.join(a.out_dir, "alto"), exist_ok=True)
    with open(a.pages, newline="") as f:
        pages = list(csv.DictReader(f, delimiter="\t"))
    by_arch = {}
    for p in pages:
        by_arch.setdefault(os.path.join(a.locbulk_dir, p["archive"]), []).append(p["doc_id"])
    jobs = [(t, ids, os.path.join(a.out_dir, "alto")) for t, ids in sorted(by_arch.items())]
    found = {}
    t0 = time.time()
    with mp.Pool(max(1, a.workers)) as pool:
        for i, (tar_path, got) in enumerate(pool.imap_unordered(scan_archive, jobs), 1):
            found.update(got)
            print(f"archives {i}/{len(jobs)} pages {len(found)}/{len(pages)} {time.time() - t0:.0f}s", flush=True)
    rows, labels = [], []
    for p in sorted(pages, key=lambda p: p["doc_id"]):
        did = p["doc_id"]
        row = {"doc_id": did, "batch": p["batch"], "status": "", "failed": ""}
        if did not in found:
            row["status"] = "no_alto"
        elif not os.path.exists(p["image_path"]):
            row["status"] = "no_image"
        else:
            w, h = image_size(p["image_path"])
            try:
                with open(found[did], "rb") as f:
                    parsed = parse_alto(f.read(), (w, h))
            except (ValueError, ET.ParseError) as e:
                row["status"] = "bad_alto"
                row["failed"] = str(e)[:80]
                parsed = None
            if parsed is not None:
                lab = label_page(parsed, w, h, a.lang)
                row.update(_metric_cells(lab["metrics"]))
                row["status"] = "accepted" if lab["passed"] else "rejected"
                row["failed"] = ",".join(lab["failed"])
                if lab["passed"]:
                    labels.append({"columns": lab["columns"], "gutters": lab["gutters"], "height": h,
                                   "image_path": p["image_path"], "licence": "public domain (LoC Chronicling America)",
                                   "lang": a.lang, "metrics": lab["metrics"], "page_id": did, "source": "loc-alto",
                                   "width": w})
        rows.append(row)
    keys = ["doc_id", "batch", "status", "failed"] + [m for m, _, _ in colgeom.FILTERS] + ["alto_lang", "lang_share"]
    with open(os.path.join(a.out_dir, "manifest.tsv"), "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=keys, delimiter="\t", extrasaction="ignore")
        w.writeheader()
        w.writerows(rows)
    with open(os.path.join(a.out_dir, "labels.jsonl"), "w") as f:
        for lab in labels:
            f.write(json.dumps(lab, sort_keys=True) + "\n")
    fail_counts = _count(x for r in rows for x in r["failed"].split(",") if r["status"] == "rejected" and x)
    summ = {"accepted": len(labels), "pages": len(rows), "statuses": _count(r["status"] for r in rows), "lang": a.lang,
            "alto_lang": _count(str(r.get("alto_lang") or "none") for r in rows if r["status"] in ("accepted", "rejected")),
            "rejected_in_lang_by_filter": _count(x for r in rows for x in r["failed"].split(",")
                                                 if r["status"] == "rejected" and x and "lang_ok" not in r["failed"]),
            "rejected_by_filter": fail_counts, "filters": colgeom.filter_spec(),
            "columns_per_accepted_page_mean": (sum(len(x["columns"]) for x in labels) / len(labels)) if labels else None}
    with open(os.path.join(a.out_dir, "summary.json"), "w") as f:
        json.dump(summ, f, indent=1, sort_keys=True)
    print(json.dumps(summ, sort_keys=True))
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("select")
    s.add_argument("--scans-index", help="scans index JSONL (doc_id, batch, batch_version); or use --held-list")
    s.add_argument("--held-list", action="append", help="held scan paths [keep/|buffer/]<batch>/<lccn>/<date>/<ed>/<seq>.jpg,"
                   " one per line, relative to --scans-root (a listing made where the scans live; files need not be"
                   " present here yet); repeatable")
    s.add_argument("--titles-json", help="title catalogue (lccn, languages); with it, only titles whose languages are"
                   " exactly [--title-lang] are eligible")
    s.add_argument("--title-lang", default="eng")
    s.add_argument("--per-title", type=int, default=10)
    s.add_argument("--scans-root", required=True)
    s.add_argument("--locstats-dir", required=True)
    s.add_argument("--locbulk-dir", required=True)
    s.add_argument("--exclude", action="append", default=[], help="NAME=FILE, repeatable")
    s.add_argument("--exclude-issues", action=argparse.BooleanOptionalAction, default=True)
    s.add_argument("--only-batches", help="file of batch names (one per line) to draw from, e.g. the batches whose"
                   " ALTO is in the target language")
    s.add_argument("--pages", type=int, default=500)
    s.add_argument("--per-batch", type=int, default=1000000)
    s.add_argument("--batch-count", type=int, default=0, help="batch-first selection: take this many batches first")
    s.add_argument("--batches-per-title", type=int, default=2)
    s.add_argument("--max-decade-share", type=float, default=0.15)
    s.add_argument("--out", required=True)
    s.add_argument("--manifest-out", required=True)
    b = sub.add_parser("build")
    b.add_argument("--pages", required=True)
    b.add_argument("--locbulk-dir", required=True)
    b.add_argument("--out-dir", required=True)
    b.add_argument("--workers", type=int, default=2)
    b.add_argument("--lang", default="eng", choices=sorted(colgeom.LANG_WORDS), help="target language of the labels")
    a = ap.parse_args(argv)
    return cmd_select(a) if a.cmd == "select" else cmd_build(a)


if __name__ == "__main__":
    sys.exit(main())
