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
  * it is not part of a ligature (`glyph_ligatures` in the language config: Fraktur ch, ck, tz, ſt; a one-letter entry
    such as ß is a sort never cut out), which the reader splits into letters at a guess;
  * its label is in the language's `glyphs` list (else `labels`, else `examples`).
Among the qualifying instances of a glyph at a level, the first --per-glyph by sha256(seed:glyph:page:word:char) are taken.

Then the quality checks (off with --no-quality-checks). Up to 20 candidates per glyph from the page and 10 from each
other page of the issue and each other issue of the title (sha256 order) are pooled. Each is ink-snapped (ink_snap: its
LSTM box refitted to the connected components of the word's ink, dilated a pixel vertically so broken strokes join; dropped when
cut at the edge, joined to a neighbour, crowded by foreign ink, too small or too dark), then dropped when its size is
outside --min-size..--max-size times the pool's median, then when its normalised shape (binarised, centred, 20 x 20) is
further from the pool's medoid than median + --mad-k x MAD. The glyph takes up to --per-glyph of the survivors, closest
to the medoid first, from the first level that has any; with none it is drawn in the typeface. The report counts each
check's drops (ink_snap) and the share of glyphs drawn in the typeface (font_share_per_key).

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
CAPS = (20, 10, 10)  # candidates per glyph checked from the page, each other page of the issue, each other issue
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
                        "wbox": [tw["box"][0], tw["box"][1], tw["box"][0] + tw["box"][2], tw["box"][1] + tw["box"][3]],
                        "conf": ch["conf"]})
    return out


def rank(seed: str, inst: dict) -> str:
    return hashlib.sha256(f"{seed}:{inst['g']}:{inst['page']}:{inst['word']}:{inst['char']}".encode()).hexdigest()


def issue_of(page: str) -> str:
    """'<lccn>/<date>/ed-N/seq-N' -> '<lccn>/<date>/ed-N'."""
    return page.rsplit("/", 1)[0]


def title_of(page: str) -> str:
    return page.split("/", 1)[0]


def levels_for(page: str, pool: dict, pages: dict, g: str, seed: str, caps: tuple) -> list:
    """[(level, candidates)] for glyph g of the task page `page`: its own page, the other pages of its issue, the other
    issues of its title, each in sha256 order and capped (caps: page, issue, title)."""
    same_issue = [p for p in pages if p != page and issue_of(p) == issue_of(page)]
    same_title = [p for p in pages if p != page and issue_of(p) != issue_of(page) and title_of(p) == title_of(page)]
    out = []
    for (level, srcs), cap in zip((("page", [page] if page in pages else []), ("issue", same_issue), ("title", same_title)), caps):
        cand = sorted((x for p in srcs for x in pool.get(p, {}).get(g, [])), key=lambda x: rank(seed, x))[:cap]
        out.append((level, cand))
    return out


def ikey(x: dict) -> tuple:
    return (x["page"], x["word"], x["char"])


def choose(page: str, pool: dict, pages: dict, glyphs: list, per_glyph: int, seed: str, font, snaps=None,
           caps=(20, 10, 10), size_band=(0.6, 1.6), mad_k=1.5) -> list:
    """For the task page `page`: [{g, level, picks: [instance]}] in `glyphs` order. `pool` maps a page to its
    instances by glyph; `pages` maps a page to its record (only pages in it are sources).

    Without `snaps`, a glyph takes its first --per-glyph candidates (sha256 order) from the first level that has any.
    With `snaps` (key -> snap(), see ink_snap), the candidates of all three levels are pooled: those whose ink snap
    failed are dropped, then those whose size (against the line's type height) is outside `size_band` times the
    pool's median, then those whose shape is further from the pool's medoid than median + mad_k x MAD (shape_filter).
    The glyph then takes up to --per-glyph survivors, closest to the medoid first, from the first level that has any;
    none at all, and it is drawn in the typeface."""
    out = []
    for g in glyphs:
        lv = levels_for(page, pool, pages, g, seed, caps)
        entry = None
        if snaps is None:
            for level, cand in lv:
                if cand:
                    entry = {"g": g, "level": level, "picks": cand[:per_glyph]}
                    break
        else:
            pooled = [(level, x) for level, cand in lv for x in cand if snaps.get(ikey(x), {}).get("ok")]
            pooled = size_filter(pooled, snaps, size_band)
            dist = shape_filter([x for _, x in pooled], snaps, mad_k)
            for level in ("page", "issue", "title"):
                keep = sorted((x for lvl, x in pooled if lvl == level and ikey(x) in dist), key=lambda x: (dist[ikey(x)], rank(seed, x)))
                if keep:
                    entry = {"g": g, "level": level, "picks": keep[:per_glyph]}
                    break
        if entry is None:
            entry = {"g": g, "level": "font" if font else "none", "picks": []}
        out.append(entry)
    return out


# ---------------------------------------------------------------- crop quality


def otsu(hist: list) -> int:
    """Otsu's threshold of a 256-bin histogram."""
    total = sum(hist)
    if not total:
        return 127
    sum_all = sum(i * h for i, h in enumerate(hist))
    w_b = sum_b = 0
    best, thr = -1.0, 127
    for t in range(256):
        w_b += hist[t]
        if not w_b:
            continue
        w_f = total - w_b
        if not w_f:
            break
        sum_b += t * hist[t]
        m_b, m_f = sum_b / w_b, (sum_all - sum_b) / w_f
        v = w_b * w_f * (m_b - m_f) ** 2
        if v > best:
            best, thr = v, t
    return thr


def components(mask: bytes, w: int, h: int) -> list:
    """8-connected components of the non-zero pixels: a label per pixel (0 = background)."""
    lab = [0] * (w * h)
    n = 0
    for start in range(w * h):
        if not mask[start] or lab[start]:
            continue
        n += 1
        lab[start] = n
        stack = [start]
        while stack:
            i = stack.pop()
            y, x = divmod(i, w)
            for dy in (-1, 0, 1):
                yy = y + dy
                if yy < 0 or yy >= h:
                    continue
                for dx in (-1, 0, 1):
                    xx = x + dx
                    if 0 <= xx < w:
                        j = yy * w + xx
                        if mask[j] and not lab[j]:
                            lab[j] = n
                            stack.append(j)
    return lab


SHAPE = 20  # side of the normalised shape bitmap


def ink_snap(word_img, origin: tuple, cbox: list, band: list, joined: float = 1.6, foreign: float = 0.2,
             dark: float = 0.75) -> dict:
    """Fit an LSTM char box to the ink. `word_img` is the greyscale word crop (with a few pixels' margin), `origin` its
    top-left in the scan, `cbox` the char box [x0, y0, x1, y1] and `band` the line's type [top, bottom], in the scan.

    The crop is binarised (Otsu), dilated by one pixel up and down so broken Fraktur strokes join, and split into connected
    components. The letter is the components whose ink lies mostly (half or more) inside the LSTM box; its box is
    theirs. It fails (`ok` False, with `why`) when over 30% of the ink inside the LSTM box belongs to pieces lying
    mostly outside it, or the letter is more than `joined` times as wide as the LSTM box (joined: a neighbour cast or
    printed onto it), when no component is inside (no_ink), when its ink reaches the crop's edge (cut),
    when other components' ink is more than `foreign` of the ink inside its box (foreign), when it is under a quarter
    of the type height or has under 12 ink pixels (small), or when its box is over `dark` ink (dark).
    On success: fit (the box in the scan), ink, and shape (a SHAPE x SHAPE bitmap of the letter alone, centred)."""
    w, h = word_img.size
    thr = otsu(word_img.histogram()[:256])
    ink_img = word_img.point(lambda v: 255 if v <= thr else 0)
    ink = ink_img.tobytes()
    # dilated by a pixel up and down only: joins a stroke broken across a hairline, not two letters side by side
    from PIL import Image, ImageChops
    grown = ink_img
    for dy in (1, -1):
        shifted = Image.new("L", (w, h), 0)
        shifted.paste(ink_img, (0, dy))
        grown = ImageChops.lighter(grown, shifted)
    lab = components(grown.tobytes(), w, h)
    ox, oy = origin
    cx0, cy0, cx1, cy1 = cbox[0] - ox, cbox[1] - oy, cbox[2] - ox, cbox[3] - oy
    total, inside = Counter(), Counter()
    for i in range(w * h):
        if ink[i]:
            c = lab[i]
            total[c] += 1
            y, x = divmod(i, w)
            if cx0 <= x < cx1 and cy0 <= y < cy1:
                inside[c] += 1
    sel = {c for c, n in inside.items() if n >= 3 and n * 2 >= total[c]}
    # ink inside the box that belongs to pieces lying mostly outside it: the letter touches a neighbour
    spill = sum(n for c, n in inside.items() if c not in sel and n >= 3)
    if spill > 0.3 * max(1, sum(inside.values())):
        return {"ok": False, "why": "joined"}
    if not sel:
        return {"ok": False, "why": "no_ink"}
    xs, ys = [], []
    for i in range(w * h):
        if ink[i] and lab[i] in sel:
            y, x = divmod(i, w)
            xs.append(x)
            ys.append(y)
    fx0, fy0, fx1, fy1 = min(xs), min(ys), max(xs) + 1, max(ys) + 1
    n_ink = len(xs)
    band_h = max(1, band[1] - band[0])
    if fx0 == 0 or fy0 == 0 or fx1 == w or fy1 == h:
        return {"ok": False, "why": "cut"}
    if fx1 - fx0 > joined * max(1, cx1 - cx0) and fx1 - fx0 > 0.35 * band_h:
        return {"ok": False, "why": "joined"}
    other = sum(1 for y in range(fy0, fy1) for x in range(fx0, fx1) if ink[y * w + x] and lab[y * w + x] not in sel)
    if other > foreign * n_ink:
        return {"ok": False, "why": "foreign"}
    if fy1 - fy0 < 0.25 * band_h or n_ink < 12:
        return {"ok": False, "why": "small"}
    if n_ink > dark * (fx1 - fx0) * (fy1 - fy0):
        return {"ok": False, "why": "dark"}
    # the letter alone, centred in a square, scaled to SHAPE x SHAPE
    side = max(fx1 - fx0, fy1 - fy0)
    sq = Image.new("L", (side, side), 0)
    only = bytes(255 if ink[(fy0 + y) * w + fx0 + x] and lab[(fy0 + y) * w + fx0 + x] in sel else 0
                 for y in range(fy1 - fy0) for x in range(fx1 - fx0))
    sq.paste(Image.frombytes("L", (fx1 - fx0, fy1 - fy0), only), ((side - (fx1 - fx0)) // 2, (side - (fy1 - fy0)) // 2))
    shape = bytes(1 if v >= 128 else 0 for v in sq.resize((SHAPE, SHAPE), Image.BILINEAR).tobytes())
    return {"ok": True, "fit": [fx0 + ox, fy0 + oy, fx1 + ox, fy1 + oy], "ink": n_ink, "shape": shape,
            "rel": [(fx1 - fx0) / band_h, (fy1 - fy0) / band_h]}


def _median(v: list) -> float:
    v = sorted(v)
    n = len(v)
    return (v[n // 2] + v[(n - 1) // 2]) / 2 if n else 0.0


def size_filter(pooled: list, snaps: dict, band: tuple) -> list:
    """Drop candidates whose width or height (in type heights) is outside `band` times the pool's median."""
    if len(pooled) < 3:
        return pooled
    mw = _median([snaps[ikey(x)]["rel"][0] for _, x in pooled])
    mh = _median([snaps[ikey(x)]["rel"][1] for _, x in pooled])
    return [(lv, x) for lv, x in pooled
            if band[0] * mw <= snaps[ikey(x)]["rel"][0] <= band[1] * mw and band[0] * mh <= snaps[ikey(x)]["rel"][1] <= band[1] * mh]


def shape_distance(a: bytes, b: bytes) -> float:
    return sum(x != y for x, y in zip(a, b)) / len(a)


def shape_filter(cands: list, snaps: dict, k: float) -> dict:
    """{key: distance to the medoid} for the candidates within median + k x MAD of it (all of them, at distance 0 for
    the medoid, when there are under 3)."""
    if not cands:
        return {}
    shapes = [snaps[ikey(x)]["shape"] for x in cands]
    if len(cands) < 3:
        return {ikey(x): 0.0 for x in cands}
    d = [[shape_distance(a, b) for b in shapes] for a in shapes]
    m = min(range(len(cands)), key=lambda i: (sum(d[i]), i))
    dist = d[m]
    med = _median(dist)
    mad = _median([abs(v - med) for v in dist])
    cut = med + k * max(mad, 0.02)
    return {ikey(x): dist[i] for i, x in enumerate(cands) if dist[i] <= cut}


def snap_page(job) -> tuple:
    """Ink-snap one source page's candidates and cut the tiles of those that pass: (stats, {key: snap}, {key: tile PNG})."""
    import io
    from PIL import Image, ImageOps
    Image.MAX_IMAGE_PIXELS = None
    path, cands, height, margin = job
    snaps, tiles, why = {}, {}, Counter()
    with Image.open(path) as img:
        img = img.convert("L")
    for x in cands:
        bx0, by0, bx1, by1 = x["wbox"]
        m = 3
        ox, oy = max(0, bx0 - m), max(0, by0 - m)
        word = img.crop((ox, oy, min(img.width, bx1 + m), min(img.height, by1 + m)))
        sn = ink_snap(word, (ox, oy), x["box"], x["band"])
        why["passed" if sn["ok"] else sn["why"]] += 1
        snaps[ikey(x)] = sn
        if sn["ok"]:
            t = ImageOps.autocontrast(img.crop(crop_box(dict(x, box=sn["fit"]), img.size, margin)), cutoff=1)
            t = t.resize((max(1, round(t.width * height / t.height)), height), Image.LANCZOS)
            buf = io.BytesIO()
            t.save(buf, "PNG")
            tiles[ikey(x)] = buf.getvalue()
    return why, snaps, tiles


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
    if args.font_only and not font:
        raise SystemExit("--font-only needs glyph_font in the language config")
    for page, p in sorted(pages.items()):
        if args.font_only:
            break  # every glyph from the typeface: no scan is read
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
    tiles, snaps, quality = {}, None, Counter()
    if args.quality_checks:
        # Every candidate any task page could take, each source scan read once (in --jobs processes).
        cands = {}
        for page in sorted(needed):
            for g in glyphs:
                for _, cand in levels_for(page, pool, pages, g, args.seed, CAPS):
                    for x in cand:
                        cands.setdefault(x["page"], {})[ikey(x)] = x
        jobs = [(_path(pages[src]["_root"], pages[src]["image"]), list(c.values()), args.height, args.margin) for src, c in sorted(cands.items())]
        snaps = {}
        import io
        from concurrent.futures import ProcessPoolExecutor
        with ProcessPoolExecutor(args.jobs) as ex:
            for why, sn, tl in ex.map(snap_page, jobs):
                quality.update(why)
                snaps.update(sn)
                tiles.update({k: Image.open(io.BytesIO(v)) for k, v in tl.items()})
    chosen = {page: choose(page, pool, pages, glyphs, args.per_glyph, args.seed, font, snaps, CAPS,
                           (args.min_size, args.max_size), args.mad_k) for page in sorted(needed)}
    if not args.quality_checks:
        picks_by_source = {}
        for entries in chosen.values():
            for e in entries:
                for x in e["picks"]:
                    picks_by_source.setdefault(x["page"], {})[(x["word"], x["char"])] = x
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
              "font": font, "font_only": args.font_only, "quality_checks": args.quality_checks,
              "ink_snap": dict(sorted(quality.items())), **dict(sorted(stats.items())),
              "glyph_levels_per_key": {k: levels_keys.get(k, 0) for k in LEVELS + ("none",)},
              "glyph_levels_per_task": {k: levels_tasks.get(k, 0) for k in LEVELS + ("none",)},
              "font_share_per_key": round(levels_keys.get("font", 0) / max(1, sum(levels_keys.values())), 4)}
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
    ap.add_argument("--no-quality-checks", dest="quality_checks", action="store_false",
                    help="skip the ink snap, size and shape checks (the key-v1 selection)")
    ap.add_argument("--min-size", type=float, default=0.6, help="a crop's width and height at least this times the glyph's median")
    ap.add_argument("--max-size", type=float, default=1.6, help="and at most this times it")
    ap.add_argument("--mad-k", type=float, default=1.5, help="shape cutoff: median + this x MAD of the distances to the medoid")
    ap.add_argument("--font-only", action="store_true", help="draw every glyph in the language's glyph_font, with no crops")
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
