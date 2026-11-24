#!/usr/bin/env python3
"""Train the v2 column detector (torchvision Faster R-CNN MobileNetV3-FPN) on column-region labels (#2107).

    train.py --labels liberte/labels.jsonl --labels loc/labels.jsonl [--labels cg/labels.jsonl]
             --source-weight loc-alto=2 --out-dir OUT --iterations 20000 --batch-size 8
             [--init none|imagenet] [--device cpu|cuda] [--cache-dir DIR] [--val-frac 0.05]

Holdout: per source, pages whose sha256("2107:colv2:val:" + source + ":" + page_id) falls in the first
--val-frac of the hash range are held out (validation loss only; the real measures run through
predict.py, to_gutters.py and layout_columns/score.py on the frozen test sets). Sampling: each batch
draws a source with probability proportional to its --source-weight (default 1) times its train pages,
then a page uniformly. Augmentation: scale 0.8-1.0, horizontal flip, contrast/brightness jitter.

Images are read once, turned grey, resized to model.MAX_SIDE on the longer side and cached as PNG in
--cache-dir (derived images, deletable). Writes OUT/model.pt (state_dict), OUT/train.json (arguments,
label-file sha256, pages per source and split, loss log). Progress: `iter i/N loss ...`.
"""
import argparse
import hashlib
import io
import json
import os
import random
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import imgread  # noqa: E402
import model as M  # noqa: E402


def sha_file(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def is_val(source, pid, frac):
    k = int(hashlib.sha256(("2107:colv2:val:%s:%s" % (source, pid)).encode()).hexdigest()[:8], 16)
    return k < frac * 0x100000000


def load_labels(paths):
    rows = []
    for p in paths:
        with open(p) as f:
            for ln in f:
                if ln.strip():
                    rows.append(json.loads(ln))
    return rows


class PageCache:
    """Grey, resized page images (PNG) keyed by source and page id."""

    def __init__(self, cache_dir):
        self.dir = cache_dir
        self.reader = imgread.Reader()
        os.makedirs(cache_dir, exist_ok=True)

    def get(self, row):
        from PIL import Image
        key = hashlib.sha256((row["source"] + ":" + str(row["page_id"])).encode()).hexdigest()[:24]
        p = os.path.join(self.dir, key + ".png")
        if os.path.exists(p):
            img = Image.open(p)
            img.load()
            return img, img.size[0] / float(row["width"])
        img = Image.open(io.BytesIO(self.reader.read(row["image_path"]))).convert("L")
        if img.size != (row["width"], row["height"]):
            raise RuntimeError("%s: image %s, label says %s" % (row["page_id"], img.size, (row["width"], row["height"])))
        img, s = M.resize_for_model(img)
        img.save(p + ".part.png")
        os.replace(p + ".part.png", p)
        return img, s


def sample_tensor(row, cache, rng, augment=True):
    import torch
    from PIL import Image, ImageEnhance
    img, s = cache.get(row)
    boxes = [[v * s for v in b] for b in row["columns"]]
    if augment:
        f = rng.uniform(0.8, 1.0)
        if f < 0.999:
            img = img.resize((max(1, round(img.size[0] * f)), max(1, round(img.size[1] * f))), Image.BILINEAR)
            boxes = [[v * f for v in b] for b in boxes]
        if rng.random() < 0.5:
            w = img.size[0]
            img = img.transpose(Image.FLIP_LEFT_RIGHT)
            boxes = [[w - b[2], b[1], w - b[0], b[3]] for b in boxes]
        img = ImageEnhance.Contrast(img).enhance(rng.uniform(0.7, 1.3))
        img = ImageEnhance.Brightness(img).enhance(rng.uniform(0.85, 1.15))
    t = M.to_tensor(img)
    bt = torch.tensor(boxes, dtype=torch.float32).reshape(-1, 4)
    keep = (bt[:, 2] - bt[:, 0] >= 1) & (bt[:, 3] - bt[:, 1] >= 1)
    bt = bt[keep]
    return t, {"boxes": bt, "labels": torch.ones(len(bt), dtype=torch.int64)}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--labels", action="append", required=True)
    ap.add_argument("--source-weight", action="append", default=[], help="SOURCE=W, repeatable")
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--cache-dir")
    ap.add_argument("--iterations", type=int, default=1000)
    ap.add_argument("--batch-size", type=int, default=4)
    ap.add_argument("--lr", type=float, default=0.01)
    ap.add_argument("--init", choices=["none", "imagenet"], default="none")
    ap.add_argument("--device", default="cpu")
    ap.add_argument("--threads", type=int, default=0)
    ap.add_argument("--val-frac", type=float, default=0.05)
    ap.add_argument("--val-pages", type=int, default=50, help="holdout pages scored for validation loss")
    ap.add_argument("--eval-every", type=int, default=500)
    ap.add_argument("--log-every", type=int, default=20)
    ap.add_argument("--seed", type=int, default=2107)
    a = ap.parse_args(argv)

    import torch
    if a.threads:
        torch.set_num_threads(a.threads)
    torch.manual_seed(a.seed)
    rng = random.Random(a.seed)
    os.makedirs(a.out_dir, exist_ok=True)
    cache = PageCache(a.cache_dir or os.path.join(a.out_dir, "cache"))
    rows = load_labels(a.labels)
    by_src = {}
    for r in rows:
        sp = "val" if is_val(r["source"], r["page_id"], a.val_frac) else "train"
        by_src.setdefault(r["source"], {"train": [], "val": []})[sp].append(r)
    weights = {k: 1.0 for k in by_src}
    for spec in a.source_weight:
        k, v = spec.split("=", 1)
        weights[k] = float(v)
    srcs = sorted(k for k in by_src if by_src[k]["train"])
    probs = [weights[k] * len(by_src[k]["train"]) for k in srcs]
    val = sorted((r for k in sorted(by_src) for r in by_src[k]["val"]),
                 key=lambda r: hashlib.sha256((r["source"] + str(r["page_id"])).encode()).hexdigest())[:a.val_pages]

    net = M.build(a.init).to(a.device)
    params = [p for p in net.parameters() if p.requires_grad]
    opt = torch.optim.SGD(params, lr=a.lr, momentum=0.9, weight_decay=1e-4)
    warm = min(500, max(1, a.iterations // 10))
    sched = torch.optim.lr_scheduler.LambdaLR(
        opt, lambda i: min(1.0, (i + 1) / warm) * (0.1 if i > 0.8 * a.iterations else 1.0))
    log, t0 = [], time.time()

    def val_loss():
        if not val:
            return None
        tot = 0.0
        with torch.no_grad():
            for r in val:
                x, y = sample_tensor(r, cache, rng, augment=False)
                ld = net([x.to(a.device)], [{k: v.to(a.device) for k, v in y.items()}])
                tot += float(sum(ld.values()))
        return tot / len(val)

    for it in range(1, a.iterations + 1):
        net.train()
        xs, ys = [], []
        for _ in range(a.batch_size):
            src = rng.choices(srcs, weights=probs)[0]
            x, y = sample_tensor(rng.choice(by_src[src]["train"]), cache, rng)
            xs.append(x.to(a.device))
            ys.append({k: v.to(a.device) for k, v in y.items()})
        ld = net(xs, ys)
        loss = sum(ld.values())
        opt.zero_grad()
        loss.backward()
        torch.nn.utils.clip_grad_norm_(params, 10.0)
        opt.step()
        sched.step()
        if it % a.log_every == 0 or it == 1 or it == a.iterations:
            e = {"iter": it, "loss": round(float(loss), 4), "s": round(time.time() - t0, 1),
                 **{k: round(float(v), 4) for k, v in ld.items()}}
            if it % a.eval_every == 0 or it == a.iterations:
                net.train()
                e["val_loss"] = val_loss()
            log.append(e)
            print("iter %d/%d loss %.4f %s" % (it, a.iterations, float(loss), json.dumps(e, sort_keys=True)), flush=True)
    torch.save(net.state_dict(), os.path.join(a.out_dir, "model.pt"))
    meta = {"args": vars(a), "label_sha256": {p: sha_file(p) for p in a.labels}, "log": log,
            "pages": {k: {sp: len(v[sp]) for sp in ("train", "val")} for k, v in sorted(by_src.items())},
            "torch": torch.__version__, "weights": weights, "seconds": round(time.time() - t0, 1)}
    with open(os.path.join(a.out_dir, "train.json"), "w") as f:
        json.dump(meta, f, indent=1, sort_keys=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
