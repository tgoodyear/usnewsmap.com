#!/usr/bin/env python3
"""Survey which Georgia Historic Newspapers (GHN) titles are not in Chronicling America
(CA) or in our published dataset.

Inputs: GHN's public title listing (/newspapers/, allowed by its robots.txt), our CA title
list (reference container, raw/titles.json: every title CA has digitized) and the published
snapshot's batches.json (the LCCNs we have pages for). Nothing is asked of loc.gov, whose
Cloudflare challenge blocks bulk lookups.

Each GHN title lands in one bucket:
  ours          pages in our published dataset
  ca_not_ours   in CA but not in our dataset (an ingestion gap)
  ghn_only      not in CA: GHN is the only source

Usage: ghn-gap-survey.py --ca-titles titles.json --batches batches.json --out OUTDIR
Get the inputs with `az storage blob download --auth-mode login --account-name ACCOUNT
-c reference -n raw/titles.json` and the same for `<version>/batches.json`, the version
being the one current.json names.
"""
import argparse
import csv
import html
import json
import re
import urllib.request
from pathlib import Path

UA = "usnewsmap-ghn-gap-survey (trevor@goodyeartechnical.com)"
GHN_LIST = "https://gahistoricnewspapers.galileo.usg.edu/newspapers/"


def fetch(url):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read().decode("utf-8", "replace")


def parse_ghn(page):
    titles = []
    for row in page.split("<tr>"):
        m = re.search(r'href="/lccn/([^/"]+)/" class="newspaper-link-bold">([^<]*)', row)
        if not m:
            continue
        county = re.search(r'np-col-county">\s*(.*?)\s*</td>', row, re.S)
        dates = re.findall(r'href="/lccn/[^/"]+/(\d{4}-\d\d-\d\d)/', row)
        issues = re.search(r"(\d[\d,]*) issues?", row)
        titles.append({
            "lccn": m[1],
            "title": html.unescape(m[2]).strip(),
            "county": re.sub(r"<br/>|\s+", " ", county[1]).strip() if county else "",
            "first": dates[0] if dates else "",
            "last": dates[-1] if dates else "",
            "issues": int(issues[1].replace(",", "")) if issues else "",
        })
    return titles


def our_lccns(path):
    data = json.loads(Path(path).read_text())
    batches = data if isinstance(data, list) else data["batches"]
    return {q for b in batches for q in b["curated"].get("lccns", [])}


def classify(ghn, ca, ours):
    for t in ghn:
        t["bucket"] = "ours" if t["lccn"] in ours else "ca_not_ours" if t["lccn"] in ca else "ghn_only"
    return ghn


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ca-titles", required=True)
    ap.add_argument("--batches", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)

    ca = json.loads(Path(a.ca_titles).read_text())
    ghn = classify(parse_ghn(fetch(GHN_LIST)), ca, our_lccns(a.batches))

    fields = ["bucket", "lccn", "title", "county", "first", "last", "issues"]
    with (out / "ghn-gap.csv").open("w", newline="") as f:
        w = csv.DictWriter(f, fields)
        w.writeheader()
        w.writerows(sorted(ghn, key=lambda t: (t["bucket"], -(t["issues"] or 0))))
    counts = {}
    for t in ghn:
        counts[t["bucket"]] = counts.get(t["bucket"], 0) + 1
    (out / "summary.json").write_text(json.dumps({"ghn_titles": len(ghn), "buckets": counts}, indent=2))
    print(json.dumps(counts))


if __name__ == "__main__":
    main()
