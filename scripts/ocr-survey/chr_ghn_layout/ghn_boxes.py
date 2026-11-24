#!/usr/bin/env python3
"""Line boxes for the 699 GHN-only pages of the Cherokee Phoenix (#1622), the staging step of the
campaign design in docs/reports/2026-11-cherokee/report.md (step 4).

The pages have no ALTO, so a line job needs boxes from somewhere else. This runs, on the machine that
holds the images, the line finder #2128 adopted and the line classifier #1830 chose, and writes one
`boxes.jsonl` a page for `usnm-boinc line-stage-ghn` and `usnm-boinc export --line-boxes`.

- Finder: S1 of #2128 (result: https://github.com/tgoodyear/usnewsmap.com/issues/2128#issuecomment-7119527011).
  The #2107 banded column finder, frozen at commit 337fc0aa (frozen/columns_337fc0aa.py, checked against
  COLUMNS_SHA256 before any page is read), cuts the page into column regions, and S0, the #1622 finder
  (Tesseract 5.5.0 PSM 3 on the Sauvola-binarised scan, then the clean-up), runs inside each region. The
  code is s1_columns.py, s0_find.py and common.py from branch chr-2128 at 89fb1017, unchanged.
- Classifier: the line classifier v2 ensemble that passed #2128's amendment 3.3 gate (line_clf_v2.py,
  frozen/line_clf_v2.json, call at p_line >= 0.46). It needs the shape score of #1830 as one feature:
  chr_line_script.py's two-stage shape classifier without the scale-bound features h, w and contrast,
  trained on the LoC lines of the 160 pages outside the #1622 sample (classify_lines.py from chr-2128 at
  8b82fe7e, unchanged). A box with no shape score (no measurable ink) is not called.
- Page checks, each written as "held": true with its reason in "held_reasons", and export passes a held
  page over until a second layout attempt or a look by hand:
  - "merged_lines": the merged-lines share (boxes over 1.8 times the page's median height,
    chr_layout_qa.py) is over --flag-share (0.08, the level #1622 used);
  - "faded": the page is in the lowest quarter of the run's pages by #2128 amendment 2's contrast
    (greyscale scan scaled to 1,000 px wide, p95 - p5), floor(N/4) pages by (contrast, doc id). The gate
    didn't test faded pages, so they wait.

The column finder's method follows jscancella/NYTribuneOCRExperiments (findText_usingSums.py, projection
profiles; GPL-3.0), reimplemented in #2107 with no code copied; #2107 also draws on
jscancella/chronam_with_yolo (YOLOv5 columns; GPL-3.0), whose model is not used here. Both repositories
were read in November 2026.

    ghn_boxes.py find  --ghn-pages ghn-pages.jsonl --images DIR --tessdata DIR --out found-s1.jsonl
                       [--select FILE] [--jobs N] [--prep-slots N]
    ghn_boxes.py write --ghn-pages ghn-pages.jsonl --images DIR --found found-s1.jsonl
                       --train feats-train-truth.jsonl --chr-tessdata DIR --eng-tessdata DIR --cnn FILE
                       --chr-text FILE --eng-text FILE --out-dir DIR [--flag-share 0.08] [--jobs N]
    ghn_boxes.py pilot --ghn-pages ghn-pages.jsonl --summary DIR/summary.jsonl --out pilot.txt [--count 5]

`find` checks each image against the page list's sha256 and writes one S1 record a page (resumable: pages
already in --out are skipped). `write` classifies every box, runs the page check and writes
DIR/ghn/<local id>/boxes.jsonl and DIR/summary.jsonl. The classifier's inputs, each checked by sha256
against frozen/line_clf_v2.json: --chr-tessdata holds chr_phoenix.traineddata, --eng-tessdata tessdata_best's
eng.traineddata, --cnn the CNN weights, --chr-text and --eng-text the two character models' training texts
(ocr/models/chr-line-clf-v2/README.md says where each comes from). `pilot` picks the pilot's pages: in the order of
sha256("1622:ghn-pilot:" + doc id), the first --count pages that have a Cherokee line and are not held, all
in the batch of the first such page (one export takes one batch). --images is the GHN fetcher's output directory (each
page's `file` is under it); --tessdata holds eng.traineddata; --select lists `ghn:` ids, one a line.

boxes.jsonl: a page row, then one row a box in S1's order (region, then Tesseract's block, paragraph and
line), keys sorted:
  {"classifier", "contrast", "doc_id", "finder", "height", "held", "held_reasons", "kind": "page",
   "scan_sha256", "tall_share", "width"}
  {"box": [x0, y0, x1, y1], "chr", "id": "r<region>b<block>p<par>l<line>", "kind": "line", "p_chr", "p_cnn",
   "p_line", "p_v2"}
"chr" is p_line >= 0.46; the p_ scores are null for a box with no shape score. Needs Tesseract 5.5.0 and
Python with numpy, scipy, Pillow, scikit-learn and PyTorch (the image of ocr/script-detect/Containerfile
has them).
"""
import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
COLUMNS_PY = HERE / "frozen" / "columns_337fc0aa.py"
# sha256 of `git show 337fc0aa:scripts/ocr-survey/layout_columns/columns.py`, the file S1 ran with.
COLUMNS_SHA256 = "37a9f2fb4da35f6f04e3a4f865db65e652324a483a5bb653504c346cf7834a04"
TESSERACT = "5.5.0"
FLAG_SHARE = 0.08
# A GHN page's id (ADR-0026): `ghn:` and a LoC doc_id's shape, one safe path segment.
DOC_ID = re.compile(r"ghn:[a-z0-9]{1,16}_[0-9]{4}-[0-9]{2}-[0-9]{2}_ed-[1-9][0-9]{0,3}_seq-[1-9][0-9]{0,4}")


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def check_columns(path=COLUMNS_PY, want=COLUMNS_SHA256):
    """The frozen column finder, refused unless it is the file S1 was scored with."""
    got = sha256_file(path)
    if got != want:
        sys.exit(f"{path} has sha256 {got}, not the frozen 337fc0aa file's {want}")
    return str(path)


def tesseract_version():
    out = subprocess.run(["tesseract", "--version"], check=True, capture_output=True, text=True)
    first = (out.stdout or out.stderr).splitlines()[0].strip()
    if first != f"tesseract {TESSERACT}":
        sys.exit(f"S1 was scored with tesseract {TESSERACT}; this is {first!r}")
    return TESSERACT


def finder_version(eng_sha256):
    return (f"S1 #2128: columns 337fc0aa (sha256 {COLUMNS_SHA256[:12]}), then S0 in each region "
            f"(chr-2128 89fb1017 s1_columns.py); tesseract {TESSERACT} eng (sha256 {eng_sha256[:12]}) "
            "psm 3 dpi 300 on Sauvola w51 k0.3; clean min-h 14")


def classifier_version(train_sha256, frozen):
    return (f"chr line classifier v2 ensemble #2128 a3.3 (chr-2128 77d692ae), p_line >= {frozen['t']}; "
            f"cnn sha256 {frozen['cnn_sha256'][:12]}, v2 model sha256 {frozen['v2_model_sha256'][:12]}; "
            f"shape p_chr #1830 (chr-2128 8b82fe7e classify_lines.py), train sha256 {train_sha256[:12]}")


def line_id(tid):
    """S1's tid "r<region>.<block>.<par>.<line>" as a line job's id."""
    r, b, p, l = tid.split(".")
    if not (r.startswith("r") and r[1:].isdigit() and b.isdigit() and p.isdigit() and l.isdigit()):
        raise ValueError(f"not an S1 tid: {tid!r}")
    return f"{r}b{b}p{p}l{l}"


def read_pages(path):
    with open(path, encoding="utf-8") as f:
        rows = [json.loads(l) for l in f if l.strip()]
    pages = {}
    for r in rows:
        if r.get("source") != "ghn" or not DOC_ID.fullmatch(str(r.get("doc_id", ""))):
            sys.exit(f"{path}: {r.get('doc_id')!r} is not a GHN page id")
        if r["doc_id"] in pages:
            sys.exit(f"{path}: {r['doc_id']} is listed twice")
        pages[r["doc_id"]] = r
    return pages


def selected(pages, select):
    if not select:
        return sorted(pages)
    with open(select, encoding="utf-8") as f:
        ids = [l.strip() for l in f if l.strip()]
    missing = [i for i in ids if i not in pages]
    if missing:
        sys.exit(f"not in the GHN page list: {', '.join(missing[:5])}")
    return ids


def image_of(images, row):
    rel = Path(row["file"])
    if rel.is_absolute() or ".." in rel.parts:
        sys.exit(f"{row['doc_id']}: image path {row['file']} leaves the fetcher's directory")
    return Path(images) / rel


def page_rows(rec, page, finder, classifier, flag_share, t, faded=False):
    """boxes.jsonl's rows for one page: S1's record `rec` after classification and the page checks. Each
    line of `rec` carries p_chr, p_v2, p_cnn and p_line (any may be None); the call is p_line >= t."""
    reasons = (["merged_lines"] if rec["tall_share"] > flag_share else []) + (["faded"] if faded else [])
    head = {"classifier": classifier, "contrast": rec.get("contrast"), "doc_id": page["doc_id"], "finder": finder,
            "height": rec["height"], "held": bool(reasons), "held_reasons": reasons, "kind": "page",
            "scan_sha256": page["scan"]["sha256"], "tall_share": rec["tall_share"], "width": rec["width"]}
    rows = [head]
    for l in rec["lines"]:
        x0, y0, x1, y1 = (int(v) for v in l["box"])
        if not (0 <= x0 and 0 <= y0 and x1 <= rec["width"] and y1 <= rec["height"]):
            sys.exit(f"{page['doc_id']}: box {l['box']} is off the {rec['width']}x{rec['height']} page")
        if x1 <= x0 or y1 <= y0:
            continue  # no area: nothing for a line job to crop
        q = l.get("p_line")
        rows.append({"box": [x0, y0, x1, y1], "chr": q is not None and q >= t, "id": line_id(l["tid"]),
                     "kind": "line", "p_chr": l.get("p_chr"), "p_cnn": l.get("p_cnn"), "p_line": q,
                     "p_v2": l.get("p_v2")})
    return rows


def area_lines(rec):
    """The lines of `rec` that boxes.jsonl keeps (those with area), as the classifier saw them in #2128."""
    return [l for l in rec["lines"] if int(l["box"][2]) > int(l["box"][0]) and int(l["box"][3]) > int(l["box"][1])]


def boxes_path(out_dir, doc_id):
    if not DOC_ID.fullmatch(doc_id):
        sys.exit(f"{doc_id!r} is not a GHN page id")
    return Path(out_dir) / "ghn" / doc_id.split(":", 1)[1] / "boxes.jsonl"


def write_rows(path, rows):
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".jsonl.tmp")
    with open(tmp, "w", encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False, sort_keys=True) + "\n")
    os.replace(tmp, path)


def _find_one(args):
    doc_id, scan, sha, tessdata, columns_py = args
    import s1_columns
    got = sha256_file(scan)
    if got != sha:
        raise SystemExit(f"{scan} is not {doc_id}'s scan (sha256 {got}, the page list has {sha})")
    rec = s1_columns.do_page((doc_id, scan, tessdata, columns_py))
    return {**rec, "doc_id": doc_id, "scan_sha256": sha}


def cmd_find(a):
    sys.path.insert(0, str(HERE))
    import s0_find
    columns_py = check_columns()
    tesseract_version()
    eng = Path(a.tessdata) / "eng.traineddata"
    eng_sha = sha256_file(eng)
    pages = read_pages(a.ghn_pages)
    ids = selected(pages, a.select)
    done = set()
    if os.path.exists(a.out):
        with open(a.out, encoding="utf-8") as f:
            done = {json.loads(l)["doc_id"] for l in f if l.strip()}
    todo = [(i, str(image_of(a.images, pages[i])), pages[i]["scan"]["sha256"], a.tessdata, columns_py)
            for i in ids if i not in done]
    finder = finder_version(eng_sha)
    print(f"{len(todo)} pages to do, {len(done)} done; {finder}", file=sys.stderr, flush=True)
    with open(a.out, "a") as out, s0_find.pool_of(a.jobs, a.prep_slots) as pool:
        for i, rec in enumerate(pool.map(_find_one, todo), 1):
            rec["finder"] = finder
            out.write(json.dumps(rec, sort_keys=True) + "\n")
            out.flush()
            print(f"{i}/{len(todo)} {rec['doc_id']} regions {len(rec['regions'])} lines {len(rec['lines'])}",
                  file=sys.stderr, flush=True)


def _qa_one(args):
    rec, scan = args
    from PIL import Image
    import chr_layout_qa as qa
    Image.MAX_IMAGE_PIXELS = None
    return qa.page_qa(Image.open(scan).convert("L"), rec["lines"])["tall_share"]


_W = {}


def _clf_init(frozen_path, chr_text, eng_text, cnn, chr_tessdata, eng_tessdata):
    import torch
    import line_clf_v2 as v2
    torch.set_num_threads(1)
    fr = v2.load_frozen(frozen_path)
    mc, me = v2.language_models(chr_text, eng_text, fr)
    _W.update(fr=fr, mc=mc, me=me, net=v2.load_cnn(cnn, fr), chr_tessdata=chr_tessdata, eng_tessdata=eng_tessdata)


def _clf_one(args):
    """The ensemble's scores for one page's lines with area, and the page's contrast."""
    rec, scan = args
    from PIL import Image
    import line_clf_v2 as v2
    Image.MAX_IMAGE_PIXELS = None
    im = Image.open(scan).convert("L")
    lines = [{"box": [int(v) for v in l["box"]], "p_chr": l.get("p_chr")} for l in area_lines(rec)]
    s = v2.score_page(im, lines, _W["fr"], _W["mc"], _W["me"], _W["net"], _W["chr_tessdata"], _W["eng_tessdata"])
    return s, v2.contrast(im)


def check_tessdata(chr_tessdata, eng_tessdata, frozen):
    """chr_phoenix and tessdata_best eng, the models the classifier was fitted with."""
    import line_clf_v2 as v2
    for path, want in ((Path(chr_tessdata) / "chr_phoenix.traineddata", frozen["chr_phoenix_sha256"]),
                       (Path(eng_tessdata) / "eng.traineddata", frozen["eng_best_sha256"])):
        got = v2.sha256_file(path)
        if got != want:
            sys.exit(f"{path}: sha256 {got}, the classifier was fitted with {want}")


def cmd_write(a):
    import concurrent.futures as cf
    sys.path.insert(0, str(HERE))
    import classify_lines as cl
    import chr_line_script as ls
    pages = read_pages(a.ghn_pages)
    with open(a.found, encoding="utf-8") as f:
        found = [json.loads(l) for l in f if l.strip()]
    finders = {r["finder"] for r in found}
    if len(finders) != 1:
        sys.exit(f"{a.found} mixes finder versions: {sorted(finders)}")
    (finder,) = finders
    for r in found:
        if r["doc_id"] not in pages or r["scan_sha256"] != pages[r["doc_id"]]["scan"]["sha256"]:
            sys.exit(f"{r['doc_id']} in {a.found} is not the page list's page")
        if r["columns_py_sha256"] != COLUMNS_SHA256:
            sys.exit(f"{r['doc_id']} was found with another column finder ({r['columns_py_sha256']})")
        r["page"] = r["doc_id"]
    import line_clf_v2 as v2
    frozen = v2.load_frozen()
    check_tessdata(a.chr_tessdata, a.eng_tessdata, frozen)
    v2.language_models(a.chr_text, a.eng_text, frozen)  # refuse wrong texts before the long run
    v2.load_cnn(a.cnn, frozen)
    classifier = classifier_version(sha256_file(a.train), frozen)
    # classify_lines.py's main, on these records: the same models, the same features.
    ls.KEEP = [i for i, n in enumerate(ls.NAMES) if n not in cl.DROP]
    scans = [image_of(a.images, pages[r["doc_id"]]) for r in found]
    te = []
    with cf.ProcessPoolExecutor(a.jobs) as pool:
        for k, rows in enumerate(pool.map(cl.page_features, zip(found, scans)), 1):
            te += rows
            if k % 25 == 0:
                print(f"features {k}/{len(found)} pages", file=sys.stderr, flush=True)
        shares = list(pool.map(_qa_one, zip(found, scans)))
    tr = [r for r in ls.load([a.train]) if r["y"] is not None]
    print(f"train {len(tr)} lines, test {len(te)} lines", file=sys.stderr, flush=True)
    p = cl.two_stage(tr, te) if te else []
    score = {(r["page"], r["i"]): float(v) for r, v in zip(te, p)}
    for rec in found:
        for i, l in enumerate(rec["lines"]):
            l["p_chr"] = score.get((rec["page"], i))
    init = (str(v2.FROZEN), a.chr_text, a.eng_text, a.cnn, a.chr_tessdata, a.eng_tessdata)
    with cf.ProcessPoolExecutor(a.jobs, initializer=_clf_init, initargs=init) as pool:
        for k, (rec, (s, con)) in enumerate(zip(found, pool.map(_clf_one, zip(found, scans))), 1):
            for l, (pv, pc, pl) in zip(area_lines(rec), s):
                l["p_v2"], l["p_cnn"], l["p_line"] = pv, pc, pl
            rec["contrast"] = con
            if k % 10 == 0:
                print(f"classified {k}/{len(found)} pages", file=sys.stderr, flush=True)
    faded = v2.faded_pages({r["doc_id"]: r["contrast"] for r in found})
    summary = []
    for rec, share in zip(found, shares):
        rec["tall_share"] = share
        rows = page_rows(rec, pages[rec["doc_id"]], finder, classifier, a.flag_share, frozen["t"],
                         faded=rec["doc_id"] in faded)
        write_rows(boxes_path(a.out_dir, rec["doc_id"]), rows)
        summary.append({"boxes": len(rows) - 1, "chr_lines": sum(r["chr"] for r in rows[1:]),
                        "contrast": rec["contrast"], "doc_id": rec["doc_id"], "held": rows[0]["held"],
                        "held_reasons": rows[0]["held_reasons"], "tall_share": share})
    write_rows(Path(a.out_dir) / "summary.jsonl", summary)
    print(json.dumps({"pages": len(summary), "held": sum(s["held"] for s in summary),
                      "faded": sum("faded" in s["held_reasons"] for s in summary),
                      "chr_lines": sum(s["chr_lines"] for s in summary),
                      "pages_with_chr": sum(s["chr_lines"] > 0 and not s["held"] for s in summary)}),
          file=sys.stderr)


def pilot_pages(pages, summary, count):
    """The pilot's `ghn:` ids, as `pilot` describes them."""
    usable = sorted({s["doc_id"] for s in summary if s["chr_lines"] > 0 and not s["held"] and s["doc_id"] in pages})
    order = sorted(usable, key=lambda d: hashlib.sha256(f"1622:ghn-pilot:{d}".encode()).hexdigest())
    if not order:
        sys.exit("no page has a Cherokee line that is not held")
    batch = pages[order[0]]["batch"]
    ids = [d for d in order if pages[d]["batch"] == batch][:count]
    if len(ids) < count:
        sys.exit(f"only {len(ids)} usable pages in {batch}; the pilot takes {count}")
    return ids


def cmd_pilot(a):
    pages = read_pages(a.ghn_pages)
    with open(a.summary, encoding="utf-8") as f:
        summary = [json.loads(l) for l in f if l.strip()]
    ids = pilot_pages(pages, summary, a.count)
    with open(a.out, "w", encoding="utf-8") as f:
        f.write("".join(f"{d}\n" for d in ids))
    print(f"{len(ids)} pilot pages in {pages[ids[0]]['batch']}", file=sys.stderr)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    f = sub.add_parser("find", help="S1 on the pages, one record a page")
    f.add_argument("--ghn-pages", required=True)
    f.add_argument("--images", required=True)
    f.add_argument("--tessdata", required=True)
    f.add_argument("--out", required=True)
    f.add_argument("--select")
    f.add_argument("--jobs", type=int, default=1)
    f.add_argument("--prep-slots", type=int, default=2, help="workers allowed to binarise at the same time")
    w = sub.add_parser("write", help="classify, check and write boxes.jsonl")
    w.add_argument("--ghn-pages", required=True)
    w.add_argument("--images", required=True)
    w.add_argument("--found", required=True)
    w.add_argument("--train", required=True)
    w.add_argument("--chr-tessdata", required=True, help="directory with chr_phoenix.traineddata")
    w.add_argument("--eng-tessdata", required=True, help="directory with tessdata_best's eng.traineddata")
    w.add_argument("--cnn", required=True, help="the classifier's CNN weights (release asset)")
    w.add_argument("--chr-text", required=True, help="the Cherokee character model's training text")
    w.add_argument("--eng-text", required=True, help="the English character model's training text")
    w.add_argument("--out-dir", required=True)
    w.add_argument("--flag-share", type=float, default=FLAG_SHARE)
    w.add_argument("--jobs", type=int, default=1)
    p = sub.add_parser("pilot", help="the pilot's pages")
    p.add_argument("--ghn-pages", required=True)
    p.add_argument("--summary", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--count", type=int, default=5)
    a = ap.parse_args(argv)
    {"find": cmd_find, "write": cmd_write, "pilot": cmd_pilot}[a.cmd](a)


if __name__ == "__main__":
    main()
