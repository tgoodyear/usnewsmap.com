"""Column gutter judgment tasks for check.usnewsmap.com (#2107): is this drawn line a gap between columns?

Input (--segments) is JSON lines, one candidate gutter segment each, in the page image's pixels:

  {"segment_id": "<id, unique within the page>", "page_id": "<lccn>/<date>/ed-N/seq-N", "image_path": "<page image>",
   "x": 1234.5, "y0": 400, "y1": 2100, "method": "<what drew the line>", "role": "<why it is in the set>",
   "truth": "yes" | "no" (optional), "column_width": 410 (optional), "source": "<page URL>" (optional),
   "x_bottom": 1228.0 (optional)}

`x` is the line's horizontal position and `y0`..`y1` its vertical extent. On a tilted page a straight line along a gutter
leans: then `x` is its position at y0 and `x_bottom` its position at y1 (absent: a vertical line, x_bottom = x). `method` and `role` stay private until the task
settles, so a volunteer never sees which method drew the line or whether it is a calibration item. `truth` is a human's
judgment of the segment (yes: it runs down a gap without crossing print): a calibration item, never loaded with seed answers.
`column_width` is the page's column width in pixels, if known. Paths are relative to --root (default: the folder of --segments).

Each segment becomes one JPEG crop: the segment (both ends of a leaning line) plus a margin on each side of 1.5 column widths (12% of the page's width where
the column width is unknown), and 3% of the page's height above and below, cut inside the page. The crop is cut from the page
image with no line drawn on it; the page draws the line over it from the task's `line` (x, y0, y1 as fractions of the crop,
and x1, x_bottom as a fraction of the crop's width, only for a row with x_bottom).
A crop whose longer side is over --max-side is scaled down to it.

Writes OUT_DIR/clips/<lang>/<set>/crops/<page>_s<segment>.jpg, OUT_DIR/tasks.jsonl and OUT_DIR/language.json (the --language-config
without its build-only fields), the layout load.sh sends. A task is

  {"id": "en-gutter/<set>/<page_id>~<segment_id>", "lang", "set", "kind": "gutter", "crop": "<clip key>",
   "line": {"x", "y0", "y1", "x1"?}, "source": ..., "private": {"role", "segment_id", "page_id", "method", "truth"?}}

Answers are `a` for yes, `b` for no and `unsure` for can't tell (worker.mjs; no schema change). Needs Pillow.

  python3 gutter_tasks.py --segments segments.jsonl --set gutter-v1 --language-config languages/en-gutter.json --out-dir OUT \\
    [--lang en-gutter] [--root DIR] [--max-side 1600] [--quality 85] [--no-clips]
"""
import argparse
import json
import os
import re

HERE = os.path.dirname(os.path.abspath(__file__))
BUILD_ONLY = ("drop_examples", "min_examples")
COLUMN_MARGIN = 1.5  # column widths either side of the line, when the column width is known
PAGE_MARGIN = 0.12  # of the page's width either side, when it is not
V_MARGIN = 0.03  # of the page's height above and below
TRUTHS = ("yes", "no")
KEY_CHARS = re.compile(r"[^A-Za-z0-9._-]+")
LANG_CODE = re.compile(r"^[a-z]{2,3}(-[A-Za-z0-9]{2,8})*$")
# What worker.mjs's PUT /admin/clips accepts (it also refuses any key containing "..").
CLIP_KEY = re.compile(r"^[A-Za-z0-9._\-/]{1,300}$")


def slug(text: str) -> str:
    return KEY_CHARS.sub("_", str(text)).strip("_")


def read_jsonl(path: str) -> list:
    with open(path, encoding="utf-8") as f:
        return [json.loads(line) for line in f if line.strip()]


def crop_box(x: float, y0: float, y1: float, width: int, height: int, column_width=None, x_bottom=None) -> tuple:
    """(left, top, right, bottom) in page pixels: the segment (both ends of a leaning line) plus its margins, cut inside the page."""
    side = COLUMN_MARGIN * column_width if column_width else PAGE_MARGIN * width
    top, bottom = y0 - V_MARGIN * height, y1 + V_MARGIN * height
    lo, hi = (x, x) if x_bottom is None else (min(x, x_bottom), max(x, x_bottom))
    return (max(0, int(lo - side)), max(0, int(top)), min(width, int(hi + side + 1)), min(height, int(bottom + 1)))


def line_fractions(x: float, y0: float, y1: float, box: tuple, x_bottom=None) -> dict:
    """The line as fractions of the crop, which is what the page draws it from; they stay right when the crop is scaled down.
    x1 (the line's x at y1) is there only for a leaning line, a row with x_bottom."""
    left, top, right, bottom = box
    w, h = right - left, bottom - top
    line = {"x": round((x - left) / w, 5), "y0": round((y0 - top) / h, 5), "y1": round((y1 - top) / h, 5)}
    if x_bottom is not None:
        line["x1"] = round((x_bottom - left) / w, 5)
    return line


def validate(row: dict, size: tuple, n: int) -> None:
    """Refuses a row that would give a wrong or a hidden-wrong task: a bad truth, a line off the page, a missing field."""
    where = f"line {n}"
    for k in ("segment_id", "page_id", "image_path", "method", "role"):
        if not isinstance(row.get(k), str) or not row[k].strip():
            raise ValueError(f"{where}: {k} must be a non-empty string")
    for k in ("x", "y0", "y1") + (("x_bottom",) if "x_bottom" in row else ()):
        if isinstance(row.get(k), bool) or not isinstance(row.get(k), (int, float)):
            raise ValueError(f"{where}: {k} must be a number")
    if "truth" in row and row["truth"] not in TRUTHS:
        raise ValueError(f"{where}: truth must be one of {', '.join(TRUTHS)}")
    cw = row.get("column_width")
    if cw is not None and (isinstance(cw, bool) or not isinstance(cw, (int, float)) or cw <= 0):
        raise ValueError(f"{where}: column_width must be a positive number")
    w, h = size
    if not (0 <= row["x"] <= w and 0 <= row["y0"] < row["y1"] <= h):
        raise ValueError(f"{where}: the line (x {row['x']}, y {row['y0']}..{row['y1']}) is not inside the {w}x{h} page")
    if "x_bottom" in row and not 0 <= row["x_bottom"] <= w:
        raise ValueError(f"{where}: the line's bottom end (x_bottom {row['x_bottom']}) is not inside the {w}x{h} page")


def task_id(lang: str, set_name: str, row: dict) -> str:
    return f"{lang}/{set_name}/{row['page_id']}~{row['segment_id']}"


def clip_key(lang: str, set_name: str, row: dict) -> str:
    # `~` is not allowed in a clip key (worker.mjs), so the segment follows `_s`, as en_pick.py's `_w`.
    return f"{lang}/{set_name}/crops/{slug(row['page_id'])}_s{slug(row['segment_id'])}.jpg"


def build_task(row: dict, lang: str, set_name: str, size: tuple) -> tuple:
    """(task body, crop box in page pixels)."""
    box = crop_box(row["x"], row["y0"], row["y1"], size[0], size[1], row.get("column_width"), row.get("x_bottom"))
    private = {"role": row["role"], "segment_id": row["segment_id"], "page_id": row["page_id"], "method": row["method"]}
    if "truth" in row:
        private["truth"] = row["truth"]
    task = {"id": task_id(lang, set_name, row), "lang": lang, "set": set_name, "kind": "gutter",
            "crop": clip_key(lang, set_name, row), "line": line_fractions(row["x"], row["y0"], row["y1"], box, row.get("x_bottom")),
            "source": row.get("source"), "private": private}
    return task, box


def write_crop(page_img, box: tuple, path: str, max_side: int, quality: int) -> None:
    img = page_img.crop(box).convert("L")
    if max(img.size) > max_side:
        scale = max_side / max(img.size)
        img = img.resize((max(1, round(img.size[0] * scale)), max(1, round(img.size[1] * scale))))
    os.makedirs(os.path.dirname(path), exist_ok=True)
    img.save(path, quality=quality)


def main(argv=None) -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--segments", required=True)
    ap.add_argument("--root", help="folder the image paths are relative to (default: the folder of --segments)")
    ap.add_argument("--lang", default="en-gutter")
    ap.add_argument("--set", required=True)
    ap.add_argument("--language-config", required=True)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--max-side", type=int, default=1600, help="longest side of a crop, in pixels")
    ap.add_argument("--quality", type=int, default=85)
    ap.add_argument("--no-clips", action="store_true", help="validate and count only: write no tasks or images")
    args = ap.parse_args(argv)
    if not LANG_CODE.match(args.lang):
        raise SystemExit(f"--lang {args.lang}: not a language code")
    if not re.match(r"^[A-Za-z0-9._-]{1,60}$", args.set):
        raise SystemExit(f"--set {args.set}: letters, digits, . _ - only")
    root = args.root or os.path.dirname(os.path.abspath(args.segments))

    from PIL import Image
    rows = read_jsonl(args.segments)
    sizes, tasks, seen = {}, [], set()
    for n, row in enumerate(rows, 1):
        path = os.path.join(root, row.get("image_path") or "")
        if path not in sizes:
            with Image.open(path) as im:
                sizes[path] = im.size
        validate(row, sizes[path], n)
        task, box = build_task(row, args.lang, args.set, sizes[path])
        if len(task["id"]) > 300 or not re.match(r"^[A-Za-z0-9._~/-]+$", task["id"]):
            raise SystemExit(f"line {n}: task id {task['id']!r} has characters or a length the site refuses; rename the page or segment")
        if not CLIP_KEY.match(task["crop"]) or ".." in task["crop"]:
            raise SystemExit(f"line {n}: crop key {task['crop']!r} would be refused by /admin/clips (worker.mjs); shorten the set, page or segment name")
        for k in (task["id"], task["crop"]):
            if k in seen:
                raise SystemExit(f"line {n}: {k} is used twice; page_id and segment_id must make each segment unique")
            seen.add(k)
        tasks.append((task, box, path))
    report = {"segments": len(tasks), "pages": len(sizes),
              "with_truth": sum(1 for t, _, _ in tasks if "truth" in t["private"]),
              "by_method": {}, "by_role": {}}
    for t, _, _ in tasks:
        for k, v in (("by_method", t["private"]["method"]), ("by_role", t["private"]["role"])):
            report[k][v] = report[k].get(v, 0) + 1
    if not args.no_clips:
        os.makedirs(args.out_dir, exist_ok=True)
        pages = {}
        with open(os.path.join(args.out_dir, "tasks.jsonl"), "w", encoding="utf-8") as f:
            for task, box, path in sorted(tasks, key=lambda t: (t[2], t[0]["id"])):
                if path not in pages:
                    pages.clear()  # one page at a time: the rows come sorted by page
                    pages[path] = Image.open(path)
                write_crop(pages[path], box, os.path.join(args.out_dir, "clips", task["crop"]), args.max_side, args.quality)
                f.write(json.dumps(task, ensure_ascii=False) + "\n")
        with open(args.language_config, encoding="utf-8") as f:
            cfg = json.load(f)
        language = {k: v for k, v in cfg.items() if k not in BUILD_ONLY}
        language["examples"] = {}
        with open(os.path.join(args.out_dir, "language.json"), "w", encoding="utf-8") as f:
            json.dump(language, f, ensure_ascii=False, indent=1)
        with open(os.path.join(args.out_dir, "report.json"), "w", encoding="utf-8") as f:
            json.dump(report, f, indent=1)
    print(json.dumps(report, indent=1))


if __name__ == "__main__":
    main()
