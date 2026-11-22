#!/usr/bin/env python3
"""OCR-engine-independent newspaper column finder (#2107, step 1).

Banded method: the page image is cut into horizontal bands; each band is deskewed by a small search
over shear angles, its vertical projection profile is computed, and gutters are the low-ink valleys
found with scipy.signal.find_peaks. Gutters found in neighbouring bands that line up are joined into
one gutter. Baseline (`--method fullheight`): one full-height profile with a fixed minimum gutter
width, the approach of the script below.

Method follows jscancella/NYTribuneOCRExperiments (findText_usingSums.py, GPL-3.0), reimplemented;
no code copied.

Datasets used to tune and score it:
  Reichsanzeiger-GT, CC BY 4.0, Mannheim University Library (tuning set de-rz).
  American Stories gold, Apache-2.0 text, Dell Research Harvard, arXiv:2308.12477 (test, en-asgold).
  Chronicling Germany, CC BY-NC 4.0, Schultze et al. 2025, University of Bonn (test, de-cg).

    columns.py --images DIR --out detections.json [--method banded|fullheight]
               [--ids-file FILE] [--shard-index I --shard-count N] [--workers K]
               [--cache-dir DIR] [--overlay-dir DIR] [--params-json JSON]

Output is keyed JSON, keys sorted: page id -> {gutters: [[x0, x1, y0, y1]], height, width}, in the
original image's pixels, plus "_meta" with the method and parameters.
"""
import argparse
import json
import math
import os
import sys
import time
from dataclasses import asdict, dataclass, fields

import numpy as np
from PIL import Image
from scipy.ndimage import uniform_filter, uniform_filter1d
from scipy.signal import find_peaks

Image.MAX_IMAGE_PIXELS = None


@dataclass
class Params:
    # image
    work_width: int = 2000
    bg_window: float = 0.05      # local-mean window, fraction of work width
    ink_ratio: float = 0.82      # ink = gray < local mean * ratio
    # profile / valley detection (shared by both methods; fractions of the band or page)
    smooth: float = 0.003        # profile smoothing, fraction of width
    prominence: float = 0.55     # valley prominence, fraction of the profile's 90th percentile
    rel_height: float = 0.7      # valley width measured at this fraction of its prominence
    valley_max: float = 0.35     # valley floor must be <= this fraction of the profile level
    min_ink: float = 0.03        # band profile level must be >= this fraction of band height
    min_gutter: float = 0.006    # minimum gutter width, fraction of page width
    min_col: float = 0.04        # minimum text width left and right of a gutter, fraction of width
    # banded
    band_frac: float = 0.05      # band height, fraction of page height
    band_overlap: float = 0.5
    deskew_deg: float = 1.0
    deskew_step: float = 0.25
    merge_overlap: float = 0.5   # x overlap (of the narrower) to join gutters of adjacent bands
    max_hole: int = 1            # bands without the gutter that a run may bridge
    min_run: float = 0.08        # shortest kept gutter, fraction of page height
    # fullheight baseline
    fh_min_gutter: float = 0.008
    fh_prominence: float = 0.5
    fh_valley_max: float = 0.35
    fh_min_col: float = 0.05


def params_from(json_text):
    p = Params()
    if json_text:
        names = {f.name for f in fields(Params)}
        for k, v in json.loads(json_text).items():
            if k not in names:
                raise SystemExit(f"unknown parameter {k}")
            setattr(p, k, type(getattr(p, k))(v))
    return p


# ---------------------------------------------------------------- image

def load_gray(path, work_width, cache_dir=None):
    """Grayscale uint8 array at about work_width pixels wide, plus (orig_w, orig_h)."""
    stem = os.path.splitext(os.path.basename(path))[0]
    cpath = os.path.join(cache_dir, f"{stem}.{work_width}.npz") if cache_dir else None
    if cpath and os.path.exists(cpath):
        z = np.load(cpath)
        return z["gray"], (int(z["size"][0]), int(z["size"][1]))
    im = Image.open(path)
    w0, h0 = im.size
    f = max(1, w0 // work_width)
    if im.format == "JPEG" and f > 1:
        im.draft("L", (w0 // f, h0 // f))
    im = im.convert("L")
    if im.size[0] != work_width and w0 > work_width:
        im = im.resize((work_width, max(1, round(h0 * work_width / w0))), Image.BOX)
    gray = np.asarray(im, dtype=np.uint8)
    if cpath:
        os.makedirs(cache_dir, exist_ok=True)
        np.savez(cpath, gray=gray, size=np.array([w0, h0]))
    return gray, (w0, h0)


def binarize(gray, p):
    g = gray.astype(np.float32)
    win = max(15, int(gray.shape[1] * p.bg_window))
    bg = uniform_filter(g, size=win, mode="nearest")
    return g < bg * p.ink_ratio


# ---------------------------------------------------------------- profiles and valleys

def shear_profile(ink, angle_deg):
    """Column sums of the band after shearing rows sideways by tan(angle) per row from the middle."""
    h, w = ink.shape
    if angle_deg == 0:
        return ink.sum(axis=0, dtype=np.float64)
    t = math.tan(math.radians(angle_deg))
    shifts = np.rint(t * (np.arange(h) - (h - 1) / 2.0)).astype(int)
    m = int(np.abs(shifts).max())
    prof = np.zeros(w + 2 * m)
    for s in np.unique(shifts):
        rows = ink[shifts == s].sum(axis=0, dtype=np.float64)
        prof[m - s: m - s + w] += rows
    return prof[m: m + w]


def best_profile(ink, p):
    """Profile at the shear angle that sharpens it most (zero angle on ties)."""
    base = shear_profile(ink, 0.0)
    if p.deskew_deg <= 0:
        return base, 0.0
    best, best_a, best_s = base, 0.0, float((base ** 2).sum())
    a = p.deskew_step
    while a <= p.deskew_deg + 1e-9:
        for ang in (a, -a):
            pr = shear_profile(ink, ang)
            s = float((pr ** 2).sum())
            if s > best_s * 1.0005:
                best, best_a, best_s = pr, ang, s
        a += p.deskew_step
    return best, best_a


def valleys(profile, band_h, min_gutter_px, min_col_px, prominence, valley_max, p):
    """Gutters [x0, x1] (profile pixels) as low-ink valleys of a column profile."""
    n = len(profile)
    k = max(1, int(p.smooth * n))
    s = uniform_filter1d(profile, size=k, mode="nearest") if k > 1 else profile
    level = float(np.percentile(s, 90))
    if level < p.min_ink * band_h:
        return []
    live = np.nonzero(s > 0.25 * level)[0]
    if len(live) < 3:
        return []
    xl, xr = int(live[0]), int(live[-1])
    sub = s[xl: xr + 1]
    peaks, props = find_peaks(-sub, prominence=prominence * level, width=max(1, min_gutter_px),
                              rel_height=p.rel_height)
    out = []
    for i, pk in enumerate(peaks):
        if sub[pk] > valley_max * level:
            continue
        x0 = xl + props["left_ips"][i]
        x1 = xl + props["right_ips"][i]
        if x0 - xl < min_col_px or xr - x1 < min_col_px:
            continue
        out.append([float(x0), float(x1)])
    return out


# ---------------------------------------------------------------- methods

def find_banded(gray, p):
    h, w = gray.shape
    ink = binarize(gray, p)
    bh = max(24, int(p.band_frac * h))
    stride = max(1, int(bh * (1 - p.band_overlap)))
    min_g = max(1, int(p.min_gutter * w))
    min_c = int(p.min_col * w)
    starts = list(range(0, max(1, h - bh + 1), stride))
    if starts and starts[-1] + bh < h:
        starts.append(h - bh)
    per_band = []
    for y0 in starts:
        prof, _ = best_profile(ink[y0: y0 + bh], p)
        per_band.append((y0, y0 + bh, valleys(prof, bh, min_g, min_c, p.prominence, p.valley_max, p)))
    # join gutters of neighbouring bands into runs
    runs = []  # dicts: x0s, x1s, y0, y1, last
    for bi, (y0, y1, gs) in enumerate(per_band):
        used = set()
        for g in gs:
            best, best_ov = None, 0.0
            for ri, r in enumerate(runs):
                if bi - r["last"] > p.max_hole + 1 or ri in used:
                    continue
                rx0, rx1 = float(np.mean(r["x0s"])), float(np.mean(r["x1s"]))
                ov = min(g[1], rx1) - max(g[0], rx0)
                if ov > 0 and ov >= p.merge_overlap * min(g[1] - g[0], rx1 - rx0) and ov > best_ov:
                    best, best_ov = ri, ov
            if best is None:
                runs.append({"x0s": [g[0]], "x1s": [g[1]], "y0": y0, "y1": y1, "last": bi})
                used.add(len(runs) - 1)
            else:
                r = runs[best]
                r["x0s"].append(g[0])
                r["x1s"].append(g[1])
                r["y1"] = y1
                r["last"] = bi
                used.add(best)
    out = []
    for r in runs:
        if r["y1"] - r["y0"] >= p.min_run * h:
            out.append([float(np.mean(r["x0s"])), float(np.mean(r["x1s"])), float(r["y0"]), float(r["y1"])])
    out.sort()
    return out


def find_fullheight(gray, p):
    h, w = gray.shape
    ink = binarize(gray, p)
    rows = ink.sum(axis=1, dtype=np.float64)
    live = np.nonzero(rows >= 0.01 * rows.max())[0] if rows.max() > 0 else []
    if len(live) < 3:
        return []
    ya, yb = int(live[0]), int(live[-1]) + 1
    prof = ink[ya:yb].sum(axis=0, dtype=np.float64)
    gs = valleys(prof, yb - ya, max(1, int(p.fh_min_gutter * w)), int(p.fh_min_col * w),
                 p.fh_prominence, p.fh_valley_max, p)
    return [[g[0], g[1], float(ya), float(yb)] for g in gs]


METHODS = {"banded": find_banded, "fullheight": find_fullheight}


def detect(gray, orig_size, method, p):
    """Gutters in original-image pixels: [x0, x1, y0, y1]."""
    sx = orig_size[0] / gray.shape[1]
    sy = orig_size[1] / gray.shape[0]
    return [[round(g[0] * sx), round(g[1] * sx), round(g[2] * sy), round(g[3] * sy)]
            for g in METHODS[method](gray, p)]


# ---------------------------------------------------------------- driver

def overlay(gray, gutters, orig_size, path):
    im = Image.fromarray(gray).convert("RGB")
    sx = gray.shape[1] / orig_size[0]
    sy = gray.shape[0] / orig_size[1]
    a = np.asarray(im).copy()
    for x0, x1, y0, y1 in gutters:
        xs, xe, ys, ye = int(x0 * sx), int(x1 * sx), int(y0 * sy), int(y1 * sy)
        reg = a[ys:ye, xs:max(xe, xs + 1)].astype(np.float32)
        reg[..., 0] = reg[..., 0] * 0.4 + 150
        reg[..., 1] *= 0.4
        reg[..., 2] *= 0.4
        a[ys:ye, xs:max(xe, xs + 1)] = reg.clip(0, 255).astype(np.uint8)
    Image.fromarray(a).save(path)


def _one(args):
    path, method, pdict, cache_dir, overlay_dir = args
    p = Params(**pdict)
    gray, size = load_gray(path, p.work_width, cache_dir)
    g = detect(gray, size, method, p)
    pid = os.path.splitext(os.path.basename(path))[0]
    if overlay_dir:
        os.makedirs(overlay_dir, exist_ok=True)
        overlay(gray, g, size, os.path.join(overlay_dir, f"{pid}.{method}.png"))
    return pid, {"gutters": g, "height": size[1], "width": size[0]}


def list_images(images_dir, ids_file=None):
    names = sorted(f for f in os.listdir(images_dir) if f.lower().endswith((".jpg", ".jpeg", ".png", ".ppm", ".tif", ".tiff")))
    if ids_file:
        with open(ids_file) as f:
            want = {ln.strip() for ln in f if ln.strip()}
        names = [n for n in names if os.path.splitext(n)[0] in want]
    return [os.path.join(images_dir, n) for n in names]


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--images", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--method", choices=sorted(METHODS), default="banded")
    ap.add_argument("--ids-file")
    ap.add_argument("--shard-index", type=int, default=0)
    ap.add_argument("--shard-count", type=int, default=1)
    ap.add_argument("--workers", type=int, default=1)
    ap.add_argument("--cache-dir")
    ap.add_argument("--overlay-dir")
    ap.add_argument("--params-json", help="JSON object overriding Params fields")
    a = ap.parse_args(argv)
    p = params_from(a.params_json)
    paths = list_images(a.images, a.ids_file)[a.shard_index:: a.shard_count]
    jobs = [(x, a.method, asdict(p), a.cache_dir, a.overlay_dir) for x in paths]
    res = {}
    t0 = time.time()
    if a.workers > 1:
        import multiprocessing as mp
        with mp.Pool(a.workers) as pool:
            for i, (pid, r) in enumerate(pool.imap_unordered(_one, jobs), 1):
                res[pid] = r
                print(f"pages {i}/{len(jobs)}", flush=True)
    else:
        for i, j in enumerate(jobs, 1):
            pid, r = _one(j)
            res[pid] = r
            print(f"pages {i}/{len(jobs)}", flush=True)
    res["_meta"] = {"method": a.method, "params": asdict(p), "pages": len(jobs),
                    "shard_count": a.shard_count, "shard_index": a.shard_index,
                    "wall_seconds": round(time.time() - t0, 2)}
    with open(a.out, "w") as f:
        json.dump(res, f, sort_keys=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
