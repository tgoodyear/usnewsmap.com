#!/usr/bin/env python3
"""Turns settled check-site transcriptions into the inputs of a new ocr-bench release tier (#738, #1479, #968).

`promote` reads the CC0 export of one or more transcription languages (GET /api/export?lang=yi-truth, saved as
JSON lines) and writes, under ocr/bench/<release>/human-truth/:

  <source>/samples.json, images/<id>.jpg, references/<id>.txt, LICENSES/NOTICE.txt   (the `imports` layout the
                                       Rust bench build reads; see docs/ocr/bench.md)
  truth.jsonl      one row per promoted unit: id, task, text sha256, method, readers (one-way ids), counts
  held.jsonl       units not promoted, each with the reason
  spec-fragment.json   `sources`, `imports` and `annotations` to merge into the release's spec.json
  INPUTS.sha256    the checksum of every file above

Rules, applied again here from the raw readings (the export's own `outcome` is only cross-checked):

  * only readings marked `qualified` count; the first two by time are the readers, and they must be two different
    people; exact agreement after normalisation settles the unit, otherwise the third counted reading, by someone
    else, is the adjudication;
  * a unit settles to text only if its reference has at least one letter and at most 2% U+FFFD (docs/ocr/gold-set.md);
  * units whose readers agreed that nothing is legible are held, not scored;
  * `--hints FILE` (JSON lines {"id" or "task", "text"}, engine text, optional): a transcription equal to any hint
    after normalisation is held as a possible copy. The check runs here only; no engine text is ever sent to readers;
  * `--rights FILE` {"confirmed": [["1910-01-01", "1928-12-31"], ...]}: units whose page date is outside every
    confirmed range are held with reason `rights-unconfirmed`. Without --rights nothing is promoted.

Releases are immutable: the script refuses v1.0.0, v1.1.0, any release listed in ocr/bench/releases.json, and an
existing output directory. It never edits ocr/bench/releases.json; publishing goes through the benchmark release PR.

Standard library only, plus Pillow to turn clips into JPEG (as the bench inputs are).
"""
import argparse
import hashlib
import io
import json
import os
import re
import sys
import urllib.request

FROZEN = {"v1.0.0", "v1.1.0"}
MAX_FFFD = 0.02
BIDI = re.compile("[​-‏‪-‮⁠-⁩﻿]")
CTRL = re.compile("[\x00-\x09\x0b-\x1f\x7f]")


def normalize(text):
    """Same as normalizeReading in edge/check/transcribe.mjs (the two are tested against the same cases)."""
    t = CTRL.sub("", BIDI.sub("", text.replace("\r\n", "\n").replace("\r", "\n")))
    lines = [re.sub(r"[ \t]{2,}", " ", l.strip(" \t")) for l in t.split("\n")]
    while lines and lines[0] == "":
        lines.pop(0)
    while lines and lines[-1] == "":
        lines.pop()
    return "\n".join(lines)


def derive(readings):
    """The settlement of one export row, from its readings alone: (outcome, text, unreadable, readers)."""
    counted = sorted((r for r in readings if r.get("qualified", True)), key=lambda r: (r["at"], r["reader"]))
    if len(counted) < 2:
        return None, None, False, []
    a, b = counted[0], counted[1]
    if a["reader"] == b["reader"]:
        return None, None, False, []
    ta, tb = normalize(a["text"] or ""), normalize(b["text"] or "")
    if bool(a["unreadable"]) == bool(b["unreadable"]) and ta == tb:
        return "agreed", ta, bool(a["unreadable"]), [a["reader"], b["reader"]]
    if len(counted) >= 3 and counted[2]["reader"] not in (a["reader"], b["reader"]):
        c = counted[2]
        return "adjudicated", normalize(c["text"] or ""), bool(c["unreadable"]), [a["reader"], b["reader"], c["reader"]]
    return None, None, False, []


def page_date(row):
    m = re.search(r"\d{4}-\d{2}-\d{2}", row.get("page") or row.get("id") or "")
    return m.group(0) if m else None


def rights_ok(row, ranges):
    d = page_date(row)
    return d is not None and any(lo <= d <= hi for lo, hi in ranges)


def load_jsonl(path):
    with open(path, encoding="utf-8") as f:
        return [json.loads(l) for l in f if l.strip()]


def sha256(b):
    return hashlib.sha256(b).hexdigest()


def to_jpeg(data):
    from PIL import Image  # Pillow
    im = Image.open(io.BytesIO(data)).convert("L")
    out = io.BytesIO()
    im.save(out, "JPEG", quality=92)
    return out.getvalue()


def fetch_clip(base, key, images_dir):
    if images_dir:
        with open(os.path.join(images_dir, key), "rb") as f:
            return f.read()
    with urllib.request.urlopen(base.rstrip("/") + "/" + key, timeout=60) as r:  # the site's public /clips/
        return r.read()


def decide(row, hints, ranges):
    """(keep, reason, outcome, text, readers)"""
    outcome, text, unreadable, readers = derive(row["readings"])
    if outcome is None:
        return False, "not-settled", None, None, []
    if row.get("outcome") not in (None, outcome) or (row.get("settled") and row.get("text") is not None and normalize(row["text"]) != text):
        return False, "export-disagrees-with-readings", outcome, text, readers
    if unreadable:
        return False, "agreed-unreadable", outcome, text, readers
    if not re.search(r"\w", text):
        return False, "no-letters", outcome, text, readers
    chars = len(text.replace("\n", ""))
    if chars and text.count("�") / chars > MAX_FFFD:
        return False, "too-many-unreadable-marks", outcome, text, readers
    if text in hints.get(row["id"], ()):
        return False, "equals-an-engine-hint", outcome, text, readers
    if not rights_ok(row, ranges):
        return False, "rights-unconfirmed", outcome, text, readers
    return True, None, outcome, text, readers


def promote(exports, release, out_root, source_conf, ranges, hints, images_dir, clips_base, releases_json, fetch=fetch_clip):
    if not re.fullmatch(r"v\d+\.\d+\.\d+", release) or release in FROZEN:
        raise SystemExit(f"{release}: must be a new release id, never v1.0.0 or v1.1.0")
    if releases_json and os.path.exists(releases_json):
        with open(releases_json, encoding="utf-8") as f:
            if release in {r["release"] for r in json.load(f)["releases"]}:
                raise SystemExit(f"{release} is already in releases.json; releases are never replaced")
    out = os.path.join(out_root, release, "human-truth")
    if os.path.exists(out):
        raise SystemExit(f"{out} exists; promote into a new release id")
    sources, imports, annotations, truth, held = {}, [], {}, [], []
    for lang, rows in exports.items():
        sc = source_conf[lang]
        src = f"usnm-human-{lang}"
        samples = []
        for row in sorted(rows, key=lambda r: r["id"]):
            keep, why, outcome, text, readers = decide(row, hints, ranges)
            if not keep:
                held.append({"id": row["id"], "lang": lang, "reason": why})
                continue
            sid = "ht-%s-%s" % (lang, sha256(row["id"].encode())[:12])
            img = to_jpeg(fetch(clips_base, row["line"], images_dir))
            d = os.path.join(out, src)
            for sub in ("images", "references", "LICENSES"):
                os.makedirs(os.path.join(d, sub), exist_ok=True)
            with open(os.path.join(d, "images", sid + ".jpg"), "wb") as f:
                f.write(img)
            ref = text.encode("utf-8")
            with open(os.path.join(d, "references", sid + ".txt"), "wb") as f:
                f.write(ref)
            samples.append({
                "id": sid, "image": f"images/{sid}.jpg", "reference": f"references/{sid}.txt",
                "source_page_id": (row.get("page") or row["id"]).replace("/", "_"),
                "provenance": {"for_scoring": True, "source_dataset": src, "page_id": (row.get("page") or row["id"]).replace("/", "_"),
                               "lang": sc["languages"][0] if len(sc["languages"]) == 1 else None, "check_task": row["id"],
                               "crop_id": row.get("crop_id"), "unit": row.get("unit")},
            })
            for s in samples[-1:]:
                s["provenance"] = {k: v for k, v in s["provenance"].items() if v is not None}
            annotations[sid] = {"damage": None, "transcription": {
                "policy": "diplomatic-v1", "reviewed_by": f"check.usnewsmap.com adjudication ({outcome}), readers " + ",".join(readers),
                "evidence": f"{len(readers)} independent blind readings on check.usnewsmap.com ({outcome}); no machine text shown or prefilled",
                "illegible_spans": [{"start": i, "end": i + 1, "reason": "reader marked a letter unreadable"} for i, ch in enumerate(text) if ch == "�"]}}
            truth.append({"id": sid, "task": row["id"], "lang": lang, "method": outcome, "readers": readers, "text_sha256": sha256(text.encode("utf-8")), "image_sha256": sha256(img)})
        if not samples:
            continue
        d = os.path.join(out, src)
        with open(os.path.join(d, "samples.json"), "w", encoding="utf-8") as f:
            json.dump(samples, f, ensure_ascii=False, indent=1, sort_keys=True)
        with open(os.path.join(d, "LICENSES", "NOTICE.txt"), "w", encoding="utf-8") as f:
            f.write(sc["notice"].rstrip() + "\n")
        sources[src] = {"annotation_status": "human-verified", "attribution": sc["attribution"], "image_license": sc["image_license"],
                        "languages": sc["languages"], "reference_license": "CC0-1.0, https://creativecommons.org/publicdomain/zero/1.0/",
                        "scripts": sc["scripts"], "unit": sc.get("unit", "line")}
        imports.append({"path": f"human-truth/{src}", "source": src, "subset": f"{src}/all-{len(samples)}"})
    os.makedirs(out, exist_ok=True)
    with open(os.path.join(out, "truth.jsonl"), "w", encoding="utf-8") as f:
        for t in truth:
            f.write(json.dumps(t, ensure_ascii=False, sort_keys=True) + "\n")
    with open(os.path.join(out, "held.jsonl"), "w", encoding="utf-8") as f:
        for h in held:
            f.write(json.dumps(h, ensure_ascii=False, sort_keys=True) + "\n")
    with open(os.path.join(out, "spec-fragment.json"), "w", encoding="utf-8") as f:
        json.dump({"annotations": annotations, "imports": imports, "sources": sources}, f, ensure_ascii=False, indent=1, sort_keys=True)
    sums = []
    for root, _, files in os.walk(out):
        for n in files:
            if n == "INPUTS.sha256":
                continue
            p = os.path.join(root, n)
            with open(p, "rb") as f:
                sums.append((os.path.relpath(p, out), sha256(f.read())))
    with open(os.path.join(out, "INPUTS.sha256"), "w", encoding="utf-8") as f:
        for rel, h in sorted(sums):
            f.write(f"{h}  {rel}\n")
    return {"promoted": len(truth), "held": len(held), "out": out}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("promote")
    p.add_argument("--export", action="append", required=True, metavar="LANG=FILE", help="e.g. yi-truth=check-yi-truth.jsonl")
    p.add_argument("--release", required=True)
    p.add_argument("--out-root", default="ocr/bench")
    p.add_argument("--sources", required=True, help="JSON {lang: {notice, attribution, image_license, languages, scripts, unit}}")
    p.add_argument("--rights", required=True)
    p.add_argument("--hints")
    p.add_argument("--images-dir", help="a folder of the clip files, keyed as the export's `line`; default: fetch from the site")
    p.add_argument("--clips-base", default="https://check.usnewsmap.com/clips")
    p.add_argument("--releases-json", default="ocr/bench/releases.json")
    a = p.parse_args()
    exports = {}
    for e in a.export:
        lang, path = e.split("=", 1)
        exports[lang] = load_jsonl(path)
    with open(a.sources, encoding="utf-8") as f:
        sc = json.load(f)
    with open(a.rights, encoding="utf-8") as f:
        ranges = [tuple(r) for r in json.load(f)["confirmed"]]
    hints = {}
    if a.hints:
        for h in load_jsonl(a.hints):
            hints.setdefault(h.get("id") or h["task"], set()).add(normalize(h["text"]))
    print(json.dumps(promote(exports, a.release, a.out_root, sc, ranges, hints, a.images_dir, a.clips_base, a.releases_json)), file=sys.stderr)


if __name__ == "__main__":
    main()
