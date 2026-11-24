#!/usr/bin/env python3
"""Column-region labels from the licensed reference sets (#2107, model v2).

The column regions are derived by colgeom.columnize from the sets' own human region annotations
(never from a model), with zones as members: a zone joins a column stack when it overlaps it in x
and lies within ZONE_VGAP of the page height below it; zones wider than colgeom.WIDE_FACTOR times the
median zone width (headlines across columns) close the stacks under them.

  teklia   rows written by layout_refs/conv_finlam.py (refs JSONL: text_regions with a type).
           Members: types article, headline, heading (in-column headings belong to the column).
           Teklia/Newspapers-finlam and Teklia/Newspapers-finlam-La-Liberte, MIT.
      conv_refs.py teklia --refs FILE.jsonl --name liberte --out-dir OUT
                   [--only NAME=FILE] [--exclude NAME=FILE ...]
  cg       Chronicling Germany PAGE-XML (data/annotations/*.xml, data/split.csv). Members: TextRegions
           of structure type paragraph and heading. Pages flagged Test in split.csv are never written
           (the official Test split is reserved). CC BY-NC 4.0, University of Bonn: research
           variant only, see LICENCE-NOTE in the plan.
      conv_refs.py cg --data-dir D/66b50f53/data --out-dir OUT [--exclude NAME=FILE ...]

--only NAME=FILE keeps only the listed pages (used to restrict La Liberte to pages already spent by
earlier #2107 tests, so the unused pool stays a test reserve); --exclude drops listed pages. Both
take one id per line or JSONL with page_id. Writes OUT/labels.jsonl (same shape as alto_weak.py) and
OUT/manifest.tsv (every page with status written | excluded:<list> | not_in_only | test_split |
no_columns).
"""
import argparse
import csv
import json
import os
import re
import sys
import xml.etree.ElementTree as ET

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import alto_weak  # noqa: E402
import colgeom  # noqa: E402

ZONE_VGAP = 0.02  # of page height
TEKLIA_TYPES = {"article", "headline", "heading"}
CG_TYPES = {"paragraph", "heading"}
LICENCE = {"finlam": "MIT (Teklia)", "liberte": "MIT (Teklia)", "cg": "CC BY-NC 4.0 (University of Bonn)"}


def _lists(specs):
    out = {}
    for spec in specs or []:
        name, path = spec.split("=", 1)
        for pid in alto_weak.read_id_list(path):
            out.setdefault(pid, name)
    return out


def zone_labels(boxes, w, h):
    regs = colgeom.columnize(boxes, w, h, max_vgap=ZONE_VGAP * h, min_members=1)
    return regs, colgeom.gutters_from_regions(regs, w, h)


def write(out_dir, labels, manifest):
    os.makedirs(out_dir, exist_ok=True)
    with open(os.path.join(out_dir, "labels.jsonl"), "w") as f:
        for lab in labels:
            f.write(json.dumps(lab, sort_keys=True) + "\n")
    with open(os.path.join(out_dir, "manifest.tsv"), "w", newline="") as f:
        wr = csv.DictWriter(f, fieldnames=["page_id", "split", "status", "columns"], delimiter="\t")
        wr.writeheader()
        wr.writerows(manifest)
    summ = {"written": len(labels), "statuses": alto_weak._count(m["status"] for m in manifest)}
    print(json.dumps(summ, sort_keys=True))
    return summ


def cmd_teklia(a):
    only = _lists(a.only) if a.only else None
    excl = _lists(a.exclude)
    labels, manifest = [], []
    with open(a.refs) as f:
        for ln in f:
            if not ln.strip():
                continue
            r = json.loads(ln)
            pid = str(r["page_id"])
            m = {"page_id": pid, "split": r.get("split"), "status": "", "columns": 0}
            if pid in excl:
                m["status"] = "excluded:" + excl[pid]
            elif only is not None and pid not in only:
                m["status"] = "not_in_only"
            else:
                boxes = [t["box"] for t in r.get("text_regions", []) if t.get("type") in TEKLIA_TYPES]
                regs, gut = zone_labels(boxes, r["width"], r["height"])
                m["columns"] = len(regs)
                if not regs:
                    m["status"] = "no_columns"
                else:
                    m["status"] = "written"
                    labels.append({"columns": regs, "gutters": gut, "height": r["height"], "image_path": r["image_path"],
                                   "licence": LICENCE[a.name], "page_id": pid, "source": a.name,
                                   "split": r.get("split"), "width": r["width"]})
            manifest.append(m)
    write(a.out_dir, labels, manifest)
    return 0


def _structure_type(el):
    m = re.search(r"structure\s*\{[^}]*?type:([^;}]*)", el.get("custom") or "")
    return m.group(1).strip() if m else None


def _coords_box(el):
    for c in el:
        if c.tag.rsplit("}", 1)[-1] == "Coords":
            pts = [tuple(float(v) for v in p.split(",")) for p in (c.get("points") or "").split() if "," in p]
            if len(pts) >= 2:
                xs, ys = [p[0] for p in pts], [p[1] for p in pts]
                return [min(xs), min(ys), max(xs), max(ys)]
    return None


def parse_page_xml(path):
    """(width, height, [member boxes]) of a Chronicling Germany PAGE-XML file."""
    root = ET.parse(path).getroot()
    page = next(e for e in root.iter() if e.tag.rsplit("}", 1)[-1] == "Page")
    w, h = int(page.get("imageWidth")), int(page.get("imageHeight"))
    boxes = []
    for e in page.iter():
        if e.tag.rsplit("}", 1)[-1] == "TextRegion" and _structure_type(e) in CG_TYPES:
            b = _coords_box(e)
            if b:
                boxes.append(b)
    return w, h, boxes


def read_cg_split(data_dir):
    """page id -> split name (Test, Validation, Training) from split.csv's flag columns."""
    out = {}
    with open(os.path.join(data_dir, "split.csv"), newline="") as f:
        for r in csv.DictReader(f):
            name = r["Filename"]
            out[name] = "Test" if r.get("Test") == "1" else ("Validation" if r.get("Validation") == "1" else "Training")
    return out


def cmd_cg(a):
    split = read_cg_split(a.data_dir)
    excl = _lists(a.exclude)
    labels, manifest = [], []
    ann = os.path.join(a.data_dir, "annotations")
    for fn in sorted(os.listdir(ann)):
        if not fn.endswith(".xml"):
            continue
        pid = fn[:-4]
        sp = split.get(pid, "unlisted")
        m = {"page_id": pid, "split": sp, "status": "", "columns": 0}
        if sp in ("Test", "unlisted"):
            m["status"] = "test_split" if sp == "Test" else "not_in_split_csv"
        elif pid in excl:
            m["status"] = "excluded:" + excl[pid]
        else:
            w, h, boxes = parse_page_xml(os.path.join(ann, fn))
            regs, gut = zone_labels(boxes, w, h)
            m["columns"] = len(regs)
            if not regs:
                m["status"] = "no_columns"
            else:
                m["status"] = "written"
                labels.append({"columns": regs, "gutters": gut, "height": h,
                               "image_path": os.path.join(a.data_dir, "images", pid + ".jpg"),
                               "licence": LICENCE["cg"], "page_id": pid, "source": "cg", "split": sp, "width": w})
        manifest.append(m)
    write(a.out_dir, labels, manifest)
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    t = sub.add_parser("teklia")
    t.add_argument("--refs", required=True)
    t.add_argument("--name", required=True, choices=["finlam", "liberte"])
    t.add_argument("--out-dir", required=True)
    t.add_argument("--only", action="append")
    t.add_argument("--exclude", action="append", default=[])
    c = sub.add_parser("cg")
    c.add_argument("--data-dir", required=True)
    c.add_argument("--out-dir", required=True)
    c.add_argument("--exclude", action="append", default=[])
    a = ap.parse_args(argv)
    return cmd_teklia(a) if a.cmd == "teklia" else cmd_cg(a)


if __name__ == "__main__":
    sys.exit(main())
