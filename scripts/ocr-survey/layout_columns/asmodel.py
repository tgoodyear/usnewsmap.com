#!/usr/bin/env python3
"""American Stories layout detector as a zero-shot contender for column gutters (#2107, step 1).

A YOLOv8 page-layout detector that Dell Research Harvard trained on Chronicling America scans:
Dell et al., NeurIPS 2023 Datasets and Benchmarks, arXiv:2308.12477; code
https://github.com/dell-research-harvard/AmericanStories. ONNX mirror used here:
https://huggingface.co/NealCaren/american-stories-onnx, revision 2558a023, file
layout_model_new.onnx (mirror card: CC BY 4.0; the upstream repository has no licence file and
distributes the weights through Dropbox). Research use only: the weights are never copied into the
repository or a PR, and the path is a flag.

Method for the finder it is compared with follows jscancella/NYTribuneOCRExperiments
(findText_usingSums.py, GPL-3.0), reimplemented; no code copied. Datasets scored against:
Reichsanzeiger-GT (CC BY 4.0, Mannheim University Library), Chronicling Germany (CC BY-NC 4.0,
Schultze et al. 2025, University of Bonn), Teklia/Newspapers-finlam and -La-Liberte (MIT),
biglam/europeana-newspapers-ground-truth (CC0), American Stories gold (Apache-2.0 text).

Zero-shot, nothing tuned. Settings are the upstream pipeline's defaults for its `yolov8` layout
backend: src/run_img2txt_yolo_pipeline.py, function get_layout_predictions:
  - letterbox to 1280 x 1280 with auto=False (aspect kept, grey 114 padding, centred), BGR to RGB,
    float32 / 255;
  - non_max_supression_yolov8(conf_thres=0.01, iou_thres=0.1, max_det=2000, agnostic=True);
  - boxes mapped back from the letterboxed frame to the page.
This file reimplements those steps with numpy and OpenCV; no upstream code is copied.

Class names (ONNX metadata `names`): 0 article, 1 author, 2 cartoon_or_advertisement, 3 headline,
4 image_caption, 5 masthead, 6 newspaper_header, 7 page_number, 8 photograph, 9 table.
Fixed mapping (set before any page was run):
  body text, defines gutters by the reference rule and the 3% cap:  article
  non-text, gutters mostly inside them are ignored:  cartoon_or_advertisement, masthead,
      newspaper_header, photograph, table
  neither:  author, headline, image_caption, page_number

    asmodel.py --model PATH --page-cache DIR --pages-from refs.json --out detections.json
               [--shard-index I --shard-count N] [--threads 2] [--ids-file FILE] [--boxes-out FILE]

Output is keyed JSON in columns.py's format (page id -> {gutters, height, width}, gutters are lists of
segments in the page's own pixels); --boxes-out also writes the raw boxes per page.
"""
import argparse
import ast
import json
import os
import sys
import time

import numpy as np

import refgutters
import refs as refsmod
import score

NAMES = ["article", "author", "cartoon_or_advertisement", "headline", "image_caption", "masthead",
         "newspaper_header", "page_number", "photograph", "table"]
BODY = {"article"}
NONTEXT = {"cartoon_or_advertisement", "masthead", "newspaper_header", "photograph", "table"}
IMGSZ = 1280
CONF = 0.01
IOU = 0.1
MAX_DET = 2000


def letterbox(img, size=IMGSZ, color=114):
    """Upstream letterbox with auto=False: scale to fit, centre, pad. Returns (image, ratio, (left, top))."""
    import cv2
    h, w = img.shape[:2]
    r = min(size / h, size / w)
    nw, nh = int(round(w * r)), int(round(h * r))
    dw, dh = (size - nw) / 2.0, (size - nh) / 2.0
    if (w, h) != (nw, nh):
        img = cv2.resize(img, (nw, nh), interpolation=cv2.INTER_LINEAR)
    top, bottom = int(round(dh - 0.1)), int(round(dh + 0.1))
    left, right = int(round(dw - 0.1)), int(round(dw + 0.1))
    img = cv2.copyMakeBorder(img, top, bottom, left, right, cv2.BORDER_CONSTANT, value=(color,) * 3)
    return img, r, (left, top)


def nms_agnostic(boxes, scores, iou, max_det):
    """Greedy class-agnostic NMS; boxes are (n, 4) x0, y0, x1, y1."""
    order = np.argsort(-scores)
    keep = []
    areas = (boxes[:, 2] - boxes[:, 0]) * (boxes[:, 3] - boxes[:, 1])
    while len(order) and len(keep) < max_det:
        i = order[0]
        keep.append(i)
        if len(order) == 1:
            break
        rest = order[1:]
        xx0 = np.maximum(boxes[i, 0], boxes[rest, 0])
        yy0 = np.maximum(boxes[i, 1], boxes[rest, 1])
        xx1 = np.minimum(boxes[i, 2], boxes[rest, 2])
        yy1 = np.minimum(boxes[i, 3], boxes[rest, 3])
        inter = np.maximum(0, xx1 - xx0) * np.maximum(0, yy1 - yy0)
        union = areas[i] + areas[rest] - inter
        order = rest[inter / np.maximum(union, 1e-9) <= iou]
    return np.array(keep, dtype=int)


def decode(out, ratio, pad, shape):
    """Model output [1, 4 + nc, N] -> list of (x0, y0, x1, y1, conf, class) in page pixels."""
    x = out[0].T
    cls = x[:, 4:]
    conf = cls.max(axis=1)
    j = cls.argmax(axis=1)
    m = conf > CONF
    x, conf, j = x[m], conf[m], j[m]
    if not len(x):
        return []
    b = np.empty((len(x), 4), dtype=np.float32)
    b[:, 0] = x[:, 0] - x[:, 2] / 2
    b[:, 1] = x[:, 1] - x[:, 3] / 2
    b[:, 2] = x[:, 0] + x[:, 2] / 2
    b[:, 3] = x[:, 1] + x[:, 3] / 2
    keep = nms_agnostic(b, conf, IOU, MAX_DET)
    b, conf, j = b[keep], conf[keep], j[keep]
    h, w = shape
    b[:, [0, 2]] = ((b[:, [0, 2]] - pad[0]) / ratio).clip(0, w)
    b[:, [1, 3]] = ((b[:, [1, 3]] - pad[1]) / ratio).clip(0, h)
    return [(float(a[0]), float(a[1]), float(a[2]), float(a[3]), float(c), int(k)) for a, c, k in zip(b, conf, j)]


class Detector:
    def __init__(self, model_path, threads=2):
        import onnxruntime as ort
        so = ort.SessionOptions()
        so.intra_op_num_threads = threads
        so.log_severity_level = 3
        self.sess = ort.InferenceSession(model_path, so, providers=["CPUExecutionProvider"])
        names = ast.literal_eval(self.sess.get_modelmeta().custom_metadata_map["names"])  # a dict literal
        if [names[i] for i in range(len(names))] != NAMES:
            raise SystemExit(f"class names differ from the fixed mapping: {names}")
        self.input = self.sess.get_inputs()[0].name

    def boxes(self, gray):
        """Detections on a grayscale page (replicated to three channels) in the page's own pixels."""
        img = np.repeat(gray[:, :, None], 3, axis=2)
        im, ratio, pad = letterbox(img)
        x = im.transpose(2, 0, 1)[None].astype(np.float32) / 255.0
        out = self.sess.run(None, {self.input: np.ascontiguousarray(x)})[0]
        return decode(out, ratio, pad, gray.shape)


def gutters_from_boxes(dets, size, scale):
    """Body-text gutters (reference rule + 3% cap), dropping those mostly inside non-text boxes.

    dets: boxes in the cached page's pixels; scale = (sx, sy) to the original page; size = original (w, h).
    """
    sx, sy = scale
    body = [(d[0] * sx, d[1] * sy, d[2] * sx, d[3] * sy) for d in dets if NAMES[d[5]] in BODY]
    body = [b for b in body if b[2] > b[0] and b[3] > b[1]]
    non = [[d[0] * sx, d[1] * sy, d[2] * sx, d[3] * sy] for d in dets if NAMES[d[5]] in NONTEXT]
    w, h = size
    chains = refgutters.merge_gutters(refgutters.pair_gutters(body), w, h) if len(body) > 1 else []
    chains = refsmod.cap_gutters(chains, w)
    return [g for g in chains if score.excluded_fraction(g, non) <= score.IGNORE_FRAC]


def main(argv=None):
    import columns
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--model", required=True)
    ap.add_argument("--page-cache", required=True)
    ap.add_argument("--pages-from", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--ids-file")
    ap.add_argument("--shard-index", type=int, default=0)
    ap.add_argument("--shard-count", type=int, default=1)
    ap.add_argument("--threads", type=int, default=2)
    ap.add_argument("--boxes-out")
    a = ap.parse_args(argv)
    ids = sorted(json.load(open(a.pages_from))["pages"])
    if a.ids_file:
        keep = {ln.strip() for ln in open(a.ids_file) if ln.strip()}
        ids = [i for i in ids if i in keep]
    ids = ids[a.shard_index:: a.shard_count]
    det = Detector(a.model, a.threads)
    res, raw = {}, {}
    t0 = time.time()
    for n, pid in enumerate(ids, 1):
        gray, size = columns.load_cached(a.page_cache, pid)
        dets = det.boxes(gray)
        sx, sy = size[0] / gray.shape[1], size[1] / gray.shape[0]
        g = gutters_from_boxes(dets, size, (sx, sy))
        res[pid] = {"gutters": [[[round(v) for v in s] for s in c] for c in g], "height": size[1], "width": size[0]}
        raw[pid] = [[round(d[0] * sx), round(d[1] * sy), round(d[2] * sx), round(d[3] * sy), round(d[4], 3), NAMES[d[5]]] for d in dets]
        print(f"pages {n}/{len(ids)}", flush=True)
    res["_meta"] = {"method": "american-stories-yolov8", "pages": len(ids), "wall_seconds": round(time.time() - t0, 2),
                    "conf": CONF, "iou": IOU, "max_det": MAX_DET, "imgsz": IMGSZ}
    with open(a.out, "w") as f:
        json.dump(res, f, sort_keys=True)
    if a.boxes_out:
        with open(a.boxes_out, "w") as f:
            json.dump(raw, f, sort_keys=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
