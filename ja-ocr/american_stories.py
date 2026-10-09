"""Compare American Stories' text with LoC's on our pages (#205).

    python3 jaocr.py american-stories --year 1865 --year 1925 [--sample-pct 10] [--diff]

American Stories (Dell et al. 2023, arXiv:2308.12477; CC BY 4.0) re-read
Chronicling America with layout detection, a legibility classifier and
EfficientOCR. Each year is one tarball on Hugging Face of one JSON file per
scan, named <date>_p<page>_<lccn>_<reel>_<date><edition>_<image>.json, whose
p<page> is our seq-<n>. Some have no page ("pNone", often with reel
"no_reel"); their page is their image's place in the issue: an issue's images
are numbered consecutively, so a page's seq is its image number less the
issue's first, plus one, or set by the issue's named pages when it has any
(the report says how often this rule agrees with the named pages). This
streams each year's tarball twice (nothing is kept on disk): once for the
names, to place every scan, and once for the text of the scans in our usual
fixed sample (quality.sampled: a hash of
the doc_id, so the same pages as the OCR quality audit), reads LoC's text for
the sampled pages of those years from the curated store, and measures, for
pages of titles whose first catalog language is English:

- join: our sampled pages in the year with an American Stories scan, and the
  sampled scans with no page of ours;
- quality: words per page, and the audit's English damage rate and function
  word share (metric v2) of each text;
- recall: for a fixed list of terms, the pages whose LoC text has the term,
  whose American Stories text has it, and either;
- legibility: the share of a page's text regions American Stories marked
  Illegible (it doesn't OCR those), against LoC's damage rate.

With --diff (#251), it also measures indexing American Stories' text only where it differs from LoC's
(diff_year, below): per year and k, the delta's tokens and terms against both texts, the n-grams and phrases
that match only in American Stories' text and how many the delta keeps (tables diff_summary, diff_phrase,
diff_phrase_group, diff_edge_examples).

Writes audit/american-stories-<version>-<execution>.json and logs each row
as an "american stories" line. One replica of the execution works (solo.py).
"""

from __future__ import annotations

import bisect
import collections
import contextlib
import difflib
import functools
import gzip
import io
import json
import os
import re
import statistics
import tarfile
import time
import unicodedata
import urllib.request

import jaocr
import quality
from solo import Solo

URL = "https://huggingface.co/datasets/dell-research-harvard/AmericanStories/resolve/main/faro_{year}.tar.gz"
NAME = re.compile(r"^(\d{4}-\d\d-\d\d)_p(\d+|None|na)_([a-z]+\d+)_(.+?)_(\d{8})(\d{2})_(\d+)\.json$")
BATCH = re.compile(r"/batches/([a-z0-9_]+?)(?:_ver\d+)?/")
# Text regions: the dataset labels bylines "author" and captions "image_caption" (the paper says byline and
# caption); both spellings count.
TEXT_CLASSES = {"article", "headline", "author", "byline", "image_caption", "caption"}
TERMS = ("lincoln", "railroad", "president", "election", "cotton", "gold", "fever", "telegraph", "slavery",
         "steamboat", "church", "automobile", "radio", "prohibition", "farmers")
DAMAGE_BANDS = (0.1, 0.25)  # quality.DAMAGED, quality.BADLY_DAMAGED
LEGIBLE_BANDS = (0.25, 0.5, 0.75)  # share of text regions Illegible
READERS = 16
ONLY_SAMPLES = 8  # pages per term and year that match only in American Stories' text, kept for a hand check
FIRST_YEAR, LAST_YEAR = 1774, 1963  # the dataset's years


def parse(name: str) -> tuple[str, str, int, int, int | None] | None:
    """(lccn, date, edition, image, page or None) from a scan file's name, or None if it doesn't parse."""
    m = NAME.match(os.path.basename(name))
    if not m:
        return None
    date, page, lccn, _reel, ymd, ed, image = m.groups()
    if ymd != date.replace("-", ""):
        return None
    return lccn, date, int(ed), int(image), int(page) if page.isdigit() else None


def place(names: list[str]) -> tuple[dict[str, str], dict[str, int]]:
    """Each scan's doc_id by file name, and counts: pages named, placed by image, unparsed, duplicates, and
    the named pages the image rule (image less the issue's first, plus one) would have got right."""
    issues: dict[tuple, list[tuple[int, int | None, str]]] = {}
    stats = {"unparsed": 0, "page_named": 0, "page_from_image": 0, "duplicates": 0,
             "image_rule_checked": 0, "image_rule_agrees": 0}
    for name in names:
        got = parse(name)
        if got is None:
            stats["unparsed"] += 1
            continue
        lccn, date, ed, image, page = got
        issues.setdefault((lccn, date, ed), []).append((image, page, name))
    out: dict[str, str] = {}
    taken: set[str] = set()
    for (lccn, date, ed), scans in issues.items():
        first = min(image for image, _, _ in scans)
        offsets = [page - image for image, page, _ in scans if page is not None]
        # The named pages set the offset when there are any (the most common one), else the first image is p1.
        offset = max(set(offsets), key=offsets.count) if offsets else 1 - first
        for image, page, name in scans:
            if page is not None:
                stats["page_named"] += 1
                stats["image_rule_checked"] += 1
                stats["image_rule_agrees"] += image - first + 1 == page
            else:
                stats["page_from_image"] += 1
            seq = page if page is not None else image + offset
            d = f"{lccn}_{date}_ed-{ed}_seq-{seq}"
            if d in taken:
                stats["duplicates"] += 1
                continue
            taken.add(d)
            out[name] = d
    return out, stats


def scan_text(scan: dict) -> tuple[str, dict[str, int]]:
    """A scan's text (headline, byline and article of each article) and its text regions by legibility."""
    parts = []
    for a in scan.get("full articles") or []:
        parts.extend(t for t in (a.get("headline"), a.get("byline"), a.get("article")) if t)
    legibility: dict[str, int] = {}
    for b in scan.get("bboxes") or []:
        if b.get("class") in TEXT_CLASSES:
            k = (b.get("legibility") or "unknown").lower()
            legibility[k] = legibility.get(k, 0) + 1
    return "\n".join(parts), legibility


# Seconds a download may go without data before it fails. Without one, a stalled connection to
# Hugging Face blocked a writer run for hours with nothing logged (#218).
READ_TIMEOUT = 300


def _open(year: int, opener):
    req = urllib.request.Request(URL.format(year=year), headers={"User-Agent": jaocr.UA, "DNT": "1"})
    if opener:
        return opener(req)
    return urllib.request.urlopen(req, timeout=READ_TIMEOUT)


class IncompleteDownload(OSError):
    """A tarball that ended early (an OSError, so the writer retries the year)."""


class _Counting:
    """A response's bytes, counted as they're read."""

    def __init__(self, f):
        self.f, self.n = f, 0

    def read(self, size: int = -1) -> bytes:
        b = self.f.read(size)
        self.n += len(b)
        return b


@contextlib.contextmanager
def tar_stream(year: int, opener=None):
    """The year's tarball as a streamed tar. A download cut short doesn't always fail by itself: a body
    that stops at a member's end can read as a whole archive. So once the caller has read every member,
    the gzip stream is read to its end marker (gzip raises EOFError when it's missing, and checks the
    CRC), and the bytes read are compared with Content-Length when the response has one."""
    with _open(year, opener) as resp:
        raw = _Counting(resp)
        gz = gzip.GzipFile(fileobj=raw, mode="rb")
        with tarfile.open(fileobj=gz, mode="r|") as tar:
            yield tar
        while gz.read(1 << 20):  # the tar's end padding, then the gzip trailer
            pass
        while raw.read(1 << 20):
            pass
        headers = getattr(resp, "headers", None)
        length = headers.get("Content-Length") if headers is not None else None
        if length is not None and raw.n != int(length):
            raise IncompleteDownload(f"faro_{year}.tar.gz: read {raw.n} of {length} bytes")


def stream_year(year: int, cut: int, opener=None, keep=lambda: None) -> tuple[dict, dict]:
    """(sampled scans by doc_id: {batch, text, legibility}, totals) for one year's tarball, read twice."""
    names = []
    with tar_stream(year, opener) as tar:
        for member in tar:
            keep()  # each pass reads the whole tarball: renew the lock throughout
            if member.isfile() and member.name.endswith(".json"):
                names.append(member.name)
    ids, stats = place(names)
    wanted = {n: d for n, d in ids.items() if quality.sampled(d, cut)}
    out: dict[str, dict] = {}
    with tar_stream(year, opener) as tar:
        for member in tar:
            keep()
            d = wanted.get(member.name)
            if d is None:
                continue
            scan = json.load(tar.extractfile(member))
            m = BATCH.search((scan.get("scan") or {}).get("raw_data_loc") or "")
            text, legibility = scan_text(scan)
            out[d] = {"batch": m.group(1) if m else None, "text": text, "legibility": legibility}
    totals = {"scans": len(names), **stats, "sampled": len(out),
              "image_rule_agreement": share(stats["image_rule_agrees"], stats["image_rule_checked"])}
    return out, totals


def loc_pages(curated, batches: list[dict], years: set[int], cut: int, keep=lambda: None) -> dict[str, dict]:
    """LoC's text for our sampled pages dated in `years`: {doc_id: {lccn, year, batch, text}}."""
    import pyarrow.parquet as pq

    picked = [b for b in batches if (c := b.get("curated") or {}).get("parts")
              and any(int(c.get("first", "0")[:4] or 0) <= y <= int(c.get("last", "9999")[:4] or 9999)
                      for y in years)]
    parts = [p for b in picked for p in b["curated"]["parts"]]
    out: dict[str, dict] = {}
    for data in jaocr.read_all(curated, parts, READERS):
        keep()
        t = pq.read_table(io.BytesIO(data), columns=["doc_id", "lccn", "date", "batch", "text_status", "text"])
        # Only the sampled pages of the years become Python objects, not every page's text.
        rows = [i for i, (d, day) in enumerate(zip(t.column("doc_id").to_pylist(), t.column("date").to_pylist()))
                if day.year in years and quality.sampled(d, cut)]
        for r in t.take(rows).to_pylist() if rows else ():
            out[r["doc_id"]] = {"lccn": r["lccn"], "year": r["date"].year, "batch": r["batch"],
                                "text": r["text"] if r["text_status"] == "ok" else ""}
    return out


def context(text: str, term: str, around: int = 80) -> str:
    """The text around the first whole-word match of `term`, on one line."""
    text = quality.DEHYPHENATE.sub(r"\1", text)  # as quality.text_words reads it: "rail-\nroad" is railroad
    m = re.search(rf"(?i)\b{re.escape(term)}\b", text)
    if not m:
        return ""
    return " ".join(text[max(0, m.start() - around):m.end() + around].split())


def loc_url(doc_id: str) -> str:
    lccn, day, ed, seq = doc_id.split("_")
    return f"https://www.loc.gov/resource/{lccn}/{day}/{ed}/?sp={seq.removeprefix('seq-')}"


def terms_in(words: list[str]) -> set[str]:
    return set(words) & set(TERMS)


def band(x: float | None, edges: tuple) -> str:
    if x is None:
        return "none"
    for i, e in enumerate(edges):
        if x < e:
            return f"lt_{e}" if i == 0 else f"{edges[i - 1]}-{e}"
    return f"ge_{edges[-1]}"


def median(xs: list[float]) -> float | None:
    return round(statistics.median(xs), 4) if xs else None


def share(n: int, d: int) -> float | None:
    return round(n / d, 4) if d else None


def compare(year: int, ours: dict[str, dict], theirs: dict[str, dict], english: set[str], models) -> dict:
    """The year's rows: summary, recall by term, LoC damage by legibility band."""
    mine = {d: p for d, p in ours.items() if p["year"] == year and p["lccn"] in english}
    both = [d for d in mine if d in theirs]
    loc_damage, as_damage, loc_fs, as_fs, loc_words, as_words = [], [], [], [], [], []
    loc_bad = as_bad = 0  # of the pages with a damage rate (the audit leaves out the rest too)
    hits = {t: {"loc": 0, "american_stories": 0, "either": 0, "only_american_stories": 0} for t in TERMS}
    only: list[dict] = []  # pages a term matches only in American Stories' text, for a hand check
    by_legibility: dict[str, list[float]] = {}
    for d in both:
        lt, at = mine[d]["text"] or "", theirs[d]["text"] or ""
        ls, as_ = quality.score_v2(lt, ("eng",), models), quality.score_v2(at, ("eng",), models)
        loc_words.append(ls["words"])
        as_words.append(as_["words"])
        for s, dmg, fs in ((ls, loc_damage, loc_fs), (as_, as_damage, as_fs)):
            if s["damage_rate"] is not None:
                dmg.append(s["damage_rate"])
            if s["function_share"] is not None:
                fs.append(s["function_share"])
        loc_bad += (ls["damage_rate"] or 0) > quality.BADLY_DAMAGED
        as_bad += (as_["damage_rate"] or 0) > quality.BADLY_DAMAGED
        lw, aw = terms_in(quality.text_words(lt)[1]), terms_in(quality.text_words(at)[1])
        for t in TERMS:
            h = hits[t]
            h["loc"] += t in lw
            h["american_stories"] += t in aw
            h["either"] += t in lw or t in aw
            h["only_american_stories"] += t in aw and t not in lw
            if t in aw and t not in lw and sum(1 for o in only if o["term"] == t) < ONLY_SAMPLES:
                only.append({"year": year, "term": t, "doc_id": d, "url": loc_url(d), "context": context(at, t)})
        leg = theirs[d]["legibility"]
        regions = sum(leg.values())
        if ls["damage_rate"] is not None:
            by_legibility.setdefault(band(share(leg.get("illegible", 0), regions), LEGIBLE_BANDS), []).append(
                ls["damage_rate"])
    theirs_year = [d for d in theirs if d.split("_")[1][:4] == str(year)]
    summary = {
        "year": year,
        "our_sampled_english_pages": len(mine),
        "with_american_stories": len(both),
        "join_share": share(len(both), len(mine)),
        "american_stories_sampled_scans": len(theirs_year),
        "american_stories_without_our_page": sum(1 for d in theirs_year if d not in ours),
        "words_median_loc": median(loc_words), "words_median_american_stories": median(as_words),
        "words_total_ratio": share(sum(as_words), sum(loc_words)),
        "damage_median_loc": median(loc_damage), "damage_median_american_stories": median(as_damage),
        "scored_pages_loc": len(loc_damage), "scored_pages_american_stories": len(as_damage),
        "badly_damaged_share_loc": share(loc_bad, len(loc_damage)),
        "badly_damaged_share_american_stories": share(as_bad, len(as_damage)),
        "function_share_median_loc": median(loc_fs), "function_share_median_american_stories": median(as_fs),
    }
    recall = [{"year": year, "term": t, **h,
               "gain_share": share(h["only_american_stories"], h["loc"])} for t, h in hits.items()]
    legibility = [{"year": year, "illegible_regions": b, "pages": len(v), "loc_damage_median": median(v)}
                  for b, v in sorted(by_legibility.items())]
    return {"summary": summary, "recall": recall, "legibility": legibility, "only_american_stories": only}


# ---------------------------------------------------------------- --diff (#251)
#
# Would it do to index American Stories' text only where it differs from LoC's? A page's delta keeps, for
# each region where American Stories' tokens differ from LoC's, that region's American Stories tokens and
# k tokens of context each side (k in DIFF_KS) (a region LoC has more tokens in, with none of American Stories', keeps
# its context only), overlapping or touching windows merged. A term in American Stories' text is in LoC's or
# in a region, so terms keep their matches. A phrase of n words that matches only in American Stories' text
# overlaps a region or the place of LoC's extra tokens, so with k >= n - 1 a window holds it; the measurement
# checks that, and what smaller k loses and each k costs.

DIFF_KS = (0, 2, 3, 5)
COMMON_PHRASES = ("new york", "united states", "civil war", "cross of gold", "yellow fever", "red cross",
                  "supreme court", "great britain", "fourth of july", "ku klux klan")
PHRASE_WORDS = (2, 3)  # the n-grams measured over every page's text
FALLBACK_CELLS = 250_000  # a gap with no token unique to both sides is aligned by difflib up to this size
EDGE_SAMPLES = 10  # pages per year of a phrase whose words are all in LoC's text but together only in AS's
MAX_TOKEN_CHARS = 40  # usnm_core::text::MAX_TOKEN_CHARS

# usnm_core::text: normalize_ocr (NFC, ligatures, long s, words broken across lines), then tokenize (split on
# what isn't a letter or digit, fold, drop tokens over 40 characters).
_OCR = str.maketrans({"ſ": "s", "ﬀ": "ff", "ﬁ": "fi", "ﬂ": "fl", "ﬃ": "ffi", "ﬄ": "ffl", "ﬅ": "st", "ﬆ": "st"})
_FOLD = str.maketrans({"ſ": "s", "æ": "ae", "œ": "oe", "ø": "o", "ł": "l", "ß": "ss", "đ": "d", "ð": "d",
                       "þ": "th", "ı": "i", "ŋ": "ng", "ħ": "h"})
_HYPHEN = re.compile(r"(?<=[^\W\d_])-[ \t]*\n\s*(?=[^\W\d_])")
_ALNUM = re.compile(r"[^\W_]+")


@functools.lru_cache(maxsize=1 << 18)
def fold(token: str) -> str:
    """usnm_core::text::fold: NFKD without combining marks, lowercased, the rest of Lucene's ASCII folding."""
    s = "".join(c for c in unicodedata.normalize("NFKD", token) if not unicodedata.combining(c))
    return s.lower().translate(_FOLD)


def index_tokens(text: str) -> list[str]:
    """A text's tokens as the index has them (usnm_core::text, approximately)."""
    text = _HYPHEN.sub("", unicodedata.normalize("NFC", text).translate(_OCR))
    return [t for m in _ALNUM.finditer(text) if len(t := fold(m.group())) <= MAX_TOKEN_CHARS]


def _anchors(a: list, b: list, alo: int, ahi: int, blo: int, bhi: int) -> list[tuple[int, int]]:
    """Tokens once in a[alo:ahi] and once in b[blo:bhi], as (i, j), the longest run increasing in both."""
    ca, cb = collections.Counter(a[alo:ahi]), collections.Counter(b[blo:bhi])
    pos_b = {b[j]: j for j in range(blo, bhi) if cb[b[j]] == 1}
    pairs = [(i, pos_b[a[i]]) for i in range(alo, ahi) if ca[a[i]] == 1 and a[i] in pos_b]
    # Longest run increasing in j (patience sorting); pairs are in i order already.
    tail_j: list[int] = []  # tail_j[n]: the smallest last j of a run of n + 1
    tail_p: list[int] = []  # that run's last pair
    back = [-1] * len(pairs)
    for p, (_, j) in enumerate(pairs):
        n = bisect.bisect_left(tail_j, j)
        back[p] = tail_p[n - 1] if n else -1
        if n == len(tail_j):
            tail_j.append(j)
            tail_p.append(p)
        else:
            tail_j[n], tail_p[n] = j, p
    out, p = [], tail_p[-1] if tail_p else -1
    while p >= 0:
        out.append(pairs[p])
        p = back[p]
    return out[::-1]


def matching_blocks(a: list, b: list) -> tuple[list[tuple[int, int, int]], int]:
    """(blocks (i, j, n) with a[i:i+n] == b[j:j+n], increasing in i and j; gaps given up on). A patience diff:
    common ends, then tokens unique to both sides as anchors, splitting the rest into gaps; a gap with no
    such token goes to difflib (autojunk off) when small, else counts as changed whole. Near linear on OCR
    of the same page, where SequenceMatcher on the whole page is quadratic in its common words."""
    blocks: list[tuple[int, int, int]] = []
    gave_up = 0
    stack = [(0, len(a), 0, len(b))]
    while stack:
        alo, ahi, blo, bhi = stack.pop()
        n = 0
        while alo + n < ahi and blo + n < bhi and a[alo + n] == b[blo + n]:
            n += 1
        if n:
            blocks.append((alo, blo, n))
            alo, blo = alo + n, blo + n
        n = 0
        while alo < ahi - n and blo < bhi - n and a[ahi - 1 - n] == b[bhi - 1 - n]:
            n += 1
        if n:
            blocks.append((ahi - n, bhi - n, n))
            ahi, bhi = ahi - n, bhi - n
        if alo == ahi or blo == bhi:
            continue
        anchors = _anchors(a, b, alo, ahi, blo, bhi)
        if anchors:
            pi, pj = alo, blo
            for i, j in anchors:
                blocks.append((i, j, 1))
                stack.append((pi, i, pj, j))
                pi, pj = i + 1, j + 1
            stack.append((pi, ahi, pj, bhi))
        elif (ahi - alo) * (bhi - blo) <= FALLBACK_CELLS:
            sm = difflib.SequenceMatcher(None, a[alo:ahi], b[blo:bhi], autojunk=False)
            blocks += [(alo + i, blo + j, n) for i, j, n in sm.get_matching_blocks() if n]
        else:
            gave_up += 1
    blocks.sort()
    return blocks, gave_up


def diff_regions(blocks: list[tuple[int, int, int]], na: int, nb: int) -> list[tuple[int, int]]:
    """The regions (j1, j2) of b that differ from a, in order; j1 == j2 where a has tokens b hasn't."""
    out, pi, pj = [], 0, 0
    for i, j, n in [*blocks, (na, nb, 0)]:
        if i > pi or j > pj:
            out.append((pj, j))
        pi, pj = i + n, j + n
    return out


def windows(regions: list[tuple[int, int]], n: int, k: int) -> list[tuple[int, int]]:
    """Each region widened by k tokens each side, within [0, n), overlapping or touching ones merged."""
    out: list[tuple[int, int]] = []
    for j1, j2 in regions:
        s, e = max(0, j1 - k), min(n, j2 + k)
        if s >= e:
            continue
        if out and s <= out[-1][1]:
            out[-1] = (out[-1][0], max(out[-1][1], e))
        else:
            out.append((s, e))
    return out


def ngrams(tokens: list[str], n: int, lo: int = 0, hi: int | None = None) -> set[tuple[str, ...]]:
    seq = tokens[lo:hi]
    return set(zip(*(seq[i:] for i in range(n))))


def window_ngrams(tokens: list[str], wins: list[tuple[int, int]], n: int) -> set[tuple[str, ...]]:
    """The n-grams inside one window (none across two)."""
    out: set[tuple[str, ...]] = set()
    for s, e in wins:
        out |= ngrams(tokens, n, s, e)
    return out


def phrase_kept(tokens: list[str], wins: list[tuple[int, int]], phrase: tuple[str, ...]) -> bool:
    """Whether the phrase matches within one of the windows."""
    return phrase in window_ngrams(tokens, wins, len(phrase))


def page_diff(loc: list[str], ast: list[str], phrases: dict[int, set], ks=DIFF_KS) -> dict:
    """One page's measures: {loc_tokens, as_tokens, identical, gave_up, terms: {loc, as, as_only},
    ngrams: {n: {as_only, edge}}, phrases: {loc, as, edge}, by_k: {k: {...}}}. `phrases` by length (1 to 3),
    as tuples of tokens. An edge n-gram or phrase matches only in American Stories' text though each of its
    words is in LoC's."""
    blocks, gave_up = matching_blocks(loc, ast)
    regions = diff_regions(blocks, len(loc), len(ast))
    loc_terms, as_terms = set(loc), set(ast)
    loc_ng = {n: ngrams(loc, n) for n in PHRASE_WORDS}
    as_ng = {n: ngrams(ast, n) for n in PHRASE_WORDS}
    as_only = {n: as_ng[n] - loc_ng[n] for n in PHRASE_WORDS}
    edge = {n: {g for g in as_only[n] if all(t in loc_terms for t in g)} for n in PHRASE_WORDS}

    def matched(terms: set, ng: dict) -> set:
        return {g for g in phrases.get(1, ()) if g[0] in terms} | {
            g for n in PHRASE_WORDS for g in phrases.get(n, set()) & ng[n]}

    loc_ph, as_ph = matched(loc_terms, loc_ng), matched(as_terms, as_ng)
    only_ph = as_ph - loc_ph
    by_k = {}
    for k in ks:
        wins = windows(regions, len(ast), k)
        win_terms = {t for s, e in wins for t in ast[s:e]}
        win_ng = {n: window_ngrams(ast, wins, n) for n in PHRASE_WORDS}
        joined = [t for s, e in wins for t in ast[s:e]]
        joined_ng = {n: ngrams(joined, n) for n in PHRASE_WORDS}
        spurious = {n: joined_ng[n] - as_ng[n] - loc_ng[n] for n in PHRASE_WORDS}
        by_k[k] = {
            "delta_tokens": sum(e - s for s, e in wins), "windows": len(wins), "delta_terms": len(win_terms),
            "terms_lost": len(as_terms - loc_terms - win_terms),
            "ngrams": {n: {"kept": len(as_only[n] & win_ng[n]), "edge_kept": len(edge[n] & win_ng[n]),
                           "spurious_if_joined": len(spurious[n])} for n in PHRASE_WORDS},
            "phrases_kept": {g for g in only_ph if (g[0] in win_terms if len(g) == 1 else g in win_ng[len(g)])},
            "phrases_spurious": {g for n in PHRASE_WORDS for g in phrases.get(n, set()) & spurious[n]},
        }
    return {"loc_tokens": len(loc), "as_tokens": len(ast), "identical": not regions, "gave_up": gave_up,
            "terms": {"loc": len(loc_terms), "as": len(as_terms), "as_only": len(as_terms - loc_terms)},
            "ngrams": {n: {"as_only": len(as_only[n]), "edge": len(edge[n])} for n in PHRASE_WORDS},
            "phrases": {"loc": loc_ph, "as": as_ph, "edge": {g for g in only_ph if all(t in loc_terms for t in g)}},
            "by_k": by_k}


def context_phrases(contexts: list[dict]) -> set[tuple[str, ...]]:
    """The bigrams and trigrams of the only_american_stories contexts, less each context's first and last
    token (the context cuts words)."""
    out: set[tuple[str, ...]] = set()
    for c in contexts:
        toks = index_tokens(c.get("context") or "")[1:-1]
        for n in PHRASE_WORDS:
            out |= ngrams(toks, n)
    return out


def phrase_groups(contexts: list[dict]) -> dict[str, set[tuple[str, ...]]]:
    named = {"terms": {(t,) for t in TERMS}, "common": {tuple(index_tokens(p)) for p in COMMON_PHRASES}}
    ctx = context_phrases(contexts) - named["common"]
    return {**named, **{f"context_{n}_words": {g for g in ctx if len(g) == n} for n in PHRASE_WORDS}}


def _quantile(xs: list[float], q: float) -> float | None:
    if not xs:
        return None
    xs = sorted(xs)
    return round(xs[min(len(xs) - 1, int(q * len(xs)))], 4)


def diff_summary(year: int, pages: list[dict], secs: float, ks=DIFF_KS) -> list[dict]:
    """A row per k: the delta's size against American Stories' and LoC's text, and what it keeps."""
    rows = []
    loc_t, as_t = sum(p["loc_tokens"] for p in pages), sum(p["as_tokens"] for p in pages)
    terms = {f: sum(p["terms"][f] for p in pages) for f in ("loc", "as", "as_only")}
    for k in ks:
        ks_ = [p["by_k"][k] for p in pages]
        delta, delta_terms = sum(b["delta_tokens"] for b in ks_), sum(b["delta_terms"] for b in ks_)
        shares = [b["delta_tokens"] / p["as_tokens"] for p, b in zip(pages, ks_) if p["as_tokens"]]
        row = {"year": year, "k": k, "pages": len(pages),
               "pages_identical": sum(p["identical"] for p in pages),
               "pages_without_loc_text": sum(1 for p in pages if not p["loc_tokens"]),
               "align_gaps_given_up": sum(p["gave_up"] for p in pages),
               "loc_tokens": loc_t, "as_tokens": as_t, "delta_tokens": delta,
               "delta_share_of_as": share(delta, as_t), "delta_vs_loc": share(delta, loc_t),
               "page_delta_share_median": median(shares), "page_delta_share_p90": _quantile(shares, 0.9),
               "windows": sum(b["windows"] for b in ks_),
               "loc_terms": terms["loc"], "as_terms": terms["as"], "as_only_terms": terms["as_only"],
               "delta_terms": delta_terms, "delta_terms_share_of_as": share(delta_terms, terms["as"]),
               "terms_lost": sum(b["terms_lost"] for b in ks_),
               "diff_secs": round(secs, 1),
               "secs_per_1000_pages": round(1000 * secs / len(pages), 1) if pages else None}
        for n in PHRASE_WORDS:
            only = sum(p["ngrams"][n]["as_only"] for p in pages)
            kept = sum(b["ngrams"][n]["kept"] for b in ks_)
            edge = sum(p["ngrams"][n]["edge"] for p in pages)
            edge_kept = sum(b["ngrams"][n]["edge_kept"] for b in ks_)
            row.update({f"ngram{n}_as_only": only, f"ngram{n}_kept": kept, f"ngram{n}_loss": share(only - kept, only),
                        f"ngram{n}_edge": edge, f"ngram{n}_edge_kept": edge_kept,
                        f"ngram{n}_edge_loss": share(edge - edge_kept, edge),
                        f"ngram{n}_spurious_if_joined": sum(b["ngrams"][n]["spurious_if_joined"] for b in ks_)})
        rows.append(row)
    return rows


def diff_phrases(year: int, pages: list[dict], groups: dict[str, set], named: tuple = ("terms", "common"),
                 ks=DIFF_KS) -> tuple[list[dict], list[dict]]:
    """(a row per named phrase, a row per group), counting pages: the phrase matches in LoC's text, in
    American Stories', only in American Stories' (of those: with every word in LoC's text, the edge case),
    and of those only-AS pages, the ones whose delta keeps the match, per k; and pages where the delta's
    windows run together would match it falsely. A group row counts each page once: it matches if any of
    the group's phrases does, and the delta keeps it if it keeps every only-AS phrase of the group there."""
    def blank() -> dict:
        return {"loc": 0, "american_stories": 0, "only_american_stories": 0, "only_words_all_in_loc": 0,
                **{f"kept_k{k}": 0 for k in ks}, **{f"edge_kept_k{k}": 0 for k in ks},
                **{f"spurious_if_joined_k{k}": 0 for k in ks}}

    group_of = {g: name for name, gs in groups.items() for g in gs}
    per = {g: blank() for name in named for g in groups.get(name, ())}
    totals = {name: blank() for name in groups}
    for p in pages:
        ph = p["phrases"]
        page = {name: blank() for name in groups}  # this page's flags per group, added to totals once
        for g in ph["loc"] | ph["as"] | {g for b in p["by_k"].values() for g in b["phrases_spurious"]}:
            name = group_of.get(g)
            if name is None:
                continue
            only = g in ph["as"] and g not in ph["loc"]
            edge = g in ph["edge"]
            flags = {"loc": g in ph["loc"], "american_stories": g in ph["as"], "only_american_stories": only,
                     "only_words_all_in_loc": edge}
            for k in ks:
                kept = g in p["by_k"][k]["phrases_kept"]
                flags |= {f"kept_k{k}": kept, f"edge_kept_k{k}": edge and kept,
                          f"spurious_if_joined_k{k}": g in p["by_k"][k]["phrases_spurious"]}
            if g in per:
                for f, v in flags.items():
                    per[g][f] += v
            # A group counts a page once: it matches if any of its phrases does; the delta keeps the page's
            # only-AS (or edge) match only if it keeps every such phrase of the group on the page.
            c, first_only, first_edge = page[name], not page[name]["only_american_stories"], \
                not page[name]["only_words_all_in_loc"]
            for f in ("loc", "american_stories", "only_american_stories", "only_words_all_in_loc"):
                c[f] = c[f] or flags[f]
            for k in ks:
                if only:
                    c[f"kept_k{k}"] = flags[f"kept_k{k}"] if first_only else c[f"kept_k{k}"] and flags[f"kept_k{k}"]
                if edge:
                    c[f"edge_kept_k{k}"] = (flags[f"edge_kept_k{k}"] if first_edge
                                            else c[f"edge_kept_k{k}"] and flags[f"edge_kept_k{k}"])
                c[f"spurious_if_joined_k{k}"] = c[f"spurious_if_joined_k{k}"] or flags[f"spurious_if_joined_k{k}"]
        for name, c in page.items():
            for f, v in c.items():
                totals[name][f] += bool(v)

    def finish(c: dict) -> dict:
        return {**c, **{f"loss_k{k}": share(c["only_american_stories"] - c[f"kept_k{k}"], c["only_american_stories"])
                        for k in ks}}

    phrase_rows = [{"year": year, "phrase": " ".join(g), "group": group_of[g], "words": len(g), **finish(c)}
                   for g, c in sorted(per.items(), key=lambda x: (group_of[x[0]], x[0]))]
    group_rows = [{"year": year, "group": name, "phrases": len(groups[name]), **finish(c)}
                  for name, c in totals.items()]
    return phrase_rows, group_rows


def diff_year(year: int, ours: dict[str, dict], theirs: dict[str, dict], english: set[str],
              groups: dict[str, set], keep=lambda: None, ks=DIFF_KS) -> dict:
    """The --diff rows of a year, over the pages compare() reads (English titles, both texts)."""
    phrases: dict[int, set] = {}
    for gs in groups.values():
        for g in gs:
            phrases.setdefault(len(g), set()).add(g)
    pages, examples, secs = [], [], 0.0
    for d, p in ours.items():
        if p["year"] != year or p["lccn"] not in english or d not in theirs:
            continue
        keep()
        t0 = time.perf_counter()
        loc, ast = index_tokens(p["text"] or ""), index_tokens(theirs[d]["text"] or "")
        got = page_diff(loc, ast, phrases, ks)
        secs += time.perf_counter() - t0
        for g in sorted(got["phrases"]["edge"]):
            if len(examples) < EDGE_SAMPLES and len(g) > 1:
                examples.append({"year": year, "phrase": " ".join(g), "doc_id": d, "url": loc_url(d),
                                 **{f"kept_k{k}": g in got["by_k"][k]["phrases_kept"] for k in ks}})
        pages.append(got)
    phrase_rows, group_rows = diff_phrases(year, pages, groups, ks=ks)
    return {"diff_summary": diff_summary(year, pages, secs, ks), "diff_phrase": phrase_rows,
            "diff_phrase_group": group_rows, "diff_edge_examples": examples}


def american_stories(reference, curated, years: list[int], sample_pct: float = 10.0, run: str | None = None,
                     opener=None, loader=None, diff: bool = False) -> dict | None:
    current = json.loads(reference.read("current.json"))
    version = current["reference"]
    if not jaocr.SAFE_SEGMENT.match(version):
        raise ValueError(f"current.json names an unsafe reference version: {version!r}")
    if not years or any(not FIRST_YEAR <= y <= LAST_YEAR for y in years):
        raise ValueError(f"years must be {FIRST_YEAR} to {LAST_YEAR} (American Stories' range), not {years}")
    if not 0 < sample_pct <= 100:
        raise ValueError(f"--sample-pct must be above 0 and at most 100, not {sample_pct}")
    cut = round(sample_pct * 100)
    if cut < 1:
        raise ValueError(f"--sample-pct {sample_pct} samples nothing: use at least 0.01")
    solo = Solo(curated, f"audit/american-stories-{version}", "american stories", run)
    if not solo.start():
        return None
    titles = quality.load_titles(reference, version)
    english = {lccn for lccn, t in titles.items() if (t.get("languages") or ["eng"])[0] == "eng"}
    jaocr.log("american stories starting", version=version, years=years, sample_pct=sample_pct, diff=diff,
              run=solo.run)
    theirs, totals = {}, {}
    for y in years:
        got, totals[y] = stream_year(y, cut, opener, solo.keep)
        theirs.update(got)
        jaocr.log("american stories year read", year=y, **totals[y])
    ours = loc_pages(curated, json.loads(reference.read(f"{version}/batches.json")), set(years), cut, solo.keep)
    models = quality.Models(loader)
    tables: dict[str, list] = {"summary": [], "recall": [], "legibility": [], "only_american_stories": []}
    for y in years:
        rows = compare(y, ours, theirs, english, models)
        tables["summary"].append({**rows["summary"], **{f"scans_{k}": v for k, v in totals[y].items()}})
        tables["recall"] += rows["recall"]
        tables["legibility"] += rows["legibility"]
        tables["only_american_stories"] += rows["only_american_stories"]
    extra = {}
    if diff:
        groups = phrase_groups(tables["only_american_stories"])
        for y in years:
            rows = diff_year(y, ours, theirs, english, groups, solo.keep)
            for name, got in rows.items():
                tables.setdefault(name, []).extend(got)
            s = rows["diff_summary"][0] if rows["diff_summary"] else {}
            jaocr.log("american stories diff year", year=y, pages=s.get("pages", 0),
                      secs_per_1000_pages=s.get("secs_per_1000_pages"))
        extra = {"diff_ks": DIFF_KS, "diff_phrases": {n: len(g) for n, g in groups.items()},
                 "diff_common_phrases": COMMON_PHRASES}
    report = {"version": version, "years": years, "sample_pct": sample_pct, "terms": TERMS, **extra, **tables,
              "source": URL.format(year="<year>"), "license": "CC BY 4.0 (Dell et al. 2023)"}
    base = f"audit/american-stories-{version}-{solo.run}"
    curated.write(f"{base}.json", json.dumps(report, ensure_ascii=False, indent=1).encode())
    for name, rows in tables.items():
        for r in rows:
            jaocr.log("american stories", table=name, version=version, **r)
    solo.finish()
    jaocr.log("american stories finished", version=version, years=years, outputs=[f"{base}.json"])
    return report
