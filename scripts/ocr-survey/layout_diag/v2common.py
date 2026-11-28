#!/usr/bin/env python3
"""Shared page loading and method runners for the banded v2 test (#2107, plan comment 7122420362).

Methods (all on the same page bytes; work-width arrays as pagecache.py makes them):
  v1     the banded finder at 337fc0aa (columns.detect, default Params, 2,000 px work width);
  v2     v1, then layout_columns/refit.py on the full-resolution page (Otsu ink);
  as     American Stories boxes (asmodel.Detector, as combo_eval.py) -> diag.gutters_generic at 0.01, `article`;
  yolo   the trained YOLOv8m (best.pt sha256 7d75048e...), predicted as eval_infer.py does (imgsz 1280, candidate
         floor 0.01, grayscale to three channels) -> diag.gutters_generic at 0.25, as thr.py scored it;
  combo  the combination at 31b624560: v1 plus AS@0.01 `article` gutters where v1 has none (diag.add_where_none).

Method for the finder follows jscancella/NYTribuneOCRExperiments (findText_usingSums.py, GPL-3.0), reimplemented;
detectors: American Stories (Dell et al., arXiv:2308.12477; weights research-use only, never copied into the
repository) and Ultralytics YOLOv8 (AGPL-3.0).
"""
import hashlib
import io
import os
import sys
import zipfile

import numpy as np
from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
for d in (HERE, os.path.join(HERE, "..", "layout_columns"), os.path.join(HERE, "..", "layout_train")):
    if d not in sys.path:
        sys.path.insert(0, d)
import columns  # noqa: E402
import pagecache  # noqa: E402
import refit  # noqa: E402

Image.MAX_IMAGE_PIXELS = None
TRAINED_SHA = "7d75048e1192c22e501ffc1f649d74d80fa4218c824455fbc699a66e5005b62f"
AS_SHA = "045b2e5588e53c700490730244bdc3e8ff21e903aff1c2af9b169dcdb1d9155e"
AS_CONF, YOLO_CONF = 0.01, 0.25
METHODS = ("v1", "v2", "as", "yolo", "combo")


def file_sha(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def read_bytes(src):
    """Image bytes from a file path, "<zip>!<member>" or "<parquet>#row=N" (page_image column)."""
    if "!" in src and src.split("!", 1)[0].endswith(".zip"):
        z, m = src.split("!", 1)
        with zipfile.ZipFile(z) as zf:
            return zf.read(m)
    if "#row=" in src:
        import pyarrow.parquet as pq
        f, r = src.rsplit("#row=", 1)
        r = int(r)
        pf = pq.ParquetFile(f)
        start = 0
        for rg in range(pf.metadata.num_row_groups):
            n = pf.metadata.row_group(rg).num_rows
            if start <= r < start + n:
                return pf.read_row_group(rg, columns=["page_image"]).column("page_image").to_pylist()[r - start]["bytes"]
            start += n
        raise SystemExit("row %d not in %s" % (r, f))
    with open(src, "rb") as f:
        return f.read()


def load_page(data):
    """(work-width gray as pagecache.py makes it, original size, full-resolution gray)."""
    gray, size = pagecache.to_gray(data, 2000)
    with Image.open(io.BytesIO(data)) as im:
        full = np.asarray(im.convert("L"), dtype=np.uint8)
    assert (full.shape[1], full.shape[0]) == tuple(size), "full-resolution size differs"
    return gray, size, full


def run_v1(gray, size):
    return columns.detect(gray, size, "banded", columns.Params())


def run_v2(v1, full):
    thr, ink = refit.page_ink(full)
    return thr, refit.refit_gutters(ink, v1)


class TrainedDetector:
    """eval_infer.py's prediction, per page (same settings and box rounding)."""

    def __init__(self, weights, threads=4):
        got = file_sha(weights)
        if got != TRAINED_SHA:
            raise SystemExit("checkpoint sha256 %s differs from %s" % (got, TRAINED_SHA))
        import torch
        from ultralytics import YOLO
        import eval_infer
        torch.set_num_threads(threads)
        self.ei = eval_infer
        self.model = YOLO(weights)
        if [self.model.names[i] for i in range(len(self.model.names))] != eval_infer.NAMES:
            raise SystemExit("class names differ: %s" % self.model.names)

    def boxes(self, gray, size):
        sx, sy = size[0] / gray.shape[1], size[1] / gray.shape[0]
        img = np.ascontiguousarray(np.repeat(gray[:, :, None], 3, axis=2))
        r = self.model.predict(img, imgsz=self.ei.IMGSZ, conf=self.ei.CONF_FLOOR, device="cpu", verbose=False)[0]
        b = r.boxes
        xyxy, conf, cls = b.xyxy.cpu().numpy(), b.conf.cpu().numpy(), b.cls.cpu().numpy().astype(int)
        return [[round(float(x[0]) * sx, 1), round(float(x[1]) * sy, 1), round(float(x[2]) * sx, 1),
                 round(float(x[3]) * sy, 1), round(float(c), 4), self.ei.NAMES[k]] for x, c, k in zip(xyxy, conf, cls)]


class ASDetector:
    """American Stories boxes as combo_eval.py run saved them (page pixels, rounded)."""

    def __init__(self, model, threads=4):
        got = file_sha(model)
        if got != AS_SHA:
            raise SystemExit("AS model sha256 %s differs from %s" % (got, AS_SHA))
        import asmodel
        self.am = asmodel
        self.det = asmodel.Detector(model, threads)

    def boxes(self, gray, size):
        sx, sy = size[0] / gray.shape[1], size[1] / gray.shape[0]
        return [[round(d[0] * sx), round(d[1] * sy), round(d[2] * sx), round(d[3] * sy), round(d[4], 3), self.am.NAMES[d[5]]]
                for d in self.det.boxes(gray)]


def detector_gutters(boxes, size, conf):
    import diag
    return diag.gutters_generic(boxes, size, conf, diag.ARTICLE, diag.OBSTACLE)


def combo_gutters(v1, as_gutters, size):
    import diag
    return v1 + diag.add_where_none(v1, as_gutters, size)
