"""Write American Stories' text for our pages to the curated store (#218).

    python3 jaocr.py american-stories-write [--year 1865 ...]   (default: every year)

American Stories (Dell et al. 2023, arXiv:2308.12477; CC BY 4.0) is a re-OCR
of Chronicling America: one JSON file per scan in one tarball per year on
Hugging Face. This streams each year twice (nothing is kept on disk), first
to resolve scan identity using american_stories.place, then for text. Repeated
page labels and unnumbered scans require authoritative LoC issue images;
unresolved scan names/physical keys are retained in american-stories/unresolved/<year>.json,
not silently deduplicated or assigned guessed sequences. It writes one row per resolved page to

    american-stories/pages/<lccn>/<year>-<nnn>.parquet

with the page's text (each article's headline, byline and text, in the order
American Stories lists them, separated by blank lines), its article
structure (each article's headline, byline, start and end in the text, its
bounding boxes in scan pixels, and the legibility of its regions), the
page's text regions by legibility, and the scan's size. A page's second copy
(the same page in two batches) is skipped. A year is written once: a marker
american-stories/years/<year>.json with its counts is written last, and a
rerun skips the years that have one, so an interrupted run resumes.

The release reads these files to index the text beside LoC's (#218). One
replica of the execution works (solo.py); start it on the one-replica job
(ja-ocr/tools/start-quality.sh --env prod --command american-stories-write).
"""

from __future__ import annotations

import http.client
import io
import json
import re
import tarfile
import time
import urllib.request
import zlib
from datetime import date

import american_stories as ams
import jaocr
from solo import Solo

PREFIX = "american-stories"
PAGES = f"{PREFIX}/pages"
YEARS = f"{PREFIX}/years"
TREE = "https://huggingface.co/api/datasets/dell-research-harvard/AmericanStories/tree/main"
YEAR_FILE = re.compile(r"^faro_(\d{4})\.tar\.gz$")
FLUSH_ROWS = 2000  # a title's buffered pages written as one part (about 50 MB of text)
FLUSH_TOTAL = 20000  # every buffer written once this many pages are held
TEXT_CLASSES = ams.TEXT_CLASSES
SEPARATOR = "\n\n"
ATTEMPTS = 3  # tries per year: a download that stalls (ams.READ_TIMEOUT) or breaks off is started again
RETRY_SECONDS = 60
# What a broken or stalled download raises: timeouts and connection errors are OSError; a body cut
# short is IncompleteRead (http.client) or a truncated gzip or tar stream.
RETRYABLE = (OSError, EOFError, http.client.HTTPException, tarfile.TarError, zlib.error)


def box(b) -> list:
    """An article's bounding box as [x0, y0, x1, y1]: the files we've read (1793, 1925) give mappings; a
    positional list is taken as it is."""
    if isinstance(b, dict):
        return [b.get("x0"), b.get("y0"), b.get("x1"), b.get("y1")]
    return list(b)[:4]


def page_row(scan: dict) -> dict:
    """A scan's text, article structure, regions by legibility and size."""
    legibility_of = {b.get("id"): (b.get("legibility") or "unknown").lower() for b in scan.get("bboxes") or []}
    text, articles = "", []
    for a in scan.get("full articles") or []:
        headline, byline, body = (a.get(k) or "" for k in ("headline", "byline", "article"))
        segment = "\n".join(t for t in (headline, byline, body) if t)
        if not segment:
            continue
        if text:
            text += SEPARATOR
        start = len(text)
        text += segment
        articles.append({
            "headline": headline, "byline": byline, "start": start, "end": len(text),
            "bboxes": [box(b) for b in a.get("bbox_list") or []],
            "legibility": sorted({legibility_of.get(i, "unknown") for i in a.get("object_ids") or []}),
        })
    regions: dict[str, int] = {}
    for b in scan.get("bboxes") or []:
        if b.get("class") in TEXT_CLASSES:
            k = (b.get("legibility") or "unknown").lower()
            regions[k] = regions.get(k, 0) + 1
    size = scan.get("scan") or {}
    return {"text": text, "articles": json.dumps(articles, ensure_ascii=False, separators=(",", ":")),
            "legibility": json.dumps(regions, sort_keys=True, separators=(",", ":")),
            "width": int(size.get("width") or 0), "height": int(size.get("height") or 0)}


def years_available(opener=None) -> list[int]:
    """The years Hugging Face has a tarball for."""
    req = urllib.request.Request(TREE, headers={"User-Agent": jaocr.UA, "DNT": "1"})
    with (opener(req) if opener else urllib.request.urlopen(req, timeout=ams.READ_TIMEOUT)) as resp:
        files = json.load(resp)
    return sorted(int(m.group(1)) for f in files if (m := YEAR_FILE.match(f.get("path", ""))))


class Parts:
    """Pages buffered by title and written as parquet parts, numbered in the order they're written."""

    def __init__(self, curated, year: int):
        self.curated, self.year = curated, year
        self.rows: dict[str, list[dict]] = {}
        self.held = 0
        self.numbers: dict[str, int] = {}
        self.written = {"pages": 0, "parts": 0}

    def add(self, row: dict) -> None:
        got = self.rows.setdefault(row["lccn"], [])
        got.append(row)
        self.held += 1
        if len(got) >= FLUSH_ROWS:
            self.flush(row["lccn"])
        elif self.held >= FLUSH_TOTAL:
            self.flush_all()

    def flush(self, lccn: str) -> None:
        import pyarrow as pa
        import pyarrow.parquet as pq

        rows = self.rows.pop(lccn, [])
        if not rows:
            return
        self.held -= len(rows)
        n = self.numbers.get(lccn, 0)
        self.numbers[lccn] = n + 1
        schema = pa.schema([("scan_key", pa.string()), ("source_name", pa.string()), ("doc_id", pa.string()), ("lccn", pa.string()), ("date", pa.date32()),
                            ("text", pa.large_string()), ("articles", pa.large_string()),
                            ("legibility", pa.string()), ("width", pa.int32()), ("height", pa.int32())])
        buf = io.BytesIO()
        pq.write_table(pa.Table.from_pylist(rows, schema=schema), buf, compression="zstd")
        self.curated.write(f"{PAGES}/{lccn}/{self.year}-{n:03d}.parquet", buf.getvalue())
        self.written["pages"] += len(rows)
        self.written["parts"] += 1

    def flush_all(self) -> None:
        for lccn in list(self.rows):
            self.flush(lccn)


def write_year(curated, year: int, opener=None, keep=lambda: None) -> dict:
    """Stream one year and write its pages; the year's counts."""
    placement_path = f"{PREFIX}/placements/{year}.json"
    if curated.exists(placement_path):
        placement = json.loads(curated.read(placement_path))
        if placement.get("identity_version") != 2:
            raise ValueError(f"unsupported American Stories placement: {placement_path}")
    else:
        names: list[str] = []
        with ams.tar_stream(year, opener) as tar:
            for member in tar:
                keep()
                if member.isfile() and member.name.endswith(".json"):
                    names.append(member.name)
        unresolved: list[dict] = []
        ids, stats = ams.place(names, ams.issue_lookup(), unresolved, keep)
        placement = {"identity_version": 2, "ids": ids, "unresolved_scans": unresolved,
                     "counts": {"scans": len(names), **stats}}
        # Freeze placement BEFORE any text parts: retries/resumes must not change
        # part boundaries when LoC metadata availability changes between attempts.
        curated.write(placement_path, json.dumps(placement, sort_keys=True).encode())
    ids = placement["ids"]
    unresolved = placement["unresolved_scans"]
    parts = Parts(curated, year)
    written: set[str] = set()
    with ams.tar_stream(year, opener) as tar:
        for member in tar:
            keep()
            doc_id = ids.get(member.name) if member.isfile() else None
            if doc_id is None or doc_id in written:
                continue
            lccn, day, _ed, _image, _page = ams.parse(member.name)
            row = {"doc_id": doc_id, "lccn": lccn, "date": date.fromisoformat(day),
                   "scan_key": json.dumps(ams.scan_key(member.name), separators=(",", ":")),
                   "source_name": member.name, **page_row(json.load(tar.extractfile(member)))}
            parts.add(row)
            written.add(doc_id)
    if written != set(ids.values()):
        raise ValueError(f"American Stories {year} archive no longer matches its frozen placement")
    parts.flush_all()
    curated.write(f"{PREFIX}/unresolved/{year}.json", json.dumps(unresolved, sort_keys=True).encode())
    return {**placement["counts"], **parts.written, "identity_version": 2}


def write_year_retrying(curated, year: int, opener=None, keep=lambda: None) -> dict:
    """write_year, started again from the top (up to ATTEMPTS times) when the download fails. A retry
    writes the same parts under the same names (the tarball is read in the same order), so a part
    from the failed try is overwritten, not doubled."""
    for attempt in range(1, ATTEMPTS + 1):
        try:
            return write_year(curated, year, opener, keep)
        except RETRYABLE as e:
            if attempt == ATTEMPTS:
                raise
            jaocr.log("american stories year failed; retrying", year=year, attempt=attempt,
                      error=f"{type(e).__name__}: {e}"[:300])
            time.sleep(RETRY_SECONDS)
    raise AssertionError("unreachable")


def write(reference, curated, years: list[int] | None = None, run: str | None = None, opener=None) -> dict | None:
    current = json.loads(reference.read("current.json"))
    version = current["reference"]
    if not jaocr.SAFE_SEGMENT.match(version):
        raise ValueError(f"current.json names an unsafe reference version: {version!r}")
    available = years_available(opener)
    if years:
        missing = sorted(set(years) - set(available))
        if missing:
            raise ValueError(f"American Stories has no file for {missing}")
    todo = sorted(set(years or available))
    solo = Solo(curated, f"{PREFIX}/write", "american stories write", run)
    if not solo.start():
        return None
    jaocr.log("american stories write starting", years=len(todo), first=todo[0], last=todo[-1], run=solo.run)
    totals = {"years_written": 0, "years_skipped": 0, "pages": 0, "parts": 0}
    for year in todo:
        marker = f"{YEARS}/{year}.json"
        if curated.exists(marker):
            totals["years_skipped"] += 1
            continue
        got = write_year_retrying(curated, year, opener, solo.keep)
        curated.write(marker, json.dumps({"year": year, **got}, sort_keys=True).encode())
        jaocr.log("american stories year written", year=year, **got)
        totals["years_written"] += 1
        totals["pages"] += got["pages"]
        totals["parts"] += got["parts"]
    solo.finish()
    jaocr.log("american stories write finished", **totals)
    return totals
