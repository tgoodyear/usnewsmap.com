"""Steps after a finder (#1622), on `found` files; JSON Lines on standard output.

  clean  --found found.jsonl --truth truth.jsonl [--min-h 14]
         Drops what is not a line of print: a box under --min-h pixels high, and a box wider than half its page
         that is under 1.5 times the page's median line height (a rule or a smear). The page width is the truth
         file's `width` field (for GHN pages, the manifest's).
  union  --primary found.jsonl --secondary found.jsonl
         Every box of the primary, and the secondary's boxes that hold print the primary missed: a plausible line
         size (0.6 to 1.8 times the primary's median height on that page, at least 60 pixels wide) with its
         centre in no primary box.
"""
import argparse
import json
import statistics


def inside(box, other):
    cx, cy = (box[0] + box[2]) / 2, (box[1] + box[3]) / 2
    return other[0] <= cx <= other[2] and other[1] <= cy <= other[3]


def union(primary, secondary):
    hs = [l["box"][3] - l["box"][1] for l in primary if l["box"][3] - l["box"][1] >= 8]
    med = statistics.median(hs) if hs else 40
    out = list(primary)
    for l in secondary:
        b = l["box"]
        h, w = b[3] - b[1], b[2] - b[0]
        if 0.6 * med <= h <= 1.8 * med and w >= 60 and not any(inside(b, p["box"]) for p in primary):
            out.append({**l, "from": "secondary"})
    return out


def clean(lines, page_w, min_h):
    hs = [l["box"][3] - l["box"][1] for l in lines if l["box"][3] - l["box"][1] >= min_h]
    med = statistics.median(hs) if hs else 40
    out = []
    for l in lines:
        h, w = l["box"][3] - l["box"][1], l["box"][2] - l["box"][0]
        if h < min_h or (w > 0.5 * page_w and h < 1.5 * med):
            continue
        out.append(l)
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("clean")
    c.add_argument("--found", required=True)
    c.add_argument("--truth", required=True)
    c.add_argument("--min-h", type=int, default=14)
    u = sub.add_parser("union")
    u.add_argument("--primary", required=True)
    u.add_argument("--secondary", required=True)
    a = ap.parse_args()
    if a.cmd == "clean":
        widths = {r["page"]: r["width"] for r in map(json.loads, open(a.truth))}
        for r in map(json.loads, open(a.found)):
            print(json.dumps({"page": r["page"], "lines": clean(r["lines"], widths[r["page"]], a.min_h)}, sort_keys=True))
        return
    sec = {r["page"]: r["lines"] for r in map(json.loads, open(a.secondary))}
    for r in map(json.loads, open(a.primary)):
        if r["page"] in sec:
            print(json.dumps({"page": r["page"], "lines": union(r["lines"], sec[r["page"]])}, sort_keys=True))


if __name__ == "__main__":
    main()
