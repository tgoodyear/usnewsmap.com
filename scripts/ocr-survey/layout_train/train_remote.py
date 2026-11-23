#!/usr/bin/env python3
"""Train the #2107 layout detector on the rented GPU (runs on the instance, not locally).

Ultralytics YOLOv8 (AGPL-3.0, https://github.com/ultralytics/ultralytics), COCO-pretrained yolov8m.pt,
fine-tuned on the dataset built by build_dataset.py. Distillation from the American Stories layout
model (Dell et al., arXiv:2308.12477) plus finlam and La Liberte (Teklia, MIT) and Beyond Words
(Library of Congress, CC0).

Settings are the plan's, nothing else tuned: imgsz 1280, epochs 60, seed 2107, fliplr 0, Ultralytics
defaults otherwise (batch 16 unless --batch says otherwise; recorded). One epoch checkpoint is saved
per epoch so that the checkpoint kept is the one with the best validation mAP50-95 (Ultralytics'
own best.pt ranks by a fitness of 0.1*mAP50 + 0.9*mAP50-95, so it is kept separately).

    train_remote.py --data-root DIR --out DIR [--batch 16]
"""
import argparse
import csv
import hashlib
import json
import os
import shutil
import sys


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--data-root", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--batch", type=int, default=16)
    ap.add_argument("--resume", action="store_true", help="continue the run from its last.pt after a crash (same settings)")
    ap.add_argument("--resume-batch", type=int, default=0, help="smaller fixed batch for a resume after an out-of-memory crash (a recorded deviation)")
    a = ap.parse_args(argv)
    import torch
    import ultralytics
    from ultralytics import YOLO
    from ultralytics.utils.torch_utils import strip_optimizer
    root = os.path.abspath(a.data_root)
    yaml = os.path.join(root, "data.yaml")
    txt = open(yaml).read().replace("DATASET_ROOT", root)
    open(yaml, "w").write(txt)
    os.makedirs(a.out, exist_ok=True)
    run0 = os.path.join(a.out, "runs", "train")
    model = YOLO(os.path.join(run0, "weights", "last.pt")) if a.resume else YOLO("yolov8m.pt")
    base = None
    for cand in ("yolov8m.pt", os.path.expanduser("~/.config/Ultralytics/yolov8m.pt")):
        if os.path.exists(cand):
            base = cand
            break
    info = {"ultralytics": ultralytics.__version__, "torch": torch.__version__,
            "gpu": torch.cuda.get_device_name(0), "gpu_mem_gb": round(torch.cuda.get_device_properties(0).total_memory / 2**30, 1),
            "yolov8m_pt_sha256": sha256(base) if base else None, "batch": a.batch}
    if not a.resume:
        json.dump(info, open(os.path.join(a.out, "versions.json"), "w"), indent=1)
    print(info, flush=True)
    if a.resume:
        model.train(resume=True, **({"batch": a.resume_batch} if a.resume_batch else {}))
    else:
        model.train(data=yaml, imgsz=1280, epochs=60, seed=2107, fliplr=0.0, batch=a.batch, save_period=1,
                    project=os.path.join(a.out, "runs"), name="train", exist_ok=True)
    run = os.path.join(a.out, "runs", "train")
    rows = list(csv.DictReader(open(os.path.join(run, "results.csv"))))
    rows = [{k.strip(): v for k, v in r.items()} for r in rows]
    key = "metrics/mAP50-95(B)"
    best = max(rows, key=lambda r: float(r[key]))
    ep = int(float(best["epoch"]))
    src = os.path.join(run, "weights", "epoch%d.pt" % (ep - 1))
    if not os.path.exists(src):
        src = os.path.join(run, "weights", "epoch%d.pt" % ep)
    dst = os.path.join(a.out, "best.pt")
    shutil.copy(src, dst)
    strip_optimizer(dst)
    shutil.copy(os.path.join(run, "weights", "best.pt"), os.path.join(a.out, "best_fitness.pt"))
    sel = {"selected_epoch": ep, "selected_from": os.path.basename(src), "mAP50-95": float(best[key]),
           "best_pt_sha256": sha256(dst), "ultralytics_best_fitness_sha256": sha256(os.path.join(a.out, "best_fitness.pt")),
           "epochs_run": len(rows)}
    json.dump(sel, open(os.path.join(a.out, "selection.json"), "w"), indent=1)
    shutil.copy(os.path.join(run, "results.csv"), os.path.join(a.out, "results.csv"))
    shutil.copy(os.path.join(run, "args.yaml"), os.path.join(a.out, "args.yaml"))
    print(sel, flush=True)
    open(os.path.join(a.out, "DONE"), "w").write("ok\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
