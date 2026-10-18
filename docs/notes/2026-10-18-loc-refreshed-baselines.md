# Latest LoC OCR: release inventory and the fixed benchmark coverage gap

Measured 18 October 2026 for [#809](https://github.com/tgoodyear/usnewsmap.com/issues/809)
and [#810](https://github.com/tgoodyear/usnewsmap.com/issues/810).
This is an initial inventory and adapter implementation, **not a completed accuracy comparison**.
No running worker, queue, production index, or published benchmark was changed.

## Sources and versions

The [improved-text guide](https://guides.loc.gov/chronicling-america/improved-text),
modified 1 October, lists the following coverage. The
[machine-readable snapshot](../data/loc-refreshed-2026-10-18.json) contains every
listed batch, its publication date, title/year hints, denominators and source hashes.

| Measurement                                            | Result                                                             |
| ------------------------------------------------------ | ------------------------------------------------------------------ |
| Reprocessed batches in guide                           | 107                                                                |
| Reported pages, summed and checked against guide total | 472,768                                                            |
| Latest batch-added date                                | 2026-09-30                                                         |
| Guide HTML SHA-256                                     | `30d7343ee5b4d2393f8fa9f26786e933cad95f37eb79177bd162b06ba18a1f44` |
| NDNP-Open-OCR latest software release                  | `v1.2.1`, published 2026-07-24T17:20:42Z                           |
| Release commit                                         | `03533f5fa79518d1750421633e593dc722ad06f6`                         |
| Upstream main at inspection                            | `cbe0ca5321a5f61c2b664ebba3cf8faf7ac1f494`, 2026-09-10             |

The datasets portal returned HTTP 403. It was not bypassed. The independently
documented public [bulk data endpoint](https://chroniclingamerica.loc.gov/data/)
worked. Its `ocr/` index lists compressed text/ALTO packages; `ocr/metadata/`
provides per-package SHA-256, byte count, archive creation timestamp, batch,
ingestion date, title identifiers and page count. The `batches/` tree provides
batch XML, issue METS and BagIt manifests; batch version changes are not proof
that two page images are identical.

**Software release, ingestion, guide-added date and archive creation are different
identities.** Do not apply the latest code tag to all published OCR.

## A bounded latest-batch sample

Downloaded only `fu_eleven_ver02.tar.bz2` (56,026,786 bytes), verified against
[LoC metadata](https://chroniclingamerica.loc.gov/data/ocr/metadata/fu_eleven_ver02.tar.bz2.json).
The archive was inspected without extracting its paths.
The [sample summary](../data/loc-refreshed-sample-2026-10-18.json) pins the metadata,
archive, per-page inventory digest and exact repair-page matches.

| Measurement                                           | Result                                                             |
| ----------------------------------------------------- | ------------------------------------------------------------------ |
| Guide-added date                                      | 2026-09-30                                                         |
| Archive creation                                      | 2026-10-05T16:52:42.586285+00:00                                   |
| Archive SHA-256                                       | `f547144b56208253548135d24127a9c2fee8c30c255b07e51417a31eb6d1b91a` |
| Metadata SHA-256                                      | `34474a697809ff9123a47a473b0f5d77416e05e4fcd40465ace3ebd23643a14f` |
| ALTO/text pairs actually present                      | 675                                                                |
| Guide's batch page count                              | 708                                                                |
| Embedded recognition software, all 675 ALTO files     | Tesseract 5.4.1                                                    |
| Embedded post-processing software, all 675 ALTO files | NDNP-Open-OCR 1.2.0                                                |
| Exact canonical page-ID matches in repair manifest    | 4                                                                  |
| Exact source-scan registration proved                 | 0                                                                  |

The 708/675 discrepancy is retained, not reconciled by assumption. An OCR archive's
page count need not equal all pages in an NDNP batch. No claim is made about the
33-page difference's cause.

For example, `sn84027621_1905-06-26_ed-1_seq-1` has ALTO member
`sn84027621/1905/06/26/ed-1/seq-1/ocr.xml`, SHA-256
`f3c7124220d86ca6e92e9943b5f884bc4419b736c65836c4ff2f3670395c9437`.
The XML uses **inch1200**, not pixels. Its page is 28,388 by 37,500 coordinate
units. Its temporary input filename does not establish the source image hash.
Inferring pixel coordinates with an assumed 300 DPI would not meet our
comparison requirements.

The four repair matches already have `fu_eleven` version 2 in the current
manifest. Availability of refreshed artifacts therefore does **not** by itself
mean the current corpus needs a replacement. Artifact equality and source
scan registration still need checking before #811 can skip or replace work.

## Coverage denominators

Benchmark: immutable `ocr-bench-v1.0.0`, content digest
`c5e38736b6e60610ef0dbe6d8296c3dc58f8a945d6e1cfea5aa47b03fb163800`.
Original 100-sample subset digest
`e99edaf91b5493fcd3eaf309cea9f31dbbeb23438bab865638ab92889cd14d04`.
The release was fetched with verification; no members or references were changed.

| Set                                 | Denominator                      | Published title/year candidates | Interpretation                                        |
| ----------------------------------- | -------------------------------- | ------------------------------- | ----------------------------------------------------- |
| Original English acceptance         | 100 crops / 18 source pages      | 0                               | Not listed, not a proof of global absence             |
| All English benchmark               | 300 crops / 18 source pages      | 0                               | Same coverage gap                                     |
| German benchmark                    | 700 line pairs / 20 source pages | Outside NDNP                    | Not eligible for an English LoC comparison            |
| Repair manifest `pages-v20261010-2` | 1,922,826 unique pages           | 55,585 unique pages             | Candidates only; no automatic coverage assertion      |
| Current keep set                    | 224,848 pages                    | 0                               | No change to fetcher/queue policy                     |
| One downloaded refreshed archive    | 675 published page IDs           | 4 exact repair matches          | OCR available by ID; source-image identity unverified |

Repair Parquet SHA-256:
`ddc1fb2520cbea24ca9633e19da32fcce25f8ec010e473e77195f83462fc5140`.
The read-only join counts a title/year range or matching batch base as a
candidate, deliberately tolerating version changes. Candidates may occur in
several batches and must not be summed to infer unique pages.
The full generated report includes 7,321 title/language/decade/status strata.
The committed compact report omits those strata and individual benchmark rows;
the same command regenerates them. The repair manifest does not assert human
reference availability, recorded as `not-in-manifest`, rather than presumed zero.
Benchmark references retain their existing human-verified status and licenses.

## Reproduction

Use the commands in [the benchmark guide](../ocr-bench.md#loc-published-baselines).
The inventory parser is offline and fails on missing tables, duplicate batches
or inconsistent published totals. Save the original guide bytes: a later live
guide can legitimately have a different checksum and coverage.

For the existing repair manifest, export newline-delimited JSON with fields
`doc_id`, `batch` (base name), `lccn`, `date` (ISO date), `lang` (`und` for null),
and `keep` (boolean). `keep` must use `usnm_ocr_contract::prefetch::set_of`'s policy:
non-English/unknown, or one of the five `WORST_ENGLISH` batch names.
The operational projection used:

```sql
COPY (
  SELECT doc_id, batch, lccn, date, coalesce(lang, 'und') AS lang,
    (coalesce(lang, 'und') <> 'eng' OR batch IN (
      'nn_kant', 'txdn_japan', 'nn_bentham', 'nn_carson', 'kyu_frenchie'
    )) AS keep
  FROM read_parquet('repair-manifest.parquet')
) TO 'repair-pages.jsonl' (FORMAT JSON);
```

The projection is for this measured manifest, not a second configurable
production keep policy. Revisit it if the shared Rust policy changes.

## Rights, history and remaining gates

The upstream [software license](https://github.com/LibraryOfCongress/ndnp-open-ocr/blob/03533f5fa79518d1750421633e593dc722ad06f6/LICENSE.md)
is US-government public domain / CC0, but explicitly identifies independently
licensed incorporated components. It is **not** a blanket license for newspaper
content, model weights, images, human references or derived layout assets.
The public package index/metadata does not supply a separate artifact license.
Item-level rights and redistribution checks for text, ALTO and layout assets
remain open; the downloaded sample is not redistributed in this change.
Public URLs, hashes, aggregate counts and software provenance are recorded here.

The current `ocr/` index lists `fu_eleven_ver02`, not a version 1 archive.
No legacy copy of these same pages was recovered. Do not designate an arbitrary
older batch or our existing fixture as the sample's historical OCR.

**#809 remains open:** extend exact page-level inspection beyond this bounded
sample, resolve historical availability and artifact rights, and prove scan
identity for selected comparison pages. A current candidate join is not a full
472,768-page coverage audit.

**#810 remains open:** the strict legacy/refreshed adapter is implemented, but
there is no verified refreshed overlap with the fixed human-reference set.
Select and publish a new named, independently transcribed overlap subset under
the benchmark release process; do not edit v1.0.0. Then run the versioned scorer
against all available engines, inspect worst cases and compute page-grouped
paired uncertainty. Whole-page coverage/reading order remains a separate task.
No refreshed CER, WER or superiority claim is reported here. WER also remains
gated on the benchmark's documented script/tokenization policy.
