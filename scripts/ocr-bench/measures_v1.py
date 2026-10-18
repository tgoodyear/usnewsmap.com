#!/usr/bin/env python3
"""Versioned extended OCR evaluation measures; independent of the legacy scorer."""
from __future__ import annotations

import argparse
import heapq
from collections import Counter, defaultdict
import json
import math
from pathlib import Path
import random
import re
import sys
import unicodedata

FORMAT = "usnm-ocr-evaluation-measures/1"
TRANSCRIPTION_RULES = ("diplomatic-v1", "legacy-ws-v1")
STATUSES = ("ok", "missing", "error", "truncated")
REVIEW_CATEGORIES = ("historical-modernization", "historical-glyph", "cross-region-contamination", "other")
STRATA_FIELDS = ("language", "script", "damage", "layout")
SEARCH_ANALYZER = "nfkc-casefold-word-v1"
SEARCH_TOKEN = re.compile(r"\w+", re.UNICODE)


def _unique_rows(rows, label):
    result = {}
    for row in rows:
        if not isinstance(row, dict) or not isinstance(row.get("id"), str) or not row["id"]:
            raise ValueError(f"{label} rows require a nonempty string id")
        if row["id"] in result:
            raise ValueError(f"duplicate {label} id: {row['id']}")
        result[row["id"]] = row
    return result


def _normalize(text, rule):
    if rule == "diplomatic-v1":
        return text
    if rule == "legacy-ws-v1":
        return " ".join(text.split())
    raise ValueError(f"unknown transcription rule: {rule}")


def _distance_row(reference, hypothesis):
    previous = list(range(len(hypothesis) + 1))
    for i, left in enumerate(reference, 1):
        current = [i]
        for j, right in enumerate(hypothesis, 1):
            current.append(min(previous[j] + 1, current[j - 1] + 1,
                               previous[j - 1] + (left != right)))
        previous = current
    return previous


def _small_edit_counts(reference, hypothesis):
    rows, cols = len(reference) + 1, len(hypothesis) + 1
    distance = [[0] * cols for _ in range(rows)]
    for i in range(rows):
        distance[i][0] = i
    for j in range(cols):
        distance[0][j] = j
    for i in range(1, rows):
        for j in range(1, cols):
            distance[i][j] = min(distance[i - 1][j] + 1, distance[i][j - 1] + 1,
                                 distance[i - 1][j - 1] + (reference[i - 1] != hypothesis[j - 1]))
    counts = Counter()
    i, j = len(reference), len(hypothesis)
    while i or j:
        if i and j and reference[i - 1] == hypothesis[j - 1] and distance[i][j] == distance[i - 1][j - 1]:
            i -= 1
            j -= 1
        elif i and j and distance[i][j] == distance[i - 1][j - 1] + 1:
            counts["substitutions"] += 1
            i -= 1
            j -= 1
        elif i and distance[i][j] == distance[i - 1][j] + 1:
            counts["deletions"] += 1
            i -= 1
        else:
            counts["insertions"] += 1
            j -= 1
    return counts


def _linear_space_edit_counts(reference, hypothesis):
    if not reference:
        return Counter(insertions=len(hypothesis))
    if not hypothesis:
        return Counter(deletions=len(reference))
    if len(reference) == 1:
        return Counter(substitutions=0 if reference[0] in hypothesis else 1,
                       insertions=len(hypothesis) - 1)
    if len(hypothesis) == 1:
        return Counter(substitutions=0 if hypothesis[0] in reference else 1,
                       deletions=len(reference) - 1)
    if len(reference) * len(hypothesis) <= 4096:
        return _small_edit_counts(reference, hypothesis)
    middle = len(reference) // 2
    left = _distance_row(reference[:middle], hypothesis)
    right = _distance_row(reference[middle:][::-1], hypothesis[::-1])
    split = min(range(len(hypothesis) + 1), key=lambda j: (left[j] + right[len(hypothesis) - j], j))
    counts = _linear_space_edit_counts(reference[:middle], hypothesis[:split])
    counts.update(_linear_space_edit_counts(reference[middle:], hypothesis[split:]))
    return counts


def edit_counts(reference, hypothesis):
    """Return exact Levenshtein counts with linear memory for page-length strings."""
    counts = _linear_space_edit_counts(reference, hypothesis)
    return {"substitutions": counts["substitutions"], "insertions": counts["insertions"],
            "deletions": counts["deletions"]}


def _ratio(numerator, denominator):
    return numerator / denominator if denominator else None


def _metric_record(counts):
    errors = counts["substitutions"] + counts["insertions"] + counts["deletions"]
    chars = counts["reference_characters"]
    return {"samples": counts["samples"], "reference_characters": chars,
            "substitutions": counts["substitutions"], "insertions": counts["insertions"],
            "deletions": counts["deletions"], "character_errors": errors, "cer": _ratio(errors, chars),
            "insertion_rate": _ratio(counts["insertions"], chars), "omission_rate": _ratio(counts["deletions"], chars),
            "missing": counts["missing"], "errors": counts["errors"], "truncated": counts["truncated"],
            "empty_outputs": counts["empty_outputs"]}


def _aggregate_metrics(refs, hyps, ids, rule):
    totals, items = Counter(), []
    for identity in ids:
        ref, out = refs[identity], hyps.get(identity)
        status = "missing" if out is None else out.get("status", "ok")
        if status not in STATUSES:
            raise ValueError(f"invalid status for {identity}: {status}")
        text = "" if out is None or status in ("missing", "error") else out.get("text", "")
        reference = ref.get("reference")
        if not isinstance(reference, str) or not isinstance(text, str):
            raise ValueError(f"reference and hypothesis text must be strings for {identity}")
        reference, text = _normalize(reference, rule), _normalize(text, rule)
        counts = edit_counts(reference, text)
        errors = sum(counts.values())
        totals.update(counts)
        totals["reference_characters"] += len(reference)
        totals["samples"] += 1
        totals["missing"] += status == "missing"
        totals["errors"] += status == "error"
        totals["truncated"] += status == "truncated"
        totals["empty_outputs"] += status not in ("missing", "error") and text == ""
        page_id = ref.get("page_id") or identity
        title_id = ref.get("title_id") or page_id
        items.append({"id": identity, "page_id": page_id, "title_id": title_id, "errors": errors,
                      "substitutions": counts["substitutions"], "insertions": counts["insertions"],
                      "deletions": counts["deletions"], "missing_outputs": status == "missing",
                      "status_errors": status == "error", "truncated_outputs": status == "truncated",
                      "empty_outputs": status not in ("missing", "error") and text == "",
                      "reference_characters": len(reference),
                      "cer": _ratio(errors, len(reference)), "status": status})
    return _metric_record(totals), items


def _metrics_from_items(items):
    totals = Counter()
    for item in items:
        for field in ("substitutions", "insertions", "deletions", "reference_characters"):
            totals[field] += item[field]
        totals["samples"] += 1
        totals["missing"] += item["missing_outputs"]
        totals["errors"] += item["status_errors"]
        totals["truncated"] += item["truncated_outputs"]
        totals["empty_outputs"] += item["empty_outputs"]
    return _metric_record(totals)


def _strata(refs, items, ids):
    result, by_id = {}, {item["id"]: item for item in items}
    for field in STRATA_FIELDS:
        groups = defaultdict(list)
        for identity in ids:
            values = refs[identity].get(field, ["unlabelled"])
            if isinstance(values, str):
                values = [values]
            if not isinstance(values, list) or not all(isinstance(v, str) for v in values):
                raise ValueError(f"{field} must be a string or list of strings")
            for value in set(values or ["unlabelled"]):
                groups[value].append(identity)
        result[field] = {key: _metrics_from_items([by_id[identity] for identity in members])
                         for key, members in sorted(groups.items())}
    return result


def _boxes(regions):
    if not isinstance(regions, list):
        raise ValueError("regions must be an array")
    result, orders = [], set()
    for region in regions:
        if not isinstance(region, dict):
            raise ValueError("each region must be an object")
        box, order, text = region.get("box"), region.get("order"), region.get("text", "")
        if (not isinstance(box, list) or len(box) != 4 or
                not all(isinstance(v, (int, float)) and math.isfinite(v) for v in box) or
                box[2] <= 0 or box[3] <= 0 or not isinstance(order, int) or
                not isinstance(text, str) or order in orders):
            raise ValueError("regions require unique integer order, positive [x,y,width,height], and text")
        orders.add(order)
        result.append({"box": box, "order": order, "text": text})
    return sorted(result, key=lambda region: region["order"])


def _iou(a, b):
    ax, ay, aw, ah = a
    bx, by, bw, bh = b
    intersection = max(0, min(ax + aw, bx + bw) - max(ax, bx)) * max(0, min(ay + ah, by + bh) - max(ay, by))
    union = aw * ah + bw * bh - intersection
    return intersection / union if union else 0.0


def _region_matches(reference, hypothesis, threshold):
    """Maximum-cardinality assignment; among those, prefer total IoU then canonical order."""
    ref_count, hyp_count = len(reference), len(hypothesis)
    if not ref_count or not hyp_count:
        return []
    source, ref_start = 0, 1
    hyp_start, sink = ref_start + ref_count, ref_start + ref_count + hyp_count
    graph = [[] for _ in range(sink + 1)]

    def add_edge(origin, target, capacity, cost):
        forward = [target, capacity, cost, len(graph[target])]
        reverse = [origin, 0, -cost, len(graph[origin])]
        graph[origin].append(forward)
        graph[target].append(reverse)
        return len(graph[origin]) - 1

    for i in range(ref_count):
        add_edge(source, ref_start + i, 1, 0)
    for j in range(hyp_count):
        add_edge(hyp_start + j, sink, 1, 0)
    max_pairs = min(ref_count, hyp_count)
    max_displacement = max(ref_count, hyp_count)
    max_tie_total = max_pairs * hyp_count
    displacement_scale = max_tie_total + 1
    overlap_scale = max_pairs * max_displacement * displacement_scale + max_tie_total + 1
    assignment_edges = {}
    incoming_costs = [[] for _ in range(hyp_count)]
    for i, ref in enumerate(reference):
        for j, hyp in enumerate(hypothesis):
            overlap = _iou(ref["box"], hyp["box"])
            if overlap < threshold:
                continue
            overlap_units = round(overlap * 1_000_000)
            displacement = abs(i - j)
            tie_preference = hyp_count - j
            weight = overlap_units * overlap_scale - displacement * displacement_scale + tie_preference
            cost = -weight
            edge_index = add_edge(ref_start + i, hyp_start + j, 1, cost)
            assignment_edges[(i, j)] = edge_index
            incoming_costs[j].append(cost)

    if not assignment_edges:
        return []
    potential = [0] * len(graph)
    for j, costs in enumerate(incoming_costs):
        if costs:
            potential[hyp_start + j] = min(costs)
    reachable_sink_costs = [potential[hyp_start + j] for j, costs in enumerate(incoming_costs) if costs]
    potential[sink] = min(reachable_sink_costs)

    while True:
        distances = [None] * len(graph)
        previous = [None] * len(graph)
        distances[source] = 0
        pending = [(0, source)]
        while pending:
            distance, origin = heapq.heappop(pending)
            if distance != distances[origin]:
                continue
            for edge_index, edge in enumerate(graph[origin]):
                target, capacity, cost, _ = edge
                if not capacity:
                    continue
                candidate = distance + cost + potential[origin] - potential[target]
                if distances[target] is None or candidate < distances[target]:
                    distances[target] = candidate
                    previous[target] = (origin, edge_index)
                    heapq.heappush(pending, (candidate, target))
        if distances[sink] is None:
            break
        for node, distance in enumerate(distances):
            if distance is not None:
                potential[node] += distance
        node = sink
        while node != source:
            origin, edge_index = previous[node]
            edge = graph[origin][edge_index]
            edge[1] -= 1
            graph[node][edge[3]][1] += 1
            node = origin

    return sorted((i, j) for (i, j), edge_index in assignment_edges.items()
                  if graph[ref_start + i][edge_index][1] == 0)


def _region_metrics(refs, hyps, ids, rule, threshold=0.5):
    ref_total = hyp_total = matched = annotated_pages = order_pairs = concordant = contamination = 0
    text_counts = Counter()
    for identity in ids:
        if "regions" not in refs[identity]:
            continue
        annotated_pages += 1
        reference = _boxes(refs[identity]["regions"])
        out = hyps.get(identity, {})
        status = out.get("status", "ok")
        hypothesis = _boxes(out.get("regions", [])) if status not in ("missing", "error") else []
        pairs = _region_matches(reference, hypothesis, threshold)
        ref_total += len(reference)
        hyp_total += len(hypothesis)
        matched += len(pairs)
        paired = []
        matched_refs = set()
        for i, j in pairs:
            matched_refs.add(i)
            rt, ht = _normalize(reference[i]["text"], rule), _normalize(hypothesis[j]["text"], rule)
            text_counts.update(edit_counts(rt, ht))
            text_counts["reference_characters"] += len(rt)
            paired.append((reference[i]["order"], hypothesis[j]["order"]))
            if ht and any(_normalize(candidate["text"], rule) for candidate in reference):
                distances = [sum(edit_counts(_normalize(candidate["text"], rule), ht).values()) /
                             max(1, len(_normalize(candidate["text"], rule))) for candidate in reference]
                other_distances = [distance for index, distance in enumerate(distances) if index != i]
                contamination += bool(other_distances and min(other_distances) < distances[i])
        for i, region in enumerate(reference):
            if i not in matched_refs:
                rt = _normalize(region["text"], rule)
                text_counts.update(edit_counts(rt, ""))
                text_counts["reference_characters"] += len(rt)
        for n, (r1, h1) in enumerate(paired):
            for r2, h2 in paired[n + 1:]:
                order_pairs += 1
                concordant += (r1 < r2) == (h1 < h2)
    text_counts["samples"] = ref_total
    for key in ("missing", "errors", "truncated", "empty_outputs"):
        text_counts[key] = 0
    text_metrics = _metric_record(text_counts)
    return {"annotated_pages": annotated_pages, "geometry_match_policy": "max-cardinality-iou-v1",
            "region_iou_match_threshold": threshold,
            "transcription": text_metrics,
            "segmentation": {"reference_regions": ref_total, "hypothesis_regions": hyp_total,
                             "matched_regions": matched, "coverage_recall": _ratio(matched, ref_total),
                             "precision": _ratio(matched, hyp_total), "f1": _ratio(2 * matched, ref_total + hyp_total)},
            "reconstruction": {"reading_order_pairs": order_pairs,
                               "reading_order_pairwise_accuracy": _ratio(concordant, order_pairs),
                               "cross_region_contamination_count": contamination,
                               "cross_region_contamination_rate": _ratio(contamination, matched)}}


def _reviews(adjudications, ids, items, system_name, system_names):
    rows, seen = {}, set()
    for row in adjudications:
        if not isinstance(row, dict):
            raise ValueError("adjudication rows must be objects")
        identity, system = row.get("id"), row.get("system")
        if not isinstance(identity, str) or identity not in ids:
            raise ValueError(f"adjudication outside reference set: {identity}")
        if not isinstance(system, str) or system not in system_names:
            raise ValueError(f"adjudication has unknown system: {system}")
        key = (system, identity)
        if key in seen:
            raise ValueError(f"duplicate adjudication for {system}/{identity}")
        seen.add(key)
        if system == system_name:
            rows[identity] = row
    counts = Counter()
    for identity, row in rows.items():
        readers, categories = row.get("qualified_readers"), row.get("categories")
        valid_readers = (isinstance(readers, list) and len(readers) >= 2 and
                         all(isinstance(reader, dict) and isinstance(reader.get("id"), str) and reader["id"] and
                             isinstance(reader.get("qualification"), str) and reader["qualification"].strip()
                             for reader in readers) and
                         len({reader["id"] for reader in readers}) == len(readers))
        if (row.get("decision") != "human-adjudicated" or not valid_readers or
                not isinstance(categories, list) or not all(isinstance(category, str) for category in categories) or
                not set(categories) <= set(REVIEW_CATEGORIES)):
            raise ValueError(f"{system_name}/{identity}: requires human adjudication by at least two qualified readers")
        counts["adjudicated_samples"] += 1
        counts["reviewed_error_samples"] += bool(categories)
        for category in set(categories):
            counts[category] += 1
    error_ids = {item["id"] for item in items if item["errors"] > 0}
    return {"sample_ids": sorted(rows), "adjudicated_samples": counts["adjudicated_samples"],
            "reviewed_error_samples": counts["reviewed_error_samples"],
            "error_review_coverage": _ratio(len(set(rows) & error_ids), len(error_ids)),
            "categories": {category: {"count": counts[category],
                                       "rate_per_adjudicated_sample": _ratio(counts[category], counts["adjudicated_samples"])}
                           for category in REVIEW_CATEGORIES}}


def _cluster_uncertainty(refs, baseline_items, candidate_items, field, seed, bootstrap_samples):
    by_system = [{item["id"]: item for item in rows} for rows in (baseline_items, candidate_items)]
    clusters = defaultdict(lambda: [Counter(), Counter()])
    for identity, ref in refs.items():
        key = ref.get(field)
        if not isinstance(key, str) or not key:
            key = (ref.get("page_id") or identity) if field == "page_id" else (ref.get("title_id") or ref.get("page_id") or identity)
        for position in (0, 1):
            item = by_system[position][identity]
            clusters[key][position]["errors"] += item["errors"]
            clusters[key][position]["chars"] += item["reference_characters"]
    names = sorted(clusters)
    eligible = [clusters[name] for name in names if clusters[name][0]["chars"] > 0 and clusters[name][1]["chars"] > 0]
    deltas = [(cluster[1]["errors"] / cluster[1]["chars"]) -
              (cluster[0]["errors"] / cluster[0]["chars"]) for cluster in eligible]
    observed = sum(deltas) / len(deltas) if deltas else None
    rng, samples = random.Random(seed), []
    for _ in range(bootstrap_samples if deltas else 0):
        samples.append(sum(deltas[rng.randrange(len(deltas))] for _ in deltas) / len(deltas))
    samples.sort()
    lo = samples[int(0.025 * (len(samples) - 1))] if samples else None
    hi = samples[int(0.975 * (len(samples) - 1))] if samples else None
    return {"clusters": len(eligible), "excluded_zero_character_clusters": len(names) - len(eligible),
            "macro_mean_cer_delta_candidate_minus_baseline": observed,
            "ci95_percentile": [lo, hi], "bootstrap_samples": len(samples), "seed": seed}


def compare_transcriptions(references, systems, *, rule="diplomatic-v1", adjudications=(),
                           good_page_cer_threshold=0.02, bootstrap_samples=2000, seed=813):
    """Score paired references/outputs and compare systems to the first output set."""
    if rule not in TRANSCRIPTION_RULES:
        raise ValueError(f"unknown transcription rule: {rule}")
    refs = _unique_rows(references, "reference")
    if not refs or not systems:
        raise ValueError("at least one reference and output system are required")
    if bootstrap_samples < 1 or not 0 <= good_page_cer_threshold <= 1:
        raise ValueError("invalid bootstrap or good-page threshold")
    ids, scored, details = sorted(refs), {}, {}
    for name, outputs in systems.items():
        hyps = _unique_rows(outputs, f"output {name}")
        unknown = set(hyps) - set(refs)
        if unknown:
            raise ValueError(f"output outside reference set for {name}: {min(unknown)}")
        metrics, items = _aggregate_metrics(refs, hyps, ids, rule)
        details[name] = {item["id"]: item for item in items}
        scored[name] = {"transcription": metrics, "strata": _strata(refs, items, ids),
                        "page_regions": _region_metrics(refs, hyps, ids, rule)}
    baseline = next(iter(systems))
    base_items = list(details[baseline].values())
    page_counts = defaultdict(lambda: [0, 0])
    for item in base_items:
        page_counts[item["page_id"]][0] += item["errors"]
        page_counts[item["page_id"]][1] += item["reference_characters"]
    good_pages = {page for page, (errors, chars) in page_counts.items()
                  if chars > 0 and errors / chars <= good_page_cer_threshold}
    comparisons = {}
    for name in list(systems)[1:]:
        candidate_items = list(details[name].values())
        candidate_pages = defaultdict(lambda: [0, 0])
        for item in candidate_items:
            candidate_pages[item["page_id"]][0] += item["errors"]
            candidate_pages[item["page_id"]][1] += item["reference_characters"]
        regressions = {page for page in good_pages
                       if candidate_pages[page][0] / candidate_pages[page][1] >
                       page_counts[page][0] / page_counts[page][1]}
        comparisons[name] = {"baseline": baseline, "good_page_cer_threshold": good_page_cer_threshold,
            "baseline_good_pages": len(good_pages), "regressed_good_pages": len(regressions),
            "good_page_regression_rate": _ratio(len(regressions), len(good_pages)),
            "paired_uncertainty_by_page": _cluster_uncertainty(refs, base_items, candidate_items, "page_id", seed, bootstrap_samples),
            "paired_uncertainty_by_title": _cluster_uncertainty(refs, base_items, candidate_items, "title_id", seed, bootstrap_samples)}
        scored[name]["good_page_regression_rate"] = comparisons[name]["good_page_regression_rate"]
    review_results = {name: _reviews(adjudications, ids, list(details[name].values()), name, set(systems))
                      for name in systems} if adjudications else {}
    return {"format": FORMAT, "measure_version": "1.0.0", "transcription_rule": rule,
            "systems": scored, "comparisons": comparisons, "human_adjudication": review_results}


def deterministic_review_sample(reference_ids, count, seed):
    """Select a fixed review sample by ID hash, before viewing any system output."""
    import hashlib
    ids = sorted(set(reference_ids))
    if count < 0 or count > len(ids):
        raise ValueError("review sample count must fit the unique reference IDs")
    return sorted(sorted(ids, key=lambda identity: (hashlib.sha256(f"{seed}\0{identity}".encode()).digest(), identity))[:count])


def _tokens(text):
    return SEARCH_TOKEN.findall(unicodedata.normalize("NFKC", text).casefold())


def _search_metrics(documents, queries, k):
    docs = _unique_rows(documents, "search document")
    if not docs or k < 1:
        raise ValueError("search evaluation requires documents and positive k")
    if any(not isinstance(row.get("text", ""), str) for row in docs.values()):
        raise ValueError("search document text must be a string")
    tokenized = {identity: Counter(_tokens(row.get("text", ""))) for identity, row in docs.items()}
    lengths = {identity: sum(tokens.values()) for identity, tokens in tokenized.items()}
    average_length = sum(lengths.values()) / len(lengths)
    df = Counter(term for terms in tokenized.values() for term in terms)
    per_query = []
    for query in queries:
        identity, relevance, text = query.get("id"), query.get("relevance"), query.get("query")
        if (not isinstance(identity, str) or not isinstance(text, str) or not isinstance(relevance, dict) or
                not relevance or any(doc not in docs or not isinstance(grade, int) or grade < 0 for doc, grade in relevance.items()) or
                not any(relevance.values())):
            raise ValueError("queries require id, text, and positive graded relevance for known documents")
        scores = {}
        for doc_id, terms in tokenized.items():
            score = 0.0
            for term in _tokens(text):
                tf = terms[term]
                if tf:
                    idf = math.log(1 + (len(docs) - df[term] + 0.5) / (df[term] + 0.5))
                    norm = tf + 1.2 * (1 - 0.75 + 0.75 * lengths[doc_id] / average_length)
                    score += idf * tf * 2.2 / norm
            scores[doc_id] = score
        full_ranking = sorted((doc for doc in docs if scores[doc] > 0), key=lambda doc: (-scores[doc], doc))
        ranked = full_ranking[:k]
        positives = [doc for doc, grade in relevance.items() if grade > 0]
        first = next((rank for rank, doc in enumerate(full_ranking, 1) if relevance.get(doc, 0) > 0), None)
        dcg = sum((2 ** relevance.get(doc, 0) - 1) / math.log2(rank + 1) for rank, doc in enumerate(ranked, 1))
        ideal = sorted(relevance.values(), reverse=True)[:k]
        idcg = sum((2 ** grade - 1) / math.log2(rank + 1) for rank, grade in enumerate(ideal, 1))
        per_query.append({"id": identity, "kind": query.get("kind", "unlabelled"),
            f"recall@{k}": sum(doc in ranked for doc in positives) / len(positives),
            "reciprocal_rank": 1 / first if first else 0.0, f"ndcg@{k}": dcg / idcg if idcg else 0.0})
    if not per_query:
        raise ValueError("at least one query is required")
    keys = (f"recall@{k}", "reciprocal_rank", f"ndcg@{k}")
    metrics = {key: sum(row[key] for row in per_query) / len(per_query) for key in keys}
    by_kind = {kind: {key: sum(row[key] for row in per_query if row["kind"] == kind) /
                      sum(row["kind"] == kind for row in per_query) for key in keys}
               for kind in sorted({row["kind"] for row in per_query})}
    return {"analyzer": SEARCH_ANALYZER, "ranker": "bm25-k1-1.2-b-0.75-v1", "k": k,
            "queries": len(per_query), **metrics, "by_kind": by_kind, "per_query": per_query}


def score_search(systems, queries, *, k=5):
    """Compare retrieval systems on a fixed relevance set with a pinned analyzer and ranker."""
    query_rows = list(_unique_rows(queries, "query").values())
    if not systems or any(not isinstance(name, str) or not name for name in systems):
        raise ValueError("at least one named search system is required")
    document_sets = [set(_unique_rows(documents, "search document")) for documents in systems.values()]
    if any(doc_ids != document_sets[0] for doc_ids in document_sets[1:]):
        raise ValueError("search systems must use the same document IDs")
    return {"format": FORMAT, "search_measure_version": "search-v1",
            "results": {name: _search_metrics(documents, query_rows, k) for name, documents in systems.items()}}


def quality_cost_decision(baseline, candidate, *, baseline_cost, candidate_cost,
                          max_cer_delta, max_missing_rate_delta, max_good_page_regression_rate):
    """Apply predeclared quality margins before comparing measured cost per usable page."""
    base, cand = baseline["transcription"], candidate["transcription"]
    if base["samples"] < 1 or cand["samples"] < 1:
        raise ValueError("quality/cost decision requires nonempty paired sample sets")
    if any(not isinstance(value, (int, float)) or not math.isfinite(value) for value in (base["cer"], cand["cer"])):
        raise ValueError("quality/cost decision requires defined baseline and candidate CER")
    base_missing = (base["missing"] + base["errors"]) / base["samples"]
    cand_missing = (cand["missing"] + cand["errors"]) / cand["samples"]
    cer_delta = cand["cer"] - base["cer"]
    regression = candidate.get("good_page_regression_rate")
    values = (baseline_cost, candidate_cost, max_cer_delta, max_missing_rate_delta, max_good_page_regression_rate)
    if any(not isinstance(x, (int, float)) or not math.isfinite(x) or x < 0 for x in values):
        raise ValueError("costs and preregistered quality margins must be finite nonnegative numbers")
    gates = {"cer_within_margin": cer_delta <= max_cer_delta,
             "missing_error_rate_within_margin": cand_missing - base_missing <= max_missing_rate_delta,
             "good_page_regressions_within_margin": regression is not None and regression <= max_good_page_regression_rate}
    return {"eligible": all(gates.values()), "quality_gates": gates, "cer_delta": cer_delta,
            "baseline_missing_error_rate": base_missing, "candidate_missing_error_rate": cand_missing,
            "baseline_cost_per_usable_page": baseline_cost, "candidate_cost_per_usable_page": candidate_cost,
            "cost_ratio_candidate_over_baseline": _ratio(candidate_cost, baseline_cost),
            "cost_better_after_quality_gates": all(gates.values()) and candidate_cost < baseline_cost}


def _jsonl(path):
    rows = []
    with Path(path).open(encoding="utf-8") as handle:
        for line_number, line in enumerate(handle, 1):
            if line.strip():
                try:
                    rows.append(json.loads(line))
                except json.JSONDecodeError as error:
                    raise ValueError(f"{path}:{line_number}: {error}") from error
    return rows


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--references", type=Path, required=True)
    parser.add_argument("--outputs", action="append", required=True, metavar="NAME=JSONL")
    parser.add_argument("--rule", choices=TRANSCRIPTION_RULES, default="diplomatic-v1")
    parser.add_argument("--adjudications", type=Path)
    parser.add_argument("--queries", type=Path)
    parser.add_argument("--search-documents", action="append", default=[], metavar="NAME=JSONL")
    parser.add_argument("--search-k", type=int, default=5)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args(argv)
    references, systems = _jsonl(args.references), {}
    for value in args.outputs:
        if "=" not in value:
            parser.error("--outputs must be NAME=JSONL")
        name, path = value.split("=", 1)
        if not name or name in systems:
            parser.error("output system names must be nonempty and unique")
        systems[name] = _jsonl(path)
    adjudications = _jsonl(args.adjudications) if args.adjudications else []
    report = compare_transcriptions(references, systems, rule=args.rule, adjudications=adjudications)
    if args.queries:
        search_systems = {}
        for value in args.search_documents:
            if "=" not in value:
                parser.error("--search-documents must be NAME=JSONL")
            name, path = value.split("=", 1)
            if not name or name in search_systems:
                parser.error("search system names must be nonempty and unique")
            search_systems[name] = _jsonl(path)
        if not search_systems:
            parser.error("--queries requires --search-documents")
        report["search"] = score_search(search_systems, _jsonl(args.queries), k=args.search_k)
    elif args.search_documents:
        parser.error("--search-documents requires --queries")
    args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        sys.exit(2)
