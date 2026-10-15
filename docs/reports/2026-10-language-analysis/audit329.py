#!/usr/bin/env python3
"""Read-only, reproducible preparation and lexical screening for issue #329.

See data/issue-329/README.md. No command writes to Azure or the index.
"""
from __future__ import annotations

import argparse
import collections
import csv
import gzip
import hashlib
import io
import json
from pathlib import Path
import sys
import time

HERE = Path(__file__).resolve().parent
DATA = HERE / "data" / "issue-329"
ROOT = HERE.parents[2]
SEED = "issue-329-v1:"
LANGUAGES = ("cze", "hrv", "srp", "rus")
LABELS = frozenset(("likely_ocr_error", "valid", "unresolved"))
CONFIDENCES = frozenset(("high", "medium", "low"))
INPUTS = {
    "pages.csv.gz": "ca9890d21db07108f90454e1b16887a18ee4b962d54329ce7cdb409f8e373514",
    "titles.json": "92368e94c4dc921b3fc014eed965a86501b6b71e2a31c2962b666d63f058cb81",
    "batches.json": "6a34bb5797d0818412891725e6be919e3ce49223f2a8d51344dec4534663d5c3",
}


def load_lines(path):
    return [json.loads(line) for line in path.read_text().splitlines()]


def write_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def selection(source):
    for name, expected in INPUTS.items():
        if hashlib.sha256((source / name).read_bytes()).hexdigest() != expected:
            raise ValueError(f"snapshot checksum mismatch: {name}")
    with gzip.open(source / "pages.csv.gz", "rt") as stream:
        rows = list(csv.DictReader(stream))
    selected, populations = [], {}
    for language in LANGUAGES:
        eligible = [r for r in rows if r["language"] == language
                    and r["decision"] == "detected" and r["damage_rate"]
                    and (language == "hrv" or float(r["damage_rate"]) > 0.25)]
        populations[language] = len(eligible)
        selected.extend(sorted(eligible, key=lambda r: hashlib.sha256(
            (SEED + r["doc_id"]).encode()).hexdigest())[:20])
    return selected, populations


def fetch(source):
    """Range-read only doc_id and selected text row groups; cache locally for resume."""
    import pyarrow.parquet as pq
    from azure.identity import AzureCliCredential
    from azure.storage.blob import ContainerClient

    selected, _ = selection(source)
    expected = {p["doc_id"] for p in load_lines(DATA / "sample.jsonl")}
    if {p["doc_id"] for p in selected} != expected:
        raise ValueError("selection differs from committed manifest")
    batches = json.loads((source / "batches.json").read_text())
    parts = {f"{b['batch']}_ver{b['curated']['version']:02d}": b["curated"]["parts"]
             for b in batches}
    client = ContainerClient("https://stusnmdffapzy.blob.core.windows.net", "curated",
                             credential=AzureCliCredential())

    class Remote(io.RawIOBase):
        def __init__(self, blob):
            self.blob, self.pos = blob, 0
            self.size = blob.get_blob_properties().size

        def readable(self):
            return True

        def seekable(self):
            return True

        def tell(self):
            return self.pos

        def seek(self, offset, whence=0):
            self.pos = offset if whence == 0 else (self.pos if whence == 1 else self.size) + offset
            return self.pos

        def read(self, n=-1):
            n = min(self.size - self.pos, n if n >= 0 else self.size)
            if n <= 0:
                return b""
            data = self.blob.download_blob(offset=self.pos, length=n, max_concurrency=1).readall()
            self.pos += len(data)
            return data

        def readinto(self, buffer):
            data = self.read(len(buffer))
            buffer[:len(data)] = data
            return len(data)

    path = source / "texts.jsonl"
    found = {r["doc_id"] for r in load_lines(path)} if path.exists() else set()
    for batch in sorted({r["batch"] for r in selected}):
        wanted = {r["doc_id"]: r for r in selected if r["batch"] == batch and r["doc_id"] not in found}
        for part in parts[batch]:
            if not wanted:
                break
            with Remote(client.get_blob_client(part)) as remote:
                parquet = pq.ParquetFile(remote)
                for group in range(parquet.num_row_groups):
                    ids = parquet.read_row_group(group, columns=["doc_id"]).column("doc_id").to_pylist()
                    hits = set(ids) & wanted.keys()
                    if hits:
                        texts = parquet.read_row_group(group, columns=["text"]).column("text").to_pylist()
                        with path.open("a") as output:
                            for doc_id, text in zip(ids, texts):
                                if doc_id in hits:
                                    output.write(json.dumps({**wanted.pop(doc_id), "part": part, "text": text},
                                                            ensure_ascii=False) + "\n")
                                    found.add(doc_id)
            time.sleep(0.5)
        if wanted:
            raise ValueError(f"missing curated pages in {batch}: {sorted(wanted)}")
    print(f"Fetched/cached {len(found)} pages; no production writes")


def label_maps():
    rules, occurrences = {}, {}
    for row in json.loads((DATA / "labels.json").read_text()):
        if row["label"] not in LABELS:
            raise ValueError(f"unknown annotation label: {row['label']!r}")
        if row["confidence"] not in CONFIDENCES:
            raise ValueError(f"unknown annotation confidence: {row['confidence']!r}")
        key = ((row["language"], row["doc_id"], row["word_index"]) if "doc_id" in row
               else (row["language"], row["word"]))
        table = occurrences if "doc_id" in row else rules
        if key in table:
            raise ValueError(f"duplicate label: {key}")
        table[key] = {k: row[k] for k in ("label", "confidence", "reason")}
    return rules, occurrences


def label_flag(flag, rules, occurrences):
    return occurrences.get((flag["language"], flag["doc_id"], flag["word_index"]),
                           rules.get((flag["language"], flag["word"]), {
                               "label": "unresolved", "confidence": "low",
                               "reason": "Needs contextual and scan review; may be a valid form, name, abbreviation, or line-break fragment.",
                           }))


def summarize(pages, flags):
    for flag in flags:
        if flag["label"] not in LABELS:
            raise ValueError(f"unknown annotation label: {flag['label']!r}")
        if flag["language"] not in LANGUAGES:
            raise ValueError(f"unknown flag language: {flag['language']!r}")
    result = {}
    for language in LANGUAGES:
        counts = collections.Counter(f["label"] for f in flags if f["language"] == language)
        errors, valid, unresolved = (counts[k] for k in ("likely_ocr_error", "valid", "unresolved"))
        total = errors + valid + unresolved
        result[language] = {
            "pages": sum(p["language"] == language for p in pages), "flagged": total,
            "likely_ocr_error": errors, "valid": valid, "unresolved": unresolved,
            "provisional_precision_classified": errors / (errors + valid) if errors + valid else None,
            "conditional_precision_bounds_all_flags": [errors / total, (errors + unresolved) / total] if total else None,
        }
    return result


def score(source):
    sys.path.insert(0, str(ROOT / "ja-ocr"))
    import damage_v2 as metric

    selected, populations = selection(source)
    texts = {p["doc_id"]: p for p in load_lines(source / "texts.jsonl")}
    models, pages, flags = metric.Models(), [], []
    rules, occurrences = label_maps()
    for row in selected:
        stored = texts[row["doc_id"]]
        text, language = stored["text"], row["language"]
        languages = tuple(row["title_languages"].split())
        scored = metric.score_v2(text, languages, models)
        if (scored["lang"] != language or scored["decision"] != row["decision"]
                or round(scored["damage_rate"], 4) != float(row["damage_rate"])):
            raise ValueError(f"historical score mismatch: {row['doc_id']}")
        _, words, _, _ = metric.text_words(text)
        script = row["script"]
        observed = [w for w in words if metric.script(w) == script]
        normalized = [w.translate(metric.SERBIAN_LATIN) for w in observed] if language == "srp" else observed
        model = models.get(language)
        if script == "latin":
            others = [models.get(l) for l in dict.fromkeys([*languages, "eng"]) if l != language
                      and models.get(l) and models.get(l).script == "latin"]
        else:
            others = [models.get(l) for l in languages if l != language
                      and script in metric.scripts_of(l) and models.get(l)]
        count = 0
        for index, word in enumerate(normalized):
            if word in model.top or word not in model.near or any(word in m.real for m in others):
                continue
            flag = {"language": language, "doc_id": row["doc_id"], "word_index": index,
                    "observed": observed[index], "word": word, "suggestion": model.near[word],
                    "context": " ".join(observed[max(0, index - 5):index + 6])}
            flags.append({**flag, **label_flag(flag, rules, occurrences)})
            count += 1
        detected = metric.detect(words, languages, models)
        if count != detected.damage.near:
            raise ValueError(f"flag count mismatch: {row['doc_id']}")
        lccn, date, edition, sequence = row["doc_id"].split("_")
        pages.append({**row, "part": stored["part"], "eligible_pages": populations[language],
                      "selection_sha256": hashlib.sha256((SEED + row["doc_id"]).encode()).hexdigest(),
                      "text_sha256": hashlib.sha256(text.encode()).hexdigest(),
                      "head": detected.damage.head, "flagged": count,
                      "rescored_damage_rate": scored["damage_rate"],
                      "scan_url": f"https://www.loc.gov/resource/{lccn}/{date}/{edition}/?sp={sequence[4:]}"})
    (DATA / "sample.jsonl").write_text("".join(json.dumps(p, ensure_ascii=False) + "\n" for p in pages))
    payload = "".join(json.dumps(f, ensure_ascii=False) + "\n" for f in flags).encode()
    (DATA / "flags.jsonl.gz").write_bytes(gzip.compress(payload, mtime=0))
    write_json(DATA / "summary.json", summarize(pages, flags))
    print(json.dumps(summarize(pages, flags), indent=2))


def check():
    pages = load_lines(DATA / "sample.jsonl")
    flags = [json.loads(line) for line in gzip.decompress((DATA / "flags.jsonl.gz").read_bytes()).splitlines()]
    by_id = {p["doc_id"]: p for p in pages}
    if len(by_id) != 80 or len(pages) != 80:
        raise ValueError("expected 80 unique sampled pages")
    keys, counts = set(), collections.Counter()
    rules, occurrences = label_maps()
    used_rules, used_occurrences = set(), set()
    for flag in flags:
        key = (flag["language"], flag["doc_id"], flag["word_index"])
        if key in keys or flag["doc_id"] not in by_id or flag["language"] != by_id[flag["doc_id"]]["language"]:
            raise ValueError(f"invalid or duplicate flag: {key}")
        keys.add(key)
        counts[flag["doc_id"]] += 1
        expected = label_flag(flag, rules, occurrences)
        if any(flag[k] != v for k, v in expected.items()):
            raise ValueError(f"label mismatch: {key}")
        if key in occurrences:
            used_occurrences.add(key)
        elif (flag["language"], flag["word"]) in rules:
            used_rules.add((flag["language"], flag["word"]))
    if used_rules != rules.keys() or used_occurrences != occurrences.keys():
        raise ValueError("unused label decisions")
    for page in pages:
        if counts[page["doc_id"]] != page["flagged"]:
            raise ValueError(f"page count mismatch: {page['doc_id']}")
        if round(page["flagged"] / (page["flagged"] + page["head"]), 4) != float(page["damage_rate"]):
            raise ValueError(f"historical rate mismatch: {page['doc_id']}")
    summary = summarize(pages, flags)
    if any(summary[l]["pages"] != 20 for l in LANGUAGES):
        raise ValueError("expected 20 pages per language")
    if summary != json.loads((DATA / "summary.json").read_text()):
        raise ValueError("summary mismatch")
    print(f"OK: {len(pages)} pages, {len(flags)} flags, labels and historical rates reconcile")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("select", "fetch", "score", "check"))
    parser.add_argument("--source-dir", type=Path, help="local scratch directory containing the three snapshot inputs")
    args = parser.parse_args()
    if args.command == "check":
        check()
    elif args.source_dir is None:
        parser.error("--source-dir is required for this command")
    elif args.command == "select":
        selected, populations = selection(args.source_dir)
        print(json.dumps({"eligible_pages": populations, "doc_ids": [r["doc_id"] for r in selected]}, indent=2))
    elif args.command == "fetch":
        fetch(args.source_dir)
    else:
        score(args.source_dir)


if __name__ == "__main__":
    main()
