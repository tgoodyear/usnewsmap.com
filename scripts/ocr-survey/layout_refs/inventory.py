#!/usr/bin/env python3
"""Inventory of the layout-reference JSONL files (#2107): description only, nothing is scored.

Reads every <refs-dir>/*.jsonl written by the converters and writes one JSON file with, per dataset
and split: page, title, language and year counts; region, separator and gutter counts; how the
gutters look; and how many pages can serve as each kind of reference.

Definitions (a page "serves" a kind of reference when):
  columns        it has at least one column gutter (gaps between the cleanest text class) or one
                 vertical separator; columns_multi: at least 3 such (a multi-column page)
  nontext        it has at least one non-text region (pages with none are only a negative
                 reference if the dataset's non-text labels are complete, see notes in the README)
  reading_order  at least 2 regions in reading_order
  articles       at least one article; articles_multi: at least 2
Gutter shape: gutters_per_page (median, p90) counts chains per page; gap_width_frac is the width of a
gutter segment over page width, height-weighted over every segment (quantiles and a histogram);
gutter_height_frac_per_page is the summed segment height over page height.

    inventory.py --refs-dir DIR --out FILE
"""
import argparse
import glob
import json
import os
import sys
from collections import Counter

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import common  # noqa: E402

GAP_BINS = [0.0, 0.005, 0.01, 0.015, 0.02, 0.03, 0.04, 0.06, 0.08, 0.12, 1.0001]


def quantile(vals, q):
    if not vals:
        return None
    v = sorted(vals)
    k = (len(v) - 1) * q
    lo = int(k)
    hi = min(lo + 1, len(v) - 1)
    return round(v[lo] + (v[hi] - v[lo]) * (k - lo), 5)


def weighted_quantile(pairs, q):
    """pairs: (value, weight)."""
    if not pairs:
        return None
    pairs = sorted(pairs)
    tot = sum(w for _, w in pairs)
    acc = 0.0
    for v, w in pairs:
        acc += w
        if acc >= q * tot:
            return round(v, 5)
    return round(pairs[-1][0], 5)


def gap_stats(chains_by_page, widths, heights):
    """Height-weighted gap width (fraction of page width) over all segments, plus a histogram."""
    pairs = []
    for chains, w in zip(chains_by_page, widths):
        for ch in chains:
            for x0, x1, y0, y1 in ch:
                if y1 > y0 and w:
                    pairs.append(((x1 - x0) / w, y1 - y0))
    hist = [0.0] * (len(GAP_BINS) - 1)
    for g, wt in pairs:
        for i in range(len(GAP_BINS) - 1):
            if GAP_BINS[i] <= g < GAP_BINS[i + 1]:
                hist[i] += wt
                break
    tot = sum(hist) or 1.0
    return {
        "bins_upper_edges": GAP_BINS[1:],
        "histogram_height_fraction": [round(h / tot, 4) for h in hist],
        "p10": weighted_quantile(pairs, 0.10), "p50": weighted_quantile(pairs, 0.50),
        "p90": weighted_quantile(pairs, 0.90), "segments": len(pairs),
    }


def height_frac(chains, h):
    return sum(y1 - y0 for ch in chains for _, _, y0, y1 in ch) / h if h else 0.0


def summarise(rows):
    n = len(rows)
    out = {"pages": n}
    first = rows[0]
    out["licence"] = first["licence"]
    out["for_publication"] = all(r["for_publication"] for r in rows)
    titles = Counter()
    for r in rows:
        m = r["meta"]
        t = m.get("title") or m.get("newspaper_name") or m.get("title_code") or m.get("lccn") or m.get("title")
        if t:
            titles[t] += 1
    out["titles"] = len(titles) if titles else None
    out["languages"] = dict(Counter(r["meta"].get("language") for r in rows if r["meta"].get("language")))
    years = [r["meta"]["year"] for r in rows if isinstance(r["meta"].get("year"), int)]
    out["years"] = {"min": min(years), "max": max(years), "pages_with_year": len(years)} if years else None
    for key in ("text_regions", "nontext_regions"):
        c = Counter(reg["class"] for r in rows for reg in r[key])
        out[key] = {"by_class": dict(sorted(c.items())), "total": sum(c.values()),
                    "pages_with_any": sum(1 for r in rows if r[key])}
    seps = [s for r in rows for s in r["separators"]]
    out["separators"] = {"total": len(seps),
                         "vertical": sum(1 for s in seps if s.get("orientation") == "vertical"),
                         "pages_with_any": sum(1 for r in rows if r["separators"])}
    for key in ("gutters", "column_gutters"):
        counts = [len(r[key]) for r in rows]
        chains = [r[key] for r in rows]
        out[key] = {
            "total": sum(counts), "pages_with_any": sum(1 for c in counts if c),
            "per_page": {"median": quantile(counts, 0.5), "p90": quantile(counts, 0.9),
                         "max": max(counts)},
            "height_frac_per_page": {"median": quantile([height_frac(r[key], r["height"]) for r in rows], 0.5),
                                     "p90": quantile([height_frac(r[key], r["height"]) for r in rows], 0.9)},
            "gap_width_frac": gap_stats(chains, [r["width"] for r in rows], [r["height"] for r in rows]),
        }

    def n_cols(r):
        return len(r["column_gutters"]) + sum(1 for s in r["separators"] if s.get("orientation") == "vertical")
    out["pages_can_serve_as"] = {
        "columns": sum(1 for r in rows if n_cols(r) >= 1),
        "columns_multi": sum(1 for r in rows if n_cols(r) >= 3),
        "nontext": sum(1 for r in rows if r["nontext_regions"]),
        "reading_order": sum(1 for r in rows if len(r["reading_order"]) >= 2),
        "articles": sum(1 for r in rows if r["articles"]),
        "articles_multi": sum(1 for r in rows if len(r["articles"]) >= 2),
    }
    out["articles"] = {"total": sum(len(r["articles"]) for r in rows)}
    probs = Counter()
    bad_rows = 0
    for r in rows:
        chk = common.check_row(r)
        if chk:
            bad_rows += 1
            for p in chk:
                probs[p] += 1
        for p in r["meta"].get("problems") or []:
            probs["meta:" + p.split(" ")[0] if not p.startswith("zone") else "meta:zone"] += 1
    out["data_problems"] = {"rows_failing_check": bad_rows,
                            "rows_with_meta_problems": sum(1 for r in rows if r["meta"].get("problems")),
                            "kinds": dict(sorted(probs.items()))}
    out["image_size"] = {"median_width": quantile([r["width"] for r in rows], 0.5),
                         "median_height": quantile([r["height"] for r in rows], 0.5)}
    ident = [r for r in rows if r["meta"].get("ca_identity")]
    if ident or any("ca_identity_parsed" in r["meta"] for r in rows):
        out["chronicling_america_identity"] = {
            "pages_parsed": len(ident), "pages_total": n,
            "distinct_lccn": len({r["meta"]["ca_identity"]["lccn"] for r in ident}),
        }
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--refs-dir", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args(argv)
    inv = {}
    for path in sorted(glob.glob(os.path.join(a.refs_dir, "*.jsonl"))):
        rows = list(common.read_jsonl(path))
        if not rows:
            continue
        ds, sp = rows[0]["dataset"], rows[0]["split"]
        inv.setdefault(ds, {})[sp] = summarise(rows)
        common.log("inventory: %s %s %d pages" % (ds, sp, len(rows)))
    with open(a.out, "w") as f:
        json.dump(inv, f, sort_keys=True, indent=1)
        f.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
