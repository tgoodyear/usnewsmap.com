#!/usr/bin/env python3
"""Run the trained #2107 layout detector over evaluation pages (one-time evaluation, no tuning).

Detector: Ultralytics YOLOv8 (AGPL-3.0), fine-tuned in layout_train/train_remote.py from COCO weights on
finlam and La Liberte (Teklia, MIT), Beyond Words (Library of Congress, CC0) and American Stories
(Dell et al., arXiv:2308.12477) teacher labels. The checkpoint's sha256 is checked before any page runs.

Inference settings are Ultralytics' predict defaults (conf 0.25, NMS IoU 0.7 per class, max_det 300)
except: imgsz 1280 (the trained size; the default would be 640) and a candidate floor of conf 0.01. The
floor only adds boxes below 0.25 (higher-confidence boxes are untouched by lower ones in NMS), so the
boxes at conf >= 0.25 are the ones a default run gives; the extra ones let box-level AP sweep confidence.
The page is fed as the step-1 contender was fed: grayscale replicated to three channels.

Two sources:
  --cache-dir DIR --refs refs.json     pagecache.py arrays (the pages step 1 scored); boxes are mapped back
                                       to the original page's pixels, as asmodel.py does
  --rows rows.jsonl --images-zip Z     Beyond Words rows (stored image frame), images read from the zip

    eval_infer.py --weights best.pt --expect-sha256 HEX --out boxes.json (--cache-dir ... --refs ... |
                  --rows ... --images-zip ...) [--shard-index I --shard-count N] [--threads 4] [--limit N]

Output: {"_meta": ..., "pages": {page_id: [[x0, y0, x1, y1, conf, class], ...]}, "sizes": {page_id: [w, h]}}
"""
import argparse
import hashlib
import json
import os
import sys
import time
import zipfile

import numpy as np

CONF_FLOOR = 0.01
IMGSZ = 1280
NAMES = ["article", "author", "cartoon_or_advertisement", "headline", "image_caption", "masthead",
         "newspaper_header", "page_number", "photograph", "table"]


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def pages(a):
    """Yield (page_id, gray uint8 array, (sx, sy), (w, h)): sx, sy map array pixels to page pixels."""
    if a.cache_dir:
        sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "layout_columns"))
        import columns
        ids = sorted(json.load(open(a.refs))["pages"])
        for pid in ids[a.shard_index:: a.shard_count][: a.limit or None]:
            gray, size = columns.load_cached(a.cache_dir, pid)
            yield pid, gray, (size[0] / gray.shape[1], size[1] / gray.shape[0]), size
    else:
        import cv2
        rows = {}
        for ln in open(a.rows):
            r = json.loads(ln)
            if r.get("split") != "val":
                raise SystemExit("not a validation row: %s" % r.get("page_id"))
            rows[r["page_id"]] = r
        zf = zipfile.ZipFile(a.images_zip)
        for pid in sorted(rows)[a.shard_index:: a.shard_count][: a.limit or None]:
            r = rows[pid]
            gray = cv2.imdecode(np.frombuffer(zf.read(r["image_path"].split("!", 1)[1]), np.uint8), cv2.IMREAD_GRAYSCALE)
            if gray is None or gray.shape != (r["height"], r["width"]):
                raise SystemExit("page %s: image does not match its row" % pid)
            yield pid, gray, (1.0, 1.0), (r["width"], r["height"])


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--weights", required=True)
    ap.add_argument("--expect-sha256", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--cache-dir")
    ap.add_argument("--refs")
    ap.add_argument("--rows")
    ap.add_argument("--images-zip")
    ap.add_argument("--shard-index", type=int, default=0)
    ap.add_argument("--shard-count", type=int, default=1)
    ap.add_argument("--threads", type=int, default=4)
    ap.add_argument("--limit", type=int, default=0)
    a = ap.parse_args(argv)
    if bool(a.cache_dir) == bool(a.rows):
        raise SystemExit("give --cache-dir with --refs, or --rows with --images-zip")
    got = sha256(a.weights)
    if got != a.expect_sha256:
        raise SystemExit("checkpoint sha256 %s differs from the expected %s" % (got, a.expect_sha256))
    import torch
    import ultralytics
    from ultralytics import YOLO
    torch.set_num_threads(a.threads)
    model = YOLO(a.weights)
    if [model.names[i] for i in range(len(model.names))] != NAMES:
        raise SystemExit("class names differ: %s" % model.names)
    out, sizes = {}, {}
    t0, n = time.time(), 0
    for pid, gray, (sx, sy), size in pages(a):
        img = np.ascontiguousarray(np.repeat(gray[:, :, None], 3, axis=2))
        r = model.predict(img, imgsz=IMGSZ, conf=CONF_FLOOR, device="cpu", verbose=False)[0]
        b = r.boxes
        xyxy, conf, cls = b.xyxy.cpu().numpy(), b.conf.cpu().numpy(), b.cls.cpu().numpy().astype(int)
        out[pid] = [[round(float(x[0]) * sx, 1), round(float(x[1]) * sy, 1), round(float(x[2]) * sx, 1),
                     round(float(x[3]) * sy, 1), round(float(c), 4), NAMES[k]] for x, c, k in zip(xyxy, conf, cls)]
        sizes[pid] = [size[0], size[1]]
        n += 1
        print("pages %d  %.1fs" % (n, time.time() - t0), flush=True)
    meta = {"checkpoint_sha256": got, "ultralytics": ultralytics.__version__, "torch": torch.__version__,
            "imgsz": IMGSZ, "conf_floor": CONF_FLOOR, "iou": 0.7, "max_det": 300, "pages": n,
            "shard_index": a.shard_index, "shard_count": a.shard_count, "wall_seconds": round(time.time() - t0, 1)}
    with open(a.out, "w") as f:
        json.dump({"_meta": meta, "pages": out, "sizes": sizes}, f, sort_keys=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
