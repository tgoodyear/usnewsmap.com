"""Features of one word for the script classifier (#1666).

Four crops feed each word, as the issue asks: the text block (region), the line before, the word's
own line, the line after, and the word. Each crop is reduced to a few measures of its letter shapes
relative to the line's body (the band between the tops and bottoms of most letters):

- how much of the ink sits in the upper third of the body (Latin lowercase leaves it nearly empty
  outside ascenders; the syllabary, cut at capital height, fills it),
- how good the scan is (`quality`): contrast, how cleanly ink separates from paper, sharpness,
  speckle, stroke width and uneven paper, so the model can weigh the readings by how degraded the
  crop is,
- how many connected shapes are short (x-height), rise above the body (ascenders) or drop below it
  (descenders), and how wide and how far apart they are (letter width and spacing).

Then the readings: what `eng` and the syllabary model read for the word and their confidences,
how plausible the `eng` reading is as Latin-script text (letter bigrams of the word list, which
also scores names and transliterations), how often its case flips, whether either reading is a known word (the 1860 New Testament forms; an English word list), the
16 capitals the 1828 syllabary shares with Latin small capitals, lowercase letters, LoC's reading,
and the same for the words either side, for the whole line and for the lines above and below.

Everything is a float in a fixed order (`NAMES`), so a model trained here can be read back anywhere.
"""
from __future__ import annotations

import re

import numpy as np

SHARED_CAPS = set("RDWGPMBAZETJKSHL")  # the 1828 syllabary letters cut from Latin small capitals
LATIN_ONLY_UPPER = set("CFINOQUVXY")  # capitals with no syllabary letter of that shape in this type
SYL = re.compile("[Ꭰ-Ᏽ]")
ALPHA = re.compile("[A-Za-z]")
PUNCT = ".,;:!?\"'“”‘’()[]-—*|"

QUALITY = ["contrast", "separability", "sharpness", "speckle", "stroke", "bg_uneven"]
SHAPE = ["upper", "short", "tall", "desc", "comp_w", "gap", "density", "n_per_h", "ink_top", "ink_bot"]


def binarize(gray):
    """Otsu threshold; ink is True."""
    g = np.asarray(gray, dtype=np.uint8)
    hist = np.bincount(g.ravel(), minlength=256).astype(float)
    p = hist / max(1, hist.sum())
    omega = np.cumsum(p)
    mu = np.cumsum(p * np.arange(256))
    with np.errstate(divide="ignore", invalid="ignore"):
        sb = (mu[-1] * omega - mu) ** 2 / (omega * (1 - omega))
    if not np.isfinite(sb).any():  # one tone only (a blank box): no ink
        return np.zeros(g.shape, dtype=bool)
    t = int(np.nanargmax(sb))
    return g <= t


def quality(gray, ink, H):
    """How good the scan of this crop is: paper-to-ink contrast, how cleanly Otsu splits them
    (between-class over total variance), sharpness (variance of the Laplacian over the contrast),
    speckle (share of tiny shapes), stroke width (median distance-transform ridge, over the line's
    body height) and how uneven the paper is (spread of the paper tone across columns)."""
    from scipy import ndimage
    g = np.asarray(gray, dtype=np.float64)
    out = dict.fromkeys(QUALITY, 0.0)
    if g.size < 16 or ink.sum() == 0 or (~ink).sum() == 0:
        return out
    paper, inkv = g[~ink], g[ink]
    c = float(np.median(paper) - np.median(inkv))
    out["contrast"] = c / 255.0
    var = g.var()
    if var > 0:
        w1 = ink.mean()
        out["separability"] = float(w1 * (1 - w1) * (paper.mean() - inkv.mean()) ** 2 / var)
    out["sharpness"] = float(np.sqrt(ndimage.laplace(g).var()) / max(1.0, c))
    lab, n = ndimage.label(ink, structure=np.ones((3, 3)))
    if n:
        sizes = np.bincount(lab.ravel())[1:]
        out["speckle"] = float(np.mean(sizes < max(3.0, 0.003 * H * H)))
    dt = ndimage.distance_transform_edt(ink)
    ridge = dt[(dt > 0) & (dt >= ndimage.maximum_filter(dt, size=3))]
    out["stroke"] = float(2 * np.median(ridge) / max(4.0, H)) if ridge.size else 0.0
    if g.shape[1] >= 8:
        cols = np.array_split(np.arange(g.shape[1]), 8)
        tones = [np.median(g[:, cc][~ink[:, cc]]) for cc in cols if (~ink[:, cc]).any()]
        out["bg_uneven"] = float(np.std(tones) / 255.0) if tones else 0.0
    return out


def body(ink):
    """Top and bottom rows of the line's body: rows with at least half the busiest row's ink,
    after a 3-row smoothing. Falls back to the whole crop."""
    prof = ink.sum(axis=1).astype(float)
    if prof.max() == 0:
        return 0, ink.shape[0]
    prof = np.convolve(prof, np.ones(3) / 3, mode="same")
    rows = np.nonzero(prof >= 0.5 * prof.max())[0]
    return int(rows[0]), int(rows[-1]) + 1


def components(ink):
    """Bounding boxes (y0, x0, y1, x1) and pixel counts of 8-connected shapes."""
    from scipy import ndimage
    lab, n = ndimage.label(ink, structure=np.ones((3, 3)))
    if n == 0:
        return np.zeros((0, 4)), np.zeros(0)
    sl = ndimage.find_objects(lab)
    boxes = np.array([[s[0].start, s[1].start, s[0].stop, s[1].stop] for s in sl], dtype=float)
    sizes = np.bincount(lab.ravel())[1:].astype(float)
    return boxes, sizes


def shape(ink, top, bot):
    """The SHAPE measures of one crop's ink, given the line body's rows in the crop."""
    H = max(4.0, float(bot - top))
    out = dict.fromkeys(SHAPE, 0.0)
    total = ink[top:bot].sum()
    if ink.sum() == 0 or total == 0:
        out["ink_top"] = out["ink_bot"] = 0.0
        return out
    out["upper"] = float(ink[top:top + int(0.33 * H)].sum() / total)
    boxes, sizes = components(ink)
    keep = (sizes >= max(4.0, 0.002 * H * H)) & ((boxes[:, 2] - boxes[:, 0]) >= 0.15 * H)
    boxes = boxes[keep]
    rows = np.nonzero(ink.any(axis=1))[0]
    out["ink_top"] = (rows[0] - top) / H
    out["ink_bot"] = (rows[-1] + 1 - bot) / H
    out["density"] = float(ink[top:bot].mean())
    if len(boxes):
        h = boxes[:, 2] - boxes[:, 0]
        w = boxes[:, 3] - boxes[:, 1]
        out["short"] = float(np.mean((h < 0.8 * H) & (boxes[:, 0] > top + 0.2 * H)))
        out["tall"] = float(np.mean(boxes[:, 0] < top - 0.15 * H))
        out["desc"] = float(np.mean(boxes[:, 2] > bot + 0.15 * H))
        out["comp_w"] = float(np.median(w) / H)
        xs = boxes[np.argsort(boxes[:, 1])]
        gaps = xs[1:, 1] - np.maximum.accumulate(xs[:-1, 3])
        out["gap"] = float(np.median(gaps) / H) if len(gaps) else 0.0
        out["n_per_h"] = float(len(boxes) * H / max(1, ink.shape[1]))
    return out


def letters(t):
    return re.sub(r"[^A-Za-zᎠ-Ᏽ]", "", t)


_BIGRAMS = {}


def bigram_model(words):
    """Letter-bigram log-probabilities (add-one smoothed, with word start and end) from a word list.
    Built once per list; a Latin-script reading that is not a dictionary word (a name, a Cherokee
    transliteration such as a-ska-ya) still scores like the language, `eng`'s readings of syllabary don't."""
    key = id(words)
    if key not in _BIGRAMS:
        import math
        from collections import Counter
        pairs, firsts = Counter(), Counter()
        for w in words:
            w = "^" + re.sub(r"[^a-z]", "", w) + "$"
            for a, b in zip(w, w[1:]):
                pairs[a + b] += 1
                firsts[a] += 1
        _BIGRAMS[key] = {a + b: math.log((pairs[a + b] + 1) / (firsts[a] + 28))
                         for a in "^abcdefghijklmnopqrstuvwxyz" for b in "abcdefghijklmnopqrstuvwxyz$"}
    return _BIGRAMS[key]


def latin_plausibility(text, words):
    """Mean letter-bigram log-probability of the alphabetic runs of a reading (lowercased); 0 when
    it has no letters, so the model sees `n` and `lower` to tell those apart."""
    lp = bigram_model(words)
    runs = [r for r in re.findall(r"[A-Za-z]+", text) if r]
    scores = []
    for r in runs:
        w = "^" + r.lower() + "$"
        scores += [lp[a + b] for a, b in zip(w, w[1:])]
    return sum(scores) / len(scores) if scores else 0.0


def reading(text, conf, vocab, english):
    """Measures of one model's reading of a word."""
    core = text.strip(PUNCT)
    al = re.sub(r"[^A-Za-z]", "", core)
    sy = SYL.findall(core)
    lower = sum(c.islower() for c in al)
    upper = [c for c in al if c.isupper()]
    return {
        "conf": conf,
        "n": float(len(letters(core))),
        "lower": lower / len(al) if al else 0.0,
        "n_lower": float(lower),
        "shared_caps": float(bool(upper) and lower == 0 and all(c in SHARED_CAPS for c in upper)),
        "latin_only_caps": float(sum(c in LATIN_ONLY_UPPER for c in upper)),
        "english": float(len(al) >= 2 and al.lower() in english),
        "english_long": float(len(al) >= 4 and al.lower() in english),
        "cherokee": float(len(sy) >= 1 and "".join(sy) in vocab),
        "syl": float(len(sy)),
        "digits": float(sum(c.isdigit() for c in core)),
        "paren": float("(" in text or ")" in text),
        "bigram": latin_plausibility(core, english),
        "case_flips": sum(1 for a_, b_ in zip(al[1:], al[2:]) if a_.isupper() != b_.isupper()) / max(1, len(al) - 1),
    }


READING = ["conf", "n", "lower", "n_lower", "shared_caps", "latin_only_caps", "english", "english_long",
           "cherokee", "syl", "digits", "paren", "bigram", "case_flips"]
LOC = ["loc_attested", "loc_syl", "loc_digits", "loc_punct_only", "loc_paren", "loc_len"]


def loc_measures(text, vocab):
    core = text.strip(PUNCT)
    return {
        "loc_attested": float(bool(core) and core in vocab),
        "loc_syl": float(len(SYL.findall(core))),
        "loc_digits": float(sum(c.isdigit() for c in core)),
        "loc_punct_only": float(not letters(core) and not any(c.isdigit() for c in core)),
        "loc_paren": float("(" in text or ")" in text),
        "loc_len": float(len(core)),
    }


def line_loc_measures(text, vocab):
    """Share of a line's LoC words that are syllabary, attested, or Latin."""
    ws = [w.strip(PUNCT) for w in (text or "").split()]
    ws = [w for w in ws if w]
    if not ws:
        return {"syl": 0.0, "attested": 0.0, "latin": 0.0, "present": 0.0}
    return {"syl": sum(bool(SYL.search(w)) for w in ws) / len(ws),
            "attested": sum(w in vocab for w in ws) / len(ws),
            "latin": sum(bool(ALPHA.search(w)) for w in ws) / len(ws), "present": 1.0}


def names():
    n = []
    n += ["word_" + k for k in SHAPE]
    n += ["word_h_rel", "word_w_rel", "word_gap_before", "word_gap_after", "word_pos", "word_index_rel"]
    n += ["line_" + k for k in SHAPE]
    n += ["d_" + k for k in ("upper", "short", "tall", "desc", "comp_w", "gap", "density", "n_per_h")]
    for side in ("prev", "next"):
        n += [side + "_" + k for k in SHAPE] + [side + "_h_rel"]
        n += [side + "_loc_" + k for k in ("syl", "attested", "latin", "present")]
    n += ["block_short", "block_upper", "block_h_rel", "block_syllabary", "block_lines"]
    for crop in ("word", "line", "block"):
        n += [crop + "_q_" + k for k in QUALITY]
    for m in ("eng", "chr"):
        n += [m + "_" + k for k in READING]
    n += ["eng_conf_minus_chr"]
    n += LOC
    for side in ("left", "right"):
        n += [side + "_" + k for k in ("present", "eng_english", "eng_lower", "chr_cherokee", "loc_attested",
                                       "loc_paren", "eng_conf", "chr_conf")]
    n += ["left_open_paren", "right_close_paren"]
    n += ["line_eng_english", "line_eng_lower", "line_chr_cherokee", "line_loc_attested", "line_eng_conf",
          "line_chr_conf", "line_words"]
    return n


NAMES = names()
