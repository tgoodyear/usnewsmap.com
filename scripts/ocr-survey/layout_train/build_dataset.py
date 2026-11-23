#!/usr/bin/env python3
"""Build the YOLO-format training set for the #2107 layout detector (CPU only, train splits only).

Sources: finlam train (623 pages), La Liberte train (2,000 pages in sha256("2107:tr:"+page_id) order),
Beyond Words train (2,846 pages; teacher labels plus verified boxes). finlam and La Liberte are
Teklia (MIT); Beyond Words is Library of Congress (CC0); the teacher is the American Stories layout
model (arXiv:2308.12477). The last 10% of each set in the same sha order is the validation holdout.

    build_dataset.py --refs-dir DIR --teacher-dir DIR --out DIR [--liberte-pages 2000]

Writes <out>/images/{train,val}/<src>_<id>.jpg (original JPEG bytes), labels/{train,val}/*.txt in YOLO
format, data.yaml, pages.json (source, split, id per file) and counts.json (boxes per class per source).
"""
import argparse
import collections
import glob
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import classmap  # noqa: E402
import rowimages  # noqa: E402

SOURCES = [("Teklia/Newspapers-finlam", "Teklia__Newspapers-finlam", None),
           ("Teklia/Newspapers-finlam-La-Liberte", "Teklia__Newspapers-finlam-La-Liberte", "liberte"),
           ("biglam/loc_beyond_words", "biglam__loc_beyond_words", None)]


def load_teacher(teacher_dir):
    t = {}
    for p in sorted(glob.glob(os.path.join(teacher_dir, "shard*.jsonl"))):
        for ln in open(p):
            if ln.strip():
                d = json.loads(ln)
                t[d["page_id"]] = d["boxes"]
    return t


def page_labels(row, teacher):
    """Final [(x0, y0, x1, y1, class, origin)] for a page. origin is 'source', 'verified' or 'teacher'."""
    own = classmap.mapped_boxes(row)
    if row["dataset"] != "biglam/loc_beyond_words":
        return [(*b, "source") for b in own]
    kept = classmap.drop_overlapping(classmap.teacher_boxes(teacher[row["page_id"]]), own)
    return [(*b, "verified") for b in own] + [(*b, "teacher") for b in kept]


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--refs-dir", required=True)
    ap.add_argument("--teacher-dir", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--liberte-pages", type=int, default=2000)
    a = ap.parse_args(argv)
    teacher = load_teacher(a.teacher_dir)
    counts = collections.defaultdict(lambda: collections.defaultdict(int))
    pages = []
    for ds, stem, cap in SOURCES:
        rows = {}
        for ln in open(os.path.join(a.refs_dir, stem + "__train.jsonl")):
            r = json.loads(ln)
            rows[r["page_id"]] = r
        order = classmap.sha_order(list(rows))
        if cap:
            order = order[: a.liberte_pages]
        if ds == "biglam/loc_beyond_words":
            missing = [p for p in order if p not in teacher]
            if missing:
                raise SystemExit("%d Beyond Words pages have no teacher labels, e.g. %s" % (len(missing), missing[:3]))
        train, val = classmap.split_in_order(order)
        split_of = {p: "train" for p in train} | {p: "val" for p in val}
        short = classmap.SHORT[ds]
        rd = rowimages.Reader()
        for pid in sorted(order, key=lambda p: rows[p]["image_path"]):
            row, split = rows[pid], split_of[pid]
            data = rd.read(row["image_path"])
            name = "%s_%s" % (short, pid)
            ext = rowimages.extension(data)
            for sub in ("images", "labels"):
                os.makedirs(os.path.join(a.out, sub, split), exist_ok=True)
            with open(os.path.join(a.out, "images", split, name + ext), "wb") as f:
                f.write(data)
            lines = []
            for b in page_labels(row, teacher):
                ln = classmap.yolo_line(b[4], b[:4], row["width"], row["height"])
                if ln:
                    lines.append(ln)
                    counts[short + ("/" + b[5] if short == "beyondwords" else "")][b[4]] += 1
                    counts[short + ":" + split][b[4]] += 1
            with open(os.path.join(a.out, "labels", split, name + ".txt"), "w") as f:
                f.write("\n".join(lines) + ("\n" if lines else ""))
            pages.append({"source": short, "split": split, "page_id": pid, "file": name + ext})
    with open(os.path.join(a.out, "data.yaml"), "w") as f:
        f.write("path: %s\ntrain: images/train\nval: images/val\nnames:\n" % "DATASET_ROOT")
        for i, n in enumerate(classmap.NAMES):
            f.write("  %d: %s\n" % (i, n))
    with open(os.path.join(a.out, "pages.json"), "w") as f:
        json.dump(sorted(pages, key=lambda p: (p["source"], p["split"], p["page_id"])), f)
    pc = collections.Counter((p["source"], p["split"]) for p in pages)
    summary = {"boxes_per_class": {k: {c: v.get(c, 0) for c in classmap.NAMES} for k, v in sorted(counts.items())},
               "pages": {"%s:%s" % k: v for k, v in sorted(pc.items())}}
    with open(os.path.join(a.out, "counts.json"), "w") as f:
        json.dump(summary, f, indent=1, sort_keys=True)
    print(json.dumps(summary, indent=1, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
