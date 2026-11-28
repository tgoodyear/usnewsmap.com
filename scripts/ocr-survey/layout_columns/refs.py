#!/usr/bin/env python3
"""Reference gutters for the second attempt at #2107 step 1: one loader over the converted sets.

Method follows jscancella/NYTribuneOCRExperiments (findText_usingSums.py, GPL-3.0), reimplemented;
no code copied.

Reads the rows written by the converters in #2113 (scripts/ocr-survey/layout_refs) and the frozen
de-rz and en-asgold regions of the first plan, and writes one reference file per set:

    refs.py --set NAME --out refs-NAME.json [--refs-dir DIR] [--heldout-dir DIR]

Sets: liberte-train (500 pages in sha256("2107:ll:"+page_id) order), finlam-train, colorado-train,
derz, plus the test and secondary sets (europeana, finlam-test, liberte-test, finlam-val,
liberte-val, colorado-val, en-asgold), which this tool can describe but the tuning never touches.

Datasets: Teklia/Newspapers-finlam and Teklia/Newspapers-finlam-La-Liberte (MIT); biglam/europeana-
newspapers-ground-truth (CC0); NealCaren/co-newspaper-layout-yolo (no licence stated: private use,
every output of that set is marked for_publication false and only aggregates leave the machine);
Reichsanzeiger-GT, CC BY 4.0, Mannheim University Library; American Stories gold, Apache-2.0 text,
Dell Research Harvard, arXiv:2308.12477.

Reference rule (frozen plan, second attempt):
  - body-text gaps: the first attempt's rule (adjacent side-by-side regions overlapping at least half
    the shorter one's height, gap over the shared height) from each set's body-text class only
    (ARTICLE-TEXT, TEXT, ARTICLE or paragraph);
  - a gap wider than 3% of the page width is not a gutter (GAP_CAP);
  - Europeana: the vertical separator regions, one segment each.

Each reference page also carries `zones`, the boxes of its body-text class, which the amended precision
(score.py) uses to find gutters that cut through text.

Reading choices (see the PR):
  - finlam (not La Liberte) stores zone coordinates as percentages of the page (0..100), although the
    converter treats them as pixels; they are scaled here and its column gutters are rebuilt from the
    scaled body-text boxes, since the chaining tolerances depend on the unit;
  - the non-text mask is: picture, advertisement, table and table_content for finlam and La Liberte;
    every non-text region for Colorado and Europeana; the first attempt's excluded types for de-rz and
    en-asgold.
"""
import argparse
import hashlib
import json
import os
import sys

import refgutters

GAP_CAP = 0.03
LL_PAGES = 500
FINLAM_PCT = "Teklia/Newspapers-finlam"
BODY = {
    "Teklia/Newspapers-finlam": "ARTICLE-TEXT",
    "Teklia/Newspapers-finlam-La-Liberte": "TEXT",
    "NealCaren/co-newspaper-layout-yolo": "ARTICLE",
    "biglam/europeana-newspapers-ground-truth": "paragraph",
}
MASK_TYPES = {
    "Teklia/Newspapers-finlam": {"picture", "advertisement", "table", "table_content"},
    "Teklia/Newspapers-finlam-La-Liberte": {"picture", "advertisement", "table", "table_content"},
}
FILES = {
    "liberte-train": "Teklia__Newspapers-finlam-La-Liberte__train.jsonl",
    "liberte-val": "Teklia__Newspapers-finlam-La-Liberte__val.jsonl",
    "liberte-test": "Teklia__Newspapers-finlam-La-Liberte__test.jsonl",
    "finlam-train": "Teklia__Newspapers-finlam__train.jsonl",
    "finlam-val": "Teklia__Newspapers-finlam__val.jsonl",
    "finlam-test": "Teklia__Newspapers-finlam__test.jsonl",
    "colorado-train": "NealCaren__co-newspaper-layout-yolo__train.jsonl",
    "colorado-val": "NealCaren__co-newspaper-layout-yolo__val.jsonl",
    "europeana": "biglam__europeana-newspapers-ground-truth__train.jsonl",
}
TUNING_SETS = ("liberte-train", "finlam-train", "colorado-train", "derz")
TEST_SETS = ("europeana", "finlam-test", "liberte-test", "finlam-val", "liberte-val",
             "colorado-val", "en-asgold")
PRIVATE = {"colorado-train", "colorado-val"}


def ll_order(page_id):
    return hashlib.sha256(("2107:ll:" + page_id).encode()).hexdigest()


def cap_gutters(gutters, width, cap=GAP_CAP):
    """Drop the segments wider than cap * width, and the gutters left with none."""
    out = []
    for g in gutters:
        kept = [s for s in g if (s[1] - s[0]) <= cap * width]
        if kept:
            out.append(kept)
    return out


def scale_row(row, sx, sy):
    """Scale every box in a row by (sx, sy)."""
    def sc(b):
        return [b[0] * sx, b[1] * sy, b[2] * sx, b[3] * sy]
    for k in ("text_regions", "nontext_regions", "separators"):
        for r in row[k]:
            r["box"] = sc(r["box"])
    return row


def row_reference(row):
    """{gutters, excluded, width, height} for one converted row."""
    ds, w, h = row["dataset"], row["width"], row["height"]
    if ds == FINLAM_PCT:
        mx = max([b["box"][i] for k in ("text_regions", "nontext_regions", "separators") for b in row[k]
                  for i in (0, 1, 2, 3)] or [0])
        if mx <= 100.5 < min(w, h):  # percentages; a fixed converter writes pixels, left as they are
            scale_row(row, w / 100.0, h / 100.0)
        boxes = [tuple(r["box"]) for r in row["text_regions"] if r["class"] == BODY[ds]]
        boxes = [b for b in boxes if b[2] > b[0] and b[3] > b[1]]
        gut = refgutters.merge_gutters(refgutters.pair_gutters(boxes), w, h) if len(boxes) > 1 else []
    elif ds.startswith("biglam/europeana"):
        gut = []
        for s in row["separators"]:
            if s.get("orientation") == "vertical":
                x0, y0, x1, y1 = s["box"]
                gut.append([[x0, x1, y0, y1]])
    else:
        gut = [[list(s) for s in g] for g in row["column_gutters"]]
    if not ds.startswith("biglam/europeana"):
        gut = cap_gutters(gut, w)
    mt = MASK_TYPES.get(ds)
    exc = [list(r["box"]) for r in row["nontext_regions"] if mt is None or r.get("type") in mt]
    zones = [list(r["box"]) for r in row["text_regions"] if r["class"] == BODY[ds]]
    return {"excluded": exc, "gutters": gut, "height": h, "width": w, "zones": zones}


def heldout_reference(regions_path, selection_path, body_type, only_ca_page=False):
    """de-rz / en-asgold: the first attempt's regions, body-text class only, with the gap cap."""
    sel = refgutters.read_selection(selection_path, only_ca_page)
    by_page = {}
    with open(regions_path) as f:
        for line in f:
            r = json.loads(line)
            if r["id"] in sel:
                by_page.setdefault(r["id"], []).append(r)
    out = {}
    for pid, row in sorted(sel.items()):
        w, h = int(row["width"]), int(row["height"])
        regs = by_page.get(pid, [])
        boxes = [tuple(r["box"]) for r in regs if r.get("type") == body_type and r.get("box")]
        gut = refgutters.merge_gutters(refgutters.pair_gutters(boxes), w, h) if len(boxes) > 1 else []
        exc = [list(r["box"]) for r in regs if refgutters.is_excluded(r.get("type")) and r.get("box")]
        zones = [list(r["box"]) for r in regs if r.get("type") == body_type and r.get("box")]
        out[pid] = {"excluded": exc, "gutters": cap_gutters(gut, w), "height": h, "width": w, "zones": zones}
    return out


def read_rows(path):
    with open(path) as f:
        for line in f:
            if line.strip():
                yield json.loads(line)


def build_set(name, refs_dir=None, heldout_dir=None):
    """(refs dict keyed by page id, extra info). The 500-page La Liberte train draw happens here."""
    if name == "derz":
        d = os.path.join(heldout_dir, "de-rz")
        refs = heldout_reference(os.path.join(d, "regions.jsonl"), os.path.join(d, "selection.tsv"),
                                 "paragraph")
        return refs, {k: os.path.join(d, "images", k + ".jpg") for k in refs}
    if name == "en-asgold":
        d = os.path.join(heldout_dir, "en-asgold")
        refs = heldout_reference(os.path.join(d, "regions.jsonl"), os.path.join(d, "selection.tsv"),
                                 "article", only_ca_page=True)
        return refs, {k: os.path.join(d, "images", k + ".jpg") for k in refs}
    rows = list(read_rows(os.path.join(refs_dir, FILES[name])))
    if name == "liberte-train":
        rows = sorted(rows, key=lambda r: ll_order(r["page_id"]))[:LL_PAGES]
    refs, paths = {}, {}
    for r in rows:
        paths[r["page_id"]] = r["image_path"]
        refs[r["page_id"]] = row_reference(r)
    return refs, paths


def counts(refs):
    gs = [g for v in refs.values() for g in v["gutters"]]
    return {"gutter_height_px": round(sum(s[3] - s[2] for g in gs for s in g)), "gutters": len(gs),
            "pages": len(refs), "pages_with_gutters": sum(1 for v in refs.values() if v["gutters"])}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--set", required=True, choices=sorted(set(TUNING_SETS) | set(TEST_SETS)))
    ap.add_argument("--out", required=True)
    ap.add_argument("--paths-out", help="also write page id -> image_path (JSON)")
    ap.add_argument("--refs-dir", default="/data/layout-refs/refs")
    ap.add_argument("--heldout-dir", default="/data/s9-heldout")
    a = ap.parse_args(argv)
    refs, paths = build_set(a.set, a.refs_dir, a.heldout_dir)
    with open(a.out, "w") as f:
        json.dump({"for_publication": a.set not in PRIVATE, "pages": refs, "set": a.set}, f, sort_keys=True)
    if a.paths_out:
        with open(a.paths_out, "w") as f:
            json.dump(paths, f, sort_keys=True)
    print(json.dumps(dict(counts(refs), set=a.set), sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
