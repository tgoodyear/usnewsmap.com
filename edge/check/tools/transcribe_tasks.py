#!/usr/bin/env python3
"""Makes transcription tasks for check.usnewsmap.com from line or crop images (transcribe.mjs).

Input, JSON lines (--items): one printed unit each:
  {"id": "...", "image": "relative/path.png", "page": "...", "source": "https://...", "label": "...",
   "unit": "line" | "crop", "crop_id": "cze-01", "priority": 0}   (crop_id, unit and priority are optional;
   tasks with a lower priority number are served first)
The images are read from --images. Nothing about any engine's reading may be in the file: the tool refuses
`a`, `b`, `hints`, `text` and `loc` fields, so no machine text can reach a task.

--calibration FILE: JSON lines {"id", "image", "truth", "label"?}: lines with a known text from a human source
(never an engine). The truth goes into the task row for the loader to keep server-side; it never reaches the page.

Writes OUT_DIR/clips/<lang>/<set>/<id>.<ext>, OUT_DIR/tasks.jsonl and OUT_DIR/language.json (the language
configuration with kind "transcribe"). Load them with load.sh as for any language. Standard library only.
"""
import argparse
import hashlib
import json
import os
import re
import shutil
import sys

FORBIDDEN = ("a", "b", "hints", "text", "loc", "a_words", "reading")


def load_jsonl(path):
    with open(path, encoding="utf-8") as f:
        return [json.loads(line) for line in f if line.strip()]


def safe(s):
    return re.sub(r"[^A-Za-z0-9._-]", "_", s)


def build(items, calibration, images, lang, set_name, config, out_dir):
    seen = set()
    tasks = []
    clips = os.path.join(out_dir, "clips", lang, set_name)
    os.makedirs(clips, exist_ok=True)
    for kind, rows in (("task", items), ("calibration", calibration)):
        for row in rows:
            bad = [k for k in FORBIDDEN if k in row]
            if bad and kind == "task":
                raise SystemExit(f"{row.get('id')}: machine-reading field(s) {bad} not allowed in a transcription task")
            if kind == "calibration" and not isinstance(row.get("truth"), str):
                raise SystemExit(f"{row.get('id')}: calibration rows need a `truth` text")
            tid = f"{lang}/{set_name}/{safe(row['id'])}"
            if tid in seen:
                raise SystemExit(f"duplicate id {tid}")
            seen.add(tid)
            src = os.path.join(images, row["image"])
            ext = os.path.splitext(src)[1].lower()
            if ext not in (".png", ".jpg", ".jpeg", ".webp"):
                raise SystemExit(f"{src}: not an image")
            if not os.path.isfile(src):
                raise SystemExit(f"{src}: missing")
            name = safe(row["id"]) + ext
            shutil.copyfile(src, os.path.join(clips, name))
            task = {"id": tid, "lang": lang, "set": set_name, "kind": "transcribe", "line": f"{lang}/{set_name}/{name}",
                    "label": row.get("label", ""), "unit": row.get("unit", "line")}
            if kind == "task":
                for k in ("page", "source", "crop_id", "priority"):
                    if row.get(k) is not None and row.get(k) != "":
                        task[k] = row[k]
            else:
                task["truth"] = row["truth"]
            tasks.append(task)
    with open(os.path.join(out_dir, "tasks.jsonl"), "w", encoding="utf-8") as f:
        for t in tasks:
            f.write(json.dumps(t, ensure_ascii=False) + "\n")
    cfg = {k: v for k, v in config.items() if k not in ("drop_examples", "min_examples")}
    cfg["kind"] = "transcribe"
    with open(os.path.join(out_dir, "language.json"), "w", encoding="utf-8") as f:
        json.dump(cfg, f, ensure_ascii=False, indent=2, sort_keys=True)
        f.write("\n")
    return len([t for t in tasks if "truth" not in t]), len(tasks) - len([t for t in tasks if "truth" not in t])


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--items", required=True)
    ap.add_argument("--calibration")
    ap.add_argument("--images", required=True)
    ap.add_argument("--lang", required=True)
    ap.add_argument("--set", required=True, dest="set_name")
    ap.add_argument("--language-config", required=True)
    ap.add_argument("--out-dir", required=True)
    a = ap.parse_args()
    with open(a.language_config, encoding="utf-8") as f:
        config = json.load(f)
    n, c = build(load_jsonl(a.items), load_jsonl(a.calibration) if a.calibration else [], a.images, a.lang, a.set_name, config, a.out_dir)
    print(f"{n} tasks, {c} calibration tasks -> {a.out_dir}", file=sys.stderr)


if __name__ == "__main__":
    main()
