#!/usr/bin/env python3
"""S0's truth-free page check on the GHN pages (#2128): per page, the boxes, the lines called Cherokee, and
`chr_layout_qa.py`'s figures: the merged-lines share (boxes over 1.8 times the page's median height), the
wide share and the share of the page's ink inside the boxes. A page is flagged when its merged-lines share
is over --flag-share (0.08 by default: the level the #1622 report used for the twelve GHN pages, five of
which were over it).

    s0_stats.py --classified classified.jsonl --scans DIR --layout ghn|loc --test-pages ghn-test-pages.txt
                --out-pages qa.jsonl --out-summary summary.json [--flag-share 0.08] [--jobs N]
"""
import argparse
import concurrent.futures as cf
import json
import statistics

import common
import chr_layout_qa as qa  # noqa: E402


def do_page(args):
    rec, scan = args
    from PIL import Image
    Image.MAX_IMAGE_PIXELS = None
    q = qa.page_qa(Image.open(scan).convert("L"), rec["lines"])
    return {"page": rec["page"], "boxes": len(rec["lines"]), "raw_boxes": rec.get("raw_lines"),
            "chr_lines": sum(bool(l.get("chr")) for l in rec["lines"]), **q}


def summarise(rows, flag):
    flagged = sorted(r["page"] for r in rows if r["tall_share"] > flag)
    return {"pages": len(rows), "boxes": sum(r["boxes"] for r in rows), "chr_lines": sum(r["chr_lines"] for r in rows),
            "pages_with_chr": sum(r["chr_lines"] > 0 for r in rows),
            "median_boxes": statistics.median(r["boxes"] for r in rows) if rows else None,
            "median_tall_share": statistics.median(r["tall_share"] for r in rows) if rows else None,
            "flag_share": flag, "flagged": len(flagged), "flagged_pages": flagged,
            "flagged_chr_lines": sum(r["chr_lines"] for r in rows if r["tall_share"] > flag)}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--classified", required=True)
    ap.add_argument("--scans", required=True)
    ap.add_argument("--layout", choices=["ghn", "loc"], required=True)
    ap.add_argument("--test-pages", required=True)
    ap.add_argument("--out-pages", required=True)
    ap.add_argument("--out-summary", required=True)
    ap.add_argument("--flag-share", type=float, default=0.08)
    ap.add_argument("--jobs", type=int, default=1)
    a = ap.parse_args()
    recs = common.read_jsonl(a.classified)
    with cf.ProcessPoolExecutor(a.jobs) as pool:
        rows = list(pool.map(do_page, [(r, common.scan_path(a.scans, r["page"], a.layout)) for r in recs]))
    common.write_jsonl(a.out_pages, rows)
    test = set(common.read_pages(a.test_pages))
    out = {"all": summarise(rows, a.flag_share),
           "test": summarise([r for r in rows if r["page"] in test], a.flag_share),
           "train": summarise([r for r in rows if r["page"] not in test], a.flag_share)}
    json.dump(out, open(a.out_summary, "w"), indent=1, sort_keys=True)
    print(json.dumps({k: {x: y for x, y in v.items() if x != "flagged_pages"} for k, v in out.items()}, indent=1))


if __name__ == "__main__":
    main()
