"""Which titles have pages LoC ships without OCR text (#135).

    python3 jaocr.py audit [--all-languages]

LoC's per-title "Pages (Full Text)" count on loc.gov includes every page,
with or without text. Our count (the published snapshot's title_pages.json,
built from each batch's counts.json) has only the pages whose archive entry
has an ocr.txt. A title where LoC's count is larger has pages with no OCR
text from LoC: Rocky Shimpo has 1,012 pages on loc.gov and 256 with text.

By default it checks the titles whose catalog languages include anything
but English (about 400 requests, ~25 minutes at loc.gov's pace); with
--all-languages, every catalog title (~4,700, ~4.5 hours). It writes
audit/loc-pages-<version>.csv to the curated store, sorted by pages
missing, and logs a summary and the titles with the largest gaps.

Titles whose batches aren't all in the published version (still waiting for
title details, say) are flagged `unpublished_batches`: their gap isn't (only)
missing text. One replica runs it: the first to create
audit/loc-pages-<version>.lock.
"""

from __future__ import annotations

import csv
import io
import json
import os
import urllib.parse
from datetime import datetime, timezone

import jaocr

FACET = "https://www.loc.gov/collections/chronicling-america/?fo=json&c=1&at=facets&fa="


def loc_pages(facets: dict) -> int | None:
    """'Pages (Full Text)' from a loc.gov facets response, or None if absent."""
    for f in facets.get("facets") or []:
        if f.get("type") == "object-type":
            for x in f.get("filters") or []:
                if x.get("title") == "Pages (Full Text)":
                    return int(x["count"])
    return None


def rows(titles: list[dict], ours: dict[str, int], loc: dict[str, int | None],
         published_batches: set[str], datasets: list[dict]) -> list[dict]:
    batches_of: dict[str, set[str]] = {}
    for d in datasets:
        for lccn in d.get("lccns") or []:
            batches_of.setdefault(lccn, set()).add(d["batch"])
    out = []
    for t in titles:
        lccn = t["lccn"]
        if lccn not in loc:
            continue
        theirs, mine = loc[lccn], ours.get(lccn, 0)
        missing = None if theirs is None else theirs - mine
        unpublished = sorted(b for b in batches_of.get(lccn, ()) if b not in published_batches)
        out.append({
            "lccn": lccn, "name": t.get("name", ""), "languages": " ".join(t.get("languages") or []),
            "loc_pages": "" if theirs is None else theirs, "our_pages": mine,
            "missing": "" if missing is None else missing,
            "missing_share": "" if not theirs else round(missing / theirs, 3),
            "unpublished_batches": " ".join(unpublished),
        })
    out.sort(key=lambda r: (-(r["missing"] if isinstance(r["missing"], int) else -1), r["lccn"]))
    return out


def audit(reference, curated, all_languages: bool) -> None:
    current = json.loads(reference.read("current.json"))
    version = current["reference"]
    if not jaocr.SAFE_SEGMENT.match(version):
        raise ValueError(f"current.json names an unsafe reference version: {version!r}")
    lock = f"audit/loc-pages-{version}.lock"
    if not curated.create(lock, json.dumps({"at": datetime.now(timezone.utc).isoformat()}).encode()):
        jaocr.log("audit already running or done for this version; nothing to do", version=version)
        return
    ours = json.loads(reference.read(f"{version}/title_pages.json"))
    titles = json.loads(reference.read("catalog/titles.json"))
    if not all_languages:
        titles = [t for t in titles if set(t.get("languages") or []) - {"eng"}]
    published = {f"{b['batch']}_ver{b['curated']['version']:02d}" for b in
                 json.loads(reference.read(f"{version}/batches.json"))}
    api = jaocr.Pacer(float(os.environ.get("JAOCR_API_GAP", "3.5")))
    datasets = json.loads(jaocr.fetch(jaocr.COLLECTION, api))["datasets"]
    jaocr.log("audit starting", version=version, titles=len(titles), all_languages=all_languages)
    loc: dict[str, int | None] = {}
    for i, t in enumerate(titles, 1):
        url = FACET + urllib.parse.quote(f"number_lccn:{t['lccn']}")
        try:
            loc[t["lccn"]] = loc_pages(json.loads(jaocr.fetch(url, api)))
        except Exception as e:  # noqa: BLE001 - one title mustn't end the audit
            jaocr.log("title failed", lccn=t["lccn"], error=f"{type(e).__name__}: {e}"[:300])
            loc[t["lccn"]] = None
        if i % 50 == 0:
            jaocr.log("audit progress", done=i, of=len(titles))
    table = rows(titles, ours, loc, published, datasets)
    buf = io.StringIO()
    w = csv.DictWriter(buf, fieldnames=list(table[0]) if table else ["lccn"])
    w.writeheader()
    w.writerows(table)
    path = f"audit/loc-pages-{version}.csv"
    curated.write(path, buf.getvalue().encode())
    gaps = [r for r in table if isinstance(r["missing"], int) and r["missing"] > 0]
    jaocr.log("audit finished", version=version, titles=len(table), with_gap=len(gaps),
              pages_missing=sum(r["missing"] for r in gaps), csv=path)
    for r in gaps[:60]:
        jaocr.log("audit gap", **r)
