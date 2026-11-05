#!/usr/bin/env python3
"""Freeze the Fraktur pilot's page manifests (#325) from read-only corpus metadata.

    sample.py --spec ocr/fraktur-pilot/spec.json --manifest manifest.parquet \\
        --sidecars index-full.jsonl [--audit-pages pages.csv.gz] [--scan-listing listing.tsv] \\
        --out ocr/fraktur-pilot
    sample.py --check --spec ocr/fraktur-pilot/spec.json --out ocr/fraktur-pilot

The spec (spec.json) declares the seed, the frame, each sample's language, decades and size,
and the exclusions. The inputs are copies of what the scan host already holds: the prefetch
manifest (parquet, or JSONL with the same columns), the scan fetcher's sidecar index (one
sidecar JSON per line), optionally the original 10% audit's page list (CSV with a doc_id
column, optionally gzipped) and a listing of the scan cache (`<path under the scans root>\\t<bytes>`
per line). It writes pages.jsonl (IDs, audit membership and image hashes; no OCR text, no
scores), summary.json (input digests, frame and exclusion counts by decade, and every
stratum's eligible and selected counts, from which the design weights follow) and SHA256SUMS.
The same spec and inputs give the same bytes. It reads local files only: no network, no queue.

--check re-verifies a frozen directory without the inputs: the checksums, the spec digest,
each record's fields, keys, scan path and audit bit, and that the records add up to the
summary's counts and allocation.
"""

import argparse
import csv
import gzip
import hashlib
import json
import sys
from collections import Counter, defaultdict
from pathlib import Path

SPEC_FORMAT = "usnm-fraktur-pilot-spec/1"
SUMMARY_FORMAT = "usnm-fraktur-pilot-summary/1"
PAGES, SUMMARY, SUMS = "pages.jsonl", "summary.json", "SHA256SUMS"
# The order exclusions are tested in; a page is counted under the first that applies.
REASONS = (
    "outside_era",
    "mixed_language",
    "below_damage",
    "no_sidecar",
    "sidecar_mismatch",
    "scan_missing",
    "duplicate_image",
)
EXT = {"jp2": "jp2", "jpeg": "jpg"}
RECORD_FIELDS = (
    "audit_10pct",
    "audit_scored",
    "batch",
    "batch_version",
    "date",
    "decade",
    "doc_id",
    "edition",
    "image_id",
    "lccn",
    "sample",
    "scan_bytes",
    "scan_format",
    "scan_md5",
    "scan_path",
    "scan_resolution",
    "scan_sha256",
    "selection_key",
    "seq",
    "stratum_rank",
)
MANIFEST_COLUMNS = ("doc_id", "batch", "batch_version", "lccn", "date", "edition", "seq", "lang", "decision", "damage_rate")


def canonical(obj):
    return json.dumps(obj, indent=2, sort_keys=True, ensure_ascii=False) + "\n"


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def selection_key(seed, doc_id):
    return hashlib.sha256(f"{seed}:{doc_id}".encode()).hexdigest()


def title_key(seed, lccn):
    return hashlib.sha256(f"{seed}:title:{lccn}".encode()).hexdigest()


def audit_member(doc_id, cut):
    """ocr/ja-ocr/quality.py sampled(): the original page audit's membership rule."""
    h = int.from_bytes(hashlib.blake2b(doc_id.encode(), digest_size=8).digest(), "big")
    return h % 10_000 < cut


def decade_of(date):
    return int(date[:4]) // 10 * 10


def scan_path(row, set_dir, fmt):
    return f"{set_dir}/{row['batch']}/{row['lccn']}/{row['date']}/{row['edition']}/{row['seq']}.{EXT[fmt]}"


def load_spec(path):
    raw = Path(path).read_bytes()
    spec = json.loads(raw)
    if spec.get("format") != SPEC_FORMAT:
        raise SystemExit(f"error: {path}: format is not {SPEC_FORMAT}")
    for name, s in spec["samples"].items():
        if not s["decades"] or s["decades"] != sorted(set(s["decades"])) or s["size"] < 1:
            raise SystemExit(f"error: {path}: sample {name} needs sorted distinct decades and a positive size")
    langs = [s["lang"] for s in spec["samples"].values()]
    if len(langs) != len(set(langs)):
        raise SystemExit(f"error: {path}: two samples share a language")
    return spec, hashlib.sha256(raw).hexdigest()


def read_manifest(path):
    """The prefetch manifest's rows, with dates as YYYY-MM-DD and only the columns used."""
    path = Path(path)
    if path.suffix == ".parquet":
        import pyarrow.parquet as pq

        for batch in pq.ParquetFile(path).iter_batches(columns=list(MANIFEST_COLUMNS)):
            for row in batch.to_pylist():
                row["date"] = row["date"].isoformat()
                yield row
        return
    with open(path, encoding="utf-8") as f:
        for line in f:
            if line.strip():
                row = json.loads(line)
                yield {k: row.get(k) for k in MANIFEST_COLUMNS}


def read_sidecars(path):
    """doc_id -> sidecar; a later line for the same page replaces an earlier one, as a re-fetch
    rewrites the sidecar. A torn last line (the index is appended while it is copied) is counted,
    not fatal."""
    sidecars, lines, unparsed, repeats = {}, 0, 0, 0
    with open(path, "rb") as f:
        for line in f:
            lines += 1
            try:
                side = json.loads(line)
                doc_id = side["doc_id"]
            except (ValueError, KeyError, TypeError):
                unparsed += 1
                continue
            repeats += doc_id in sidecars
            sidecars[doc_id] = side
    return sidecars, {"lines": lines, "pages": len(sidecars), "repeated_pages": repeats, "unparsed_lines": unparsed}


def read_audit(path):
    opener = gzip.open if str(path).endswith(".gz") else open
    with opener(path, "rt", encoding="utf-8", newline="") as f:
        return {row["doc_id"] for row in csv.DictReader(f)}


def read_listing(path):
    files = {}
    with open(path, encoding="utf-8") as f:
        for line in f:
            if line.strip():
                name, size = line.rstrip("\n").rsplit("\t", 1)
                files[name] = int(size)
    return files


def check_scan(row, side, listing, frame):
    """The first scan exclusion that applies to a page, or None; and the page's scan record."""
    if side is None:
        return "no_sidecar", None
    fmt = side.get("format", "jpeg")
    if (
        side.get("doc_id") != row["doc_id"]
        or side.get("batch") != row["batch"]
        or side.get("batch_version") != row["batch_version"]
        or side.get("resolution") != frame["resolution"]
        or fmt not in frame["formats"]
        or fmt not in EXT
        or (fmt == "jp2" and not side.get("md5"))
        or not side.get("sha256")
    ):
        return "sidecar_mismatch", None
    path = scan_path(row, frame["set"], fmt)
    if listing is not None and (listing.get(path) != side.get("bytes") or path + ".json" not in listing):
        return "scan_missing", None
    return None, {
        "image_id": side.get("iiif_base"),
        "scan_bytes": side.get("bytes"),
        "scan_format": fmt,
        "scan_md5": side.get("md5"),
        "scan_path": path,
        "scan_resolution": side["resolution"],
        "scan_sha256": side["sha256"],
    }


def allocate(total, order, capacity):
    """Split `total` equally over `order`, the remainder to the first; a member with less
    capacity than its share takes all it has and the rest is split again among the others."""
    alloc, open_, left = {}, list(order), total
    while open_ and left > 0:
        share, extra = divmod(left, len(open_))
        want = {k: share + (i < extra) for i, k in enumerate(open_)}
        short = [k for k in open_ if capacity[k] < want[k]]
        if not short:
            alloc.update(want)
            left = 0
            break
        for k in short:
            alloc[k] = capacity[k]
            left -= capacity[k]
        open_ = [k for k in open_ if k not in short]
    return {k: alloc.get(k, 0) for k in order}


def draw(spec, rows, sidecars, audit, listing):
    """The selected records and the summary's per-sample counts."""
    seed, frame, cut = spec["seed"], spec["frame"], spec["audit"]["cut"]
    by_lang = {s["lang"]: name for name, s in spec["samples"].items()}
    frame_pages = Counter()
    excluded = {name: defaultdict(Counter) for name in spec["samples"]}
    eligible = {name: [] for name in spec["samples"]}
    for row in rows:
        name = by_lang.get(row["lang"])
        if name is None:
            continue
        frame_pages[name] += 1
        decade = decade_of(row["date"])
        if decade not in spec["samples"][name]["decades"]:
            reason, scan = "outside_era", None
        elif row["decision"] not in frame["decisions"]:
            reason, scan = "mixed_language", None
        elif row["damage_rate"] is None or row["damage_rate"] < frame["min_damage"]:
            reason, scan = "below_damage", None
        else:
            reason, scan = check_scan(row, sidecars.get(row["doc_id"]), listing, frame)
        if reason:
            excluded[name][reason][decade] += 1
            continue
        eligible[name].append((selection_key(seed, row["doc_id"]), decade, row, scan))
    # A scan shared by two pages counts once, in any samples: the page with the lower key keeps it.
    first = {}
    for name in spec["samples"]:
        for key, decade, row, scan in eligible[name]:
            sha = scan["scan_sha256"]
            if sha not in first or key < first[sha]:
                first[sha] = key
    records, samples = [], {}
    for name, s in spec["samples"].items():
        strata = defaultdict(list)
        for key, decade, row, scan in eligible[name]:
            if first[scan["scan_sha256"]] != key:
                excluded[name]["duplicate_image"][decade] += 1
                continue
            strata[(decade, row["lccn"])].append((key, row, scan))
        per_decade = {d: sum(len(v) for (dd, _), v in strata.items() if dd == d) for d in s["decades"]}
        decade_alloc = allocate(s["size"], s["decades"], per_decade)
        decades, stratum_list = {}, []
        for d in s["decades"]:
            titles = sorted((lccn for dd, lccn in strata if dd == d), key=lambda t: (title_key(seed, t), t))
            title_alloc = allocate(decade_alloc[d], titles, {t: len(strata[(d, t)]) for t in titles})
            picked = []
            for t in titles:
                pages = sorted(strata[(d, t)], key=lambda p: p[0])
                n = title_alloc[t]
                stratum_list.append({"decade": d, "eligible": len(pages), "lccn": t, "selected": n})
                for rank, (key, row, scan) in enumerate(pages[:n], start=1):
                    record = {
                        "audit_10pct": audit_member(row["doc_id"], cut),
                        "audit_scored": None if audit is None else row["doc_id"] in audit,
                        "batch": row["batch"],
                        "batch_version": row["batch_version"],
                        "date": row["date"],
                        "decade": d,
                        "doc_id": row["doc_id"],
                        "edition": row["edition"],
                        "lccn": row["lccn"],
                        "sample": name,
                        "selection_key": key,
                        "seq": row["seq"],
                        "stratum_rank": rank,
                        **scan,
                    }
                    picked.append(record)
            records.extend(picked)
            decades[str(d)] = {
                "allocated": decade_alloc[d],
                "audit_10pct": sum(r["audit_10pct"] for r in picked),
                "eligible": per_decade[d],
                "formats": dict(sorted(Counter(r["scan_format"] for r in picked).items())),
                "selected": len(picked),
                "titles_eligible": len(titles),
                "titles_selected": sum(1 for t in titles if title_alloc[t]),
            }
        selected = sum(v["selected"] for v in decades.values())
        samples[name] = {
            "decades": decades,
            "eligible": sum(per_decade.values()),
            "excluded": {
                reason: {str(d): n for d, n in sorted(excluded[name][reason].items())}
                for reason in REASONS
                if excluded[name][reason]
            },
            "frame_pages": frame_pages[name],
            "lang": s["lang"],
            "selected": selected,
            "shortfall": s["size"] - selected,
            "size": s["size"],
            "strata": stratum_list,
        }
        if audit is not None:
            pages = [r for r in records if r["sample"] == name]
            samples[name]["audit_rule_disagreements"] = sum(r["audit_10pct"] != r["audit_scored"] for r in pages)
    order = {name: i for i, name in enumerate(sorted(spec["samples"]))}
    records.sort(key=lambda r: (order[r["sample"]], r["decade"], r["lccn"], r["date"], r["edition"], r["seq"]))
    return records, samples


def write(out, spec_path, spec_digest, spec, records, samples, inputs):
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    summary = {
        "format": SUMMARY_FORMAT,
        "inputs": inputs,
        "issue": spec["issue"],
        "pages": len(records),
        "samples": samples,
        "seed": spec["seed"],
        "spec": Path(spec_path).name,
        "spec_sha256": spec_digest,
    }
    (out / PAGES).write_text("".join(json.dumps(r, sort_keys=True) + "\n" for r in records), encoding="utf-8")
    (out / SUMMARY).write_text(canonical(summary), encoding="utf-8")
    files = {PAGES: out / PAGES, SUMMARY: out / SUMMARY, Path(spec_path).name: Path(spec_path)}
    sums = "".join(f"{sha256_file(files[n])}  {n}\n" for n in sorted(files))
    (out / SUMS).write_text(sums, encoding="utf-8")


def check(out, spec_path):
    """Problems with a frozen directory, as strings; empty when it is consistent."""
    out, problems = Path(out), []
    spec, digest = load_spec(spec_path)
    try:
        sums = (out / SUMS).read_text(encoding="utf-8").splitlines()
        summary_raw = (out / SUMMARY).read_text(encoding="utf-8")
        lines = (out / PAGES).read_text(encoding="utf-8").splitlines()
    except FileNotFoundError as e:
        return [f"missing {e.filename}"]
    listed = {}
    for line in sums:
        h, _, name = line.partition("  ")
        listed[name] = h
    for name in sorted({PAGES, SUMMARY, Path(spec_path).name} | set(listed)):
        path = Path(spec_path) if name == Path(spec_path).name else out / name
        if name not in listed:
            problems.append(f"{SUMS} does not list {name}")
        elif not path.exists() or sha256_file(path) != listed[name]:
            problems.append(f"{name} does not match {SUMS}")
    summary = json.loads(summary_raw)
    if canonical(summary) != summary_raw:
        problems.append(f"{SUMMARY} is not laid out as json.dumps(indent=2, sort_keys=True) writes it")
    if summary.get("format") != SUMMARY_FORMAT or summary.get("spec_sha256") != digest:
        problems.append(f"{SUMMARY} has another format or was drawn from another spec")
    if summary.get("seed") != spec["seed"] or set(summary.get("samples", {})) != set(spec["samples"]):
        problems.append(f"{SUMMARY} has another seed or other samples than the spec")
        return problems
    seen, counts, strata = set(), Counter(), Counter()
    by_name = {name: s for name, s in spec["samples"].items()}
    for i, line in enumerate(lines, start=1):
        r = json.loads(line)
        where = f"{PAGES} line {i}"
        if json.dumps(r, sort_keys=True) != line:
            problems.append(f"{where}: keys are not sorted or the layout differs")
        if tuple(sorted(r)) != RECORD_FIELDS:
            problems.append(f"{where}: fields are not {', '.join(RECORD_FIELDS)}")
            continue
        s = by_name.get(r["sample"])
        if s is None or r["decade"] not in s["decades"] or decade_of(r["date"]) != r["decade"]:
            problems.append(f"{where}: {r['doc_id']} is outside its sample's declared decades")
        if r["doc_id"] in seen:
            problems.append(f"{where}: {r['doc_id']} appears twice")
        seen.add(r["doc_id"])
        if r["selection_key"] != selection_key(spec["seed"], r["doc_id"]):
            problems.append(f"{where}: selection_key is not sha256 of seed and doc_id")
        if r["audit_10pct"] != audit_member(r["doc_id"], spec["audit"]["cut"]):
            problems.append(f"{where}: audit_10pct does not follow the audit rule")
        expected_id = f"{r['lccn']}_{r['date']}_ed-{r['edition']}_seq-{r['seq']}"
        if r["doc_id"] != expected_id:
            problems.append(f"{where}: doc_id is not {expected_id}")
        if r["scan_format"] not in EXT or r["scan_path"] != scan_path(r, spec["frame"]["set"], r["scan_format"]):
            problems.append(f"{where}: scan_path does not follow the scan cache layout")
        if len(r["scan_sha256"] or "") != 64 or (r["scan_format"] == "jp2" and not r["scan_md5"]):
            problems.append(f"{where}: the scan hashes are missing")
        counts[(r["sample"], r["decade"])] += 1
        strata[(r["sample"], r["decade"], r["lccn"])] += 1
    for name, s in summary["samples"].items():
        declared = by_name[name]
        if s["size"] != declared["size"] or [int(d) for d in s["decades"]] != declared["decades"]:
            problems.append(f"{SUMMARY}: {name}'s size or decades differ from the spec")
            continue
        caps = {int(d): v["eligible"] for d, v in s["decades"].items()}
        alloc = allocate(declared["size"], declared["decades"], caps)
        for d, v in s["decades"].items():
            n = counts[(name, int(d))]
            if v["allocated"] != alloc[int(d)] or v["selected"] != n or n != alloc[int(d)]:
                problems.append(f"{SUMMARY}: {name} {d}s: allocation or selected count does not add up")
        for d in declared["decades"]:
            listed = {st["lccn"]: st for st in s["strata"] if st["decade"] == d}
            titles = sorted(listed, key=lambda t: (title_key(spec["seed"], t), t))
            title_alloc = allocate(alloc[d], titles, {t: listed[t]["eligible"] for t in titles})
            if sum(st["eligible"] for st in listed.values()) != caps[d]:
                problems.append(f"{SUMMARY}: {name} {d}s: the strata's eligible pages do not add up")
            for t, st in listed.items():
                if not st["selected"] == strata[(name, d, t)] == title_alloc[t]:
                    problems.append(f"{SUMMARY}: {name} {d}s {t}: selected count does not add up")
        if s["selected"] != sum(counts[(name, d)] for d in declared["decades"]) or s["shortfall"] != s["size"] - s["selected"]:
            problems.append(f"{SUMMARY}: {name}'s selected total does not add up")
    if summary["pages"] != len(lines):
        problems.append(f"{SUMMARY}: pages is not the number of records")
    return problems


def main(argv=None):
    parser = argparse.ArgumentParser(description="Freeze or check the Fraktur pilot's page manifests (#325).")
    parser.add_argument("--spec", required=True, type=Path, help="the declared design, spec.json")
    parser.add_argument("--out", required=True, type=Path, help="directory for pages.jsonl, summary.json and SHA256SUMS")
    parser.add_argument("--manifest", type=Path, help="the prefetch manifest (.parquet, or .jsonl with the same columns)")
    parser.add_argument("--sidecars", type=Path, help="the scan fetcher's sidecar index (JSONL)")
    parser.add_argument("--audit-pages", type=Path, help="the 10%% audit's page list (CSV with doc_id; .gz allowed)")
    parser.add_argument("--scan-listing", type=Path, help="files under the scans root: '<path>\\t<bytes>' per line")
    parser.add_argument("--check", action="store_true", help="verify a frozen --out directory instead of drawing")
    args = parser.parse_args(argv)
    if args.check:
        problems = check(args.out, args.spec)
        for p in problems:
            print(f"error: {p}", file=sys.stderr)
        if not problems:
            print(f"{args.out}: consistent with {args.spec}")
        return 1 if problems else 0
    if not args.manifest or not args.sidecars:
        parser.error("drawing needs --manifest and --sidecars")
    spec, digest = load_spec(args.spec)
    sidecars, side_counts = read_sidecars(args.sidecars)
    audit = read_audit(args.audit_pages) if args.audit_pages else None
    listing = read_listing(args.scan_listing) if args.scan_listing else None
    rows = list(read_manifest(args.manifest))
    records, samples = draw(spec, rows, sidecars, audit, listing)
    inputs = {
        "audit_pages": None
        if audit is None
        else {"name": args.audit_pages.name, "pages": len(audit), "sha256": sha256_file(args.audit_pages)},
        "manifest": {"name": args.manifest.name, "rows": len(rows), "sha256": sha256_file(args.manifest)},
        "scan_listing": None
        if listing is None
        else {"files": len(listing), "name": args.scan_listing.name, "sha256": sha256_file(args.scan_listing)},
        "sidecars": {"name": args.sidecars.name, "sha256": sha256_file(args.sidecars), **side_counts},
    }
    write(args.out, args.spec, digest, spec, records, samples, inputs)
    for name, s in samples.items():
        print(f"{name}: {s['selected']} of {s['size']} from {s['eligible']} eligible ({s['frame_pages']} in the frame)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
