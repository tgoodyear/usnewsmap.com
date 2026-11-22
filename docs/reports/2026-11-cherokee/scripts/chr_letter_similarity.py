"""Can image similarity carry one letter decision to near-identical Cherokee Phoenix crops?

Reads the archived check-site task sets (chr/phoenix-nt-v1 and chr-lines/disputes-v1), drops every
held task, cuts each disputed word clip into letters, labels the letters where the two machine
readings agree, embeds them two ways (pixels plus HOG, DINOv2-small) and scores k-nearest-neighbour
retrieval with a page-grouped split. It also runs the descriptive word-level test and the
shared-error count. Writes results.json and a Markdown file of tables.

The letters are cut by a stated method (split_word below, a port of the approach in
crates/usnm-glyphs), not read from ground-truth boxes: no per-letter boxes exist in the data.

Needs numpy, pillow, scikit-image, scikit-learn and (for DINOv2) torch with transformers. The
tier, label, k-NN and threshold functions need only numpy.
"""
import argparse
import collections
import json
import os
import random

import numpy as np

LOOKALIKES = [("Ꭱ", "Ꮢ"), ("Ꮧ", "Ꮨ"), ("Ꮎ", "Ꮻ"), ("Ꮘ", "Ꮱ"), ("Ꮐ", "Ᏻ")]
SETS = [("nt", "chr/phoenix-nt-v1"), ("disputes", "chr-lines/disputes-v1")]
SIDE = 32


# ---------------------------------------------------------------- data


def load_jsonl(path):
    with open(path, encoding="utf-8") as f:
        return [json.loads(line) for line in f if line.strip()]


def held_ids(export_dir, lang):
    """Task ids held in the public export for one language (empty when no file)."""
    path = os.path.join(export_dir, f"export-{lang}.jsonl")
    if not os.path.exists(path):
        return set()
    return {r["id"] for r in load_jsonl(path) if r.get("held")}


def today_export(export_dir, lang):
    path = os.path.join(export_dir, f"export-{lang}.jsonl")
    return {r["id"]: r for r in load_jsonl(path)} if os.path.exists(path) else {}


def load_tasks(root, set_dir, held, today):
    """Unheld tasks of one set: dicts with the body fields, outcome, settled and the clip path.
    Outcome and settled come from the public export when it has the task, else the archived one."""
    base = os.path.join(root, set_dir)
    export = {r["id"]: r for r in load_jsonl(os.path.join(base, "public-export-20261114.jsonl"))}
    export.update(today)
    out = []
    for r in load_jsonl(os.path.join(base, "admin-dump-tasks-20261114.jsonl")):
        if r["id"] in held or r.get("held"):
            continue
        b = json.loads(r["body"])
        e = export.get(r["id"], {})
        b.update(
            id=r["id"],
            outcome=e.get("outcome") if e.get("settled") else None,
            settled=bool(e.get("settled")),
            clip=os.path.join(base, "clips", b["word"]),
        )
        out.append(b)
    return out


def agreed_positions(task):
    """Positions where both readings carry the same letter, for equal-length readings only."""
    a, b = task["a"], task["b"]
    if len(a) != len(b):
        return []
    skip = set(task.get("a_diff", [])) | set(task.get("b_diff", []))
    return [i for i in range(len(a)) if a[i] == b[i] and i not in skip]


def attestation_pool(nt_tasks):
    """Spellings seen in unheld NT-set reference readings, with the tasks that carry them."""
    pool = collections.defaultdict(set)
    for t in nt_tasks:
        pool[t["a"]].add(t["id"])
    return pool


def candidate_spellings(task):
    """The spellings a task's word may have: the settled winner, else either reading."""
    if task["settled"] and task["outcome"] in ("a", "b"):
        return [task[task["outcome"]]]
    return [task["a"], task["b"]]


def tier_of(task, pool):
    """A: agreed letter in a word whose candidate spelling is a reference spelling in another unheld
    NT task. B: agreed only."""
    for s in candidate_spellings(task):
        if pool.get(s, set()) - {task["id"]}:
            return "A"
    return "B"


# ---------------------------------------------------------------- segmentation


def otsu(gray):
    hist, _ = np.histogram(gray, bins=256, range=(0, 256))
    total = gray.size
    sum_all = float(np.dot(np.arange(256), hist))
    wb = sb = 0.0
    best, thr = -1.0, 128
    for t in range(256):
        wb += hist[t]
        if wb == 0:
            continue
        wf = total - wb
        if wf == 0:
            break
        sb += t * hist[t]
        mb, mf = sb / wb, (sum_all - sb) / wf
        v = wb * wf * (mb - mf) ** 2
        if v > best:
            best, thr = v, t
    return thr


def ink_mask(gray):
    """True where there is ink (dark on a lighter page)."""
    return gray <= otsu(gray)


def split_word(ink, n):
    """Boxes (x0, y0, x1, y1) of n letters in a word crop, left to right, or None.

    Port of crates/usnm-glyphs split_word: connected components in the word's text band (stacked
    ones merge, small and flat marks drop out); then the narrowest gaps merge, or the widest boxes
    are cut at their thinnest column, until there are n. A split with very uneven widths is refused.
    """
    from skimage.measure import label, regionprops

    if n <= 0:
        return None
    h, w = ink.shape
    comps = []
    for r in regionprops(label(ink, connectivity=2)):
        if r.area >= 10:
            y0, x0, y1, x1 = r.bbox
            comps.append([x0, y0, x1, y1, int(r.area)])
    heights = sorted(c[3] - c[1] for c in comps if c[4] >= 30)
    body = max(heights[len(heights) // 2] if heights else h // 2, 1)
    centre = h / 2.0
    comps = [c for c in comps if abs((c[1] + c[3]) / 2.0 - centre) < 0.75 * body + 4.0]
    comps.sort(key=lambda c: c[0])
    boxes = []
    for c in comps:
        if boxes:
            m = boxes[-1]
            overlap = min(m[2], c[2]) - max(m[0], c[0])
            if overlap > 0.5 * min(m[2] - m[0], c[2] - c[0]):
                boxes[-1] = [min(m[0], c[0]), min(m[1], c[1]), max(m[2], c[2]), max(m[3], c[3])]
                continue
        boxes.append(c[:4])
    boxes = [b for b in boxes if (b[3] - b[1]) >= 0.45 * body and (b[3] - b[1]) >= 0.35 * (b[2] - b[0])]
    if not boxes:
        return None
    while len(boxes) > n:
        i = min(range(len(boxes) - 1), key=lambda i: boxes[i + 1][0] - boxes[i][2])
        b = boxes.pop(i + 1)
        a = boxes[i]
        boxes[i] = [min(a[0], b[0]), min(a[1], b[1]), max(a[2], b[2]), max(a[3], b[3])]
    while len(boxes) < n:
        i = max(range(len(boxes)), key=lambda i: boxes[i][2] - boxes[i][0])
        x0, y0, x1, y1 = boxes[i]
        if x1 - x0 < 8:
            return None
        lo, hi = x0 + (x1 - x0) // 4, x0 + 3 * (x1 - x0) // 4
        if hi <= lo:
            return None
        cut = min(range(lo, hi), key=lambda x: int(ink[y0:y1, x].sum()))
        boxes[i : i + 1] = [[x0, y0, cut, y1], [cut, y0, x1, y1]]
    widths = sorted(b[2] - b[0] for b in boxes)
    med = widths[len(widths) // 2]
    if n > 1 and (widths[-1] > 2.0 * med or widths[0] < 0.35 * med):
        return None
    return boxes


def square_crop(gray, box, margin=1):
    """The box (with a margin) padded with the crop's median to a square, as a uint8 array."""
    h, w = gray.shape
    x0, y0, x1, y1 = box
    x0, y0, x1, y1 = max(0, x0 - margin), max(0, y0 - margin), min(w, x1 + margin), min(h, y1 + margin)
    c = gray[y0:y1, x0:x1]
    side = max(c.shape)
    pad = int(np.median(gray))
    out = np.full((side, side), pad, dtype=np.uint8)
    oy, ox = (side - c.shape[0]) // 2, (side - c.shape[1]) // 2
    out[oy : oy + c.shape[0], ox : ox + c.shape[1]] = c
    return out


def read_gray(path):
    from PIL import Image

    return np.asarray(Image.open(path).convert("L"))


# ---------------------------------------------------------------- embeddings


def resize_square(img, side):
    from PIL import Image

    return np.asarray(Image.fromarray(img).resize((side, side), Image.BICUBIC))


def pixel_hog(crops):
    from skimage.feature import hog

    rows = []
    for c in crops:
        s = resize_square(c, SIDE).astype(np.float32)
        p = (s - s.mean()).ravel()
        p /= np.linalg.norm(p) + 1e-9
        g = hog(s, orientations=9, pixels_per_cell=(8, 8), cells_per_block=(2, 2), feature_vector=True)
        g = g / (np.linalg.norm(g) + 1e-9)
        v = np.concatenate([p, g])
        rows.append(v / (np.linalg.norm(v) + 1e-9))
    return np.stack(rows)


def dino(crops, model_name="facebook/dinov2-small", batch=64):
    import torch
    from transformers import AutoModel

    torch.set_num_threads(2)
    model = AutoModel.from_pretrained(model_name).eval()
    mean = np.array([0.485, 0.456, 0.406], dtype=np.float32)
    std = np.array([0.229, 0.224, 0.225], dtype=np.float32)
    out = []
    for i in range(0, len(crops), batch):
        x = np.stack([resize_square(c, 224) for c in crops[i : i + batch]]).astype(np.float32) / 255.0
        x = (x[:, None, :, :] - mean[None, :, None, None]) / std[None, :, None, None]
        with torch.no_grad():
            o = model(pixel_values=torch.from_numpy(x.astype(np.float32)))
        out.append(o.last_hidden_state[:, 0].numpy())
    v = np.concatenate(out)
    return v / (np.linalg.norm(v, axis=1, keepdims=True) + 1e-9)


# ---------------------------------------------------------------- k-NN


def grouped_folds(groups, k, seed):
    """Fold index per item; every group falls in one fold; groups are shuffled with the seed."""
    names = sorted(set(groups))
    random.Random(seed).shuffle(names)
    fold_of = {g: i % k for i, g in enumerate(names)}
    return np.array([fold_of[g] for g in groups])


def knn_predict(emb, labels, folds):
    """Out-of-fold 1-NN and 5-NN labels, and the 1-NN cosine similarity. emb rows are unit length."""
    n = len(labels)
    p1, p5, s1, nn = [None] * n, [None] * n, np.zeros(n), [-1] * n
    for f in sorted(set(folds)):
        te = np.where(folds == f)[0]
        tr = np.where(folds != f)[0]
        if len(tr) == 0:
            continue
        sim = emb[te] @ emb[tr].T
        for row, i in enumerate(te):
            order = np.argsort(-sim[row], kind="stable")
            top = order[0]
            p1[i], s1[i], nn[i] = labels[tr[top]], sim[row, top], int(tr[top])
            votes = collections.defaultdict(float)
            for j in order[:5]:
                votes[labels[tr[j]]] += float(sim[row, j])
            p5[i] = max(sorted(votes), key=lambda c: votes[c])
    return p1, p5, s1, nn


def acc(pred, truth, mask):
    idx = [i for i in range(len(truth)) if mask[i]]
    if not idx:
        return None, 0
    return sum(pred[i] == truth[i] for i in idx) / len(idx), len(idx)


def wilson(k, n, z=1.96):
    if n == 0:
        return (None, None)
    p = k / n
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    m = z * ((p * (1 - p) / n + z * z / (4 * n * n)) ** 0.5) / d
    return (c - m, c + m)


def pair_stats(pred, truth, mask):
    out = []
    for x, y in LOOKALIKES:
        idx = [i for i in range(len(truth)) if mask[i] and truth[i] in (x, y)]
        ok = sum(pred[i] == truth[i] for i in idx)
        other = sum(1 for i in idx if pred[i] != truth[i] and pred[i] == (y if truth[i] == x else x))
        out.append({"pair": x + "/" + y, "n": len(idx), "correct": ok, "to_other_member": other})
    return out


def confusions(pred, truth, mask, top=10):
    c = collections.Counter((truth[i], pred[i]) for i in range(len(truth)) if mask[i] and pred[i] != truth[i])
    return [{"true": t, "predicted": p, "count": n} for (t, p), n in c.most_common(top)]


def lookalike_set():
    return {c for pair in LOOKALIKES for c in pair}


def threshold_table(pred, truth, sim, mask, coverage_targets=(0.1, 0.2, 0.3, 0.5, 0.7, 1.0)):
    """For thresholds on the 1-NN similarity: coverage, accuracy among covered, and the same for
    look-alike letters. Also the best coverage at which look-alike accuracy is at least 0.98."""
    la = lookalike_set()
    idx = [i for i in range(len(truth)) if mask[i]]
    if not idx:
        return {"rows": [], "best_98": None}
    order = sorted(idx, key=lambda i: -sim[i])

    def row_at(m):
        cov = order[:m]
        okc = sum(pred[i] == truth[i] for i in cov)
        lac = [i for i in cov if truth[i] in la]
        lok = sum(pred[i] == truth[i] for i in lac)
        return {
            "threshold": float(sim[order[m - 1]]),
            "coverage": m / len(order),
            "n_covered": m,
            "accuracy": okc / m,
            "lookalike_n": len(lac),
            "lookalike_accuracy": (lok / len(lac)) if lac else None,
        }

    rows = [row_at(max(1, int(round(t * len(order))))) for t in coverage_targets]
    best = None
    ok = lk = ln = 0
    for m in range(1, len(order) + 1):
        i = order[m - 1]
        if truth[i] in la:
            ln += 1
            lk += pred[i] == truth[i]
        if ln and lk / ln >= 0.98 and (m == len(order) or sim[order[m]] < sim[i]):
            best = row_at(m)
    return {"rows": rows, "best_98": best}


# ---------------------------------------------------------------- letters


def build_letters(tasks_by_set, pool, log):
    """Cut every unheld word with equal-length readings into letters; label the agreed ones."""
    letters, stats = [], collections.Counter()
    for name, tasks in tasks_by_set.items():
        for t in tasks:
            stats[f"{name}_tasks"] += 1
            if len(t["a"]) != len(t["b"]):
                stats[f"{name}_dropped_unequal_length"] += 1
                continue
            pos = agreed_positions(t)
            if not pos:
                stats[f"{name}_dropped_no_agreed_letter"] += 1
                continue
            try:
                gray = read_gray(t["clip"])
            except OSError:
                stats[f"{name}_dropped_missing_clip"] += 1
                continue
            boxes = split_word(ink_mask(gray), len(t["a"]))
            if boxes is None:
                stats[f"{name}_dropped_split_failed"] += 1
                continue
            stats[f"{name}_words_used"] += 1
            tier = tier_of(t, pool)
            for i in pos:
                letters.append(
                    {
                        "set": name,
                        "task": t["id"],
                        "page": t["page"],
                        "pos": i,
                        "label": t["a"][i],
                        "tier": tier,
                        "crop": square_crop(gray, boxes[i]),
                    }
                )
    stats["letters"] = len(letters)
    log(f"letters: {len(letters)}; {dict(stats)}")
    return letters, dict(stats)


def score_embedding(name, emb, letters, folds):
    labels = [L["label"] for L in letters]
    tiers = [L["tier"] for L in letters]
    sets = [L["set"] for L in letters]
    p1, p5, s1, _ = knn_predict(emb, labels, folds)
    la = lookalike_set()
    res = {"name": name}
    masks = {
        "overall": [True] * len(labels),
        "tier_A": [t == "A" for t in tiers],
        "tier_B": [t == "B" for t in tiers],
        "nt_set": [s == "nt" for s in sets],
        "disputes_set": [s == "disputes" for s in sets],
        "lookalike": [c in la for c in labels],
        "lookalike_tier_A": [c in la and t == "A" for c, t in zip(labels, tiers)],
    }
    res["accuracy"] = {}
    for k, m in masks.items():
        a1, n = acc(p1, labels, m)
        a5, _ = acc(p5, labels, m)
        lo, hi = wilson(round((a1 or 0) * n), n)
        res["accuracy"][k] = {"n": n, "k1": a1, "k5": a5, "k1_ci95": [lo, hi]}
    res["pairs_all"] = pair_stats(p1, labels, masks["overall"])
    res["pairs_tier_A"] = pair_stats(p1, labels, masks["tier_A"])
    res["confusions_all"] = confusions(p1, labels, masks["overall"])
    res["thresholds_tier_A"] = threshold_table(p1, labels, s1, masks["tier_A"])
    res["thresholds_all"] = threshold_table(p1, labels, s1, masks["overall"])
    return res


def examples_check(root, crops_emb_fn, letters, embs, log):
    """Supplement: the language's hand-picked letter example pictures, classified by 1-NN against
    all cut letters (no page overlap is possible: they come from other pages)."""
    from PIL import Image

    cfg = json.loads(load_jsonl_any(os.path.join(root, "chr/phoenix-nt-v1", "language-20261114.json"))["config"])
    items = []
    for ch, paths in cfg.get("examples", {}).items():
        for p in paths:
            f = os.path.join(root, "chr/phoenix-nt-v1", "clips", p)
            if os.path.exists(f):
                g = np.asarray(Image.open(f).convert("L"))
                items.append((ch, square_crop(g, [0, 0, g.shape[1], g.shape[0]], 0)))
    labels = [L["label"] for L in letters]
    out = {"n": len(items)}
    for name, fn in crops_emb_fn.items():
        e = fn([c for _, c in items])
        sim = e @ embs[name].T
        pred = [labels[int(np.argmax(sim[i]))] for i in range(len(items))]
        ok = [p == t for p, (t, _) in zip(pred, items)]
        out[name] = {"k1": sum(ok) / max(1, len(ok))}
    return out


def load_jsonl_any(path):
    with open(path, encoding="utf-8") as f:
        text = f.read()
    return json.loads(text.splitlines()[0]) if path.endswith(".jsonl") else json.loads(text)


# ---------------------------------------------------------------- words


def word_regions(tasks):
    """The image region around the differing letters of each task, or the whole clip."""
    out = []
    for t in tasks:
        gray = read_gray(t["clip"])
        region, how = gray, "whole_clip"
        if len(t["a"]) == len(t["b"]):
            boxes = split_word(ink_mask(gray), len(t["a"]))
            diff = sorted(set(t.get("a_diff", [])) | set(t.get("b_diff", [])))
            if boxes and diff and all(i < len(boxes) for i in diff):
                bb = [boxes[i] for i in diff]
                box = [min(b[0] for b in bb), min(b[1] for b in bb), max(b[2] for b in bb), max(b[3] for b in bb)]
                region, how = square_crop(gray, box, 2), "diff_letters"
        if how == "whole_clip":
            region = square_crop(gray, [0, 0, gray.shape[1], gray.shape[0]], 0)
        out.append((t, region, how))
    return out


def diff_key(t):
    a = "".join(t["a"][i] for i in sorted(t.get("a_diff", [])))
    b = "".join(t["b"][i] for i in sorted(t.get("b_diff", [])))
    return (a, b)


def side_of(t):
    return {"a": "reference", "b": "loc"}[t["outcome"]]


def word_test(items, emb, thresholds):
    """Nearest settled neighbour (other page) sharing the differing-letter pair; precision of its
    winning side against the word's own, per threshold."""
    res = {"n_words": len(items), "rows": []}
    keys = [diff_key(t) for t, _, _ in items]
    sims = emb @ emb.T
    best = []
    for i, (t, _, _) in enumerate(items):
        cands = [j for j, (u, _, _) in enumerate(items) if u["page"] != t["page"] and keys[j] == keys[i]]
        if not cands:
            best.append(None)
            continue
        j = max(cands, key=lambda j: sims[i, j])
        best.append((j, float(sims[i, j])))
    res["with_same_pair_neighbour"] = sum(b is not None for b in best)
    sides = [side_of(t) for t, _, _ in items]
    for th in thresholds:
        sel = [(i, b) for i, b in enumerate(best) if b is not None and b[1] >= th]
        ok = sum(sides[i] == sides[b[0]] for i, b in sel)
        res["rows"].append({"threshold": th, "n": len(sel), "coverage": len(sel) / len(items), "same_side": ok})
    maj = collections.Counter(sides).most_common(1)[0]
    res["majority_side"] = {"side": maj[0], "share": maj[1] / len(items)}
    return res


# ---------------------------------------------------------------- shared errors


def shared_error_counts(nt_tasks, disputes_manifest, disputes_tasks):
    """What the data can say about errors the two machine readings share. See the report."""
    settled = [t for t in nt_tasks if t["settled"]]
    out = {
        "nt_settled_unheld": len(settled),
        "nt_outcomes": dict(collections.Counter(t["outcome"] for t in settled)),
    }
    agreed_in_winner = 0
    for t in settled:
        if t["outcome"] in ("a", "b") and len(t["a"]) == len(t["b"]):
            agreed_in_winner += len(agreed_positions(t))
    out["nt_agreed_letters_in_a_or_b_settled_equal_length_words"] = agreed_in_winner
    # Cross-set: lines in both sets; the nt word's LoC reading is the disputes manifest's LoC word.
    man = {m["id"]: m for m in disputes_manifest}
    cross = collections.Counter()
    for t in settled:
        line, _, n = t["id"].rsplit("/", 1)[-1].partition("~")
        m = man.get(line)
        w = t["word"].rsplit("_w", 1)[-1].split(".")[0]
        if not m or not w.isdigit():
            continue
        w = int(w)
        if w >= len(m["loc"]) or m["loc"][w] != t["b"]:
            cross["alignment_failed"] += 1
            continue
        ours = m["ours"][w]
        cross["aligned_settled_words"] += 1
        if t["outcome"] == "a":
            cross["reference_right__chr_phoenix_equals_loc_shared_error"] += ours == t["b"]
            cross["reference_right__chr_phoenix_equals_reference"] += ours == t["a"]
            cross["reference_right__chr_phoenix_other"] += ours not in (t["a"], t["b"])
        elif t["outcome"] == "b":
            cross["loc_right__chr_phoenix_equals_loc"] += ours == t["b"]
            cross["loc_right__chr_phoenix_other"] += ours != t["b"]
        else:
            cross["neither_right"] += 1
    out["cross_set"] = dict(cross)
    out["disputes_settled_unheld"] = sum(1 for t in disputes_tasks if t["settled"])
    out["disputes_tasks"] = len(disputes_tasks)
    return out


# ---------------------------------------------------------------- report tables


def pct(x):
    return "n/a" if x is None else f"{100 * x:.1f}%"


def tables(res):
    L = []
    L.append("## Letter k-NN accuracy (page-grouped folds)\n")
    L.append("| Embedding | Slice | n | 1-NN | 5-NN | 1-NN 95% interval |")
    L.append("| --- | --- | ---: | ---: | ---: | --- |")
    for e in res["letters"]["embeddings"]:
        for k, v in e["accuracy"].items():
            lo, hi = v["k1_ci95"]
            ci = "n/a" if lo is None else f"{pct(lo)} to {pct(hi)}"
            L.append(f"| {e['name']} | {k} | {v['n']} | {pct(v['k1'])} | {pct(v['k5'])} | {ci} |")
    for key, title in (("pairs_all", "all letters"), ("pairs_tier_A", "tier A letters")):
        L.append(f"\n## Look-alike pairs, 1-NN, {title}\n")
        L.append("| Embedding | Pair | n | Correct | Errors that are the other member |")
        L.append("| --- | --- | ---: | ---: | ---: |")
        for e in res["letters"]["embeddings"]:
            for p in e[key]:
                L.append(f"| {e['name']} | {p['pair']} | {p['n']} | {p['correct']} | {p['to_other_member']} |")
    L.append("\n## Top confusions, 1-NN, all letters\n")
    for e in res["letters"]["embeddings"]:
        L.append(f"{e['name']}: " + ", ".join(f"{c['true']} read as {c['predicted']} ({c['count']})" for c in e["confusions_all"]) + "\n")
    for key, title in (("thresholds_tier_A", "tier A"), ("thresholds_all", "all letters")):
        L.append(f"\n## Similarity threshold analysis, {title}\n")
        L.append("| Embedding | Threshold | Coverage | Covered | 1-NN accuracy | Look-alike letters covered | Look-alike accuracy |")
        L.append("| --- | ---: | ---: | ---: | ---: | ---: | ---: |")
        for e in res["letters"]["embeddings"]:
            for r in e[key]["rows"]:
                L.append(
                    f"| {e['name']} | {r['threshold']:.3f} | {pct(r['coverage'])} | {r['n_covered']} | {pct(r['accuracy'])} | {r['lookalike_n']} | {pct(r['lookalike_accuracy'])} |"
                )
            b = e[key]["best_98"]
            if b:
                L.append(f"| {e['name']} (best at 98% look-alike) | {b['threshold']:.3f} | {pct(b['coverage'])} | {b['n_covered']} | {pct(b['accuracy'])} | {b['lookalike_n']} | {pct(b['lookalike_accuracy'])} |")
            else:
                L.append(f"| {e['name']} (best at 98% look-alike) | none | 0% | 0 | n/a | 0 | n/a |")
    return "\n".join(L) + "\n"


# ---------------------------------------------------------------- main


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--check-site-dir", required=True, help="the check-site directory holding chr/, chr-lines/")
    ap.add_argument("--export-dir", required=True, help="directory with export-chr.jsonl and export-chr-lines.jsonl (the public exports)")
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--folds", type=int, default=5)
    ap.add_argument("--seed", type=int, default=20261115)
    ap.add_argument("--skip-dino", action="store_true", help="run the pixel and HOG baseline only")
    a = ap.parse_args()
    os.makedirs(a.out_dir, exist_ok=True)
    random.seed(a.seed)
    np.random.seed(a.seed)
    log = lambda m: print(m, flush=True)

    held = held_ids(a.export_dir, "chr") | held_ids(a.export_dir, "chr-lines")
    tasks = {n: load_tasks(a.check_site_dir, d, held, today_export(a.export_dir, d.split("/")[0])) for n, d in SETS}
    log({n: len(v) for n, v in tasks.items()})
    pool = attestation_pool(tasks["nt"])
    letters, stats = build_letters(tasks, pool, log)
    groups = [L["page"] for L in letters]
    folds = grouped_folds(groups, a.folds, a.seed)
    crops = [L["crop"] for L in letters]
    res = {"letters": {"stats": stats, "tiers": dict(collections.Counter(L["tier"] for L in letters)), "embeddings": []}}
    res["letters"]["tiers_by_set"] = {f"{s}_{t}": n for (s, t), n in collections.Counter((L["set"], L["tier"]) for L in letters).items()}
    res["letters"]["labels"] = len(set(L["label"] for L in letters))
    embs, fns = {}, {}
    embs["pixel+HOG"], fns["pixel+HOG"] = pixel_hog(crops), pixel_hog
    if not a.skip_dino:
        embs["DINOv2-small"], fns["DINOv2-small"] = dino(crops), dino
    for n, e in embs.items():
        res["letters"]["embeddings"].append(score_embedding(n, e, letters, folds))
        log(f"{n}: {res['letters']['embeddings'][-1]['accuracy']['overall']}")
    try:
        res["letters"]["gold_examples"] = examples_check(a.check_site_dir, fns, letters, embs, log)
    except Exception as ex:  # supplement only
        res["letters"]["gold_examples"] = {"error": str(ex)}

    # Test 2: words
    settled = [t for t in tasks["nt"] if t["settled"] and t["outcome"] in ("a", "b")]
    items = word_regions(settled)
    res["words"] = {"how": dict(collections.Counter(h for _, _, h in items)), "embeddings": {}}
    for n, fn in fns.items():
        e = fn([r for _, r, _ in items])
        ths = [0.5, 0.6, 0.7, 0.8, 0.9] if n == "pixel+HOG" else [0.5, 0.6, 0.7, 0.8, 0.9]
        res["words"]["embeddings"][n] = word_test(items, e, ths)

    man = load_jsonl(os.path.join(a.check_site_dir, "chr-lines/disputes-v1/repo-check-lines-manifest.jsonl"))
    res["shared_error"] = shared_error_counts(tasks["nt"], man, tasks["disputes"])

    with open(os.path.join(a.out_dir, "results.json"), "w", encoding="utf-8") as f:
        json.dump(res, f, ensure_ascii=False, indent=1, sort_keys=True)
    with open(os.path.join(a.out_dir, "tables.md"), "w", encoding="utf-8") as f:
        f.write(tables(res))
    log("done")


if __name__ == "__main__":
    main()
