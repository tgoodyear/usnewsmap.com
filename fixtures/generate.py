#!/usr/bin/env python3
"""Generate the SYNTHETIC fixture corpus used for local development and tests.

Everything here is invented: place and title names are labelled "Fixture",
LCCNs use the unassigned `sn99…` range, and page text is templated. It must
never be presented as real Chronicling America data.

Usage: python3 fixtures/generate.py   (writes fixtures/data/…)
"""
import json
import random
from datetime import date, timedelta
from pathlib import Path

OUT = Path(__file__).parent / "data"
VERSION = "fixture-v1"
EPOCH = date(1700, 1, 1)

PLACES = [
    # id, ordinal, name, state, lat, lon, precision
    ("P00001", 1, "Fixture City A (Chicago area)", "IL", 41.88, -87.63, "city"),
    ("P00002", 2, "Fixture City B (New York area)", "NY", 40.71, -74.01, "city"),
    ("P00003", 3, "Fixture City C (San Francisco area)", "CA", 37.77, -122.42, "city"),
    ("P00004", 4, "Fixture City D (Atlanta area)", "GA", 33.75, -84.39, "city"),
    ("P00005", 5, "Fixture City E (Charleston area)", "SC", 32.78, -79.93, "city"),
    ("P00006", 6, "Fixture County F (Nebraska)", "NE", 41.26, -95.94, "county"),
]

FILLER = (
    "the council met on tuesday and the price of cotton rose while the railroad "
    "announced a new schedule for the northern line and the weather remained fair"
).split()

TOPICS = {
    "gold": "you shall not crucify mankind upon a cross of gold said the speaker to the convention",
    "silver": "the friends of free silver gathered to hear the orator on the money question",
    "standard": "bankers defended the gold standard against the silver movement in the east",
    "fever": "reports of yellow fever reached the port and the board of health met",
}

# Days after 1896-07-09 when each place first carries the speech (east first, then west).
SPREAD = {"P00001": 1, "P00002": 4, "P00004": 9, "P00005": 12, "P00003": 20, "P00006": 30}


def day_number(d):
    return (d - EPOCH).days


def page_text(rng, topics):
    words = []
    for _ in range(4):
        words += rng.sample(FILLER, 12)
        if topics:
            words += TOPICS[topics.pop(0)].split()
    words += rng.sample(FILLER, 10)
    return " ".join(words).capitalize() + "."


def main():
    rng = random.Random(1896)
    OUT.mkdir(parents=True, exist_ok=True)
    titles, docs_base, docs_delta, baselines = [], [], [], {}
    for pid, ordinal, name, state, *_ in PLACES:
        lccn = f"sn99{ordinal:06d}"
        titles.append({
            "lccn": lccn, "name": f"The Fixture Gazette {ordinal}", "place_id": pid,
            "state": state, "languages": ["eng"], "first": "1895-01-01", "last": "1897-12-31",
        })
        counts = {}
        d = date(1895, 1, 5)
        while d <= date(1897, 12, 31):
            for seq in (1, 2):
                topics = []
                if date(1896, 7, 9) + timedelta(days=SPREAD[pid]) <= d <= date(1896, 12, 31) and rng.random() < 0.8:
                    topics.append("gold")
                if date(1896, 1, 1) <= d <= date(1896, 11, 30) and rng.random() < 0.3:
                    topics.append("silver")
                if rng.random() < 0.1:
                    topics.append("standard")
                if state in ("GA", "SC") and date(1897, 8, 1) <= d <= date(1897, 10, 31) and rng.random() < 0.6:
                    topics.append("fever")
                text = "" if rng.random() < 0.02 else page_text(rng, topics)
                dn = day_number(d)
                counts[dn] = counts.get(dn, 0) + 1
                if not text:
                    continue  # empty OCR: counted in baselines, excluded from the index
                doc = {
                    "doc_id": f"{lccn}_{d.isoformat()}_ed-1_seq-{seq}",
                    "day": dn, "ym": d.year * 12 + d.month - 1, "year": d.year,
                    "place_id": pid, "place_shard": ordinal % 8, "lccn": lccn,
                    "state": state, "language": ["eng"], "front_page": seq == 1,
                    "edition": 1, "seq": seq, "text": text,
                }
                (docs_delta if d >= date(1897, 7, 1) else docs_base).append(doc)
            d += timedelta(days=7)
        baselines[pid] = sorted(counts.items())

    snap = OUT / VERSION
    snap.mkdir(exist_ok=True)
    (OUT / "indexes").mkdir(exist_ok=True)
    for index_id, docs in (("pages-base-fixture", docs_base), ("pages-delta-fixture-1", docs_delta)):
        with open(OUT / "indexes" / f"{index_id}.jsonl", "w") as f:
            for doc in docs:
                f.write(json.dumps(doc) + "\n")
    places = [
        {"id": p[0], "ordinal": p[1], "name": p[2], "state": p[3], "lat": p[4], "lon": p[5], "precision": p[6]}
        for p in PLACES
    ]
    (snap / "places.json").write_text(json.dumps(places, indent=1) + "\n")
    (snap / "titles.json").write_text(json.dumps(titles, indent=1) + "\n")
    (snap / "baselines.json").write_text(json.dumps(baselines) + "\n")
    current = {
        "index_version": VERSION,
        "backend": "memory",
        "indexes": ["pages-base-fixture", "pages-delta-fixture-1"],
        "reference": VERSION,
        "bounds": {"from": "1895-01-01", "to": "1897-12-31"},
        "published_at": "2026-09-27T00:00:00Z",
        "synthetic": True,
    }
    (OUT / "current.json").write_text(json.dumps(current, indent=1) + "\n")
    print(f"{len(docs_base)} base + {len(docs_delta)} delta docs, {len(titles)} titles")


if __name__ == "__main__":
    main()
