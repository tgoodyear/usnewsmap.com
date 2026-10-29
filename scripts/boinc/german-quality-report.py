#!/usr/bin/env python3
"""Summarize collected German OCR comparisons, never model-derived ground truth."""

import argparse
import collections
import datetime
import hashlib
import json
import math
import re
import statistics
from pathlib import Path


PAGE_METRICS = ("cer_vs_loc", "dice_vs_loc", "wer_vs_loc", "total_ms")
CROP_METRICS = ("cer_vs_vlm", "dice_vs_vlm")
DOC_ID = re.compile(
    r"(sn\d{8})_(\d{4}-\d{2}-\d{2})_ed-[1-9]\d*_seq-[1-9]\d*"
)


def page_id(value):
    if not isinstance(value, str):
        raise ValueError("doc_id must be a LoC page identifier")
    local = value.removeprefix("loc:")
    match = DOC_ID.fullmatch(local)
    if not match:
        raise ValueError(f"invalid LoC doc_id: {value!r}")
    date = datetime.date.fromisoformat(match[2])
    return f"loc:{local}", match[1], date.year


def read_jsonl(path):
    raw = path.read_bytes()
    rows = []
    for number, line in enumerate(raw.splitlines(), 1):
        if not line.strip():
            continue
        try:
            row = json.loads(line)
            if not isinstance(row, dict):
                raise ValueError("expected an object")
        except ValueError as exc:
            raise ValueError(f"{path}:{number}: {exc}") from exc
        rows.append(row)
    return rows, hashlib.sha256(raw).hexdigest()


def metric(row, name):
    value = row.get(name)
    if value is not None and (
        isinstance(value, bool)
        or not isinstance(value, (int, float))
        or not math.isfinite(value)
        or value < 0
        or (name.startswith("dice_") and value > 1)
    ):
        raise ValueError(f"{name} must be null or a finite nonnegative number")
    return value


def stats(rows, name):
    values = [value for row in rows if (value := metric(row, name)) is not None]
    return {
        "observed": len(values),
        "missing": len(rows) - len(values),
        "median": statistics.median(values) if values else None,
        "mean": statistics.mean(values) if values else None,
    }


def summarize(ids, readers, pages, crops, crops_per_page):
    page_report = {}
    crop_report = {}
    crop_keys = {(doc, crop) for doc, crop, _ in crops if doc in ids}
    for reader in readers:
        outcomes = collections.Counter()
        successes = []
        for doc in sorted(ids):
            row = pages.get((doc, reader))
            outcome = row["outcome"] if row is not None else "missing"
            outcomes[outcome] += 1
            if outcome == "ok":
                successes.append(row)
        page_report[reader] = {
            "outcomes": dict(sorted(outcomes.items())),
            "metrics_on_successes": {
                name: stats(successes, name) for name in PAGE_METRICS
            },
        }
        selected = [
            row for (doc, _, rd), row in crops.items()
            if doc in ids and rd == reader
        ]
        comparable = [
            row for row in selected
            if row["vlm_text"] is not None and row["vlm_text"].strip()
        ]
        crop_report[reader] = {
            "observed_rows": len(selected),
            "missing_rows_for_observed_crops": len(crop_keys) - len(selected),
            "missing_reference_rows": sum(row["vlm_text"] is None for row in selected),
            "empty_reference_rows": sum(
                row["vlm_text"] is not None and not row["vlm_text"].strip()
                for row in selected
            ),
            "metrics_with_nonempty_reference": {
                name: stats(comparable, name) for name in CROP_METRICS
            },
        }
    return {
        "pages": len(ids),
        "planned_crops": len(ids) * crops_per_page,
        "observed_crops": len(crop_keys),
        "unobserved_planned_crops": len(ids) * crops_per_page - len(crop_keys),
        "page_readers": page_report,
        "crop_readers": crop_report,
    }


def report(sample, page_rows, crop_rows, readers, crops_per_page):
    if not readers or len(set(readers)) != len(readers) or any(
        not isinstance(reader, str) or not reader.strip() or reader == "vlm"
        for reader in readers
    ):
        raise ValueError("provide distinct nonempty OCR reader names, not vlm")
    if isinstance(crops_per_page, bool) or not isinstance(crops_per_page, int) or crops_per_page < 1:
        raise ValueError("crops per page must be a positive integer")
    cohort = {}
    for row in sample:
        doc, title, year = page_id(row.get("doc_id"))
        if row.get("lang") != "deu":
            raise ValueError(f"{doc}: the frozen sample must declare lang=deu")
        if doc in cohort:
            raise ValueError(f"duplicate sample page: {doc}")
        cohort[doc] = (title, year)
    if not cohort:
        raise ValueError("the sample is empty")
    pages = {}
    crops = {}
    references = {}
    models = set()
    unreported_models = 0
    for rows, dest, metrics, is_crop in (
        (page_rows, pages, PAGE_METRICS, False),
        (crop_rows, crops, CROP_METRICS, True),
    ):
        for row in rows:
            doc, _, _ = page_id(row.get("doc_id"))
            reader = row.get("reader")
            if doc not in cohort or reader not in [*readers, *(["vlm"] if is_crop else [])]:
                raise ValueError(f"{doc}: row outside the declared sample/readers")
            for name in metrics:
                metric(row, name)
            if is_crop:
                crop = row.get("crop_id")
                if not isinstance(crop, str) or not crop:
                    raise ValueError(f"{doc}: missing crop_id")
                if "vlm_text" not in row:
                    raise ValueError(f"{doc}/{crop}: missing vlm_text field")
                text = row["vlm_text"]
                if text is not None and not isinstance(text, str):
                    raise ValueError(f"{doc}/{crop}: vlm_text must be null or a string")
                model = row.get("vlm_model")
                if model is not None and (not isinstance(model, str) or not model.strip()):
                    raise ValueError(f"{doc}/{crop}: invalid vlm_model")
                if text is not None and text.strip() and model is None:
                    raise ValueError(f"{doc}/{crop}: a nonempty VLM reference needs its model identifier")
                reference = (text, model)
                previous = references.setdefault((doc, crop), reference)
                if previous != reference:
                    raise ValueError(f"{doc}/{crop}: inconsistent VLM reference")
                if model is not None:
                    models.add(model)
                else:
                    unreported_models += 1
                key = (doc, crop, reader)
            else:
                outcome = row.get("outcome")
                if not isinstance(outcome, str) or not outcome.strip() or outcome == "missing":
                    raise ValueError(f"{doc}/{reader}: missing or reserved outcome")
                language = row.get("language")
                if language is not None and (not isinstance(language, str) or not language.strip()):
                    raise ValueError(f"{doc}/{reader}: invalid reported language")
                key = (doc, reader)
            if key in dest:
                raise ValueError(f"duplicate result row: {key}")
            dest[key] = row
    if len(models) > 1:
        raise ValueError("do not pool comparisons against different VLM models")
    per_page_crops = collections.Counter(doc for doc, _ in references)
    if any(count > crops_per_page for count in per_page_crops.values()):
        raise ValueError("observed crops exceed the declared crops-per-page plan")
    groups = collections.defaultdict(set)
    for doc, (title, year) in cohort.items():
        groups[(title, year)].add(doc)
    return {
        "schema_version": 1,
        "interpretation": "Model agreement, not human CER, calibrated accuracy or a release gate.",
        "limitations": [
            "Metrics are the collector's values; no additional normalization is applied.",
            "Layout-derived and hat-estimated accuracy metrics are deliberately omitted.",
            "VLM identifiers are reported metadata, not verified executable/model digests.",
            "total_ms is reported per reader; do not sum it as campaign wall or CPU time.",
            "An absent crop is unobserved, not a successful comparison or a known timeout.",
            "Stratum summaries are unweighted; the frozen sample controls representativeness.",
            "The sample declares German pages; reported scorer languages are retained separately.",
        ],
        "sample_pages": sorted(cohort),
        "reported_page_languages": dict(sorted(collections.Counter(
            row.get("language") or "unreported" for row in pages.values()
        ).items())),
        "reported_vlm_models": sorted(models),
        "crop_rows_without_model_identity": unreported_models,
        "overall": summarize(set(cohort), readers, pages, crops, crops_per_page),
        "by_title_year": [
            {
                "title": title,
                "year": year,
                **summarize(ids, readers, pages, crops, crops_per_page),
            }
            for (title, year), ids in sorted(groups.items())
        ],
        "per_page": [
            {
                "page_id": doc,
                **summarize({doc}, readers, pages, crops, crops_per_page),
            }
            for doc in sorted(cohort)
        ],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sample", required=True, type=Path, help="Frozen quality sample JSONL")
    parser.add_argument("--pages", required=True, type=Path, help="Collected page rows as JSONL")
    parser.add_argument("--crops", required=True, type=Path, help="Collected crop rows as JSONL")
    parser.add_argument("--reader", action="append", required=True, help="Expected OCR reader; repeat")
    parser.add_argument("--crops-per-page", required=True, type=int, help="Frozen staging plan's k_crops")
    parser.add_argument("--out", required=True, type=Path, help="New report; existing files are refused")
    args = parser.parse_args()
    try:
        sample, sample_sha = read_jsonl(args.sample)
        pages, pages_sha = read_jsonl(args.pages)
        crops, crops_sha = read_jsonl(args.crops)
        result = report(sample, pages, crops, args.reader, args.crops_per_page)
        result["input_sha256"] = {
            "sample": sample_sha,
            "pages": pages_sha,
            "crops": crops_sha,
            "reporter": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        }
        encoded = json.dumps(result, indent=2, sort_keys=True, allow_nan=False) + "\n"
        with args.out.open("x", encoding="utf-8") as output:
            output.write(encoded)
    except (OSError, ValueError) as exc:
        parser.exit(2, f"error: {exc}\n")


if __name__ == "__main__":
    main()
