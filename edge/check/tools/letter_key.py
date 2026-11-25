"""Letter keys for check.usnewsmap.com pick-between-readings tasks (edge/check/README.md, "Letter key").

A letter key is the language's glyph set as it is printed in the task's own paper: for each letter, up to --per-glyph
small pictures of that letter cut from print both readers agree on, so a judge can compare the disputed letters with
known-good letters in the same type.

Sources, in order, recorded per glyph as its `level`:
  page   the task's own page;
  issue  another page of the same issue (same LCCN, date and edition);
  title  another page of the same title (same LCCN);
  font   none of those: the letter drawn in the language's period typeface (`glyph_font` in languages/<lang>.json, a
         font the site bundles); a language without one leaves the glyph out (`none` in the report).
A glyph takes all its pictures from the first level that has any. Only pages in --pages are sources.

An instance (one printed letter) qualifies only when
  * its word is a word both readers read the same: a Tesseract word (hOCR) paired with an ALTO word by mutual best box
    overlap (IoU >= --min-iou), whose readings are equal once normalised (NFKC, the language's `equivalents` applied,
    punctuation removed; case is kept, so a key never shows a capital under a small letter's label);
  * Tesseract's confidence in the letter (hOCR x_conf of its ocrx_cinfo) is at least --min-conf;
  * it is not on a line that holds a task's word: for every task on the page, the ALTO line of its word (`loc_word` in
    its private part; a task without one, or one outside the page's ALTO, stops the build) and every Tesseract line that overlaps that line's box are left out whole, so no key shows the
    disputed word or its neighbours. A page's key serves all its tasks, so all their lines are left out;
  * its line is set in the page's body type: the line's type height (below) within --min-line-height..--max-line-height
    times the page's median (no headlines, no lines that ran into each other);
  * it is not part of a ligature (`glyph_ligatures` in the language config: Fraktur ch, ck, tz, ſt), which the reader
    splits into letters at a guess;
  * its label is in the language's `glyphs` list (else `labels`, else `examples`).
Among the qualifying instances of a glyph at a level, the first --per-glyph by sha256(seed:glyph:page:word:char) are taken.

Each picture spans the line's type, from the 5th percentile of its letters' tops to the 95th of their bottoms (so capitals,
x-height and descenders keep their proportions, and a neighbouring line's descenders stay out), and the letter's box plus
a small margin, contrast stretched, scaled to --height pixels and packed into one greyscale sprite per
task page.

Actions (named flags only):

  --action hocr   run Tesseract with char boxes on every page of --pages that has no hOCR yet:
                  tesseract IMAGE OUT -l MODEL --psm 3 --dpi 300 [--tessdata-dir DIR] -c hocr_char_boxes=1 hocr
                  (OMP_THREAD_LIMIT=1, --jobs at a time; a JPEG 2000 that Tesseract refuses is read through Pillow as PNG).
  --action build  read the build dir's tasks.jsonl and write OUT_DIR: the build dir copied, plus per task page
                  clips/<lang>/<set>/<key-name>/<page_id>.jpg (the sprite) and .json (the map), each task body with
                  `key` (the map's clip key) put before `private`, and letter-key-report.json (glyph counts by level).

The map: {"v": 1, "sprite": KEY, "w": W, "h": H, "font": FAMILY or null,
          "glyphs": [{"g": "a", "level": "page", "crops": [[x, y, w], ...]}, {"g": "ß", "level": "font"}, ...]},
crops [x, y, w] in sprite pixels, each H tall. Clip keys are new (--key-name); clips are cached as immutable, so a
changed key must get a new --key-name, never overwrite an old one.

Pages (--pages, JSON lines, as english_pages.py writes them): {"page": "<lccn>/<date>/ed-N/seq-N", "page_id", "alto",
"image"}, paths relative to --root (default: the folder of --pages). hOCR files are --hocr-dir/<page_id>.hocr.

  python3 letter_key.py --action hocr --pages P.jsonl --hocr-dir H --model eng --jobs 16
  python3 letter_key.py --action build --pages P.jsonl --hocr-dir H --build-dir SET --out-dir SET-key \
      --language-config languages/en.json
"""
import argparse
import hashlib
import html.parser
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unicodedata
from collections import Counter
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)  # `python3 -I` leaves the script directory off the path
import en_pick  # noqa: E402

LEVELS = ("page", "issue", "title", "font")
KEY_NAME = re.compile(r"^[A-Za-z0-9._-]{1,40}$")


# ---------------------------------------------------------------- reading the inputs


class _Hocr(html.parser.HTMLParser):
    """Tesseract hOCR with hocr_char_boxes: lines, words and chars with boxes and confidences."""

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.lines, self.stack, self.char = [], [], None

    @staticmethod
    def _title(attrs):
        t = dict(attrs).get("title") or ""
        out = {}
        for part in t.split(";"):
            bits = part.split()
            if bits:
                out[bits[0]] = bits[1:]
        return out

    def handle_starttag(self, tag, attrs):
        cls = (dict(attrs).get("class") or "").split()
        kind = None
        if {"ocr_line", "ocr_header", "ocr_caption", "ocr_textfloat"} & set(cls):
            kind = "line"
            t = self._title(attrs)
            x0, y0, x1, y1 = (int(v) for v in t["bbox"][:4])
            self.lines.append({"box": [x0, y0, x1, y1], "words": []})
        elif "ocrx_word" in cls and self.lines:
            kind = "word"
            t = self._title(attrs)
            x0, y0, x1, y1 = (int(v) for v in t["bbox"][:4])
            self.lines[-1]["words"].append({"box": [x0, y0, x1 - x0, y1 - y0], "chars": [], "text": ""})
        elif "ocrx_cinfo" in cls and self.lines and self.lines[-1]["words"]:
            kind = "char"
            t = self._title(attrs)
            if "x_bboxes" in t:
                x0, y0, x1, y1 = (int(v) for v in t["x_bboxes"][:4])
                conf = float(t.get("x_conf", ["0"])[0])
                self.char = {"text": "", "box": [x0, y0, x1, y1], "conf": conf}
        self.stack.append(kind)

    def handle_endtag(self, tag):
        kind = self.stack.pop() if self.stack else None
        if kind == "char" and self.char is not None:
            self.lines[-1]["words"][-1]["chars"].append(self.char)
            self.char = None

    def handle_data(self, data):
        if self.char is not None:
            self.char["text"] += data


def parse_hocr(text: str) -> list:
    """Tesseract's words: {text, box: [x, y, w, h], line: index, line_box: [x0, y0, x1, y1], chars: [{text, box, conf}]}."""
    p = _Hocr()
    p.feed(text)
    words = []
    for k, ln in enumerate(p.lines):
        band = _band([c for w in ln["words"] for c in w["chars"] if c["text"].strip()]) or [ln["box"][1], ln["box"][3]]
        for w in ln["words"]:
            chars = [c for c in w["chars"] if c["text"].strip()]
            w = dict(w, chars=chars, text="".join(c["text"] for c in chars), line=k, line_box=ln["box"], band=band)
            if w["text"]:
                words.append(w)
    return words


def _band(chars: list):
    """A line's type as its letters set it: [top, bottom] from the 5th percentile of the letters' tops to the 95th of
    their bottoms. Tesseract's line box can take in a stray mark or a neighbouring line's descenders; this does not."""
    if not chars:
        return None
    tops = sorted(c["box"][1] for c in chars)
    bots = sorted(c["box"][3] for c in chars)
    return [tops[int(0.05 * (len(tops) - 1))], bots[int(round(0.95 * (len(bots) - 1)))]]


# ---------------------------------------------------------------- agreement


def normalise(text: str, equivalents: dict) -> str:
    t = unicodedata.normalize("NFKC", text)
    for k, v in equivalents.items():
        t = t.replace(k, v)
    return "".join(ch for ch in t if not unicodedata.category(ch).startswith(("P", "Z", "S")))


def _area(b):
    return max(0.0, b[2]) * max(0.0, b[3])


def _inter(a, b):
    w = min(a[0] + a[2], b[0] + b[2]) - max(a[0], b[0])
    h = min(a[1] + a[3], b[1] + b[3]) - max(a[1], b[1])
    return w * h if w > 0 and h > 0 else 0.0


def _iou(a, b):
    i = _inter(a, b)
    return i / (_area(a) + _area(b) - i) if i else 0.0


def _grid(boxes, step=64):
    g = {}
    for k, b in enumerate(boxes):
        for gx in range(int(b[0]) // step, int(b[0] + b[2]) // step + 1):
            for gy in range(int(b[1]) // step, int(b[1] + b[3]) // step + 1):
                g.setdefault((gx, gy), []).append(k)
    return g, step


def _near(index, b):
    g, step = index
    out = set()
    for gx in range(int(b[0]) // step, int(b[0] + b[2]) // step + 1):
        for gy in range(int(b[1]) // step, int(b[1] + b[3]) // step + 1):
            out.update(g.get((gx, gy), ()))
    return out


def pair_words(loc: list, tess: list, min_iou: float) -> list:
    """(i, j) for LoC word i and Tesseract word j that are each other's best overlap, at --min-iou or more."""
    li, ti = _grid([w["box"] for w in loc]), _grid([w["box"] for w in tess])

    def best(src, idx, dst, k):
        top, arg = 0.0, None
        for o in sorted(_near(idx, src[k]["box"])):
            v = _iou(src[k]["box"], dst[o]["box"])
            if v > top:
                top, arg = v, o
        return arg, top

    out = []
    for i in range(len(loc)):
        j, v = best(loc, ti, tess, i)
        if j is not None and v >= min_iou and best(tess, li, loc, j)[0] == i:
            out.append((i, j))
    return out


def excluded_lines(loc: list, tess: list, task_words: list) -> tuple:
    """The ALTO lines and Tesseract lines a page's tasks sit on: {ALTO line index}, {Tesseract line index}."""
    alto, boxes = set(), []
    for i in task_words:
        if isinstance(i, int) and 0 <= i < len(loc):
            alto.add(loc[i]["line"])
            x0, y0, x1, y1 = loc[i]["line_box"]
            boxes.append([x0, y0, x1 - x0, y1 - y0])
            boxes.append(loc[i]["box"])
    tl = set()
    for w in tess:
        x0, y0, x1, y1 = w["line_box"]
        lb = [x0, y0, x1 - x0, y1 - y0]
        if any(_inter(lb, b) > 0 for b in boxes):
            tl.add(w["line"])
    return alto, tl


def body_lines(tess: list, band: tuple) -> set:
    """The Tesseract lines set in the page's body type: line height within `band` times the median line height.
    Headlines, display type and lines that ran into each other are left out."""
    heights = {}
    for w in tess:
        heights.setdefault(w["line"], w["band"][1] - w["band"][0])
    if not heights:
        return set()
    hs = sorted(heights.values())
    med = hs[len(hs) // 2]
    return {k for k, h in heights.items() if band[0] * med <= h <= band[1] * med}


def tied_letters(chars: list, ligatures, equivalents=None) -> set:
    """Indexes of letters cast as one piece with a neighbour (a ligature: Fraktur ch, ck, tz, ſt; English fi, fl): the
    reader splits such a piece into letters at a guess, so neither half's box is the letter alone. Runs are matched with
    the language's one-letter `equivalents` applied to both sides, as agreement is, so a reader's "st" for a printed ſt
    is caught too."""
    eq = {k: v for k, v in (equivalents or {}).items() if len(k) == 1 and len(v) == 1}
    text = [eq.get(t, t) for t in (unicodedata.normalize("NFC", c["text"]) for c in chars)]
    out = set()
    for lig in ligatures or ():
        run = [eq.get(ch, ch) for ch in lig]
        n = len(run)
        for k in range(len(text) - n + 1):
            if text[k:k + n] == run:
                out.update(range(k, k + n))
    return out


def instances(page_id: str, loc: list, tess: list, task_words: list, glyphs: set, equivalents: dict,
              min_iou: float, min_conf: float, band: tuple = (0.75, 1.35), ligatures=()) -> list:
    """The page's qualifying printed letters: {g, page, word, char, box, line_box, conf}."""
    alto_x, tess_x = excluded_lines(loc, tess, task_words)
    body = body_lines(tess, band)
    out = []
    for i, j in pair_words(loc, tess, min_iou):
        lw, tw = loc[i], tess[j]
        if lw["line"] in alto_x or tw["line"] in tess_x or tw["line"] not in body:
            continue
        a, b = normalise(lw["text"], equivalents), normalise(tw["text"], equivalents)
        if not a or a != b:
            continue
        tied = tied_letters(tw["chars"], ligatures, equivalents)
        for c, ch in enumerate(tw["chars"]):
            if c in tied:
                continue
            g = unicodedata.normalize("NFC", ch["text"])
            x0, y0, x1, y1 = ch["box"]
            if g not in glyphs or ch["conf"] < min_conf or x1 - x0 < 2 or y1 - y0 < 3:
                continue
            out.append({"g": g, "page": page_id, "word": j, "char": c, "box": ch["box"], "band": tw["band"],
                        "conf": ch["conf"]})
    return out


def rank(seed: str, inst: dict) -> str:
    return hashlib.sha256(f"{seed}:{inst['g']}:{inst['page']}:{inst['word']}:{inst['char']}".encode()).hexdigest()


def issue_of(page: str) -> str:
    """'<lccn>/<date>/ed-N/seq-N' -> '<lccn>/<date>/ed-N'."""
    return page.rsplit("/", 1)[0]


def title_of(page: str) -> str:
    return page.split("/", 1)[0]


def choose(page: str, pool: dict, pages: dict, glyphs: list, per_glyph: int, seed: str, font) -> list:
    """For the task page `page`: [{g, level, picks: [instance]}] in `glyphs` order. `pool` maps a page to its
    instances by glyph; `pages` maps a page to its record (only pages in it are sources)."""
    same_issue = [p for p in pages if p != page and issue_of(p) == issue_of(page)]
    same_title = [p for p in pages if p != page and issue_of(p) != issue_of(page) and title_of(p) == title_of(page)]
    out = []
    for g in glyphs:
        entry = None
        for level, srcs in (("page", [page] if page in pages else []), ("issue", same_issue), ("title", same_title)):
            cand = [x for p in srcs for x in pool.get(p, {}).get(g, [])]
            if cand:
                entry = {"g": g, "level": level, "picks": sorted(cand, key=lambda x: rank(seed, x))[:per_glyph]}
                break
        if entry is None:
            entry = {"g": g, "level": "font" if font else "none", "picks": []}
        out.append(entry)
    return out


# ---------------------------------------------------------------- pictures


def crop_box(inst: dict, img_size: tuple, margin: float) -> tuple:
    x0, y0, x1, y1 = inst["box"]
    top, bot = inst["band"]
    pad = max(1, round(margin * (bot - top)))
    top, bot = min(top, y0) - pad, max(bot, y1) + pad
    return (max(0, x0 - pad), max(0, top), min(img_size[0], x1 + pad), min(img_size[1], bot))


def cut_tiles(picks: list, image, height: int, margin: float) -> dict:
    """The pictures of one source page's picks: {(page, word, char): greyscale Image, `height` tall, contrast stretched}."""
    from PIL import Image, ImageOps
    out = {}
    for x in picks:
        box = crop_box(x, image.size, margin)
        if box[2] - box[0] < 1 or box[3] - box[1] < 1:
            continue
        t = ImageOps.autocontrast(image.crop(box).convert("L"), cutoff=1)
        out[(x["page"], x["word"], x["char"])] = t.resize((max(1, round(t.width * height / t.height)), height), Image.LANCZOS)
    return out


def build_sprite(entries: list, tiles: dict, height: int, max_width: int):
    """Pack every pick that has a tile into one greyscale image. Returns (Image, {(page, word, char): [x, y, w]})."""
    from PIL import Image
    order = [(k, tiles[k]) for e in entries for k in ((x["page"], x["word"], x["char"]) for x in e["picks"]) if k in tiles]
    rows, row, cur = [], [], 0
    for key, t in order:
        if row and cur + t.width > max_width:
            rows.append(row)
            row, cur = [], 0
        row.append((key, t))
        cur += t.width + 2
    if row:
        rows.append(row)
    width = max([sum(t.width + 2 for _, t in r) for r in rows] or [1])
    sprite = Image.new("L", (width, max(1, len(rows)) * (height + 2)), 255)
    where = {}
    for r, rw in enumerate(rows):
        x = 0
        for key, t in rw:
            sprite.paste(t, (x, r * (height + 2)))
            where[key] = [x, r * (height + 2), t.width]
            x += t.width + 2
    return sprite, where


# ---------------------------------------------------------------- the actions


def read_jsonl(path):
    with open(path, encoding="utf-8") as f:
        return [json.loads(ln) for ln in f if ln.strip()]


def glyph_list(cfg: dict) -> list:
    g = cfg.get("glyphs") or list((cfg.get("labels") or {}).keys()) or list((cfg.get("examples") or {}).keys())
    if not g:
        raise SystemExit("the language config has no glyphs, labels or examples")
    return [unicodedata.normalize("NFC", x) for x in g]


def _path(root, p):
    return p if os.path.isabs(p) else os.path.join(root, p)


def run_hocr(args) -> None:
    pages = read_jsonl(args.pages)
    root = args.root or os.path.dirname(os.path.abspath(args.pages))
    os.makedirs(args.hocr_dir, exist_ok=True)
    env = dict(os.environ, OMP_THREAD_LIMIT="1")
    extra = ["-l", args.model, "--psm", "3", "--dpi", "300"] + (["--tessdata-dir", args.tessdata_dir] if args.tessdata_dir else [])

    def one(p):
        out = os.path.join(args.hocr_dir, p["page_id"])
        if os.path.exists(out + ".hocr"):
            return p["page_id"], "kept"
        image = _path(root, p["image"])
        cmd = lambda src: subprocess.run(["tesseract", src, out] + extra + ["-c", "hocr_char_boxes=1", "hocr"], env=env,
                                         stdin=subprocess.DEVNULL, capture_output=True).returncode
        rc = cmd(image)
        if rc != 0 and image.lower().endswith((".jp2", ".j2k")):
            from PIL import Image
            with tempfile.TemporaryDirectory() as td:
                png = os.path.join(td, "page.png")
                Image.open(image).save(png)
                rc = cmd(png)
        return p["page_id"], "ok" if rc == 0 and os.path.exists(out + ".hocr") else f"failed rc={rc}"

    with ThreadPoolExecutor(args.jobs) as ex:
        for n, (pid, how) in enumerate(ex.map(one, pages), 1):
            print(f"hocr {n}/{len(pages)} {pid} {how}", flush=True)


def build(args) -> None:
    from PIL import Image
    Image.MAX_IMAGE_PIXELS = None  # whole newspaper scans, from held files
    if not KEY_NAME.match(args.key_name) or ".." in args.key_name:  # the Worker refuses clip keys with ".."
        raise SystemExit("--key-name: letters, digits, . _ - only")
    with open(args.language_config, encoding="utf-8") as f:
        cfg = json.load(f)
    glyphs = glyph_list(cfg)
    equivalents = cfg.get("equivalents") or {}
    font = cfg.get("glyph_font")
    tasks = read_jsonl(os.path.join(args.build_dir, "tasks.jsonl"))
    if os.path.abspath(args.out_dir) == os.path.abspath(args.build_dir):
        raise SystemExit("--out-dir must be a new folder next to the build dir")
    if os.path.exists(args.out_dir):
        raise SystemExit(f"{args.out_dir} exists")
    pages = {}
    for f in args.pages:
        root = args.root or os.path.dirname(os.path.abspath(f))
        for p in read_jsonl(f):
            pages.setdefault(p["page"], dict(p, _root=root))
    task_words = {}
    for t in tasks:
        i = (t.get("private") or {}).get("loc_word")
        if not isinstance(i, int):
            # Without the ALTO word the task's line can't be left out of its key: refuse rather than risk showing it.
            raise SystemExit(f"{t['id']}: no private.loc_word, so its line can't be left out of the key")
        task_words.setdefault(t["page"], []).append(i)
    stats = Counter()
    pool = {}
    gset = set(glyphs)
    needed = {t["page"] for t in tasks}
    # Only the pages that can serve some task: the task pages and pages of the same titles.
    titles = {title_of(p) for p in needed}
    for page, p in sorted(pages.items()):
        if title_of(page) not in titles:
            continue
        hocr = os.path.join(args.hocr_dir, p["page_id"] + ".hocr")
        if not os.path.exists(hocr):
            stats["source_pages_without_hocr"] += 1
            continue
        with Image.open(_path(p["_root"], p["image"])) as img:
            size = img.size
        with open(_path(p["_root"], p["alto"]), "rb") as f:
            loc = en_pick.parse_alto(f.read(), size)
        with open(hocr, encoding="utf-8") as f:
            tess = parse_hocr(f.read())
        bad = [i for i in task_words.get(page, []) if not 0 <= i < len(loc)]
        if bad:
            raise SystemExit(f"{page}: task words {bad} are not in its ALTO ({len(loc)} words); is it the ALTO the set was built from?")
        found = instances(page, loc, tess, task_words.get(page, []), gset, equivalents, args.min_iou, args.min_conf,
                          (args.min_line_height, args.max_line_height), cfg.get("glyph_ligatures") or ())
        by = {}
        for x in found:
            by.setdefault(x["g"], []).append(x)
        pool[page] = by
        stats["source_pages"] += 1
        stats["instances"] += len(found)
    shutil.copytree(args.build_dir, args.out_dir, ignore=shutil.ignore_patterns("*.private"))
    keys, levels_keys, levels_tasks = {}, Counter(), Counter()
    lang, set_ = tasks[0]["lang"], tasks[0]["set"]
    kdir = os.path.join(args.out_dir, "clips", lang, set_, args.key_name)
    os.makedirs(kdir, exist_ok=True)
    chosen = {page: choose(page, pool, pages, glyphs, args.per_glyph, args.seed, font) for page in sorted(needed)}
    picks_by_source = {}
    for entries in chosen.values():
        for e in entries:
            for x in e["picks"]:
                picks_by_source.setdefault(x["page"], {})[(x["word"], x["char"])] = x
    tiles = {}
    for src, picks in sorted(picks_by_source.items()):  # each source scan is opened once
        p = pages[src]
        with Image.open(_path(p["_root"], p["image"])) as img:
            img.load()
            tiles.update(cut_tiles(list(picks.values()), img, args.height, args.margin))
    for page, entries in chosen.items():
        pid = page.replace("/", "_")
        sprite, where = build_sprite(entries, tiles, args.height, args.max_width)
        base = f"{lang}/{set_}/{args.key_name}/{pid}"
        out = {"v": 1, "sprite": base + ".jpg", "w": sprite.width, "h": args.height, "font": font, "glyphs": []}
        for e in entries:
            if e["level"] == "none":
                levels_keys["none"] += 1
                continue
            crops = [where[(x["page"], x["word"], x["char"])] for x in e["picks"] if (x["page"], x["word"], x["char"]) in where]
            level = e["level"] if crops or e["level"] == "font" else ("font" if font else "none")
            if level == "none":
                levels_keys["none"] += 1
                continue
            item = {"g": e["g"], "level": level}
            if level != "font":
                item["crops"] = crops
            out["glyphs"].append(item)
            levels_keys[level] += 1
        sprite.save(os.path.join(kdir, pid + ".jpg"), quality=args.quality, optimize=True)
        with open(os.path.join(kdir, pid + ".json"), "w", encoding="utf-8") as f:
            json.dump(out, f, ensure_ascii=False, separators=(",", ":"))
        keys[page] = (base + ".json", Counter(g["level"] for g in out["glyphs"]) + Counter(none=len(entries) - len(out["glyphs"])))
    with open(os.path.join(args.out_dir, "tasks.jsonl"), "w", encoding="utf-8") as f:
        for t in tasks:
            key, lv = keys[t["page"]]
            levels_tasks.update(lv)
            body = {}
            for k, v in t.items():
                if k == "key":
                    continue  # a key from an earlier build is replaced, never kept
                if k == "private":
                    body["key"] = key
                body[k] = v
            body["key"] = key
            f.write(json.dumps(body, ensure_ascii=False) + "\n")
    report = {"tasks": len(tasks), "task_pages": len(needed), "glyphs": len(glyphs), "key_name": args.key_name,
              "min_conf": args.min_conf, "min_iou": args.min_iou, "per_glyph": args.per_glyph, "seed": args.seed,
              "font": font, **dict(sorted(stats.items())),
              "glyph_levels_per_key": {k: levels_keys.get(k, 0) for k in LEVELS + ("none",)},
              "glyph_levels_per_task": {k: levels_tasks.get(k, 0) for k in LEVELS + ("none",)}}
    with open(os.path.join(args.out_dir, "letter-key-report.json"), "w") as f:
        json.dump(report, f, indent=1)
    print(json.dumps(report, indent=1))


def main(argv=None) -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--action", choices=["hocr", "build"], default="build")
    ap.add_argument("--pages", action="append", required=True, help="pages JSON lines (repeat for several)")
    ap.add_argument("--root", help="folder the pages' paths are relative to (default: the folder of each --pages)")
    ap.add_argument("--hocr-dir", required=True)
    ap.add_argument("--model", default="eng", help="hocr: the Tesseract model (-l)")
    ap.add_argument("--tessdata-dir")
    ap.add_argument("--jobs", type=int, default=8)
    ap.add_argument("--build-dir", help="build: the set's build dir (tasks.jsonl, language.json, clips/)")
    ap.add_argument("--out-dir", help="build: a new folder for the build dir with keys")
    ap.add_argument("--language-config", help="build: languages/<lang>.json, for glyphs, equivalents and glyph_font")
    ap.add_argument("--key-name", default="key-v1", help="the clip folder of the keys; new keys need a new name")
    ap.add_argument("--seed", default="letter-key")
    ap.add_argument("--per-glyph", type=int, default=3)
    ap.add_argument("--min-conf", type=float, default=90.0)
    ap.add_argument("--min-iou", type=float, default=0.5)
    ap.add_argument("--min-line-height", type=float, default=0.75, help="body type: a line at least this times the median height")
    ap.add_argument("--max-line-height", type=float, default=1.35, help="and at most this times it")
    ap.add_argument("--height", type=int, default=48)
    ap.add_argument("--max-width", type=int, default=1200)
    ap.add_argument("--margin", type=float, default=0.06)
    ap.add_argument("--quality", type=int, default=82)
    args = ap.parse_args(argv)
    if args.action == "hocr":
        if len(args.pages) != 1:
            ap.error("hocr takes one --pages")
        args.pages = args.pages[0]
        run_hocr(args)
    else:
        for k in ("build_dir", "out_dir", "language_config"):
            if not getattr(args, k):
                ap.error(f"build needs --{k.replace('_', '-')}")
        build(args)


if __name__ == "__main__":
    main()
