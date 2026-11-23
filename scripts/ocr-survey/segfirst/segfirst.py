#!/usr/bin/env python3
"""Segment-first OCR test (#2122): the three arms, L (LoC ALTO), W (whole page) and C (by column).

    segfirst.py pages --overproof-dir D --asgold-dir D --out pages.json
    segfirst.py pages --images-dir D --ids-file F --out pages.json        (pages with no truth, for the check)
    segfirst.py run --pages pages.json --out-dir D [--arms C,L,W] [--workers 3]
    segfirst.py check --pages pages.json --out-dir D [--overlay-dir D]

The plan is frozen in issue #2122. Arms:
  L  the text of the page's LoC ALTO, in the order LoC gives (TextLine order; a HYP goes on the line's last word).
  W  Tesseract `eng` at PSM 3, the worker's arguments (`--psm 3 --dpi 300`, one thread), on the page image file.
  C  by column. The banded finder (columns.py, byte-identical to 337fc0aa, default Params) finds gutters; the page
     is cut into cells at the gutters it reports in each band; cells in neighbouring bands whose left and right
     edges both line up within 1% of page width are joined into one column region; each region is read by
     Tesseract `eng` at PSM 4 (`--dpi 300`, one thread). Regions are read left to right (by the column their top
     cell's left edge falls in) and top to bottom within a column, so a region spanning several columns is read
     where its top falls.

Assembly details fixed here (#2122 left them open):
  - Bands are the finder's band core ranges (find_banded's own grid). The finder's last band overlaps the one
    before it; the overlap goes to the earlier band, so the bands tile the page.
  - A band is cut at the centre of each gutter segment the finder reports in it; segments of one band that
    overlap in x (the finder can report one valley twice, in two runs) are merged first. A run's bridged hole
    (a band with no segment) is not cut. Cells tile the page: every pixel is in exactly one cell.
  - Joined cells keep their own x range: a region is read from a white canvas holding only its cells' pixels.
  - Columns for the order: regions' top-cell left edges, sorted and grouped where consecutive edges are within
    1% of page width.
Outputs per page and arm: OUT/<arm>/<page id>.json {lines: [[[x0, y0, x1, y1], ...] per output line],
regions (C only), text, errors (Tesseract failures, recorded, not retried)}; finder gutters in OUT/gutters/<page id>.json. Nothing is deleted or fetched.
"""
import argparse
import hashlib
import json
import multiprocessing as mp
import os
import subprocess
import sys
import tempfile
import time
import xml.etree.ElementTree as ET
from dataclasses import asdict

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import columns  # noqa: E402

from PIL import Image  # noqa: E402

Image.MAX_IMAGE_PIXELS = None

# columns.py must be the finder of 337fc0aa (scripts/ocr-survey/layout_columns/columns.py), unchanged.
COLUMNS_SHA256 = "37a9f2fb4da35f6f04e3a4f865db65e652324a483a5bb653504c346cf7834a04"
JOIN_TOL = 0.01      # cells line up, and columns group, within this fraction of page width
TESS_LANG = "eng"
PSM_W, PSM_C, DPI = 3, 4, 300
ARMS = ("C", "L", "W")


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def write_json(path, obj):
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(obj, f, sort_keys=True)
    os.replace(tmp, path)


# ---------------------------------------------------------------- pages


def pages_truth(overproof_dir, asgold_dir):
    """The 25 pages: Overproof dataset 3 pages held in loc/ (18), en-asgold pages matched to a CA page (7)."""
    out = {}
    loc = os.path.join(overproof_dir, "loc")
    for key in sorted(json.load(open(os.path.join(loc, "pages.json")))):
        out[key] = {"alto": os.path.join(loc, key + ".xml"), "image": os.path.join(loc, key + ".jpg"),
                    "set": "overproof", "truth_key": key}
    rep = json.load(open(os.path.join(asgold_dir, "out", "pages.json")))
    for p in rep["pages"]:
        if not p.get("used"):
            continue
        a = os.path.join(asgold_dir, "alto")
        out[p["stem"]] = {"alto": os.path.join(a, p["stem"] + ".xml"), "image": os.path.join(a, p["stem"] + ".jpg"),
                          "set": "asgold", "truth_key": p["id"]}
    return out


def pages_images(images_dir, ids_file):
    ids = [ln.strip() for ln in open(ids_file) if ln.strip()]
    names = {os.path.splitext(n)[0]: n for n in os.listdir(images_dir)}
    return {i: {"alto": None, "image": os.path.join(images_dir, names[i]), "set": "nontruth", "truth_key": None}
            for i in ids}


def cmd_pages(a):
    if a.images_dir:
        pages = pages_images(a.images_dir, a.ids_file)
    else:
        pages = pages_truth(a.overproof_dir, a.asgold_dir)
    for pid, p in pages.items():
        for k in ("alto", "image"):
            if p[k] and not os.path.isfile(p[k]):
                raise SystemExit(f"{pid}: missing {p[k]}")
    write_json(a.out, pages)
    listing = "\n".join(sorted(pages)) + "\n"
    print(json.dumps({"by_set": {s: sum(p["set"] == s for p in pages.values()) for s in sorted({p["set"] for p in pages.values()})},
                      "page_ids_sha256": hashlib.sha256(listing.encode()).hexdigest(), "pages": len(pages)}, sort_keys=True))
    return 0


# ---------------------------------------------------------------- finder and assembly


def find_gutters(image_path):
    """Banded finder at its frozen defaults, as combo_eval.py calls it; plus the band grid it used."""
    p = columns.Params()
    gray, size = columns.load_gray(image_path, p.work_width)
    gutters = columns.detect(gray, size, "banded", p)
    return {"bands": band_cores(gray.shape[0], size[1], p), "gutters": gutters, "height": size[1], "width": size[0]}


def band_cores(h, orig_h, p):
    """find_banded's band core ranges [y0, y1] in original pixels, rounded as columns.detect rounds segments.
    Returns [[seg_y0, seg_y1, cell_y0, cell_y1]]: the finder's range (to match segments) and the tiling range."""
    bh = max(24, int(p.band_frac * h))
    stride = max(1, int(bh * (1 - p.band_overlap)))
    starts = list(range(0, max(1, h - bh + 1), stride))
    if starts and starts[-1] + bh < h:
        starts.append(h - bh)
    pad = (bh - stride) / 2.0
    sy = orig_h / h
    out = []
    for bi, y0 in enumerate(starts):
        c0 = y0 if bi == 0 else y0 + pad
        c1 = y0 + bh if bi == len(starts) - 1 else y0 + bh - pad
        out.append([round(float(c0) * sy), round(float(c1) * sy)])
    rows, prev = [], 0
    for s0, s1 in out:
        t0, t1 = prev, max(prev, s1)
        rows.append([s0, s1, t0, t1])
        prev = t1
    rows[-1][3] = orig_h
    return rows


def merge_overlapping(intervals):
    """Gutter x intervals of one band with overlapping (or touching) ones merged. The finder can report the
    same valley twice in a band (two find_peaks minima share one width), in two runs."""
    out = []
    for x0, x1 in sorted(intervals):
        if out and x0 <= out[-1][1]:
            out[-1][1] = max(out[-1][1], x1)
        else:
            out.append([x0, x1])
    return out


def cells_of(found):
    """Cells per band: [[x0, x1, y0, y1], ...] cut at the centre of each gutter in the band (overlapping
    segments of one band merged first)."""
    w = found["width"]
    by_band = {i: [] for i in range(len(found["bands"]))}
    index = {(b[0], b[1]): i for i, b in enumerate(found["bands"])}
    for g in found["gutters"]:
        for x0, x1, y0, y1 in g:
            if (y0, y1) not in index:
                raise ValueError(f"gutter segment y {y0}-{y1} is not a band core range")
            by_band[index[(y0, y1)]].append((x0, x1))
    out = []
    for i, b in enumerate(found["bands"]):
        y0, y1 = b[2], b[3]
        if y1 <= y0:
            out.append([])
            continue
        cuts = [round((g0 + g1) / 2.0) for g0, g1 in merge_overlapping(by_band[i])]
        edges = [0] + cuts + [w]
        if any(e1 <= e0 for e0, e1 in zip(edges, edges[1:])):
            raise ValueError(f"band {i}: cuts not increasing {edges}")
        out.append([[e0, e1, y0, y1] for e0, e1 in zip(edges, edges[1:])])
    return out


def join_regions(cells, width, tol=JOIN_TOL):
    """Chains of cells in neighbouring bands whose left and right edges are both within tol * width."""
    t = tol * width
    regions = []          # each: list of cells
    open_by_cell = {}     # index of a cell in the previous band -> region index
    prev = []
    for band in cells:
        cand = []
        for j, c in enumerate(band):
            for i, p in enumerate(prev):
                d0, d1 = abs(c[0] - p[0]), abs(c[1] - p[1])
                if d0 <= t and d1 <= t:
                    cand.append((max(d0, d1), d0 + d1, i, j))
        cand.sort()
        took_p, took_c, nxt = set(), set(), {}
        for _, _, i, j in cand:
            if i in took_p or j in took_c:
                continue
            took_p.add(i)
            took_c.add(j)
            r = open_by_cell[i]
            regions[r].append(band[j])
            nxt[j] = r
        for j, c in enumerate(band):
            if j not in took_c:
                regions.append([c])
                nxt[j] = len(regions) - 1
        open_by_cell, prev = nxt, band
    return regions


def reading_order(regions, width, tol=JOIN_TOL):
    """Region indices left to right (column of the top cell's left edge), top to bottom within a column."""
    lefts = sorted(set(r[0][0] for r in regions))
    col, k, last = {}, -1, None
    for x in lefts:
        if last is None or x - last > tol * width:
            k += 1
        col[x] = k
        last = x
    return sorted(range(len(regions)), key=lambda i: (col[regions[i][0][0]], regions[i][0][2], regions[i][0][0]))


def assemble(found):
    """Regions in reading order: [{cells, box, column_key}]."""
    cells = cells_of(found)
    regions = join_regions(cells, found["width"])
    order = reading_order(regions, found["width"])
    out = []
    for i in order:
        r = regions[i]
        out.append({"box": [min(c[0] for c in r), r[0][2], max(c[1] for c in r), r[-1][3]], "cells": r})
    return out


def tiling_ok(regions, width, height):
    """Every pixel in exactly one cell: total area equals the page and no two cells overlap."""
    cs = [c for r in regions for c in r["cells"]]
    area = sum((c[1] - c[0]) * (c[3] - c[2]) for c in cs)
    if area != width * height:
        return False
    rows = {}
    for c in cs:
        rows.setdefault((c[2], c[3]), []).append((c[0], c[1]))
    ys = sorted(rows)
    if ys[0][0] != 0 or ys[-1][1] != height or any(a[1] != b[0] for a, b in zip(ys, ys[1:])):
        return False
    return all(sorted(v)[0][0] == 0 and sorted(v)[-1][1] == width and
               all(p[1] == q[0] for p, q in zip(sorted(v), sorted(v)[1:])) for v in rows.values())


# ---------------------------------------------------------------- engines


def tesseract(image_path, psm, workdir):
    """Text and TSV lines of one Tesseract run (eng, --dpi 300, one thread)."""
    base = os.path.join(workdir, "out")
    env = dict(os.environ, OMP_THREAD_LIMIT="1")
    subprocess.run(["tesseract", image_path, base, "-l", TESS_LANG, "--psm", str(psm), "--dpi", str(DPI), "txt", "tsv"],
                   check=True, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    text = open(base + ".txt", encoding="utf-8").read().replace("\f", "")
    return text, tsv_lines(open(base + ".tsv", encoding="utf-8").read())


def tsv_lines(tsv, dx=0, dy=0):
    """Word boxes [x0, y0, x1, y1] grouped by Tesseract line, in output order."""
    lines, order = {}, []
    for ln in tsv.splitlines()[1:]:
        f = ln.split("\t")
        if len(f) < 12 or f[0] != "5" or not f[11].strip():
            continue
        key = (f[1], f[2], f[3], f[4])
        if key not in lines:
            lines[key] = []
            order.append(key)
        x, y, w, h = (int(v) for v in f[6:10])
        lines[key].append([x + dx, y + dy, x + w + dx, y + h + dy])
    return [lines[k] for k in order]


def page_image(path):
    im = Image.open(path)
    if im.mode not in ("L", "RGB"):
        im = im.convert("L")
    im.load()
    return im


def region_image(im, region):
    """The region's cells pasted at their places on a white canvas of the region's box."""
    x0, y0, x1, y1 = region["box"]
    canvas = Image.new(im.mode, (x1 - x0, y1 - y0), 255 if im.mode == "L" else (255, 255, 255))
    for cx0, cx1, cy0, cy1 in region["cells"]:
        canvas.paste(im.crop((cx0, cy0, cx1, cy1)), (cx0 - x0, cy0 - y0))
    return canvas


def engine_error(e):
    return f"{type(e).__name__}: {e}" + (f" {e.stderr.decode(errors='replace')[-300:]}" if getattr(e, "stderr", None) else "")


def arm_w(page, workdir):
    """A Tesseract failure is recorded, as the worker records any exit, and the page's text is then empty."""
    try:
        text, lines = tesseract(page["image"], PSM_W, workdir)
    except subprocess.CalledProcessError as e:
        return {"errors": [engine_error(e)], "lines": [], "text": ""}
    return {"errors": [], "lines": lines, "text": text}


def arm_c(page, found, workdir):
    regions = assemble(found)
    if not tiling_ok(regions, found["width"], found["height"]):
        raise ValueError("cells do not tile the page")
    im = page_image(page["image"])
    if im.size != (found["width"], found["height"]):
        raise ValueError(f"image size {im.size} differs from the finder's {found['width']}x{found['height']}")
    texts, lines, errors = [], [], []
    for k, r in enumerate(regions):
        p = os.path.join(workdir, f"region-{k:03d}.png")
        region_image(im, r).save(p)
        try:
            t, ls = tesseract(p, PSM_C, workdir)
        except subprocess.CalledProcessError as e:
            errors.append(f"region {k}: {engine_error(e)}")
            t, ls = "", []
        os.remove(p)  # a derived crop, not a scan
        texts.append(t.strip("\n"))
        x0, y0 = r["box"][0], r["box"][1]
        lines += [[[b[0] + x0, b[1] + y0, b[2] + x0, b[3] + y0] for b in ln] for ln in ls]
    return {"errors": errors, "lines": lines, "regions": regions, "text": "\n\n".join(t for t in texts if t)}


def _local(tag):
    return tag.rsplit("}", 1)[-1]


def arm_l(page):
    """LoC ALTO text in its order: TextLines in document order, a block break as a blank line."""
    root = ET.parse(page["alto"]).getroot()
    with Image.open(page["image"]) as im:
        iw, ih = im.size
    pg = next((e for e in root.iter() if _local(e.tag) == "Page"), None)
    sx, sy = iw / float(pg.get("WIDTH")), ih / float(pg.get("HEIGHT"))
    blocks, lines = [], []
    for tb in root.iter():
        if _local(tb.tag) != "TextBlock":
            continue
        out = []
        for tl in tb:
            if _local(tl.tag) != "TextLine":
                continue
            words, boxes = [], []
            for el in tl:
                t = _local(el.tag)
                if t == "String" and el.get("CONTENT"):
                    words.append(el.get("CONTENT"))
                    try:
                        x, y, w, h = (float(el.get(k)) for k in ("HPOS", "VPOS", "WIDTH", "HEIGHT"))
                        boxes.append([x * sx, y * sy, (x + w) * sx, (y + h) * sy])
                    except (TypeError, ValueError):
                        pass
                elif t == "HYP" and words:
                    words[-1] += el.get("CONTENT") or "-"
            if words:
                out.append(" ".join(words))
                lines.append(boxes)
        if out:
            blocks.append("\n".join(out))
    return {"errors": [], "lines": lines, "text": "\n\n".join(blocks)}


# ---------------------------------------------------------------- driver


def _gutter_job(args):
    pid, page, out_dir = args
    path = os.path.join(out_dir, "gutters", pid + ".json")
    if not os.path.exists(path):
        write_json(path, find_gutters(page["image"]))
    return pid


def _arm_job(args):
    pid, arm, page, out_dir = args
    path = os.path.join(out_dir, arm, pid + ".json")
    if os.path.exists(path):
        return pid, arm, 0.0
    t0 = time.time()
    with tempfile.TemporaryDirectory(prefix="segfirst-") as wd:
        if arm == "L":
            res = arm_l(page)
        elif arm == "W":
            res = arm_w(page, wd)
        else:
            res = arm_c(page, json.load(open(os.path.join(out_dir, "gutters", pid + ".json"))), wd)
    res["seconds"] = round(time.time() - t0, 1)
    write_json(path, res)
    return pid, arm, res["seconds"]


def run_meta(pages_path, arms):
    v = subprocess.run(["tesseract", "--version"], capture_output=True, text=True)
    lp = subprocess.run(["tesseract", "--list-langs"], capture_output=True, text=True)
    tessdata = None
    for ln in (lp.stdout + lp.stderr).splitlines():
        if "List of available languages in" in ln:
            tessdata = ln.split('"')[1] if '"' in ln else ln.split(" in ")[-1].rstrip(":")
    td = os.path.join(tessdata, TESS_LANG + ".traineddata") if tessdata else None
    return {"arms": sorted(arms), "columns_sha256": sha256_file(os.path.join(HERE, "columns.py")),
            "dpi": DPI, "join_tol": JOIN_TOL, "pages_sha256": sha256_file(pages_path),
            "params": asdict(columns.Params()), "psm_c": PSM_C, "psm_w": PSM_W, "script_sha256": sha256_file(__file__),
            "tesseract_version": (v.stdout + v.stderr).splitlines()[0].strip(),
            "traineddata": td, "traineddata_sha256": sha256_file(td) if td and os.path.exists(td) else None}


def cmd_run(a):
    if sha256_file(os.path.join(HERE, "columns.py")) != COLUMNS_SHA256:
        raise SystemExit("columns.py differs from the finder at 337fc0aa")
    pages = json.load(open(a.pages))
    arms = [x for x in a.arms.split(",") if x]
    if any(x not in ARMS for x in arms):
        raise SystemExit(f"arms must be among {ARMS}")
    for d in ["gutters"] + arms:
        os.makedirs(os.path.join(a.out_dir, d), exist_ok=True)
    meta = run_meta(a.pages, arms)
    print(json.dumps(meta, sort_keys=True), flush=True)
    write_json(os.path.join(a.out_dir, f"run-{time.strftime('%Y%m%dT%H%M%S')}.json"), meta)
    ids = sorted(pages)
    t0 = time.time()
    with mp.Pool(a.workers) as pool:
        for n, pid in enumerate(pool.imap_unordered(_gutter_job, [(i, pages[i], a.out_dir) for i in ids]), 1):
            print(f"gutters {n}/{len(ids)} {pid} {time.time() - t0:.0f}s", flush=True)
        jobs = [(i, arm, pages[i], a.out_dir) for i in ids for arm in arms if not (arm == "L" and not pages[i]["alto"])]
        for n, (pid, arm, s) in enumerate(pool.imap_unordered(_arm_job, jobs), 1):
            print(f"arms {n}/{len(jobs)} {arm} {pid} {s}s {time.time() - t0:.0f}s", flush=True)
    return 0


# ---------------------------------------------------------------- structural check (pages with no truth)


def cmd_check(a):
    """Per page: regions, tiling, word counts of C against W, and an overlay of regions in reading order."""
    import score2122
    pages = json.load(open(a.pages))
    rows = {}
    for pid in sorted(pages):
        g = json.load(open(os.path.join(a.out_dir, "gutters", pid + ".json")))
        c = json.load(open(os.path.join(a.out_dir, "C", pid + ".json")))
        regs = c["regions"]
        row = {"bands": len(g["bands"]), "cells": sum(len(r["cells"]) for r in regs), "gutters": len(g["gutters"]),
               "regions": len(regs), "regions_multi_cell": sum(len(r["cells"]) > 1 for r in regs),
               "tiling_ok": tiling_ok(regs, g["width"], g["height"]),
               "bridged_holes": sum(1 for gt in g["gutters"] for s0, s1 in zip(gt, gt[1:]) if s1[2] > s0[3])}
        segs = [s for gt in g["gutters"] for s in gt]
        cross = lambda arm: sum(score2122.crosses(ln, segs) for ln in arm["lines"])  # noqa: E731
        row["lines_C"], row["cross_C"] = len(c["lines"]), cross(c)
        wpath = os.path.join(a.out_dir, "W", pid + ".json")
        tc = score2122.tokens(c["text"])
        row["words_C"] = len(tc)
        if os.path.exists(wpath):
            w = json.load(open(wpath))
            tw = score2122.tokens(w["text"])
            row["words_W"], row["lines_W"], row["cross_W"] = len(tw), len(w["lines"]), cross(w)
            m = score2122.clipped_matches(score2122.ngrams(tw, 1), score2122.ngrams(tc, 1))
            row["W_words_found_in_C"] = round(m / max(1, len(tw)), 4)
        row["errors_C"] = c["errors"]
        if os.path.exists(wpath):
            row["errors_W"] = w["errors"]
        rows[pid] = row
        if a.overlay_dir:
            os.makedirs(a.overlay_dir, exist_ok=True)
            overlay(pages[pid]["image"], g, regs, os.path.join(a.overlay_dir, pid + ".png"))
    print(json.dumps(rows, indent=1, sort_keys=True))
    return 0


def overlay(image_path, found, regions, out_path, width=1600):
    from PIL import ImageDraw
    im = page_image(image_path).convert("RGB")
    s = width / im.size[0]
    im = im.resize((width, round(im.size[1] * s)), Image.BOX)
    dr = ImageDraw.Draw(im, "RGBA")
    for gt in found["gutters"]:
        for x0, x1, y0, y1 in gt:
            dr.rectangle([x0 * s, y0 * s, max(x1 * s, x0 * s + 1), y1 * s], fill=(255, 0, 0, 60))
    for k, r in enumerate(regions):
        x0, y0, x1, y1 = (v * s for v in r["box"])
        col = (0, 90, 255, 255) if len(r["cells"]) > 1 else (0, 160, 0, 255)
        dr.rectangle([x0 + 2, y0 + 2, x1 - 2, y1 - 2], outline=col, width=2)
        dr.text((x0 + 6, y0 + 4), str(k + 1), fill=(200, 0, 120, 255))
    im.save(out_path)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sp = ap.add_subparsers(dest="cmd", required=True)
    p = sp.add_parser("pages")
    p.add_argument("--overproof-dir", help="data/overproof-ca (loc/ with pages.json, images and ALTO)")
    p.add_argument("--asgold-dir", help="the en-asgold directory (out/pages.json, alto/)")
    p.add_argument("--images-dir", help="page images for a run with no truth (instead of the two above)")
    p.add_argument("--ids-file", help="page ids, one per line (with --images-dir)")
    p.add_argument("--out", required=True)
    r = sp.add_parser("run")
    r.add_argument("--pages", required=True)
    r.add_argument("--out-dir", required=True)
    r.add_argument("--arms", default="C,L,W")
    r.add_argument("--workers", type=int, default=3)
    c = sp.add_parser("check")
    c.add_argument("--pages", required=True)
    c.add_argument("--out-dir", required=True)
    c.add_argument("--overlay-dir")
    a = ap.parse_args(argv)
    if a.cmd == "pages":
        if bool(a.images_dir) == bool(a.overproof_dir and a.asgold_dir) or (a.images_dir and not a.ids_file):
            raise SystemExit("give --overproof-dir and --asgold-dir, or --images-dir and --ids-file")
        return cmd_pages(a)
    return cmd_run(a) if a.cmd == "run" else cmd_check(a)


if __name__ == "__main__":
    sys.exit(main())
