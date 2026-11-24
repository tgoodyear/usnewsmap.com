"""Classifier v2 dev work on spent, labelled pages (#2128, after the Amendment 3.2 gate). Dev only:
no new labels, no gate sample.

  dev3.py build   --spec spec.json --out-dir D        # labels + box universe of the spent pages
  dev3.py errors  --dir D --sample gateb --reads gate-reads.jsonl --t 0.97
  dev3.py crops   --dir D --silver S --aligned-ghn R --aligned v2/aligned.jsonl --loc-images L
                  --fitset fitset-oof.jsonl --inventory INV --ghn-root G
  dev3.py cnn     --dir D [--epochs 4] [--folds 5]
  dev3.py eval    --dir D [--folds 5] [--draws 2000]

spec.json: {"samples": [{"name", "key", "labels": [a, b, c], "agree_only": bool}], "scored": [...],
"noink": [...]}. Labels are combined as clf_score.py (307d960a) does; X is left out; agree_only keeps
only crops where A = B. Each labelled box keeps its own sample's HT weight (1 for a box its sample took
with certainty, N/n for a sampled one), so any classifier can be evaluated on any sample:
recall = sum(w chr call) / sum(w chr), Latin share = sum(w lat call) / sum(w call).
Folds: pages, by sha256("2128:dev3-fold:"+page_id) mod --folds. t is chosen inside each training fold
(max recall with Latin share <= 0.03, grid 0.01), then applied to the held-out fold.
"""
import argparse
import collections
import json
import math
import re
import sys
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent / "clf"))
import clf2  # noqa: E402
import clf_score as sc  # noqa: E402

cs = clf2.cs
sha, jsonl = cs.sha, cs.jsonl
H, W = 32, 256
GRID = np.round(np.arange(0.01, 1.0, 0.01), 2)


def fold_of(p, k):
    return int(sha("2128:dev3-fold:" + p), 16) % k


# ---------------------------------------------------------------- data
def cmd_build(a):
    spec = json.load(open(a.spec))
    out = Path(a.out_dir)
    out.mkdir(parents=True, exist_ok=True)
    uni = {}
    for f in spec["scored"]:
        for r in jsonl(f):
            uni[(r["page_id"], r["box_id"])] = {"page_id": r["page_id"], "box_id": r["box_id"], "box": r["box"],
                                                "scan": r["scan"], "p_v2": r["prob"], "x": r["x"], "noink": False}
    for f in spec["noink"]:
        for r in jsonl(f):
            uni.setdefault((r["page_id"], r["box_id"]), {"page_id": r["page_id"], "box_id": r["box_id"], "box": r["box"],
                                                         "scan": r["scan"], "p_v2": 0.0, "x": None, "noink": True})
    labs, stats = [], {}
    for s in spec["samples"]:
        key = [k for k in jsonl(s["key"]) if k.get("source") == "ghn"]
        A, B, C = (sc.read_tsv(p) for p in s["labels"])
        c = collections.Counter()
        for k in key:
            i = k["crop_id"]
            f = sc.final(A, B, C, i)
            if s.get("agree_only") and A[i] != B[i]:
                c["dropped_disagree"] += 1
                continue
            t = {"C": 1, "M": 1, "L": 0}.get(f)
            if t is None:
                c["X"] += 1
                continue
            if (k["page_id"], k["box_id"]) not in uni:
                c["not_in_universe"] += 1
                continue
            c["chr" if t else "lat"] += 1
            labs.append({"sample": s["name"], "page_id": k["page_id"], "box_id": k["box_id"], "y": t,
                         "w": float(k["weight"]), "label": f})
        stats[s["name"]] = dict(c, pages=len({k["page_id"] for k in key}))
    pages = {l["page_id"] for l in labs}
    rows = [u for u in uni.values() if u["page_id"] in pages]
    rows.sort(key=lambda u: (u["page_id"], u["box_id"]))
    clf2.cs_write(out / "universe.jsonl", rows)
    clf2.cs_write(out / "labels.jsonl", labs)
    stats["universe_boxes"] = len(rows)
    stats["pages"] = len(pages)
    (out / "build.json").write_text(json.dumps(stats, indent=1) + "\n")
    print(json.dumps(stats, indent=1))


def cmd_errors(a):
    """Tally of the misses (true Cherokee, v2 prob < t) of one sample, first matching rule wins:
    mixed (label M); crop error (height > 1.8x or < 0.55x the page median: merged or cut);
    heading/display (1.3x to 1.8x); short (< 8 syllabary letters in the chr_phoenix reading);
    faded (crop p95 - p5 < 110 on the greyscale scan); other."""
    from PIL import Image
    d = Path(a.dir)
    uni = {(u["page_id"], u["box_id"]): u for u in jsonl(d / "universe.jsonl")}
    reads = {r["item_id"]: r for r in jsonl(a.reads)}
    miss = [l for l in jsonl(d / "labels.jsonl") if l["sample"] == a.sample and l["y"] == 1
            and uni[(l["page_id"], l["box_id"])]["p_v2"] < a.t]
    imgs, tally, wt = {}, collections.Counter(), collections.Counter()
    rows = []
    for l in miss:
        u = uni[(l["page_id"], l["box_id"])]
        if u["scan"] not in imgs:
            imgs.clear()
            imgs[u["scan"]] = np.asarray(Image.open(u["scan"]).convert("L"))
        im = imgs[u["scan"]]
        x0, y0, x1, y1 = u["box"]
        crop = im[y0:y1, x0:x1]
        con = float(np.percentile(crop, 95) - np.percentile(crop, 5)) if crop.size else 0.0
        rel_h = u["x"][5] if u["x"] else None
        r = reads.get(f"{u['page_id']}#{u['box_id']}")
        nsyl = len(cs.SYLLABARY.findall(r["chr_text"])) if r else 0
        if l["label"] == "M":
            cat = "mixed"
        elif rel_h is not None and (rel_h > 1.8 or rel_h < 0.55):
            cat = "crop_error"
        elif rel_h is not None and rel_h > 1.3:
            cat = "heading_display"
        elif nsyl < 8:
            cat = "short"
        elif con < 110:
            cat = "faded"
        elif u["noink"]:
            cat = "no_ink"
        else:
            cat = "other"
        tally[cat] += 1
        wt[cat] += l["w"]
        rows.append({**l, "cat": cat, "rel_h": rel_h, "contrast": con, "syl": nsyl, "p_v2": u["p_v2"]})
    clf2.cs_write(d / f"errors-{a.sample}.jsonl", rows)
    res = {"misses": len(miss), "weighted": round(sum(wt.values()), 1), "count": dict(tally),
           "weighted_by_cat": {k: round(v, 1) for k, v in wt.items()},
           "per_page": dict(collections.Counter(r["page_id"] for r in rows))}
    (d / f"errors-{a.sample}.json").write_text(json.dumps(res, indent=1) + "\n")
    print(json.dumps(res, indent=1))


def norm_crop(im, box, pad=4):
    """Greyscale crop, height H, aspect kept, left-aligned on white, truncated at W."""
    from PIL import Image
    Wi, Hi = im.size
    x0, y0, x1, y1 = box
    c = im.crop((max(0, x0 - pad), max(0, y0 - pad), min(Wi, x1 + pad), min(Hi, y1 + pad)))
    w, h = c.size
    nw = max(1, min(W, round(w * H / max(h, 1))))
    c = c.resize((nw, H), Image.BILINEAR)
    arr = np.full((H, W), 255, np.uint8)
    arr[:, :nw] = np.asarray(c)[:, :nw]
    return arr


def cmd_crops(a):
    from PIL import Image
    Image.MAX_IMAGE_PIXELS = None
    d = Path(a.dir)
    uni = jsonl(d / "universe.jsonl")
    inv = {r["id"]: r for r in jsonl(a.inventory)}
    ext = []
    for path in (a.silver, a.aligned_ghn):
        for r in jsonl(path):
            ext.append({"scan": str(Path(a.ghn_root) / inv[r["page"]]["file"]), "box": r["box"], "y": 1,
                        "src": "nt_ghn", "page_id": "ghn:" + r["page"].replace("/", "_")})
    sizes = {}
    for r in jsonl(a.aligned):
        if r["drop"] not in cs.CAL_TIERS:
            continue
        img = Path(a.loc_images) / (r["page"].replace("/", "_") + ".jpg")
        if img not in sizes:
            with Image.open(img) as im:
                sizes[img] = im.size
        ext.append({"scan": str(img), "box": list(cs.alto_to_px(r["box"], r["alto_page_size"], sizes[img])), "y": 1,
                    "src": "nt_v2", "page_id": "loc:" + r["page"]})
    lat = sorted((r for r in jsonl(a.fitset) if r["y"] == 0), key=lambda r: sha("2128:dev3-eng:" + r["item_id"]))
    for r in lat[:a.n_english]:
        ext.append({"scan": r["scan"], "box": r["box"], "y": 0, "src": "loc_english", "page_id": r["page_id"]})
    allr = [("u", i, u) for i, u in enumerate(uni)] + [("e", i, e) for i, e in enumerate(ext)]
    allr.sort(key=lambda t: t[2]["scan"])
    U = np.lib.format.open_memmap(d / "crops-u.npy", "w+", np.uint8, (len(uni), H, W))
    E = np.lib.format.open_memmap(d / "crops-e.npy", "w+", np.uint8, (len(ext), H, W))
    cur, im = None, None
    for kind, i, r in allr:
        if r["scan"] != cur:
            cur = r["scan"]
            im = Image.open(cur).convert("L")
        (U if kind == "u" else E)[i] = norm_crop(im, r["box"])
    U.flush()
    E.flush()
    clf2.cs_write(d / "ext.jsonl", ext)
    print(json.dumps({"universe": len(uni), "external": dict(collections.Counter(e["src"] for e in ext))}))


# ---------------------------------------------------------------- column clip
CLIP_MARGIN = 0.01   # of page width


def clip_boxes(rows, width):
    """Clip each box horizontally to its S1 column's text edges plus CLIP_MARGIN * page width. A column's
    edges are the median left and right edges of its boxes (justified text: most lines start and end
    there); S1's region rectangles reach the page edge for outer columns, so they don't bound the spill
    into the gutter or the scan margin. Columns with fewer than 5 boxes are left alone; a box is never
    widened, and never clipped below 8 px wide."""
    m = round(CLIP_MARGIN * width)
    g = collections.defaultdict(list)
    for r in rows:
        mm = re.match(r"(r\d+)", r["box_id"])
        g[mm.group(1) if mm else ""].append(r)
    out = {}
    for col, rs in g.items():
        if len(rs) < 5:
            continue
        lo = float(np.median([r["box"][0] for r in rs])) - m
        hi = float(np.median([r["box"][2] for r in rs])) + m
        for r in rs:
            x0, y0, x1, y1 = r["box"]
            nx0, nx1 = max(x0, round(lo)), min(x1, round(hi))
            if (nx0, nx1) != (x0, x1) and nx1 - nx0 >= 8:
                out[r["box_id"]] = [nx0, y0, nx1, y1]
    return out


def cmd_clip(a):
    """Write a clipped copy of the dev universe (--out-dir) and the items whose box changed (to reread)."""
    d, o = Path(a.dir), Path(a.out_dir)
    o.mkdir(parents=True, exist_ok=True)
    uni = jsonl(d / "universe.jsonl")
    width = {r["doc_id"]: r["width"] for r in jsonl(a.found)}
    by = collections.defaultdict(list)
    for u in uni:
        by[u["page_id"]].append(u)
    clipped = {}
    for p, rs in by.items():
        for bid, b in clip_boxes(rs, width[p]).items():
            clipped[(p, bid)] = b
    items = []
    for u in uni:
        b = clipped.get((u["page_id"], u["box_id"]))
        u["box_orig"] = u["box"]
        if b:
            u["box"] = b
            if not u["noink"]:
                items.append({"item_id": f"{u['page_id']}#{u['box_id']}", "page_id": u["page_id"], "box_id": u["box_id"],
                              "scan": u["scan"], "box": b, "p_chr": None, "rel_h": u["x"][5]})
    clf2.cs_write(o / "universe-clipped.jsonl", uni)
    clf2.cs_write(o / "reread-items.jsonl", items)
    labs = jsonl(d / "labels.jsonl")
    ch = [l for l in labs if (l["page_id"], l["box_id"]) in clipped]
    res = {"margin_frac": CLIP_MARGIN, "boxes": len(uni), "clipped": len(clipped), "reread": len(items),
           "labelled_clipped": len(ch), "labelled_clipped_by_label": dict(collections.Counter(l["label"] for l in ch))}
    (o / "clip.json").write_text(json.dumps(res, indent=1) + "\n")
    print(json.dumps(res, indent=1))


def cmd_rescore(a):
    """v2 scores for the clipped universe: clipped boxes get their new readings, the rest keep the old
    ones; p_chr and relative height are the unclipped box's (the shape score isn't recomputed)."""
    o = Path(a.out_dir)
    uni = jsonl(o / "universe-clipped.jsonl")
    reads = {}
    for f in a.reads:
        for r in jsonl(f):
            reads[r["item_id"]] = r
    for r in jsonl(o / "reread.jsonl"):
        reads[r["item_id"]] = r
    model = json.load(open(a.model))
    mc, me, info = clf2.build_models(a)
    assert info["chr"]["sha256"] == model["lm"]["chr"]["sha256"] and info["eng"]["text_sha256"] == model["lm"]["eng"]["text_sha256"]
    live = [u for u in uni if not u["noink"]]
    items = [{"item_id": f"{u['page_id']}#{u['box_id']}", "rel_h": u["x"][5], "p_chr": u["x"][6]} for u in live]
    X = clf2.features(items, [reads[i["item_id"]] for i in items], mc, me)
    p = clf2.predict(model, X)
    for u, x, pp in zip(live, X, p):
        u["x"], u["p_v2"] = [round(v, 5) for v in x], round(float(pp), 6)
    clf2.cs_write(o / "universe.jsonl", uni)
    print(json.dumps({"scored": len(live)}))


# ---------------------------------------------------------------- CNN
def make_net():
    import torch.nn as nn

    def blk(i, o):
        return nn.Sequential(nn.Conv2d(i, o, 3, padding=1), nn.BatchNorm2d(o), nn.ReLU(), nn.MaxPool2d(2))
    return nn.Sequential(blk(1, 16), blk(16, 32), blk(32, 64), blk(64, 64), nn.AdaptiveAvgPool2d((1, 4)),
                         nn.Flatten(), nn.Dropout(0.2), nn.Linear(256, 1))


def augment(x, g):
    """x: float batch in [0,1] (1 = paper). Random fading (ink pulled toward paper), blur, brightness, noise."""
    import torch
    import torch.nn.functional as F
    n = x.shape[0]
    ink = 1 - x
    fade = torch.empty(n, 1, 1, 1).uniform_(0.25, 1.0, generator=g)
    x = 1 - ink * fade
    sig = torch.empty(n).uniform_(0, 1.6, generator=g)
    k = torch.arange(-3, 4, dtype=torch.float32)
    out = []
    for i in range(n):
        if sig[i] < 0.3:
            out.append(x[i:i + 1])
            continue
        w = torch.exp(-k ** 2 / (2 * sig[i] ** 2))
        w = w / w.sum()
        y = F.conv2d(F.pad(x[i:i + 1], (3, 3, 0, 0), mode="replicate"), w.view(1, 1, 1, 7))
        y = F.conv2d(F.pad(y, (0, 0, 3, 3), mode="replicate"), w.view(1, 1, 7, 1))
        out.append(y)
    x = torch.cat(out)
    x = x * torch.empty(n, 1, 1, 1).uniform_(0.8, 1.05, generator=g) + torch.randn(x.shape, generator=g) * 0.03
    return x.clamp(0, 1)


def train_predict(Xtr, ytr, Xte, epochs, seed, return_net=False):
    import torch
    torch.manual_seed(seed)
    g = torch.Generator().manual_seed(seed)
    net = make_net()
    opt = torch.optim.Adam(net.parameters(), 1e-3)
    pos = ytr.mean()
    lossf = torch.nn.BCEWithLogitsLoss(pos_weight=torch.tensor((1 - pos) / max(pos, 1e-6)))
    idx = np.arange(len(ytr))
    rng = np.random.default_rng(seed)
    for ep in range(epochs):
        net.train()
        rng.shuffle(idx)
        tot = 0.0
        for s in range(0, len(idx), 128):
            b = idx[s:s + 128]
            x = torch.from_numpy(Xtr[np.sort(b)].astype(np.float32) / 255).unsqueeze(1)
            yb = torch.from_numpy(ytr[np.sort(b)].astype(np.float32))
            x = augment(x, g)
            opt.zero_grad()
            loss = lossf(net(x).squeeze(1), yb)
            loss.backward()
            opt.step()
            tot += float(loss) * len(b)
        print(f"  epoch {ep + 1}: loss {tot / len(idx):.4f}", file=sys.stderr, flush=True)
    net.eval()
    if return_net:
        return net
    out = []
    with torch.no_grad():
        for s in range(0, len(Xte), 512):
            x = torch.from_numpy(np.asarray(Xte[s:s + 512]).astype(np.float32) / 255).unsqueeze(1)
            out.append(torch.sigmoid(net(x).squeeze(1)).numpy())
    return np.concatenate(out) if out else np.zeros(0)


def cmd_cnn_final(a):
    """The frozen CNN: trained on every spent labelled line plus the external lines; saves the weights."""
    import hashlib
    import torch
    torch.set_num_threads(a.threads)
    d = Path(a.dir)
    uni = jsonl(d / "universe.jsonl")
    U = np.load(d / "crops-u.npy", mmap_mode="r")
    E = np.load(d / "crops-e.npy")
    ye = np.array([e["y"] for e in jsonl(d / "ext.jsonl")], np.int64)
    pos = {(u["page_id"], u["box_id"]): i for i, u in enumerate(uni)}
    labs = jsonl(d / "labels.jsonl")
    li = np.array([pos[(l["page_id"], l["box_id"])] for l in labs])
    ly = np.array([l["y"] for l in labs], np.int64)
    o = np.argsort(li)
    net = train_predict(np.concatenate([E, U[li[o]]]), np.concatenate([ye, ly[o]]), U[:0], a.epochs, 2128, return_net=True)
    torch.save(net.state_dict(), d / "cnn-final.pt")
    h = hashlib.sha256(open(d / "cnn-final.pt", "rb").read()).hexdigest()
    print(json.dumps({"train": int(len(ye) + len(ly)), "sha256": h}))


def cmd_cnn(a):
    import torch
    torch.set_num_threads(a.threads)
    d = Path(a.dir)
    uni = jsonl(d / "universe.jsonl")
    U = np.load(d / "crops-u.npy", mmap_mode="r")
    E = np.load(d / "crops-e.npy")
    ye = np.array([e["y"] for e in jsonl(d / "ext.jsonl")], np.int64)
    pos = {(u["page_id"], u["box_id"]): i for i, u in enumerate(uni)}
    labs = jsonl(d / "labels.jsonl")
    li = np.array([pos[(l["page_id"], l["box_id"])] for l in labs])
    ly = np.array([l["y"] for l in labs], np.int64)
    lf = np.array([fold_of(l["page_id"], a.folds) for l in labs])
    uf = np.array([fold_of(u["page_id"], a.folds) for u in uni])
    p = np.full(len(uni), np.nan)
    for k in range(a.folds):
        m = lf != k
        Xtr = np.concatenate([E, U[np.sort(li[m])]])
        ytr = np.concatenate([ye, ly[m][np.argsort(li[m])]])
        te = np.where(uf == k)[0]
        print(f"fold {k}: train {len(ytr)} ({int(ytr.sum())} Cherokee), predict {len(te)}", file=sys.stderr, flush=True)
        p[te] = train_predict(Xtr, ytr, U[te], a.epochs, 2128 + k)
    np.save(d / "cnn-oof.npy", p)
    print(json.dumps({"boxes": len(uni), "nan": int(np.isnan(p).sum())}))


# ---------------------------------------------------------------- smoothing and evaluation
def columns(uni):
    """Index lists of the universe boxes per S1 column (page + region prefix of the box id), in reading order."""
    g = collections.defaultdict(list)
    for i, u in enumerate(uni):
        m = re.match(r"(r\d+)", u["box_id"])
        g[(u["page_id"], m.group(1) if m else "")].append(i)
    out = []
    for idx in g.values():
        idx.sort(key=lambda i: ((uni[i]["box"][1] + uni[i]["box"][3]) / 2, uni[i]["box"][0]))
        out.append(np.array(idx))
    return out


def hmm(p, cols, stay):
    """Two-state HMM posterior: emissions p / (1-p) (scores as likelihoods under a flat prior), symmetric
    transitions with P(stay) = stay, forward-backward within each column."""
    p = np.clip(p, 1e-4, 1 - 1e-4)
    out = p.copy()
    T = np.array([[stay, 1 - stay], [1 - stay, stay]])
    for idx in cols:
        e = np.stack([1 - p[idx], p[idx]], 1)
        n = len(idx)
        f = np.zeros((n, 2))
        b = np.ones((n, 2))
        f[0] = 0.5 * e[0]
        f[0] /= f[0].sum()
        for i in range(1, n):
            f[i] = (f[i - 1] @ T) * e[i]
            f[i] /= f[i].sum()
        for i in range(n - 2, -1, -1):
            b[i] = T @ (e[i + 1] * b[i + 1])
            b[i] /= b[i].sum()
        post = f * b
        out[idx] = post[:, 1] / post.sum(1)
    return out


def rates(s, y, w, t):
    c = s >= t
    chr_ = (w * y).sum()
    calls = (w * c).sum()
    rec = (w * y * c).sum() / chr_ if chr_ else float("nan")
    ls = (w * (1 - y) * c).sum() / calls if calls else 0.0
    return rec, ls


def pick_t(s, y, w, cap=0.03):
    best = None
    for t in GRID:
        rec, ls = rates(s, y, w, t)
        if ls <= cap and (best is None or rec > best[1] or (rec == best[1] and t > best[0])):
            best = (float(t), rec, ls)
    return best[0] if best else 0.99


def logit(p):
    p = np.clip(p, 1e-4, 1 - 1e-4)
    return np.log(p / (1 - p))


def cmd_eval(a):
    from sklearn.linear_model import LogisticRegression
    d = Path(a.dir)
    uni = jsonl(d / "universe.jsonl")
    cols = columns(uni)
    pos = {(u["page_id"], u["box_id"]): i for i, u in enumerate(uni)}
    labs = jsonl(d / "labels.jsonl")
    li = np.array([pos[(l["page_id"], l["box_id"])] for l in labs])
    y = np.array([l["y"] for l in labs], float)
    w = np.array([l["w"] for l in labs])
    lp = np.array([l["page_id"] for l in labs])
    ls_ = np.array([l["sample"] for l in labs])
    lf = np.array([fold_of(p, a.folds) for p in lp])
    v2 = np.array([u["p_v2"] for u in uni])
    cnn = np.load(d / "cnn-oof.npy") if (d / "cnn-oof.npy").exists() else None
    stays = [0.5, 0.6, 0.7, 0.8, 0.9, 0.95, 0.98]

    def wll(s, m):
        s = np.clip(s[li[m]], 1e-4, 1 - 1e-4)
        return -(w[m] * (y[m] * np.log(s) + (1 - y[m]) * np.log(1 - s))).sum() / w[m].sum()

    def ens_scores(m):
        X = np.stack([logit(v2), logit(cnn)], 1)
        lr = LogisticRegression().fit(X[li[m]], y[m], sample_weight=w[m])
        return lr.predict_proba(X)[:, 1], lr

    variants = {"v2": lambda m: (v2, {})}
    def smooth_of(base):
        def f(m):
            s0, info = base(m)
            cache = {st: hmm(s0, cols, st) for st in stays}
            st = min(stays, key=lambda st: wll(cache[st], m))
            return cache[st], {**info, "stay": st}
        return f
    variants["v2+smooth"] = smooth_of(variants["v2"])
    if cnn is not None:
        variants["cnn"] = lambda m: (cnn, {})
        def ens(m):
            s, lr = ens_scores(m)
            return s, {"coef": lr.coef_[0].round(3).tolist()}
        variants["ensemble"] = ens
        variants["ensemble+smooth"] = smooth_of(ens)

    res = {}
    rng = np.random.default_rng(2128)
    pages = np.array(sorted(set(lp)))
    pidx = {p: np.where(lp == p)[0] for p in pages}
    draws = [rng.integers(0, len(pages), len(pages)) for _ in range(a.draws)]
    for name, fn in variants.items():
        call = np.zeros(len(labs), bool)
        info_f = []
        for k in range(a.folds):
            tr, te = lf != k, lf == k
            s, info = fn(tr)
            t = pick_t(s[li[tr]], y[tr], w[tr])
            call[te] = s[li[te]] >= t
            info_f.append({**info, "t": t})
        c = call.astype(float)

        def m_(ix):
            chr_ = (w[ix] * y[ix]).sum()
            calls = (w[ix] * c[ix]).sum()
            return ((w[ix] * y[ix] * c[ix]).sum() / chr_ if chr_ else float("nan"),
                    (w[ix] * (1 - y[ix]) * c[ix]).sum() / calls if calls else 0.0)
        allix = np.arange(len(labs))
        rec, lsh = m_(allix)
        bs = np.array([m_(np.concatenate([pidx[pages[j]] for j in dr])) for dr in draws])
        per_sample = {sname: dict(zip(("recall", "latin_share"), [round(v, 3) for v in m_(np.where(ls_ == sname)[0])]))
                      for sname in sorted(set(ls_))}
        s_all, info_all = fn(np.ones(len(labs), bool))
        t_all = pick_t(s_all[li], y, w)
        r_all, l_all = rates(s_all[li], y, w, t_all)
        res[name] = {"cv_recall": round(rec, 3), "cv_recall_ci": np.nanquantile(bs[:, 0], [0.025, 0.975]).round(3).tolist(),
                     "cv_latin_share": round(lsh, 4), "cv_latin_share_ci": np.quantile(bs[:, 1], [0.025, 0.975]).round(4).tolist(),
                     "per_sample": per_sample, "folds": info_f,
                     "pooled_t": t_all, "pooled_in_sample": [round(r_all, 3), round(l_all, 4)], "pooled_info": info_all}
        print(name, json.dumps({k: res[name][k] for k in ("cv_recall", "cv_recall_ci", "cv_latin_share", "cv_latin_share_ci",
                                                         "per_sample", "pooled_t")}), flush=True)
    (d / "eval.json").write_text(json.dumps(res, indent=1) + "\n")


def cmd_freeze(a):
    """Frozen ensemble: LR on [logit v2, logit CNN] fitted on every spent labelled line (HT weights, the
    CNN's out-of-fold scores), t chosen on the pooled spent samples (max recall, Latin share <= 0.03)."""
    from sklearn.linear_model import LogisticRegression
    d = Path(a.dir)
    uni = jsonl(d / "universe.jsonl")
    pos = {(u["page_id"], u["box_id"]): i for i, u in enumerate(uni)}
    labs = jsonl(d / "labels.jsonl")
    li = np.array([pos[(l["page_id"], l["box_id"])] for l in labs])
    y = np.array([l["y"] for l in labs], float)
    w = np.array([l["w"] for l in labs])
    X = np.stack([logit(np.array([u["p_v2"] for u in uni])), logit(np.load(d / "cnn-oof.npy"))], 1)
    lr = LogisticRegression().fit(X[li], y, sample_weight=w)
    s = lr.predict_proba(X)[:, 1]
    t = pick_t(s[li], y, w)
    rec, ls = rates(s[li], y, w, t)
    import hashlib
    fr = {"issue": "#2128 classifier v2 ensemble (dev, spent pages), frozen pending review",
          "v2_model": a.v2_model, "v2_model_sha256": hashlib.sha256(open(a.v2_model, "rb").read()).hexdigest(),
          "cnn": "cnn-final.pt", "cnn_sha256": hashlib.sha256(open(d / "cnn-final.pt", "rb").read()).hexdigest(),
          "cnn_input": f"grey crop of the S1 box (pad 4 px), height {H}, aspect kept, left-aligned on white, width {W}",
          "clip": None, "ensemble": {"features": ["logit(p_v2)", "logit(p_cnn)"], "coef": lr.coef_[0].tolist(),
                                     "intercept": float(lr.intercept_[0])},
          "t": t, "pooled_in_sample": {"recall": rec, "latin_share": ls}, "labelled_lines": len(labs)}
    (d / "frozen.json").write_text(json.dumps(fr, indent=1) + "\n")
    print(json.dumps(fr, indent=1))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("build"); s.add_argument("--spec", required=True); s.add_argument("--out-dir", required=True)
    s = sub.add_parser("errors")
    for k in ("--dir", "--sample", "--reads"):
        s.add_argument(k, required=True)
    s.add_argument("--t", type=float, default=0.97)
    s = sub.add_parser("crops")
    for k in ("--dir", "--silver", "--aligned-ghn", "--aligned", "--loc-images", "--fitset", "--inventory", "--ghn-root"):
        s.add_argument(k, required=True)
    s.add_argument("--n-english", type=int, default=6000)
    s = sub.add_parser("clip")
    for k in ("--dir", "--found", "--out-dir"):
        s.add_argument(k, required=True)
    s = sub.add_parser("rescore")
    for k in ("--out-dir", "--model", "--chr-text", "--phoenix-pages", "--alto-root", "--wordlist"):
        s.add_argument(k, required=True)
    s.add_argument("--reads", action="append", required=True)
    s = sub.add_parser("freeze"); s.add_argument("--dir", required=True); s.add_argument("--v2-model", required=True)
    s = sub.add_parser("cnn_final"); s.add_argument("--dir", required=True)
    s.add_argument("--epochs", type=int, default=4); s.add_argument("--threads", type=int, default=6)
    s = sub.add_parser("cnn"); s.add_argument("--dir", required=True)
    s.add_argument("--epochs", type=int, default=4); s.add_argument("--folds", type=int, default=5)
    s.add_argument("--threads", type=int, default=6)
    s = sub.add_parser("eval"); s.add_argument("--dir", required=True)
    s.add_argument("--folds", type=int, default=5); s.add_argument("--draws", type=int, default=2000)
    a = ap.parse_args(argv)
    globals()["cmd_" + a.cmd](a)


if __name__ == "__main__":
    main()
