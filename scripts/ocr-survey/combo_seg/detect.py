#!/usr/bin/env python3
"""Run the pooled detectors of the combined segmenter (#2107) over cached pages.

Writes, in --out-dir:
  v1.json          banded finder (layout_columns/columns.py at its frozen defaults) gutters, columns.py format
  as-boxes.json    American Stories boxes (conf floor 0.01, agnostic NMS 0.1), {page id: [[x0, y0, x1, y1, conf, class]]}
  tr-boxes.json    trained YOLOv8m boxes (conf floor 0.01, per-class NMS 0.7, imgsz 1280), same shape
Gutters from the two box sets are drawn at pooling time (as at 0.01, tr at 0.25), by diag.gutters_generic.
The banded v2 finder is not run here: when branch banded-v2-2107 lands, its output is given to
build_features.py as v2.json.

American Stories: Dell et al., arXiv:2308.12477; ONNX mirror NealCaren/american-stories-onnx (research use only:
weights are never committed, the path is a flag). Trained model: Ultralytics YOLOv8 (AGPL-3.0), #2107 best.pt.

    detect.py --page-cache DIR --pages-from refs.json --out-dir DIR --as-model PATH --as-sha256 HEX
              [--trained-model best.pt --trained-sha256 HEX] [--threads 4] [--shard-index I --shard-count N]
"""
import argparse
import hashlib
import json
import os
import sys
import time

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "layout_columns"))
import columns  # noqa: E402

TR_NAMES = ["article", "author", "cartoon_or_advertisement", "headline", "image_caption", "masthead",
            "newspaper_header", "page_number", "photograph", "table"]


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def page_ids(path):
    if path.endswith(".json"):
        return sorted(json.load(open(path))["pages"])
    return sorted(ln.strip() for ln in open(path) if ln.strip())


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--page-cache", required=True)
    ap.add_argument("--pages-from", required=True, help="refs JSON (pages keys) or a file of page ids")
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--as-model", required=True)
    ap.add_argument("--as-sha256", required=True)
    ap.add_argument("--trained-model")
    ap.add_argument("--trained-sha256")
    ap.add_argument("--threads", type=int, default=4)
    ap.add_argument("--shard-index", type=int, default=0)
    ap.add_argument("--shard-count", type=int, default=1)
    a = ap.parse_args(argv)
    got = sha256(a.as_model)
    if got != a.as_sha256:
        raise SystemExit("American Stories model sha256 %s, expected %s" % (got, a.as_sha256))
    import asmodel
    det = asmodel.Detector(a.as_model, a.threads)
    tr = None
    if a.trained_model:
        got_tr = sha256(a.trained_model)
        if got_tr != a.trained_sha256:
            raise SystemExit("trained model sha256 %s, expected %s" % (got_tr, a.trained_sha256))
        import torch
        from ultralytics import YOLO
        torch.set_num_threads(a.threads)
        tr = YOLO(a.trained_model)
        if [tr.names[i] for i in range(len(tr.names))] != TR_NAMES:
            raise SystemExit("trained model class names differ: %s" % tr.names)
    ids = page_ids(a.pages_from)[a.shard_index:: a.shard_count]
    os.makedirs(a.out_dir, exist_ok=True)
    p0 = columns.Params()
    v1, asb, trb = {}, {}, {}
    t0 = time.time()
    for n, pid in enumerate(ids, 1):
        gray, size = columns.load_cached(a.page_cache, pid)
        sx, sy = size[0] / gray.shape[1], size[1] / gray.shape[0]
        v1[pid] = {"gutters": columns.detect(gray, size, "banded", p0), "height": size[1], "width": size[0]}
        asb[pid] = [[round(d[0] * sx), round(d[1] * sy), round(d[2] * sx), round(d[3] * sy), round(d[4], 3), asmodel.NAMES[d[5]]]
                    for d in det.boxes(gray)]
        if tr is not None:
            img = np.ascontiguousarray(np.repeat(gray[:, :, None], 3, axis=2))
            r = tr.predict(img, imgsz=1280, conf=0.01, device="cpu", verbose=False)[0]
            b = r.boxes
            xyxy, conf, cls = b.xyxy.cpu().numpy(), b.conf.cpu().numpy(), b.cls.cpu().numpy().astype(int)
            trb[pid] = [[round(float(x[0]) * sx, 1), round(float(x[1]) * sy, 1), round(float(x[2]) * sx, 1),
                         round(float(x[3]) * sy, 1), round(float(c), 4), TR_NAMES[k]] for x, c, k in zip(xyxy, conf, cls)]
        if n % 10 == 0 or n == len(ids):
            print("pages %d/%d %.0fs" % (n, len(ids), time.time() - t0), flush=True)
    sfx = "" if a.shard_count == 1 else ".%d-of-%d" % (a.shard_index, a.shard_count)
    v1["_meta"] = {"method": "banded", "params": "columns.Params() defaults", "pages": len(ids)}
    json.dump(v1, open(os.path.join(a.out_dir, "v1%s.json" % sfx), "w"), sort_keys=True)
    json.dump(asb, open(os.path.join(a.out_dir, "as-boxes%s.json" % sfx), "w"), sort_keys=True)
    if tr is not None:
        json.dump(trb, open(os.path.join(a.out_dir, "tr-boxes%s.json" % sfx), "w"), sort_keys=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
