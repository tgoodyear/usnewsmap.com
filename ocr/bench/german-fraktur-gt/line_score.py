#!/usr/bin/env python3
"""Line-aligned CER of Tesseract on German Fraktur page ground truth (rule A v2).

Page CER (score.py) compares the whole page text, so a column or region read in
another order than the truth's counts as edits. This scorer takes reading order
out: for each ground-truth line it collects the engine's words whose box centre
falls inside the line's bounding box, left to right, and compares that with the
line's text. A truth line with no words counts in full as deletions ("lost
text"); engine words outside every truth line are not counted.

Stdlib + Pillow only. One CSV row per (page, arm) with score.py's columns plus
`lost_chars` (reference characters on lines with no words), `lines` and
`lost_lines`; never any OCR or GT text. The engine's ALTO for each page is kept
in --alto-dir (it holds OCR text: keep it out of any repository or release) and
reused on a rerun.

  python3 line_score.py --data-root DIR --out lines.csv --alto-dir DIR \
    --cmd 'podman run ... /in/{imgname} stdout -l {model} --psm 3 --dpi 300 alto' \
    [--sets chronicling,reichsanzeiger] [--arms frak2021,deu_latf] [--workers 4] \
    [--gpu-study-pages FILE] [--years 1850-1920] [--tessdata DIR]
  python3 analyze.py --csv lines.csv --rule A2

Text rules are score.py's: raw and folded normalisation per line, CER = edits
/ raw reference characters, pooled as sum(edits) / sum(ref_chars) over lines.
"""
import argparse, csv, hashlib, json, multiprocessing as mp, os, shlex, subprocess, sys, tempfile
import xml.etree.ElementTree as ET

import score

FIELDS = score.FIELDS + ["lost_chars", "lines", "lost_lines"]
DEFAULT_CMD = ("tesseract {img} stdout -l {model} --psm 3 --dpi 300 "
               "--tessdata-dir {tessdata} alto")


def _ln(el):
    return el.tag.rsplit("}", 1)[-1]


def _bbox(points):
    xs, ys = [p[0] for p in points], [p[1] for p in points]
    return min(xs), min(ys), max(xs), max(ys)


def gt_lines(xml_path):
    """Truth lines as (x0, y0, x1, y1, text), plus the page size the coordinates
    are in ((w, h) or None)."""
    root = ET.parse(xml_path).getroot()
    out, size = [], None
    if _ln(root) == "alto":
        for el in root.iter():
            if _ln(el) == "Page" and el.get("WIDTH"):
                size = (float(el.get("WIDTH")), float(el.get("HEIGHT")))
            if _ln(el) != "TextLine":
                continue
            txt = ""
            for ch in el:
                n = _ln(ch)
                if n == "String":
                    txt += (" " if txt else "") + (ch.get("CONTENT") or "")
                elif n == "HYP":
                    txt += ch.get("CONTENT") or "-"
            x, y = float(el.get("HPOS", 0)), float(el.get("VPOS", 0))
            out.append((x, y, x + float(el.get("WIDTH", 0)), y + float(el.get("HEIGHT", 0)), txt))
        return out, size
    for el in root.iter():
        if _ln(el) == "Page" and el.get("imageWidth"):
            size = (float(el.get("imageWidth")), float(el.get("imageHeight")))
        if _ln(el) != "TextLine":
            continue
        pts, txt = None, score._line_text_page(el)
        for ch in el:
            if _ln(ch) == "Coords" and ch.get("points"):
                pts = [tuple(float(v) for v in p.split(",")) for p in ch.get("points").split()]
        if txt is not None and score.norm_raw(txt):
            if not pts:     # silently dropping it would shrink the reference
                raise ValueError("truth line %s has text but no usable Coords"
                                 % (el.get("id") or "?"))
            out.append((*_bbox(pts), txt))
    return out, size


def alto_words(data):
    """Engine words as (x0, y0, x1, y1, text) and the ALTO page size."""
    root = ET.fromstring(data)
    words, size = [], None
    for el in root.iter():
        n = _ln(el)
        if n == "Page" and el.get("WIDTH"):
            size = (float(el.get("WIDTH")), float(el.get("HEIGHT")))
        elif n == "String":
            x, y = float(el.get("HPOS", 0)), float(el.get("VPOS", 0))
            words.append((x, y, x + float(el.get("WIDTH", 0)), y + float(el.get("HEIGHT", 0)),
                          el.get("CONTENT") or ""))
    return words, size


def align(lines, words, gt_size=None, ocr_size=None):
    """Per truth line, the words whose centre falls inside its box, left to right.
    Engine coordinates are scaled to the truth's page size when both are known."""
    sx = sy = 1.0
    if gt_size and ocr_size and ocr_size[0] and ocr_size[1]:
        sx, sy = gt_size[0] / ocr_size[0], gt_size[1] / ocr_size[1]
    centres = [((w[0] + w[2]) / 2 * sx, (w[1] + w[3]) / 2 * sy, w[0] * sx, w[4]) for w in words]
    out = []
    for x0, y0, x1, y1, text in lines:
        inside = sorted((c for c in centres if x0 <= c[0] <= x1 and y0 <= c[1] <= y1),
                        key=lambda c: c[2])
        out.append((text, " ".join(c[3] for c in inside), bool(inside)))
    return out


def score_lines(pairs):
    """Pooled raw and folded edits over lines; lost text is reference characters
    on lines that got no words."""
    tot = {"ref_chars": 0, "edits_raw": 0, "edits_folded": 0, "lost_chars": 0,
           "lines": 0, "lost_lines": 0}
    refs, hyps = [], []
    for ref, hyp, has in pairs:
        rr = score.norm_raw(ref)
        if not rr:
            continue
        tot["lines"] += 1
        tot["ref_chars"] += len(rr)
        tot["edits_raw"] += score.lev(rr, score.norm_raw(hyp))
        tot["edits_folded"] += score.lev(score.norm_fold(ref), score.norm_fold(hyp))
        if not has:
            tot["lost_chars"] += len(rr)
            tot["lost_lines"] += 1
        refs.append(ref)
        hyps.append(hyp)
    n = tot["ref_chars"]
    tot["cer_raw"] = tot["edits_raw"] / n if n else 0.0
    tot["cer_folded"] = tot["edits_folded"] / n if n else 0.0
    tot["word_f1"] = score.word_f1(score.norm_fold("\n".join(refs)), score.norm_fold("\n".join(hyps)))
    return tot


def _manifest_ok(cache, want):
    """True when the cached ALTO and its manifest match `want` (provenance and
    the ALTO's own hash), so a stale, truncated or foreign file is rerun."""
    try:
        with open(cache + ".json", encoding="utf-8") as f:
            have = json.load(f)
        return have == {**want, "alto_sha256": hashlib.sha256(open(cache, "rb").read()).hexdigest()}
    except (OSError, ValueError):
        return False


def job(args):
    page, arm, template, tessdata, timeout, gpu, alto_dir, prov = args
    try:
        lines, gt_size = gt_lines(page["xml"])
        cache = os.path.join(alto_dir, page["set"], arm, page["page_id"] + ".xml")
        want = {"model_sha256": prov.get("model_sha256"), "tesseract_version": prov.get("version"),
                "cmd": template, "image_sha256": score.sha256_file(page["img"])}
        cpu = 0.0
        if os.path.exists(cache) and _manifest_ok(cache, want):
            with open(cache, "rb") as f:
                data = f.read()
            alto_words(data)
        else:
            with tempfile.TemporaryDirectory() as td:
                img = score.prepared_image(page["img"], td)
                text, cpu = score.run_tesseract(template, img, arm, tessdata, timeout)
            data = text.encode("utf-8")
            alto_words(data)        # must parse before it can enter the cache
            os.makedirs(os.path.dirname(cache), exist_ok=True)
            with open(cache + ".part", "wb") as f:
                f.write(data)
            man = {**want, "alto_sha256": hashlib.sha256(data).hexdigest()}
            with open(cache + ".json.part", "w", encoding="utf-8") as f:
                json.dump(man, f, sort_keys=True)
            os.replace(cache + ".part", cache)
            os.replace(cache + ".json.part", cache + ".json")
        words, ocr_size = alto_words(data)
        s = score_lines(align(lines, words, gt_size, ocr_size))
    except Exception as e:  # not recorded, so a rerun retries it
        return None, "%s/%s [%s]: %s" % (page["set"], page["page_id"], arm,
                                         str(e).replace("\n", " ")[:200])
    row = {k: page[k] for k in ("set", "stratum", "page_id", "title", "year")}
    row.update(arm=arm, cpu_s=round(cpu, 3), **s)
    for k in ("cer_raw", "cer_folded", "word_f1"):
        row[k] = round(row[k], 6)
    row["gpu_study_flag"] = score.gpu_flag(page["set"], page["page_id"], gpu)
    return row, None


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--data-root", default=os.environ.get("DATA_ROOT"))
    ap.add_argument("--sets", default="chronicling,reichsanzeiger")
    ap.add_argument("--out", required=True, help="CSV to append to (resumable)")
    ap.add_argument("--alto-dir", required=True, help="cache of the engine's page ALTO")
    ap.add_argument("--arms", default="frak2021,deu_latf")
    ap.add_argument("--tessdata", default=os.environ.get("TESSDATA_PREFIX", "."))
    ap.add_argument("--cmd", default=DEFAULT_CMD, help="command template printing ALTO on "
                    "stdout; fields as score.py's --cmd")
    ap.add_argument("--version-cmd", default="tesseract --version")
    ap.add_argument("--workers", type=int, default=1)
    ap.add_argument("--years", default="1850-1920")
    ap.add_argument("--meta", help="CSV/TSV with page_id,title,year,stratum")
    ap.add_argument("--gpu-study-pages")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--timeout", type=int, default=900)
    a = ap.parse_args(argv)
    if not a.data_root:
        ap.error("--data-root or $DATA_ROOT required")
    y0, y1 = (int(x) for x in a.years.split("-"))
    gpu = set()
    if a.gpu_study_pages:
        with open(a.gpu_study_pages, encoding="utf-8") as f:
            gpu = {score.gpu_norm(l) for l in f
                   if l.strip().lstrip("﻿") and not l.lstrip().startswith("#")}
            gpu.discard("")
    arms = [x for x in a.arms.split(",") if x]
    gpu_ids = sorted(gpu)
    info = {"scorer": "line_score.py", "arms": {}, "cmd": a.cmd, "years": a.years,
            "limit": a.limit, "gpu_study_ids": gpu_ids,
            "gpu_study_sha256": hashlib.sha256("\n".join(gpu_ids).encode("utf-8")).hexdigest()}
    for arm in arms:
        p = os.path.join(a.tessdata, arm + ".traineddata")
        info["arms"][arm] = score.sha256_file(p) if os.path.exists(p) else None
        if info["arms"][arm] and arm in score.EXPECTED_SHA and info["arms"][arm] != score.EXPECTED_SHA[arm]:
            print("WARNING: %s sha256 differs from the deployed model" % arm, file=sys.stderr)
    try:
        v = subprocess.run(shlex.split(a.version_cmd), stdout=subprocess.PIPE,
                           stderr=subprocess.STDOUT, timeout=120)
        info["tesseract_version"] = v.stdout.decode("utf-8", "replace").strip()
    except Exception as e:
        info["tesseract_version"] = "unknown (%s)" % e
    done = score.done_keys(a.out)
    side = a.out + ".meta.json"
    old = None
    if done:
        problems = []
        try:
            with open(side, encoding="utf-8") as f:
                old = json.load(f)
        except (OSError, ValueError) as e:
            problems.append("sidecar %s unreadable (%s)" % (side, e))
        if old is not None:
            problems += score.resume_problems(old, info, {k[2] for k in done})
        if problems:
            print("REFUSING to resume %s: %s" % (a.out, "; ".join(problems)), file=sys.stderr)
            return 2
        info["arms"] = {**old["arms"], **info["arms"]}
    meta_all = score.load_meta(a.meta)
    jobs, missing_all, expected = [], {}, {arm: {} for arm in arms}
    for s in a.sets.split(","):
        d = os.path.join(a.data_root, s)
        meta = dict(score.load_meta(os.path.join(d, "meta.csv"))) \
            if os.path.exists(os.path.join(d, "meta.csv")) else {}
        meta.update(meta_all)
        pages, skipped, missing = score.find_pages(s, d, meta, (y0, y1), False)
        if missing:
            missing_all[s] = sorted(missing)
        if a.limit:
            pages = pages[:a.limit]
        print("%s: %d pages in %d-%d (%d skipped by year)" % (s, len(pages), y0, y1, skipped),
              file=sys.stderr)
        for arm in arms:
            expected[arm][s] = sorted({pg["page_id"] for pg in pages})
        for pg in pages:
            for arm in arms:
                if (s, pg["page_id"], arm) not in done:
                    jobs.append((pg, arm, a.cmd, a.tessdata, a.timeout, gpu, a.alto_dir,
                                 {"model_sha256": info["arms"].get(arm),
                                  "version": info["tesseract_version"]}))
    if done:        # keep earlier expectations and unresolved missing images
        expected, missing_all = score.merge_resume(old, expected, missing_all)
    info["expected"] = expected
    info["missing_images"] = missing_all
    with open(side, "w", encoding="utf-8") as f:
        json.dump(info, f, indent=1, sort_keys=True)
    new = not os.path.exists(a.out) or os.path.getsize(a.out) == 0
    failed = 0
    with open(a.out, "a", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=FIELDS)
        if new:
            w.writeheader()
        pool = mp.Pool(a.workers) if a.workers > 1 else None
        it = pool.imap_unordered(job, jobs) if pool else map(job, jobs)
        for row, err in it:
            if err:
                failed += 1
                print("FAILED " + err, file=sys.stderr)
                continue
            w.writerow(row)
            f.flush()
        if pool:
            pool.close()
            pool.join()
    print("done: %d rows, %d failed" % (len(jobs) - failed, failed), file=sys.stderr)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
