#!/usr/bin/env python3
"""Step 1 of #2167: validate the ALTO-only `alto_cross_share` on the 107 joined pages of #2107 step 2 (#2116).

    validate.py plant --step2-code DIR --join-dir DIR --out DIR
    validate.py score --join-dir DIR --orig orig.jsonl --planted planted.jsonl --image image.json --out result.json

`--step2-code` is scripts/ocr-survey/layout_alto of branch layout-alto-2107 (#2116), whose altolayout.py and
signals.py are imported unchanged. `--join-dir` holds #2116's join outputs: join.json, banded.json, testb.json, alto/.

plant: for each joined page with banded gutters, #2116 Test B's planting (signals.plant_merge: each line merged
with the line beside it across its nearest gutter, in image pixels) is applied to the ALTO itself: the partner
TextLine's children are moved into the visited TextLine (left line's words first), the visited line's
HPOS/VPOS/WIDTH/HEIGHT become the union of both, and the partner element is removed. The pairs are those of
plant_merge exactly: the merged boxes are checked against signals.plant_merge, and the image cross_share
(original and planted) against testb.json. Writes orig/ (copies), planted/ and image.json.

score: Check 1, AUC of alto_cross_share planted against original over the pages #2116 scored; Check 2, Spearman
of alto_cross_share with #2116's image cross_share over the joined pages. Page bootstrap, 1,000 draws, seed 2167.
alto_cross_share comes from the Rust example crates/usnm-alto/examples/alto_cross_share.rs (split_columns at its
current settings), one JSON line per page.
"""
import argparse
import json
import os
import shutil
import sys
import xml.etree.ElementTree as ET

import numpy as np

AUC_BAR, RHO_BAR, DRAWS, SEED = 0.90, 0.60, 1000, 2167


def _local(tag):
    return tag.rsplit("}", 1)[-1]


def pairs_of(boxes, gutters, signals):
    """signals.plant_merge's pairs (visited i, partner j, visited is left of the gutter), same visiting order."""
    items = [(i, b) for i, b in enumerate(boxes) if b is not None and b[3] > b[1]]
    items.sort(key=lambda t: ((t[1][1] + t[1][3]) / 2.0, t[1][0], t[0]))
    used, pairs = set(), []
    for i, b in items:
        if i in used:
            continue
        act = signals._active(gutters, (b[1] + b[3]) / 2.0)
        if not act:
            continue
        gi, cx = min(act, key=lambda a: (abs(a[1] - b[2]), a[1]))
        left_side = (b[0] + b[2]) / 2.0 < cx
        best, best_key = None, None
        for j, c in items:
            if j == i or j in used:
                continue
            ccx = (c[0] + c[2]) / 2.0
            if (ccx > cx) != left_side:
                continue
            ov = min(b[3], c[3]) - max(b[1], c[1])
            if ov < 0.5 * min(b[3] - b[1], c[3] - c[1]):
                continue
            near = (c[0] - cx) if left_side else (cx - c[2])
            key = (abs(near), -ov, j)
            if best_key is None or key < best_key:
                best, best_key = j, key
        if best is None:
            continue
        used.update((i, best))
        pairs.append((i, best, left_side))
    return pairs


def _num(el, *keys):
    try:
        return [float(el.get(k)) for k in keys]
    except (TypeError, ValueError):
        return None


def alto_box(el):
    """A TextLine's box in ALTO units: own attributes, else the union of its String boxes (altolayout's rule)."""
    v = _num(el, "HPOS", "VPOS", "WIDTH", "HEIGHT")
    if v is not None:
        return [v[0], v[1], v[0] + v[2], v[1] + v[3]]
    bs = []
    for s in el:
        if _local(s.tag) == "String" and s.get("CONTENT"):
            w = _num(s, "HPOS", "VPOS", "WIDTH", "HEIGHT")
            if w is not None:
                bs.append([w[0], w[1], w[0] + w[2], w[1] + w[3]])
    if not bs:
        return None
    return [min(b[0] for b in bs), min(b[1] for b in bs), max(b[2] for b in bs), max(b[3] for b in bs)]


def fmt(v):
    return str(int(v)) if float(v).is_integer() else repr(float(v))


def plant_xml(xml, pairs):
    """The ALTO with each (i, j, i_left) pair of TextLines (document order) merged into line i."""
    for _, (prefix, uri) in ET.iterparse(__import__("io").BytesIO(xml), events=("start-ns",)):
        try:
            ET.register_namespace(prefix, uri)
        except ValueError:  # an "ns<N>" prefix: ElementTree writes its own
            pass
    root = ET.fromstring(xml)
    lines = [e for e in root.iter() if _local(e.tag) == "TextLine"]
    parent = {c: p for p in root.iter() for c in p}
    for i, j, i_left in pairs:
        a, b = lines[i], lines[j]
        ba, bb = alto_box(a), alto_box(b)
        ka, kb = list(a), list(b)
        for c in ka + kb:
            if c in a:
                a.remove(c)
        for c in (ka + kb if i_left else kb + ka):
            a.append(c)
        u = [bx for bx in (ba, bb) if bx]
        if u:
            x0, y0 = min(q[0] for q in u), min(q[1] for q in u)
            x1, y1 = max(q[2] for q in u), max(q[3] for q in u)
            for k, v in (("HPOS", x0), ("VPOS", y0), ("WIDTH", x1 - x0), ("HEIGHT", y1 - y0)):
                a.set(k, fmt(v))
        parent[b].remove(b)
    return ET.tostring(root, encoding="utf-8", xml_declaration=True)


def plant(a):
    sys.path.insert(0, a.step2_code)
    import altolayout  # noqa: E402
    import signals  # noqa: E402
    join = json.load(open(os.path.join(a.join_dir, "join.json")))
    gut = json.load(open(os.path.join(a.join_dir, "banded.json")))
    tb = json.load(open(os.path.join(a.join_dir, "testb.json")))["banded"]["test_b"]["per_page"]
    ids = sorted(p for p, v in join["pages"].items() if v["status"] == "joined")
    for d in ("orig", "planted"):
        os.makedirs(os.path.join(a.out, d), exist_ok=True)
    image, mism = {}, []
    for pid in ids:
        src = os.path.join(a.join_dir, "alto", pid + ".xml")
        shutil.copyfile(src, os.path.join(a.out, "orig", pid + ".xml"))
        g = gut.get(pid)
        xml = open(src, "rb").read()
        if g is None or not g["gutters"]:
            image[pid] = {"skipped": "no_gutter"}
            continue
        size = (g["width"], g["height"])
        parsed = altolayout.parse(xml, size)
        boxes = [ln["box"] for ln in parsed["lines"]]
        segs = signals.segments(g["gutters"])
        before = signals.cross_share_of(boxes, segs, size[0])
        new, merges = signals.plant_merge(boxes, g["gutters"])
        prs = pairs_of(boxes, g["gutters"], signals)
        assert len(prs) == merges, pid
        rec = {"cross_share": before, "merges": merges, "scored_2116": "cross_share" in tb.get(pid, {})}
        if merges:
            planted_xml = plant_xml(xml, prs)
            with open(os.path.join(a.out, "planted", pid + ".xml"), "wb") as f:
                f.write(planted_xml)
            rec["cross_share_planted"] = signals.cross_share_of(new, segs, size[0])
            # The planted ALTO, read back the same way, must give plant_merge's boxes.
            back = [ln["box"] for ln in altolayout.parse(planted_xml, size)["lines"]]
            key = lambda bs: sorted(tuple(round(v, 6) for v in b) for b in bs if b is not None and b[3] > b[1])
            if key(back) != key(new):
                mism.append(pid)
        t = tb.get(pid, {})
        if "cross_share" in t and (abs(t["cross_share"] - before) > 1e-12 or abs(t["cross_share_planted"] - rec["cross_share_planted"]) > 1e-12):
            mism.append(pid + ":2116")
        image[pid] = rec
    with open(os.path.join(a.out, "image.json"), "w") as f:
        json.dump({"pages": image, "mismatches": mism}, f, indent=1, sort_keys=True)
    print(f"pages {len(ids)}, planted {sum('cross_share_planted' in v for v in image.values())}, mismatches {mism}")


def auc(y, s):
    from scipy.stats import rankdata
    y, s = np.asarray(y), np.asarray(s, dtype=float)
    pos = int(y.sum())
    neg = len(y) - pos
    r = rankdata(s)
    return float((r[y == 1].sum() - pos * (pos + 1) / 2) / (pos * neg))


def rows(path):
    out = {}
    for ln in open(path):
        r = json.loads(ln)
        out[os.path.splitext(os.path.basename(r["alto"]))[0]] = r
    return out


def score(a):
    from scipy.stats import spearmanr
    orig, planted = rows(a.orig), rows(a.planted)
    image = json.load(open(a.image))["pages"]
    errors = sorted({p for d in (orig, planted) for p, r in d.items() if "error" in r or r.get("alto_cross_share") is None})
    bad_count = sorted({p for d in (orig, planted) for p, r in d.items() if "error" not in r and not r["cut_matches_repair"]})
    # Check 1: pages #2116 scored in Test B.
    c1 = sorted(p for p, v in image.items() if v.get("scored_2116") and p not in errors)
    o = np.array([orig[p]["alto_cross_share"] for p in c1])
    q = np.array([planted[p]["alto_cross_share"] for p in c1])
    n = len(c1)
    a1 = auc([0] * n + [1] * n, np.r_[o, q])
    # Check 2: every joined page with an image cross_share.
    c2 = sorted(p for p, v in image.items() if v.get("cross_share") is not None and p not in errors)
    x = np.array([orig[p]["alto_cross_share"] for p in c2])
    y = np.array([image[p]["cross_share"] for p in c2])
    r2 = float(spearmanr(x, y).statistic)
    rng = np.random.default_rng(SEED)
    b1, b2 = [], []
    for _ in range(DRAWS):
        k = rng.integers(0, n, n)
        b1.append(auc([0] * n + [1] * n, np.r_[o[k], q[k]]))
        m = rng.integers(0, len(c2), len(c2))
        b2.append(float(spearmanr(x[m], y[m]).statistic))
    ci = lambda v: [float(np.nanpercentile(v, 2.5)), float(np.nanpercentile(v, 97.5))]
    allr = list(orig.values())
    res = {
        "check1_auc": {"pages": n, "auc": a1, "ci95": ci(b1), "bar": AUC_BAR, "passes": a1 >= AUC_BAR,
                       "mean_original": float(o.mean()), "mean_planted": float(q.mean()),
                       "planted_higher": int((q > o).sum()), "equal": int((q == o).sum())},
        "check2_spearman": {"pages": len(c2), "rho": r2, "ci95": ci(b2), "bar": RHO_BAR, "passes": r2 >= RHO_BAR},
        "rust_errors": errors, "cut_count_mismatch": bad_count,
        "original_pages": {"n": len(allr),
                           "with_gutter": sum(r.get("gutters", 0) > 0 for r in allr),
                           "gutters_total": sum(r.get("gutters", 0) for r in allr),
                           "lines_total": sum(r.get("lines", 0) for r in allr),
                           "lines_cut_total": sum(r.get("cut", 0) for r in allr)},
        "planted_pages": {"with_gutter": sum(planted[p].get("gutters", 0) > 0 for p in c1),
                          "lines_cut_total": sum(planted[p].get("cut", 0) for p in c1)},
        "bootstrap": {"draws": DRAWS, "seed": SEED, "unit": "page"},
        "per_page": {p: {"alto_cross_share": orig[p].get("alto_cross_share"), "gutters": orig[p].get("gutters"),
                         "cut": orig[p].get("cut"), "lines": orig[p].get("lines"),
                         "alto_cross_share_planted": planted.get(p, {}).get("alto_cross_share"),
                         "image_cross_share": image.get(p, {}).get("cross_share")} for p in sorted(orig)},
    }
    with open(a.out, "w") as f:
        json.dump(res, f, indent=1, sort_keys=True)
    print(json.dumps({k: v for k, v in res.items() if k != "per_page"}, indent=1))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("plant")
    p.add_argument("--step2-code", required=True)
    p.add_argument("--join-dir", required=True)
    p.add_argument("--out", required=True)
    s = sub.add_parser("score")
    for k in ("--orig", "--planted", "--image", "--out"):
        s.add_argument(k, required=True)
    a = ap.parse_args(argv)
    plant(a) if a.cmd == "plant" else score(a)
    return 0


if __name__ == "__main__":
    sys.exit(main())
