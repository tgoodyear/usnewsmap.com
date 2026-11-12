#!/usr/bin/env python3
"""The held-out draw of the reader comparison (#968, docs/reports/2026-11-reader-comparison/protocol.md).

    reader_draw.py --dataset newseye-nlf-fi --src DIR --out NEW_DIR --exclude-pages EXCLUDED.txt
                   [--count 60] [--per-page 3] [--tag reader-cmp-202611]

Writes what `gold.py public-convert` writes, but picks the regions by the protocol's rule instead of
gold.py's seeded shuffle: pages not in the exclusion list, in order of sha256(TAG:dataset:page_id);
on each page up to --per-page regions inside public-convert's length window for the dataset, in order
of sha256(TAG:dataset:region); until --count crops or no pages are left. The exclusion list is one
page_id per line. Crop cutting, manifest rows, licence files and the validity checks are gold.py's.
"""

from __future__ import annotations

import argparse
import hashlib
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import gold  # noqa: E402


def digest(tag: str, dataset: str, key: str) -> str:
    return hashlib.sha256(f"{tag}:{dataset}:{key}".encode("utf-8")).hexdigest()


def draw(regions: list[dict], excluded: set[str], dataset: str, tag: str, count: int, per_page: int,
         lo: int, hi: int) -> list[dict]:
    """The protocol's draw, over regions of `public_regions`."""
    by_page: dict[str, list[dict]] = {}
    for r in regions:
        if r["page_id"] in excluded:
            continue
        if lo <= len(gold.normalize(r["text"])) <= hi:
            by_page.setdefault(r["page_id"], []).append(r)
    picked: list[dict] = []
    for page in sorted(by_page, key=lambda p: (digest(tag, dataset, p), p)):
        rows = sorted(by_page[page], key=lambda r: (digest(tag, dataset, str(r["region"])), str(r["region"])))
        for r in rows[:per_page]:
            if len(picked) < count:
                picked.append(r)
        if len(picked) >= count:
            break
    return picked


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dataset", required=True, choices=sorted(gold.PUBLIC))
    ap.add_argument("--src", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--exclude-pages", type=Path, help="page_id per line, never drawn from")
    ap.add_argument("--count", type=int, default=60)
    ap.add_argument("--per-page", type=int, default=3)
    ap.add_argument("--tag", default="reader-cmp-202611")
    a = ap.parse_args(argv)
    excluded = set(a.exclude_pages.read_text(encoding="utf-8").split()) if a.exclude_pages else set()

    def pick(regions, count, per_page, seed, lo, hi):  # gold.pick_public's signature
        return draw(regions, excluded, a.dataset, a.tag, count, per_page, lo, hi)

    gold.pick_public = pick
    ns = argparse.Namespace(dataset=a.dataset, src=a.src, out=a.out, count=a.count, per_page=a.per_page,
                            seed=0, min_chars=None, max_chars=None, not_for_scoring=False)
    return gold.cmd_public_convert(ns)


if __name__ == "__main__":
    sys.exit(main())
