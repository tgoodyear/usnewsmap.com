#!/usr/bin/env python3
"""Glyph templates cut from a page's own ink, for the render-vs-scan test (#2103, second try).

    templates.py build --alto-dir LOC --set-dir SET --out templates.npz [--words words.jsonl] [--jobs 3]
                       [--min-wc 0.9] [--min-samples 5] [--font-cache DIR] [--limit-pages N]

The first run drew words in Old Standard TT, which is not the printed type; a one-letter look-alike swap on real crops
was seen at about 0.60 AUC. This draws each word from letters cut out of the same page's own ink instead.

Harvest (per page, from the full LoC ALTO in --alto-dir). A word is trusted when it is in the English word list
(crates/usnm-core/data/english-words.txt.zst), has WC >= --min-wc, and is three or more ASCII letters. LEAKAGE RULE: a
trusted word whose box overlaps any gold crop box of the page (the SET's manifest.jsonl, page pixels) is dropped, so no
template is cut from the words the test scores (`select_words`).

Cut. The word's ink is binarised as distances.py does it (render.crop_frame, ink.binarise). The word is split into its
letters by dynamic programming over the ink column profile: each of the n-1 boundaries sits where few ink pixels cross
the column and near where an Old Standard rendering of the same word puts it (its advance widths, spread over the
word's ink). A word is rejected when a letter's width is outside 0.5 to 1.7 of the prior's, when a cut still crosses
more than 60% of a typical column of ink, when its height does not fit the line's x-height (more than 20% off the line's
median), or when it has no ink. The accept rate is recorded.

Normalise. Rows are put on the baseline and scaled to the line's x-height (24 canonical pixels). The x-height and
baseline of a word come from the Old Standard rendering's ink rows against the scan's; a line's x-height is the median of
its words'. A letter with at least --min-samples samples gets the pixel median of them (case kept); the inter-letter gap
is the median gap between cut letters' ink on the page.

Templates, per letter: this page's, else the same LCCN's other pages' (pooled samples), else Old Standard scaled to the
same x-height (`TemplateStore.renderer`). The record per page (samples per letter, letters covered, fallback share of the
letters in --words) is written next to the .npz as <out>.json.
"""

from __future__ import annotations

import argparse
import json
import math
import statistics
import sys
from collections import Counter, defaultdict
from pathlib import Path

import numpy as np

import paths  # noqa: F401
import fonts
import gold
import ink
import loader
import render

U = 24  # canonical pixels per x-height
ASC, DESC = 2.0, 0.8  # canonical rows above and below the baseline, in x-heights
BASE = int(round(ASC * U))
H = BASE + int(round(DESC * U))
MIN_SAMPLES = 5
MIN_WC = 0.9
WIDTH_RATIO = (0.5, 1.7)
CUT_INK_MAX = 0.6  # of a typical ink column
XH_TOLERANCE = 0.2
LEXICON = paths.ROOT / "crates" / "usnm-core" / "data" / "english-words.txt.zst"
DEFAULT_GAP = 2.0
FALLBACK_QUANTILE = 0.75
MIN_TRUSTED = 200


# ---------------------------------------------------------------- harvest: which words may be used


def read_lexicon(path: Path = LEXICON) -> frozenset:
    """The English word list, lower-cased, read as compare.py's add_lexicon_oov reads it."""
    read_zst = loader._zst_reader()
    return frozenset(w.lower() for w in read_zst(path).split("\n") if w)


def crop_boxes_by_stem(manifest: list[dict]) -> dict[str, list[list[float]]]:
    """Gold crop boxes [x0, y0, x1, y1] in page pixels, per page image stem."""
    out: dict[str, list[list[float]]] = defaultdict(list)
    for m in manifest:
        if m.get("box") is not None and m.get("source_image"):
            out[m["source_image"].rsplit(".", 1)[0]].append(list(m["box"]))
    return out


def overlaps(box_xywh: list[float], crop: list[float]) -> bool:
    """True when the word box [x, y, w, h] and the crop box [x0, y0, x1, y1] share any area."""
    x, y, w, h = box_xywh
    return x < crop[2] and x + w > crop[0] and y < crop[3] and y + h > crop[1]


def select_words(words: list[dict], lexicon: frozenset, crops: list[list[float]], min_wc: float = MIN_WC,
                 min_len: int = 3, fallback_quantile: float | None = FALLBACK_QUANTILE,
                 min_trusted: int = MIN_TRUSTED) -> tuple[list[dict], dict]:
    """The trusted words of a page (loader.parse_words rows) and counts of why others were left out.

    WC >= min_wc. WC is not on one scale across LoC's pages: on some pages (1887-1903 here) no word reaches 0.9, and
    on three pages WC is one constant. When fewer than `min_trusted` lexicon words reach `min_wc`, the cut-off is
    instead the `fallback_quantile` quantile of WC among the page's lexicon words (ties kept), and the page's
    stats say so (`wc_cutoff`, `wc_mode`)."""
    letters = [w for w in words if w["text"].isascii() and w["text"].isalpha() and len(w["text"]) >= min_len]
    eligible = [w for w in letters if w["wc"] is not None and w["text"].lower() in lexicon]
    cutoff, mode = min_wc, "absolute"
    if fallback_quantile is not None and sum(w["wc"] >= min_wc for w in eligible) < min_trusted and eligible:
        cutoff, mode = min(min_wc, float(np.quantile([w["wc"] for w in eligible], fallback_quantile))), "page_quantile"
    keep, c = [], Counter()
    for w in words:
        t = w["text"]
        if not (t.isascii() and t.isalpha()) or len(t) < min_len:
            c["not_letters_3plus"] += 1
        elif w["wc"] is None or w["wc"] < cutoff:
            c["low_wc"] += 1
        elif t.lower() not in lexicon:
            c["not_in_lexicon"] += 1
        elif any(overlaps(w["box"], cb) for cb in crops):
            c["in_gold_crop"] += 1
        else:
            keep.append(w)
    c["trusted"] = len(keep)
    out = dict(c)
    out.update({"wc_cutoff": cutoff, "wc_mode": mode})
    return keep, out


# ---------------------------------------------------------------- cutting a word into letters


def xheight_96(font_path) -> int:
    """Ink height of 'x' at render.BITMAP_PX, in pixels."""
    return render.glyph_bitmap("x", font_path).shape[0]


def prior_fractions(text: str, font_path) -> list[float]:
    """Where each letter boundary falls, as a fraction of the word's advance, from the font: len(text) + 1 values."""
    f = render._font(str(font_path), render.BITMAP_PX)
    cum = [f.getlength(text[:i]) for i in range(len(text) + 1)]
    return [c / cum[-1] for c in cum]


def cut_word(mask: np.ndarray, x0: int, x1: int, fractions: list[float]) -> tuple[list[int] | None, str]:
    """Letter boundaries [x0, c1 ... c(n-1), x1] of a word whose ink fills columns x0..x1 of `mask`, or (None, why).

    DP: boundary k is a column near its prior position with little ink across it; the cost is the column's ink over a
    typical column's, plus the squared offset from the prior in units of the mean letter width."""
    n = len(fractions) - 1
    span = x1 - x0
    prof = mask.sum(axis=0).astype(np.float64)
    nz = prof[x0:x1][prof[x0:x1] > 0]
    if span < 2 * n or nz.size == 0:
        return None, "too_narrow"
    ref = max(1.0, float(np.median(nz)))
    p = [x0 + f * span for f in fractions]
    pw = [p[i + 1] - p[i] for i in range(n)]
    mean_w = max(4.0, span / n)
    cand, cost = [], []
    for k in range(1, n):
        w = max(3.0, 0.6 * max(pw[k - 1], pw[k]))
        cols = [c for c in range(int(round(p[k] - w)), int(round(p[k] + w)) + 1) if x0 < c < x1]
        if not cols:
            return None, "too_narrow"
        cand.append(cols)
        cost.append([prof[c] / ref + ((c - p[k]) / mean_w) ** 2 for c in cols])
    # dp over boundaries; minimum letter width keeps the order
    best = [list(cost[0])]
    back: list[list[int]] = [[-1] * len(cand[0])]
    for k in range(1, n - 1):
        mw = max(2, int(0.35 * pw[k]))
        row, brow = [], []
        for j, c in enumerate(cand[k]):
            opts = [(best[k - 1][i], i) for i, c0 in enumerate(cand[k - 1]) if c - c0 >= mw]
            if not opts:
                row.append(math.inf)
                brow.append(-1)
            else:
                v, i = min(opts)
                row.append(v + cost[k][j])
                brow.append(i)
        best.append(row)
        back.append(brow)
    last = len(cand) - 1
    mw_last = max(2, int(0.35 * pw[n - 1]))
    ends = [(best[last][j], j) for j, c in enumerate(cand[last]) if x1 - c >= mw_last]
    if not ends or not math.isfinite(min(ends)[0]):
        return None, "no_cut"
    j = min(ends)[1]
    cuts = []
    for k in range(last, -1, -1):
        cuts.append(cand[k][j])
        j = back[k][j]
    cuts.reverse()
    bounds = [x0] + cuts + [x1]
    for i in range(n):
        ratio = (bounds[i + 1] - bounds[i]) / max(pw[i], 1e-6)
        if not WIDTH_RATIO[0] <= ratio <= WIDTH_RATIO[1]:
            return None, "letter_width"
    if any(prof[c] / ref > CUT_INK_MAX for c in cuts):
        return None, "cut_through_ink"
    return bounds, "ok"


def letter_sample(mask: np.ndarray, c0: int, c1: int, baseline: float, xh: float) -> tuple[np.ndarray, int, int] | None:
    """The letter in columns c0..c1 of `mask` on the canonical grid (H rows, baseline at BASE, x-height U pixels),
    trimmed to its ink columns: (uint8 array, columns trimmed on the left, on the right); None for no ink."""
    from PIL import Image

    s = U / xh
    top, bot = baseline - ASC * xh, baseline + DESC * xh
    m = int(math.ceil(2.5 * xh)) + 2
    padded = np.pad(mask.astype(np.uint8) * 255, m)
    w = max(1, int(round((c1 - c0) * s)))
    im = Image.fromarray(padded).resize((w, H), Image.BOX, box=(c0 + m, top + m, c1 + m, bot + m))
    a = np.asarray(im)
    cols = np.nonzero(a.max(axis=0) >= 64)[0]
    if len(cols) < 2 or (a >= 128).sum() < 8:
        return None
    return a[:, cols[0]: cols[-1] + 1], int(cols[0]), int(w - 1 - cols[-1])


def harvest_page(stem: str, page, words: list[dict], lexicon: frozenset, crops: list[list[float]], font_path,
                 min_wc: float = MIN_WC, fallback_quantile: float | None = FALLBACK_QUANTILE) -> dict:
    """Samples per letter, letter gaps and counts from one page: {"samples": {char: [uint8 arrays]}, "gaps": [...],
    "stats": {...}}. `page` is a PIL image."""
    trusted, stats = select_words(words, lexicon, crops, min_wc, fallback_quantile=fallback_quantile)
    stats["alto_words"] = len(words)
    xh96 = xheight_96(font_path)
    cands = []
    for w in trusted:
        frame, pad, (bx, by, bw, bh) = render.crop_frame(page, w["box"])
        scan = ink.drop_specks(ink.binarise(frame))
        inside = np.zeros_like(scan)
        inside[by: by + bh, bx: bx + bw] = scan[by: by + bh, bx: bx + bw]
        rows, cols = ink.ink_rows(inside), ink.ink_cols(inside)
        g = render.glyph_bitmap_pos(w["text"], font_path)
        if rows is None or cols is None or g is None or rows[1] - rows[0] < 6:
            stats["no_ink"] = stats.get("no_ink", 0) + 1
            continue
        sy = (rows[1] - rows[0]) / g[0].shape[0]
        cands.append({"w": w, "mask": inside, "rows": rows, "cols": cols, "xh": xh96 * sy,
                      "baseline": rows[0] - g[1][1] * sy, "fr": prior_fractions(w["text"], font_path)})
    by_line: dict[int, list[float]] = defaultdict(list)
    for c in cands:
        by_line[c["w"]["line"]].append(c["xh"])
    samples: dict[str, list] = defaultdict(list)
    gaps: list[float] = []
    reasons: Counter = Counter()
    accepted = 0
    for c in cands:
        line_xh = by_line[c["w"]["line"]]
        xh = statistics.median(line_xh) if len(line_xh) >= 3 else c["xh"]
        if len(line_xh) >= 3 and abs(c["xh"] / xh - 1) > XH_TOLERANCE:
            reasons["xheight_off_line"] += 1
            continue
        bounds, why = cut_word(c["mask"], c["cols"][0], c["cols"][1], c["fr"])
        if bounds is None:
            reasons[why] += 1
            continue
        letters = [letter_sample(c["mask"], bounds[i], bounds[i + 1], c["baseline"], xh) for i in range(len(bounds) - 1)]
        if any(s is None for s in letters):
            reasons["empty_letter"] += 1
            continue
        accepted += 1
        for ch, (arr, _, _) in zip(c["w"]["text"], letters):
            samples[ch].append(arr)
        gaps += [letters[i][2] + letters[i + 1][1] for i in range(len(letters) - 1)]
    stats.update({"candidates": len(cands), "accepted": accepted, "rejected": dict(reasons),
                  "accept_rate_of_candidates": accepted / len(cands) if cands else None,
                  "accept_rate_of_trusted": accepted / len(trusted) if trusted else None,
                  "median_gap": float(np.median(gaps)) if gaps else None})
    return {"samples": dict(samples), "gaps": gaps, "stats": stats}


# ---------------------------------------------------------------- templates


def median_template(samples: list[np.ndarray]) -> np.ndarray:
    """Pixel median of letter samples after resizing each to the median width (uint8)."""
    from PIL import Image

    w = max(1, int(round(statistics.median(a.shape[1] for a in samples))))
    stack = [np.asarray(Image.fromarray(a).resize((w, H), Image.BILINEAR)) if a.shape[1] != w else a for a in samples]
    return np.median(np.stack(stack), axis=0).astype(np.uint8)


class TemplateStore:
    """Letter templates per page and per LCCN, with the record of how they were made."""

    def __init__(self, pages: dict, lccns: dict, gaps: dict, meta: dict):
        self.pages, self.lccns, self.gaps, self.meta = pages, lccns, gaps, meta
        self._font_letters: dict = {}

    @staticmethod
    def lccn(stem: str) -> str:
        return stem.split("-")[0]

    @classmethod
    def from_harvest(cls, harvests: dict[str, dict], min_samples: int = MIN_SAMPLES, words: list[dict] | None = None,
                     font_path=None) -> "TemplateStore":
        pages, lccns, gaps, pooled, pooled_gaps = {}, {}, {}, defaultdict(lambda: defaultdict(list)), defaultdict(list)
        for stem, h in harvests.items():
            lc = cls.lccn(stem)
            pages[stem] = {ch: median_template(a) for ch, a in h["samples"].items() if len(a) >= min_samples}
            gaps[stem] = float(np.median(h["gaps"])) if h["gaps"] else None
            for ch, a in h["samples"].items():
                pooled[lc][ch] += a
            pooled_gaps[lc] += h["gaps"]
        for stem in harvests:
            pooled[cls.lccn(stem)]
        for lc, d in pooled.items():
            lccns[lc] = {ch: median_template(a) for ch, a in d.items() if len(a) >= min_samples}
            gaps["lccn:" + lc] = float(np.median(pooled_gaps[lc])) if pooled_gaps[lc] else None
        store = cls(pages, lccns, gaps, {"min_samples": min_samples, "canonical": {"U": U, "H": H, "BASE": BASE}})
        store.meta["pages"] = {}
        for stem, h in harvests.items():
            lc = cls.lccn(stem)
            rec = {"samples_per_letter": {ch: len(a) for ch, a in sorted(h["samples"].items())},
                   "letters_covered_page": sorted(pages[stem]),
                   "letters_covered_lccn_only": sorted(set(lccns[lc]) - set(pages[stem])),
                   "gap": gaps[stem], **h["stats"]}
            store.meta["pages"][stem] = rec
        if words is not None and font_path is not None:
            store.add_fallback_share(words)
        return store

    def add_fallback_share(self, words: list[dict]) -> None:
        """Per page: of the letters in the OCR words, the share drawn from this page's templates, the LCCN's, the font."""
        by: dict[str, Counter] = defaultdict(Counter)
        for r in words:
            stem = r["source_image"].rsplit(".", 1)[0]
            for ch in r["ocr_word"]:
                if ch.isalpha():
                    by[stem][self.source(stem, ch)] += 1
        tot = Counter()
        for stem, c in by.items():
            n = sum(c.values())
            if stem in self.meta["pages"]:
                self.meta["pages"][stem]["letter_share"] = {k: c[k] / n for k in ("page", "lccn", "font")}
                self.meta["pages"][stem]["fallback_share"] = (c["lccn"] + c["font"]) / n
            tot += c
        n = sum(tot.values())
        if n:
            self.meta["letter_share"] = {k: tot[k] / n for k in ("page", "lccn", "font")}
            self.meta["fallback_share"] = (tot["lccn"] + tot["font"]) / n

    def source(self, stem: str, ch: str) -> str:
        if ch in self.pages.get(stem, {}):
            return "page"
        if ch in self.lccns.get(self.lccn(stem), {}):
            return "lccn"
        return "font"

    # -- files

    def save(self, path: Path) -> None:
        arrays = {f"p|{s}|{ord(c)}": a for s, d in self.pages.items() for c, a in d.items()}
        arrays.update({f"l|{s}|{ord(c)}": a for s, d in self.lccns.items() for c, a in d.items()})
        path.parent.mkdir(parents=True, exist_ok=True)
        np.savez_compressed(path, **arrays)
        path.with_suffix(".json").write_text(json.dumps({"gaps": self.gaps, **self.meta}, indent=1, sort_keys=True) + "\n",
                                             encoding="utf-8")

    @classmethod
    def load(cls, path: Path) -> "TemplateStore":
        meta = json.loads(path.with_suffix(".json").read_text(encoding="utf-8"))
        gaps = meta.pop("gaps")
        pages: dict = defaultdict(dict)
        lccns: dict = defaultdict(dict)
        with np.load(path) as z:
            for k in z.files:
                kind, name, o = k.split("|")
                (pages if kind == "p" else lccns)[name][chr(int(o))] = z[k]
        return cls(dict(pages), dict(lccns), gaps, meta)

    # -- the renderer

    def font_letter(self, ch: str, font_path) -> np.ndarray | None:
        """Old Standard's letter on the canonical grid (x-height U), uint8, trimmed to its ink columns."""
        key = (str(font_path), ch)
        if key not in self._font_letters:
            px = max(8, int(round(U * render.BITMAP_PX / xheight_96(font_path))))
            g = render.glyph_bitmap_pos(ch, font_path, px)
            a = None
            if g is not None:
                bmp, (_, dy) = g
                canvas = np.zeros((H, bmp.shape[1]), np.uint8)
                r0 = BASE + dy
                lo, hi = max(0, r0), min(H, r0 + bmp.shape[0])
                if hi > lo:
                    canvas[lo:hi] = (bmp[lo - r0: hi - r0] * 255).astype(np.uint8)
                    a = canvas
            self._font_letters[key] = a
        return self._font_letters[key]

    def word_ink(self, text: str, stem: str, font_path) -> tuple[np.ndarray | None, dict]:
        """The word laid out on the baseline: float ink on [0, 1] cropped to its ink box, and the letter counts by source."""
        lc = self.lccn(stem)
        gap = self.gaps.get(stem)
        if gap is None:
            gap = self.gaps.get("lccn:" + lc)
        gap = DEFAULT_GAP if gap is None else gap
        parts, used = [], Counter()
        for ch in text:
            a = None
            src = "font"
            if ch in self.pages.get(stem, {}):
                a, src = self.pages[stem][ch], "page"
            elif ch in self.lccns.get(lc, {}):
                a, src = self.lccns[lc][ch], "lccn"
            else:
                a = self.font_letter(ch, font_path)
            if a is None:
                continue
            used[src] += 1
            parts.append(a)
        meta = {"tpl_page_letters": used["page"], "tpl_lccn_letters": used["lccn"], "tpl_font_letters": used["font"],
                "tpl_gap": round(float(gap), 2)}
        if not parts:
            return None, meta
        total = sum(p.shape[1] for p in parts) + int(round(gap * (len(parts) - 1)))
        canvas = np.zeros((H, max(1, total)), np.uint8)
        x = 0
        for p in parts:
            seg = canvas[:, x: x + p.shape[1]]
            np.maximum(seg, p[:, : seg.shape[1]], out=seg)
            x += p.shape[1] + int(round(gap))
        ys, xs = np.nonzero(canvas > 12)
        if len(xs) == 0:
            return None, meta
        return canvas[ys.min(): ys.max() + 1, xs.min(): xs.max() + 1].astype(np.float32) / 255.0, meta

    def renderer(self, stem: str, font_path):
        """A `word_bitmap(text) -> (bitmap, meta)` for render.render_for_crop, drawing with this page's templates."""

        def word_bitmap(text: str):
            return self.word_ink(text, stem, font_path)

        return word_bitmap


# ---------------------------------------------------------------- command line


def _harvest_job(args):
    from PIL import Image

    stem, alto_dir, crops, lexicon_path, font_path, min_wc, fq = args
    Image.MAX_IMAGE_PIXELS = None
    lex = read_lexicon(Path(lexicon_path))
    with Image.open(Path(alto_dir) / f"{stem}.jpg") as im:
        page = im.convert("L")
    words = loader.parse_words((Path(alto_dir) / f"{stem}.xml").read_bytes(), page.size)
    return stem, harvest_page(stem, page, words, lex, crops, font_path, min_wc, fq)


def build(alto_dir: Path, set_dir: Path, out: Path, words_path: Path | None, jobs: int, min_wc: float, min_samples: int,
          font_cache: str | None, limit_pages: int | None, lexicon: Path = LEXICON,
          fallback_quantile: float | None = FALLBACK_QUANTILE) -> TemplateStore:
    fs = fonts.resolve("latin", None, font_cache)
    if not fs:
        raise SystemExit(f"latin: {fonts.unavailable_reason('latin')}")
    font_path = fs[0].path
    crops = crop_boxes_by_stem(gold.read_jsonl(set_dir / "manifest.jsonl"))
    stems = sorted(p.stem for p in alto_dir.glob("*.xml") if (alto_dir / f"{p.stem}.jpg").exists())[:limit_pages]
    jobargs = [(s, str(alto_dir), crops.get(s, []), str(lexicon), str(font_path), min_wc, fallback_quantile) for s in stems]
    if jobs > 1:
        import multiprocessing as mp

        with mp.get_context("spawn").Pool(jobs) as pool:
            res = dict(pool.imap_unordered(_harvest_job, jobargs))
    else:
        res = dict(_harvest_job(a) for a in jobargs)
    words = gold.read_jsonl(words_path) if words_path else None
    store = TemplateStore.from_harvest(res, min_samples, words, font_path)
    store.meta["font"] = fs[0].record()
    store.meta["settings"] = {"min_wc": min_wc, "min_samples": min_samples, "wc_fallback_quantile": fallback_quantile, "min_trusted": MIN_TRUSTED, "width_ratio": WIDTH_RATIO, "cut_ink_max": CUT_INK_MAX,
                              "xh_tolerance": XH_TOLERANCE}
    store.save(out)
    return store


def summary_line(store: TemplateStore) -> str:
    lines = []
    for stem, r in sorted(store.meta["pages"].items()):
        lines.append(f"{stem}: trusted {r['trusted']} (wc>={r['wc_cutoff']:.2f} {r['wc_mode']}; in_gold_crop dropped {r.get('in_gold_crop', 0)}), accepted {r['accepted']} "
                     f"({(r['accept_rate_of_trusted'] or 0):.0%} of trusted), letters {len(r['letters_covered_page'])} page "
                     f"+ {len(r['letters_covered_lccn_only'])} lccn, fallback share {r.get('fallback_share', float('nan')):.3f}")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    b = sub.add_parser("build", help="harvest, cut and write templates")
    b.add_argument("--alto-dir", type=Path, required=True)
    b.add_argument("--set-dir", type=Path, required=True, help="gold set with manifest.jsonl (crop boxes, kept out of the harvest)")
    b.add_argument("--out", type=Path, required=True, help="templates .npz (a .json record is written beside it)")
    b.add_argument("--words", type=Path, help="loader.py rows, to compute each page's fallback share")
    b.add_argument("--jobs", type=int, default=1)
    b.add_argument("--min-wc", type=float, default=MIN_WC)
    b.add_argument("--min-samples", type=int, default=MIN_SAMPLES)
    b.add_argument("--wc-fallback-quantile", type=float, default=FALLBACK_QUANTILE,
                   help="on a page where fewer than 200 lexicon words reach --min-wc: the WC quantile of its lexicon words "
                        "used as the cut-off instead (0 or less turns the fallback off)")
    b.add_argument("--font-cache")
    b.add_argument("--limit-pages", type=int)
    a = ap.parse_args(argv)
    store = build(a.alto_dir, a.set_dir, a.out, a.words, a.jobs, a.min_wc, a.min_samples, a.font_cache, a.limit_pages,
                  fallback_quantile=a.wc_fallback_quantile if a.wc_fallback_quantile > 0 else None)
    print(summary_line(store), file=sys.stderr)
    print(json.dumps({k: store.meta.get(k) for k in ("letter_share", "fallback_share")}), file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
