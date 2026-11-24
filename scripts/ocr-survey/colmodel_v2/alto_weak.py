#!/usr/bin/env python3
"""Weak English column labels from LoC ALTO for Chronicling America pages we hold (#2107, model v2).

No American Stories output is used anywhere: the labels come only from LoC's own ALTO (TextLine and
TextBlock geometry) and the column rule in colgeom.py, and a page is kept only when that geometry
looks clean (colgeom.FILTERS, recorded in every manifest row).

Two steps, both on a dev container:

  select  pick candidate pages: scans we hold (the scans index, image file present) joined to the
          per-batch locstats parquet index (which archive holds the page's ALTO), minus every
          reserved test page, in sha256(PREFIX + doc_id) order, at most --per-batch pages from each
          of at most --batches batches (batches also in sha256(PREFIX + batch) order), until --pages.
      alto_weak.py select --scans-index index-full.jsonl --scans-root /tank/ocr/scans/keep
                          --locstats-dir /data/locstats --locbulk-dir /data/locbulk/ocr
                          --exclude en2107=pages-300.txt --exclude bwval=bw-val.txt ...
                          --pages 500 --per-batch 50 --out pages.tsv --manifest-out select-manifest.tsv

  build   stream each needed archive once (tar r|bz2; never a whole-locbulk scan), take the wanted
          pages' ocr.xml, derive column regions and the filter metrics, write labels.
      alto_weak.py build --pages pages.tsv --locbulk-dir /data/locbulk/ocr --out-dir OUT --workers 4

Exclusion lists (--exclude NAME=FILE, repeatable): one page id per line (first tab or comma field;
a header line is skipped), or JSONL with a page_id / doc_id field. A page is excluded when its id is
listed or, with --exclude-issues (default on), when any page of the same issue (LCCN, date, edition)
is listed. The manifest names the list that excluded it.

Outputs of build: OUT/labels.jsonl (accepted pages: page_id, source "loc-alto", licence, image_path,
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

    lines, blocks, words = [], [], []
    for el in root.iter():
        t = _local(el.tag)
        if t == "TextBlock":
            b = box(el)
            if b:
                blocks.append(b)
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
    return {"lines": lines, "blocks": blocks, "words": words, "alto_size": (d[0], d[1])}


def label_page(parsed, width, height):
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
    metrics = {
        "aspect_err": abs((aw / ah) / (width / height) - 1.0),
        "block_align": colgeom.block_align(parsed["blocks"], regions),
        "columns": len(regions),
        "coverage": colgeom.coverage(lines, regions),
        "cross_share": colgeom.cross_share(lines, gutters, width, bridge=vgap, body_h=body_h),
        "english": colgeom.english_share(parsed["words"]),
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


def cmd_select(a):
    excl = Exclusions(a.exclude, issues=a.exclude_issues)
    held = {}
    with open(a.scans_index) as f:
        for ln in f:
            if not ln.strip():
                continue
            r = json.loads(ln)
            did, b = r.get("doc_id"), r.get("batch")
            if not did or not b or not DOC_RE.match(did):
                continue
            held.setdefault(b, {})[did] = r.get("batch_version")
    batches = sorted(held, key=lambda b: (sha_key(b), b))
    manifest, chosen, used_batches = [], [], 0
    for b in batches:
        if len(chosen) >= a.pages or used_batches >= a.batches:
            break
        cands = sorted(held[b], key=lambda d: (sha_key(d), d))
        stats = _locstats_rows(a.locstats_dir, [b])
        took = 0
        for did in cands:
            if took >= a.per_batch or len(chosen) >= a.pages:
                break
            why = excl.reason(did)
            img = os.path.join(a.scans_root, b, image_rel(did))
            rows = stats.get((b, did), [])
            arch = None
            if rows:
                ver = held[b][did]
                pick = [r for r in rows if ver is None or r["batch_version"] == ver] or rows
                arch = pick[0]["archive"]
            if why is None and not os.path.exists(img):
                why = "no_image"
            if why is None and arch is None:
                why = "not_in_locstats"
            if why is None and not os.path.exists(os.path.join(a.locbulk_dir, arch)):
                why = "archive_missing"
            manifest.append({"doc_id": did, "batch": b, "status": why or "selected"})
            if why is None:
                chosen.append({"doc_id": did, "batch": b, "archive": arch, "image_path": img})
                took += 1
        if took:
            used_batches += 1
        print(f"batches {used_batches}/{a.batches} pages {len(chosen)}/{a.pages}", flush=True)
    with open(a.out, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=["doc_id", "batch", "archive", "image_path"], delimiter="\t")
        w.writeheader()
        w.writerows(chosen)
    with open(a.manifest_out, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=["doc_id", "batch", "status"], delimiter="\t")
        w.writeheader()
        w.writerows(manifest)
    summ = {"exclusion_lists": excl.counts, "pages": len(chosen), "batches": used_batches,
            "statuses": _count(m["status"] for m in manifest), "prefix": PREFIX}
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
                lab = label_page(parsed, w, h)
                row.update(_metric_cells(lab["metrics"]))
                row["status"] = "accepted" if lab["passed"] else "rejected"
                row["failed"] = ",".join(lab["failed"])
                if lab["passed"]:
                    labels.append({"columns": lab["columns"], "gutters": lab["gutters"], "height": h,
                                   "image_path": p["image_path"], "licence": "public domain (LoC Chronicling America)",
                                   "metrics": lab["metrics"], "page_id": did, "source": "loc-alto", "width": w})
        rows.append(row)
    keys = ["doc_id", "batch", "status", "failed"] + [m for m, _, _ in colgeom.FILTERS]
    with open(os.path.join(a.out_dir, "manifest.tsv"), "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=keys, delimiter="\t", extrasaction="ignore")
        w.writeheader()
        w.writerows(rows)
    with open(os.path.join(a.out_dir, "labels.jsonl"), "w") as f:
        for lab in labels:
            f.write(json.dumps(lab, sort_keys=True) + "\n")
    fail_counts = _count(x for r in rows for x in r["failed"].split(",") if r["status"] == "rejected" and x)
    summ = {"accepted": len(labels), "pages": len(rows), "statuses": _count(r["status"] for r in rows),
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
    s.add_argument("--scans-index", required=True)
    s.add_argument("--scans-root", required=True)
    s.add_argument("--locstats-dir", required=True)
    s.add_argument("--locbulk-dir", required=True)
    s.add_argument("--exclude", action="append", default=[], help="NAME=FILE, repeatable")
    s.add_argument("--exclude-issues", action=argparse.BooleanOptionalAction, default=True)
    s.add_argument("--pages", type=int, default=500)
    s.add_argument("--per-batch", type=int, default=50)
    s.add_argument("--batches", type=int, default=20)
    s.add_argument("--out", required=True)
    s.add_argument("--manifest-out", required=True)
    b = sub.add_parser("build")
    b.add_argument("--pages", required=True)
    b.add_argument("--locbulk-dir", required=True)
    b.add_argument("--out-dir", required=True)
    b.add_argument("--workers", type=int, default=2)
    a = ap.parse_args(argv)
    return cmd_select(a) if a.cmd == "select" else cmd_build(a)


if __name__ == "__main__":
    sys.exit(main())
