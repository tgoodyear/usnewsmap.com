"""How much English and Japanese the Japanese titles' pages hold that search can't reach yet.

    python3 jaocr.py mixed

The Japanese OCR job reads a page only when LoC's text for it is missing,
empty, short or under 35% word-like (jaocr.needs_ocr); a Japanese query
searches only our OCR, any other query only LoC's text. So two kinds of text
can fall through:

1. Japanese on pages LoC read as words. A page with an English column and a
   Japanese one, read in English, has real words for the first and garbled
   Latin for the second, and can pass the 35% test. For every page with LoC
   text of a title that lists Japanese, in the batches holding such titles,
   this counts pages by word-like share (the same test). The other titles in
   those batches are the control: same scanning and OCR, English only. An
   excess of Japanese titles' pages in the middle bands is the mixed pages.
2. English on the pages we read. NDLOCR-Lite is a Japanese engine; what it
   reads of an English column sits only in the Japanese index, which English
   queries don't search. For every page of our OCR (ocr-ja/pages), this counts
   the tokens of 3 letters or more that are among English's 5,000 most
   frequent words, and the Latin share of its letters.

Writes audit/mixed-pages-<version>.json (every table) and a CSV per table in
the curated store, and logs each row as a "mixed pages" line. One replica of
an execution does the work (a lock per execution); the others exit.
"""

from __future__ import annotations

import csv
import hashlib
import io
import json
import os
import re
from concurrent.futures import ThreadPoolExecutor

import jaocr
import quality

# Word-like share bands, low to high: under 0.35 is a target of our OCR already.
BANDS = (0.35, 0.5, 0.65, 0.8, 0.9)
BAND_NAMES = ("lt_0.35", "0.35-0.5", "0.5-0.65", "0.65-0.8", "0.8-0.9", "ge_0.9")
# Real English words found in our OCR of a page, low to high.
WORD_BANDS = (1, 10, 50, 200)
WORD_BAND_NAMES = ("0", "1-9", "10-49", "50-199", "ge_200")
SAMPLES = 15  # page ids kept per band and group, for looking at the scans
READERS = 16  # parallel blob reads
LATIN_WORD = re.compile(r"[A-Za-z]{3,}")
OURS = f"{jaocr.PREFIX}/pages"


def band(x: float, edges=BANDS, names=BAND_NAMES) -> str:
    for edge, name in zip(edges, names):
        if x < edge:
            return name
    return names[-1]


def english_words(text: str, top: frozenset) -> int:
    return sum(1 for w in LATIN_WORD.findall(text) if w.lower() in top)


def latin_share(text: str) -> float | None:
    """Latin letters over all letters, or None with no letters."""
    latin = letters = 0
    for c in text:
        if c.isalpha():
            letters += 1
            latin += c.isascii()
    return latin / letters if letters else None


def english_top() -> frozenset:
    import wordfreq

    return frozenset(w for w in wordfreq.top_n_list("en", 5000) if len(w) >= 3)


def loc_url(doc_id: str) -> str:
    lccn, date, ed, seq = doc_id.split("_")
    return f"https://www.loc.gov/resource/{lccn}/{date}/{ed}/?sp={seq.removeprefix('seq-')}"


class Samples:
    """A stable sample of page ids per key: the SAMPLES with the smallest hash."""

    def __init__(self):
        self.by_key: dict[tuple, list[tuple[bytes, str]]] = {}

    def add(self, key: tuple, doc_id: str) -> None:
        h = hashlib.blake2b(doc_id.encode(), digest_size=8).digest()
        got = self.by_key.setdefault(key, [])
        got.append((h, doc_id))
        if len(got) > 4 * SAMPLES:
            got.sort()
            del got[SAMPLES:]

    def rows(self, fields: tuple[str, ...]) -> list[dict]:
        out = []
        for key in sorted(self.by_key):
            for _, doc_id in sorted(self.by_key[key])[:SAMPLES]:
                out.append({**dict(zip(fields, key)), "doc_id": doc_id, "url": loc_url(doc_id)})
        return out


def loc_pages(curated, batches: list[dict], jpn: set[str], names: dict[str, str]):
    """(1) LoC's text on pages of titles that list Japanese, and on the other titles in their batches."""
    import pyarrow.parquet as pq

    counts: dict[tuple[str, str], dict[str, int]] = {}  # (group, lccn) -> band -> pages
    samples = Samples()
    parts = [p for b in batches for p in b["curated"]["parts"]]
    with ThreadPoolExecutor(READERS) as ex:
        for data in ex.map(curated.read, parts):
            t = pq.read_table(io.BytesIO(data), columns=["doc_id", "lccn", "text_status", "text"])
            for r in t.to_pylist():
                if r["text_status"] != "ok":
                    continue
                group = "japanese" if r["lccn"] in jpn else "control"
                b = band(jaocr.wordlike_share(r["text"] or ""))
                row = counts.setdefault((group, r["lccn"]), dict.fromkeys(BAND_NAMES, 0))
                row[b] += 1
                if group == "japanese" and b != BAND_NAMES[0]:
                    samples.add((b,), r["doc_id"])
    by_title, totals = [], {}
    for (group, lccn), row in sorted(counts.items()):
        pages = sum(row.values())
        total = totals.setdefault(group, dict.fromkeys(BAND_NAMES, 0))
        for k, v in row.items():
            total[k] += v
        if group == "japanese":
            by_title.append({"lccn": lccn, "name": names.get(lccn, ""), "pages_with_loc_text": pages, **row})
    summary = []
    for group in ("japanese", "control"):
        total = totals.get(group, dict.fromkeys(BAND_NAMES, 0))
        pages = sum(total.values())
        summary.append({"group": group, "pages_with_loc_text": pages, **total,
                        **{f"share_{k}": round(v / pages, 4) if pages else None for k, v in total.items()}})
    return summary, by_title, samples.rows(("band",))


def our_pages(curated, jpn_names: dict[str, str], top: frozenset):
    """(2) The English our OCR read on the pages it read."""
    import pyarrow.parquet as pq

    newest: dict[str, tuple] = {}  # doc_id -> (ocred_at, lccn, loc_text, words, latin)
    parts = sorted(p for p in curated.list(OURS) if p.endswith(".parquet"))
    with ThreadPoolExecutor(READERS) as ex:
        for data in ex.map(curated.read, parts):
            t = pq.read_table(io.BytesIO(data), columns=["doc_id", "lccn", "loc_text", "text", "ocred_at"])
            for r in t.to_pylist():
                at = r["ocred_at"]
                if r["doc_id"] in newest and newest[r["doc_id"]][0] >= at:
                    continue
                text = r["text"] or ""
                newest[r["doc_id"]] = (at, r["lccn"], r["loc_text"], english_words(text, top), latin_share(text))
    by_loc_text: dict[str, dict[str, int]] = {}
    by_title: dict[str, dict[str, int]] = {}
    samples = Samples()
    for doc_id, (_, lccn, loc_text, words, _) in newest.items():
        b = band(words, WORD_BANDS, WORD_BAND_NAMES)
        by_loc_text.setdefault(loc_text, dict.fromkeys(WORD_BAND_NAMES, 0))[b] += 1
        by_title.setdefault(lccn, dict.fromkeys(WORD_BAND_NAMES, 0))[b] += 1
        if b in WORD_BAND_NAMES[2:]:
            samples.add((b,), doc_id)
    rows = [{"loc_text": k, "pages": sum(v.values()), **v} for k, v in sorted(by_loc_text.items())]
    titles = [{"lccn": k, "name": jpn_names.get(k, ""), "pages": sum(v.values()), **v}
              for k, v in sorted(by_title.items())]
    return rows, titles, samples.rows(("english_words",)), len(parts)


def write_csv(rows: list[dict]) -> bytes:
    buf = io.StringIO()
    w = csv.DictWriter(buf, fieldnames=list(rows[0]) if rows else ["empty"], lineterminator="\n")
    w.writeheader()
    w.writerows(rows)
    return buf.getvalue().encode()


def mixed(reference, curated, top: frozenset | None = None, run: str | None = None) -> dict | None:
    current = json.loads(reference.read("current.json"))
    version = current["reference"]
    if not jaocr.SAFE_SEGMENT.match(version):
        raise ValueError(f"current.json names an unsafe reference version: {version!r}")
    owner = os.environ.get("CONTAINER_APP_REPLICA_NAME") or os.environ.get("HOSTNAME") or "local"
    run = run or quality.run_name(owner)
    if not jaocr.SAFE_SEGMENT.match(run):
        raise ValueError(f"unsafe run name: {run!r}")
    if not curated.create(f"audit/mixed-pages-{version}.run/{run}.lock", json.dumps({"owner": owner}).encode()):
        jaocr.log("mixed pages running in another replica", run=run)
        return None
    titles = json.loads(reference.read(f"{version}/titles.json"))
    if reference.exists("catalog/titles.json"):
        titles = titles + json.loads(reference.read("catalog/titles.json"))
    names = {t["lccn"]: t.get("name", "") for t in titles}
    jpn = jaocr.japanese_lccns(titles)
    batches = [b for b in json.loads(reference.read(f"{version}/batches.json"))
               if jpn & set((b.get("curated") or {}).get("lccns") or [])]
    jaocr.log("mixed pages starting", version=version, japanese_titles=len(jpn), batches=len(batches))
    loc_summary, loc_titles, loc_samples = loc_pages(curated, batches, jpn, names)
    ours, ours_titles, ours_samples, ours_parts = our_pages(curated, {k: names[k] for k in jpn if k in names},
                                                            top if top is not None else english_top())
    tables = {"loc_text_bands": loc_summary, "loc_text_by_title": loc_titles, "loc_text_samples": loc_samples,
              "our_ocr_english": ours, "our_ocr_by_title": ours_titles, "our_ocr_samples": ours_samples}
    base = f"audit/mixed-pages-{version}"
    report = {"version": version, "batches": len(batches), "our_parts": ours_parts,
              "bands": dict(zip(BAND_NAMES, (0,) + BANDS)), "word_bands": WORD_BAND_NAMES, **tables}
    curated.write(f"{base}.json", json.dumps(report, ensure_ascii=False, indent=1).encode())
    for name, rows in tables.items():
        curated.write(f"{base}-{name.replace('_', '-')}.csv", write_csv(rows))
        for r in rows:
            jaocr.log("mixed pages", table=name, version=version, **r)
    jaocr.log("mixed pages finished", version=version, batches=len(batches), our_parts=ours_parts,
              outputs=[f"{base}.json"])
    return report
