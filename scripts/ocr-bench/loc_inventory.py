#!/usr/bin/env python3
"""Inventory published LoC reprocessing coverage without treating it as page-level proof."""
import argparse
from collections import Counter
from datetime import date
import hashlib
from html.parser import HTMLParser
import json
from pathlib import Path
import re
from urllib.parse import parse_qs, urlparse

GUIDE = "https://guides.loc.gov/chronicling-america/improved-text"
BATCH = re.compile(r"[a-z0-9_]+_ver\d+\Z")


class Tables(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.rows = []
        self.row = None
        self.cell = None
        self.links = []

    def handle_starttag(self, tag, attrs):
        if tag == "tr":
            self.row = []
        elif tag in ("td", "th"):
            self.cell, self.links = [], []
        elif tag == "a" and self.cell is not None:
            self.links.append(dict(attrs).get("href", ""))
        elif tag == "br" and self.cell is not None:
            self.cell.append(" ")

    def handle_data(self, data):
        if self.cell is not None:
            self.cell.extend((data, " "))

    def handle_endtag(self, tag):
        if tag in ("td", "th") and self.cell is not None:
            if self.row is not None:
                self.row.append((" ".join("".join(self.cell).split()), self.links))
            self.cell = None
        elif tag == "tr":
            if self.row:
                self.rows.append(self.row)
            self.row = None


def sha(data):
    return hashlib.sha256(data).hexdigest()


def coverage(data):
    parser = Tables()
    parser.feed(data.decode("utf-8"))
    batches = {}
    for cells in parser.rows:
        if len(cells) != 5 or not re.fullmatch(r"\d{4}-\d{2}-\d{2}", cells[0][0]):
            continue
        added = date.fromisoformat(cells[0][0]).isoformat()
        name = cells[2][0]
        if not BATCH.fullmatch(name) or name in batches:
            raise ValueError("invalid or duplicate reprocessed batch")
        pages = int(cells[3][0].replace(",", ""))
        if pages <= 0:
            raise ValueError("nonpositive published page count")
        # Split by title links, not a single min/max year for a multi-title batch.
        content = cells[4][0]
        titles = []
        matches = list(re.finditer(r"\((sn\d+)\)", content))
        for number, match in enumerate(matches):
            end = matches[number + 1].start() if number + 1 < len(matches) else len(content)
            years = [int(year) for year in re.findall(r"\b(?:17|18|19|20)\d{2}\b", content[match.end():end])]
            titles.append({"lccn": match[1], "years": sorted(set(years))})
        if not titles:
            raise ValueError("reprocessed batch has no title identifiers")
        batches[name] = {"batch": name, "added": added, "pages_reported": pages,
                         "contributor": cells[1][0], "content": content, "titles": titles}
    if not batches:
        raise ValueError("no reprocessed batch table found; source structure may have changed")
    total = re.search(r"Total Reprocessed Pages to Date:\s*([\d,]+)", data.decode("utf-8"))
    reported = int(total[1].replace(",", "")) if total else None
    counted = sum(row["pages_reported"] for row in batches.values())
    if reported is not None and reported != counted:
        raise ValueError("published total differs from parsed table")
    return {"format": "usnm-loc-reprocessed-inventory/1", "source_url": GUIDE,
            "source_sha256": sha(data), "batches": list(batches.values()),
            "batch_count": len(batches), "pages_reported": counted,
            "latest_added": max(row["added"] for row in batches.values()),
            "scope": "published guide snapshot, not exhaustive page-level or engine-version proof"}


def candidates(inventory, lccn, year, batch=None):
    result = []
    for row in inventory["batches"]:
        if batch and re.sub(r"_ver\d+$", "", batch) == re.sub(r"_ver\d+$", "", row["batch"]):
            result.append(row["batch"])
            continue
        for title in row["titles"]:
            years = title["years"]
            if title["lccn"] == lccn and (not years or min(years) <= year <= max(years)):
                result.append(row["batch"])
                break
    return sorted(set(result))


def benchmark_overlap(inventory, manifest):
    rows = []
    for sample in manifest["samples"]:
        provenance = sample["provenance"]
        url = urlparse(provenance.get("page_url", ""))
        match = re.fullmatch(r"/resource/(sn\d+)/(\d{4}-\d{2}-\d{2})/ed-(\d+)/?", url.path)
        if url.hostname != "www.loc.gov" or not match:
            rows.append({"id": sample["id"], "source_page_id": sample["source_page_id"],
                         "status": "outside-ndnp", "candidate_batches": []})
            continue
        seq = parse_qs(url.query).get("sp", [])
        if len(seq) != 1 or not seq[0].isdigit() or int(seq[0]) <= 0:
            raise ValueError("missing unambiguous page sequence")
        batches = candidates(inventory, match[1], int(match[2][:4]))
        rows.append({"id": sample["id"], "source_page_id": sample["source_page_id"],
                     "doc_id": f"{match[1]}_{match[2]}_ed-{match[3]}_seq-{seq[0]}",
                     "languages": sample["languages"], "source": sample["source"],
                     "image_sha256": sample["image"]["sha256"],
                     "reference_sha256": sample["reference"]["sha256"],
                     "candidate_batches": batches,
                     "status": "candidate-needs-page-verification" if batches else "not-listed-in-guide"})
    if len({row["id"] for row in rows}) != len(rows):
        raise ValueError("duplicate benchmark sample identity")
    subsets = {}
    by_id = {row["id"]: row for row in rows}
    for name, subset in manifest["subsets"].items():
        selected = [by_id[identity] for identity in subset["members"]]
        subsets[name] = {"samples": len(selected),
                         "source_pages": len({row["source_page_id"] for row in selected}),
                         "statuses": dict(Counter(row["status"] for row in selected))}
    return {"rows": rows, "subsets": subsets,
            "warning": "not-listed is not proof of no reprocessing; candidates are not confirmed page coverage"}


def repair_overlap(inventory, pages):
    counts = Counter()
    seen = set()
    candidate_batches = Counter()
    strata = Counter()
    keep = Counter()
    cache = {}
    for row in pages:
        identity = row["doc_id"]
        if identity in seen:
            raise ValueError("duplicate repair page")
        seen.add(identity)
        year = int(row["date"][:4])
        key = row["lccn"], year, row["batch"]
        if key not in cache:
            cache[key] = candidates(inventory, *key)
        batches = cache[key]
        status = "candidate-needs-page-verification" if batches else "not-listed-in-guide"
        counts[status] += 1
        strata[(row["lccn"], row["lang"], year // 10 * 10, status)] += 1
        if row["keep"]:
            keep[status] += 1
        for batch in batches:
            candidate_batches[batch] += 1
    return {"pages": len(seen), "statuses": dict(counts),
            "keep_statuses": dict(keep), "reference_status": "not-in-manifest",
            "candidate_batch_counts": dict(candidate_batches),
            "strata": [{"lccn": title, "lang": lang, "decade": decade, "status": status, "pages": count}
                       for (title, lang, decade, status), count in sorted(strata.items())]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--coverage-html", type=Path, required=True)
    parser.add_argument("--benchmark", type=Path, required=True)
    parser.add_argument("--repair-pages", type=Path)
    parser.add_argument("--retrieved-at", required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--summary-out", type=Path)
    args = parser.parse_args()
    result = coverage(args.coverage_html.read_bytes())
    result["retrieved_at"] = args.retrieved_at
    data = args.benchmark.read_bytes()
    result["benchmark_manifest_sha256"] = sha(data)
    result["benchmark"] = benchmark_overlap(result, json.loads(data))
    if args.repair_pages:
        digest = hashlib.sha256()
        with args.repair_pages.open("rb") as source:
            for chunk in iter(lambda: source.read(1 << 20), b""):
                digest.update(chunk)
        result["repair_input_sha256"] = digest.hexdigest()
        with args.repair_pages.open() as source:
            result["repair"] = repair_overlap(result, (json.loads(line) for line in source))
    with args.out.open("x") as output:
        json.dump(result, output, indent=2, sort_keys=True)
        output.write("\n")
    if args.summary_out:
        result["benchmark"].pop("rows")
        if "repair" in result:
            result["repair"]["strata_count_in_full_report"] = len(result["repair"].pop("strata"))
        with args.summary_out.open("x") as output:
            json.dump(result, output, indent=2, sort_keys=True)
            output.write("\n")
    print(json.dumps({key: result[key] for key in ("batch_count", "pages_reported", "latest_added")}))


if __name__ == "__main__":
    main()
