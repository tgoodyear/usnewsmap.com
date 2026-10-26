# Japanese release count reconciliation (#324)

Archived data-file links below download the [report-data archive](https://github.com/tgoodyear/usnewsmap.com/releases/download/data-report-data-20261023-48fbb2a/report-data.tar.gz), not individual files. The [published member list](https://github.com/tgoodyear/usnewsmap.com/releases/download/data-report-data-20261023-48fbb2a/report-data.members.txt) identifies each original path. Extract the archive at the repository root to restore those paths before running the historical reproduction commands.

Read-only reconciliation completed on 18 October 2026 (UTC), for
`pages-v20261006-2`, built from commit
`df5ab03d9397d13790a4b8d31d007cf03fb5a4e3`.

## Answer

The 109 pages were **intentionally excluded by the historical OCR-status
predicate**, not lost to deduplication, language routing, or an unexplained
split failure. Their selected OCR rows have **107 `short` results (2–19 text
characters) and 2 `empty` results (zero characters)**. All 109 have a non-null
text field. The historical rule was `ok && printed.is_some()`; `ok` meant
`text_status == "ok"`.

[excluded-pages.csv](https://github.com/tgoodyear/usnewsmap.com/releases/download/data-report-data-20261023-48fbb2a/report-data.tar.gz) contains every excluded page ID,
status, text length, source batch, selected timestamp, selected part path,
and that part's historical SHA-256. [summary.json](https://github.com/tgoodyear/usnewsmap.com/releases/download/data-report-data-20261023-48fbb2a/report-data.tar.gz) contains
counts and hashes, including the digest of the reconstructed eligible IDs
(sorted by doc_id, UTF-8, one per line with a final newline).

| Stage | Count |
| --- | ---: |
| Historical parts, all SHA-256 and length verified | 5,239 |
| Verified input bytes | 114,177,160 |
| Raw rows | 17,286 |
| Duplicate rows beyond the distinct page IDs | 6,872 |
| Distinct IDs / selected pages | 10,414 |
| Out-of-version or uncatalogued pages (`skipped`) | 0 |
| Selected `ok` rows with text / indexable pages | 10,305 |
| Selected `short` rows excluded | 107 |
| Selected `empty` rows excluded | 2 |
| Selected rows excluded for missing text | 0 |

The 6,872 duplicate rows are resolved **before** the 10,414 selected-page
counter, so they do not explain the subtraction of 109. Selected exclusion
rows have `loc_text` values `garbled` (70), `missing` (33), and `short` (6);
that field describes LoC's original text, not our OCR eligibility status.
The historical `near_blank` kind count of 120 is also not the eligibility
rule. The later mixed-page Japanese-character threshold did not exist in
this release and is not part of this calculation.

## Evidence and method

Used existing Entra login to read Azure Blob Storage account
`stusnmdffapzy`. No keys, restore, undelete, writes to cloud storage, jobs,
rebuilds, infrastructure changes, or deployment were used.

1. Read the release's `reference/pages-v20261006-2/manifest.json`,
   `ocr_ja.json`, `batches.json`, and `titles.json`. Their SHA-256 digests are
   retained in `summary.json`; the three snapshot artifacts match the
   manifest's recorded lengths and hashes. `ocr_ja.json.parts` exactly
   matches `manifest.json.built_from.ocr_ja`.
2. Downloaded **every** part named in that historical list from
   `curated/ocr-ja/pages/`, not today's prefix listing. Each of the 5,239
   downloaded byte sequences matched its historical length and SHA-256.
   Thus newer/unlisted OCR writes were not substituted for release inputs.
3. Read all 17,286 Parquet rows. Checked each `doc_id` against its
   `lccn`, date, edition and sequence, and checked non-null timestamps.
   Among rows with a batch in historical `batches.json` and a title in
   historical `titles.json`, selected the greatest `(ocred_at, part_path)`
   tuple per ID, matching the historical loader. Applied the historical
   status-and-present-text predicate to all 10,414 selected rows.
4. Reproduced **exactly** `pages=10414`, `indexed=10305`, `skipped=0` in
   `ocr_ja.json`. The manifest's Japanese layout independently records
   10,305 documents in two splits (7,632 + 2,673; 90,938,874 bytes).

Historical source:

- [Selection, row decoding and eligibility](https://github.com/tgoodyear/usnewsmap.com/blob/df5ab03d9397d13790a4b8d31d007cf03fb5a4e3/crates/usnm-ingest/src/ocr_ja.rs#L59-L205).
- [Index construction](https://github.com/tgoodyear/usnewsmap.com/blob/df5ab03d9397d13790a4b8d31d007cf03fb5a4e3/crates/usnm-ingest/src/release.rs#L852-L882)
  sends every eligible document and calls `sink.finish(n)`.
- [OCR text status](https://github.com/tgoodyear/usnewsmap.com/blob/df5ab03d9397d13790a4b8d31d007cf03fb5a4e3/ja-ocr/jaocr.py#L446-L448)
  classifies empty/short text with `MIN_PAGE_CHARS = 20`.

### Reproduction

With Azure CLI already logged in, download each reference artifact using
this pattern (change `manifest.json` to each of the other three names):

```sh
az storage blob download --account-name stusnmdffapzy --auth-mode login \
  --container-name reference \
  --name pages-v20261006-2/manifest.json --file manifest.json --no-progress
```

For every entry in `ocr_ja.json.parts`, retrieve the named blob from
container `curated` with `--auth-mode login`, preserving its relative
`ocr-ja/pages/...` path. Verify its `bytes` and `sha256` before reading it.
The following Python (requires PyArrow) summarizes the selection; run it
from the directory containing the four reference JSONs and downloaded
`ocr-ja/pages/` hierarchy:

```python
import collections
import hashlib
import io
import json
from pathlib import Path

import pyarrow.parquet as pq

record = json.loads(Path("ocr_ja.json").read_text())
batches = {r["batch"] for r in json.loads(Path("batches.json").read_text())}
titles = {r["lccn"] for r in json.loads(Path("titles.json").read_text())}
newest, seen = {}, set()
raw = 0
for part in sorted(record["parts"], key=lambda p: p["path"]):
    data = Path(part["path"]).read_bytes()
    assert len(data) == part["bytes"]
    assert hashlib.sha256(data).hexdigest() == part["sha256"]
    for row in pq.read_table(io.BytesIO(data)).to_pylist():
        raw += 1
        key = row["doc_id"]
        assert key == (f"{row['lccn']}_{row['date']}_ed-{row['edition']}"
                       f"_seq-{row['seq']}")
        seen.add(key)
        if row["batch"] not in batches or row["lccn"] not in titles:
            continue
        row["part"] = part["path"]
        old = newest.get(key)
        if old is None or (row["ocred_at"], row["part"]) > (old["ocred_at"], old["part"]):
            newest[key] = row
excluded = {k: r for k, r in newest.items()
            if r["text_status"] != "ok" or r["text"] is None}
assert (raw, len(seen), len(newest), len(excluded)) == (17286, 10414, 10414, 109)
assert len(newest) - len(excluded) == record["indexed"] == 10305
print(collections.Counter(r["text_status"] for r in excluded.values()))
print("\n".join(sorted(excluded)))
```

## Scope and rebuild implications

This is a **complete, hash-verified historical input reconstruction**, not
an exported historical index ID set. The historical splits remain
soft-deleted and no recovery was attempted. No claim is made that a modern
search endpoint exports those historical documents. The reconstruction
identifies exactly the 109 inputs the actual build deliberately did not
send to its Japanese index and accounts for the entire recorded count
difference; an independent post-publication doc-ID audit would be a
separate integrity check, not an unexplained remainder of this count.

No ingest/index-filter defect was found, so there is no code fix and no
repair rebuild required for #324. #283 continues to own rebuilds: its next
release should load its own complete, current overlay snapshot and apply
that release's eligibility rules, rather than force these 109 historical
non-OK results into the index. Later OK OCR replacements can become
eligible; no assertion is made here about the current status of these IDs.
Re-OCR or human assessment of their scans is outside this reconciliation.
