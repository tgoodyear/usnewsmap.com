# LoC re-OCR: actual Florida package and full repair-manifest join

Measured 27 October 2026 for [#809](https://github.com/tgoodyear/usnewsmap.com/issues/809).
This supersedes the earlier [guide-only refresh](2026-10-18-loc-refreshed-baselines.md#coverage-refresh-27-october-2026).
It is a bounded inventory, not an accuracy comparison or a queue-policy change.

## What was actually inspected

The official guide snapshot still reports **111 batches / 491,472 pages**, latest
addition **24 October**. Its SHA-256 is
`ceccea70c39fa69e1a4f4ce3b8a96d8ad9569c727b28b7c73b561fcaa5d7d511`.
NDNP-Open-OCR's latest verified software release remains v1.2.1, commit
`03533f5fa79518d1750421633e593dc722ad06f6`; this is not per-package provenance.

Downloaded the published `fu_three_ver02.tar.bz2` only, verified its official
metadata SHA-256 and byte count, then streamed every member without filesystem
extraction:

| Measurement                             | Observed                                                           |
| --------------------------------------- | ------------------------------------------------------------------ |
| Compressed archive                      | 91,788,172 bytes                                                   |
| Archive SHA-256                         | `79c3d15acf138ef13aefedb843c944bf2a78da947dc66ee54917ba4e2428d2d5` |
| Metadata SHA-256                        | `e1b3bc3aa7626c569e57e004f008548da127d159b69b3f15762c0f436c48215b` |
| Archive creation / ingestion            | 27 October 2026 11:23:06 UTC / 23 October 2026 11:57:32 -04:00     |
| Actual complete ALTO/text pairs         | 1,706, all `sn95026977`, 1906-01-01 through 1909-09-30             |
| Guide count                             | 1,722; the 16-page difference remains unexplained                  |
| Uncompressed member bytes               | 846,217,788                                                        |
| Explicit inspection bound               | 1,073,741,824 bytes (1 GiB)                                        |
| Embedded software, all 1,706 ALTO files | Tesseract 5.4.1 and ndnp-open-ocr 1.2.1                            |
| Exact repair-manifest page-ID matches   | 1,226; none in the keep set                                        |
| Verified source-image registrations     | 0                                                                  |

The actual archive exposed a code blocker: the inspector's fixed 512 MiB
uncompressed bound rejected this 846 MB package. `--max-uncompressed-mib 1024`
now explicitly permits this measured package; **the default remains 512 MiB**.
Checksum, 16 MiB per-member, canonical path/type, duplicate and complete-pair
checks remain unchanged. The configured bound is recorded in the evidence.

The previous 675-page `fu_eleven` sample embedded pipeline 1.2.0. Only this
inspected new sample establishes 1.2.1 inside its artifacts; do not extend that
attribution to all 111 packages.

## Current full-manifest denominators

Read-only snapshot: `pages-v20261010-2/manifest.parquet`, SHA-256
`ddc1fb2520cbea24ca9633e19da32fcce25f8ec010e473e77195f83462fc5140`.
The existing frozen projection policy produces **1,922,826 unique rows** and
**224,848 keep rows**, projected JSONL SHA-256
`a5423f5032d5c225fcee6c5647798d42e5fdc69e489767fec81be9acdc557976`.
This is not a modification of the live keep policy.

| Population                  |                 Denominator | Result                                                             |
| --------------------------- | --------------------------: | ------------------------------------------------------------------ |
| Full repair manifest        |             1,922,826 pages | 70,388 unique title/year-or-batch candidates; 1,852,438 not listed |
| Frozen keep set             |               224,848 pages | All not listed                                                     |
| Downloaded sample           |         1,706 canonical IDs | 1,226 exact repair-ID matches                                      |
| Original English acceptance | 100 crops / 18 source pages | Zero listed                                                        |
| Expanded English benchmark  | 300 crops / 18 source pages | All not listed                                                     |
| German benchmark            | 700 lines / 20 source pages | Outside NDNP                                                       |

The new **70,388** candidate count replaces the historical 55,585, rather than
adding guide totals to it. Candidate batch counts overlap and must not be summed.
The full report contains 7,326 title/language/decade/status strata. Repair human
reference availability remains `not-in-manifest`, not presumed absent.

The benchmark is unchanged `ocr-bench-v1.0.0`, content digest
`c5e38736b6e60610ef0dbe6d8296c3dc58f8a945d6e1cfea5aa47b03fb163800`,
manifest SHA-256 `ffd1d54147966c562d1f6e13c0fe2c2badb12c726406cbd50d61d8f8a395fe29`.
The original English subset digest is
`e99edaf91b5493fcd3eaf309cea9f31dbbeb23438bab865638ab92889cd14d04`;
all-1000 is `3f8972e16e61f568394aac5e2ec59591d654e5dde15a9b7b53b54e03ae31a39c`.

## Reproducible evidence

[Published metadata/hash-only receipts](https://github.com/tgoodyear/usnewsmap.com/releases/tag/data-loc-package-audit-20261027)
include both full/compact reports, the actual per-page artifact inventory,
official package metadata, frozen projection receipt/helper, and independent
rights/history observations. No raw OCR, ALTO, images or benchmark transcriptions
are redistributed. Downloaded release assets must pass `SHA256SUMS`.

```sh
gh release download data-loc-package-audit-20261027 \
  --repo tgoodyear/usnewsmap.com --dir loc-audit
(cd loc-audit && shasum -a 256 -c SHA256SUMS)
gh release download data-loc-inventory-20261027-ceccea7 \
  --repo tgoodyear/usnewsmap.com --dir loc-guide
(cd loc-guide && shasum -a 256 -c SHA256SUMS)
```

Obtain an authorized read-only copy of the exact repair Parquet above; it is not
an asset in this release. Using Python with PyArrow 25.0.1, the published
`project-repair.py` accepts that file and an existing empty output directory.
It checks the input hash, preserves row order and serializes the documented
fields as sorted-key compact JSON plus newline. Its output and receipt were
independently regenerated and compared byte-for-byte before publication.
Fetch and verify the benchmark through [the existing release protocol](../ocr-bench.md).

```sh
mkdir loc-reproduced
python3 loc-audit/project-repair.py /path/to/repair-manifest.parquet loc-reproduced
cmp loc-audit/repair-projection-receipt.json loc-reproduced/repair-projection-receipt.json
python3 scripts/ocr-bench/loc_inventory.py \
  --coverage-html loc-guide/guide.html --benchmark /path/to/verified/manifest.json \
  --repair-pages loc-reproduced/repair-pages.jsonl --retrieved-at 2026-10-27 \
  --out loc-reproduced/repair-inventory.json --summary-out loc-reproduced/repair-summary.json
cmp loc-audit/repair-inventory.json loc-reproduced/repair-inventory.json
cmp loc-audit/repair-summary.json loc-reproduced/repair-summary.json
```

For a separately authorized bounded retrieval, use the official package URL in
`fu-three-metadata.json` and respectful shared source pacing; do not start an
independent source consumer beside the active LoC fetcher. Verify both compressed
size and SHA-256 before inspection. The inspector then verifies them again:

```sh
python3 scripts/ocr-bench/loc_archive.py \
  --archive /path/to/fu_three_ver02.tar.bz2 --metadata loc-audit/fu-three-metadata.json \
  --repair-pages loc-reproduced/repair-pages.jsonl --retrieved-at 2026-10-27 \
  --max-uncompressed-mib 1024 --out loc-reproduced/fu-three-inventory.json \
  --summary-out loc-reproduced/fu-three-summary.json
cmp loc-audit/fu-three-inventory.json loc-reproduced/fu-three-inventory.json
cmp loc-audit/fu-three-summary.json loc-reproduced/fu-three-summary.json
```

The canonical page-list digest is
`fe5dc5d8258efb7273d5dc9779724a55b7dde92a9fac5d8e689d11acd9ad0021`.
Use fresh output paths; existing evidence is never overwritten.

## Rights, missing history and source identity

The [inspected official page metadata](https://www.loc.gov/resource/sn95026977/1909-07-01/ed-1/?sp=7&fo=json)
contains LoC's underlying-newspaper public-domain/no-known-restrictions statement,
including US newspapers older than 95 years; it also leaves independent legal
assessment to users. That is not a separately granted OCR/layout license.
The package has only its 3,412 canonical OCR members, with no license file.
All 1,706 ALTO Description sections were inspected without rights/license metadata.
The inspected index, package metadata and item response supply no separate text,
ALTO or layout-artifact license. These independent qualifications are recorded
by asset type in `rights-and-history.json`; upstream software CC0 is not inherited.
Raw sampled OCR/layout assets remain unredistributed pending source-specific terms.

The saved official OCR index links `fu_three_ver02`, not a version 1 package.
No same-page legacy archive was recovered; this is not proof of global absence.
The 16-page coverage discrepancy remains unknown, not invented as successful OCR.

The selected page's `fulltext_file` points to batch version 2's `0013.xml`,
whereas its resource thumbnail points to frame `0007`. Neither that thumbnail
nor the ALTO temporary input filename establishes the selected scan's bytes.
ALTO coordinates are `inch1200`, not pixels. Exact canonical IDs prove artifact
availability, **not** legacy/refreshed scan equality or current-corpus replacement
need. Unsupported page-level source/version attribution is deliberately explicit.

Before #810/#811 comparisons or replacement policy, the source/evaluation owner
must obtain applicable OCR/layout redistribution terms, map the same scan using
authoritative source files/hashes and coordinate transforms, and establish an
independently transcribed overlap set. No source-specific permission, human
ground truth, refreshed CER or queue-skip acceptance is manufactured here.
