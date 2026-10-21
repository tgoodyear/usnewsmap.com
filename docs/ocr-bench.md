# OCR benchmark releases

A benchmark release is one versioned archive of newspaper images and their reference
transcriptions. Any OCR engine can be run and scored on it, and the scores can be compared
because everyone uses the same bytes. Each release is a GitHub release of this repository,
tagged `ocr-bench-<version>`. It has a content digest to cite and named subsets that resolve to
the same files ([#738](https://github.com/tgoodyear/usnewsmap.com/issues/738)).

Candidate licensing, model-availability and bounded local CPU findings are recorded in [the 2026-10-18 feasibility note](notes/2026-10-18-ocr-bench-candidate-feasibility.md).

## Fetch a release

```sh
scripts/ocr-bench/fetch --version v1.0.0 --verify
scripts/ocr-bench/fetch --version v1.0.0 --verify --subset legacy/overproof-en-100
```

The script reads the archive name, its sha256 and the content digest from
[`ocr/bench/releases.json`](../ocr/bench/releases.json). It downloads the archive from the
GitHub release and refuses it if the sha256 differs. It then unpacks it to
`ocr-bench/usnm-ocr-bench-v1.0.0/`, or `--dest DIR`. `--verify` checks every file against
`SHA256SUMS`, refuses files that aren't listed, and checks that `SHA256SUMS` hashes to the
content digest. `--subset` prints the subset's image paths. It needs bash, curl, tar, jq and
`sha256sum` or `shasum`. It needs no Azure account, credentials or BOINC attachment.

Cite a result as release, subset, subset digest and content digest, for example
`v1.0.0`, `legacy/overproof-en-100`, plus the two digests from `releases.json`.

## What a release holds

| Path                    | Contents                                                                     |
| ----------------------- | ---------------------------------------------------------------------------- |
| `images/<id>.jpg`       | The exact image bytes.                                                       |
| `references/<id>.txt`   | The reference transcription as UTF-8, unchanged from the source.             |
| `examples/<name>.jsonl` | Recorded engine outputs (`{"id", "text"}`) used for scorer parity.           |
| `LICENSES/<source>/`    | The source's licence files and a `NOTICE.txt` with attribution and licences. |
| `RELEASE-NOTES.md`      | Subsets, known limitations and licences.                                     |
| `manifest.json`         | Samples, subsets, sources, coverage, scoring rules and limitations.          |
| `SHA256SUMS`            | Every other file, the manifest included. Its sha256 is the content digest.   |

Each sample in the manifest records:

- its id, source, source page id and unit (`crop` or `line-pair`);
- the image's path, sha256, size and pixel dimensions;
- the reference's path, sha256 and status (`human-verified` in v1);
- languages, scripts, typeface, language mix and damage labels;
- its split;
- the source's provenance record, unchanged, including the crop box on the source page;
- the campaign and job it was first scored in.

Each subset records its members, sample and source-page counts, how it was made, and a digest:
the sha256 of one line per member, `id image-sha256 reference-sha256`, in id order.

## Annotation and source admission

The [transcription, illegibility-mask and damage policy](ocr-bench-annotation-policy.md)
defines new reviewed annotations, Unicode offsets and real versus unknown damage.
The [source audit and measured coverage](notes/2026-10-18-ocr-bench-source-audit.md)
separates admissible public GT from candidates and unmet tier targets.

## Release v1.0.0

v1.0.0 preserves the samples of the Iris Xe campaigns
([#703](https://github.com/tgoodyear/usnewsmap.com/issues/703)) byte for byte. The images are
the `page.jpg` files the campaigns dispatched. The references are the campaigns' own published
references, and each sample's IDs, checksums and provenance are copied unchanged. The recorded
outputs of the accepted 100-crop pilot are included. The build rescores them and refuses to
finish unless it gets the pilot's 1.975175% CER exactly.

| Subset                                        |  Samples | How it was made                                                          |
| --------------------------------------------- | -------: | ------------------------------------------------------------------------ |
| `legacy/overproof-en-100`                     |      100 | The original 100-English-crop acceptance set, unchanged.                 |
| `legacy/overproof-en-paired-a-100`            |      100 | The first 100-crop paired campaign.                                      |
| `legacy/overproof-en-paired-b-100`            |      100 | The second 100-crop paired campaign.                                     |
| `legacy/reichsanzeiger-de-lines-000` … `-600` | 100 each | The seven German line-pair campaigns.                                    |
| `overproof-en/all-300`                        |      300 | The three English sets.                                                  |
| `overproof-en/smoke-10`                       |       10 | Taken from `legacy/overproof-en-100` by seeded hash order.               |
| `reichsanzeiger-de/all-700`                   |      700 | The seven German sets.                                                   |
| `reichsanzeiger-de/acceptance-100`            |      100 | Taken from `reichsanzeiger-de/all-700` by seeded hash order.             |
| `reichsanzeiger-de/smoke-10`                  |       10 | Taken from `reichsanzeiger-de/acceptance-100`.                           |
| `all-1000`                                    |    1,000 | Everything. English and German are not balanced; report each source too. |

The manifest and `releases.json` give the source-page count for each subset. The 1,000 samples
are cut from 38 newspaper pages (18 English, 20 German), so they are not 1,000 independent
pages.

### Sources and licences

| Source                                                            | Images                              | References                               |
| ----------------------------------------------------------------- | ----------------------------------- | ---------------------------------------- |
| Overproof dataset 3 (Chronicling America), via HIPE-OCRepair-2026 | Public domain (Library of Congress) | CC BY-NC-SA 4.0 (HIPE's structured copy) |
| Reichsanzeiger GT, Mannheim University Library                    | Public Domain Mark 1.0              | CC BY 4.0 (Zenodo record)                |

Because of the NC clause, the English references may be redistributed for non-commercial use
only. Keep the attribution in `LICENSES/` with any copy.

### Coverage and gaps

v1.0.0 does not meet these targets from #738:

| Area            | v1.0.0                   | Missing                                                                                                  |
| --------------- | ------------------------ | -------------------------------------------------------------------------------------------------------- |
| Languages       | English, German          | Japanese, Spanish, French, Italian, Danish, Finnish, Lithuanian, Czech, Russian, Serbian, Hebrew/Yiddish |
| German typeface | Not labelled             | Fraktur and Antiqua labels                                                                               |
| Mixed language  | None                     | Real mixed pages, regions and lines with verified references                                             |
| Image damage    | Unannotated              | Every damage label and severity                                                                          |
| Units           | Crops, line pairs        | Full pages (`pages-10/100/1000`)                                                                         |
| Tiers           | Smoke 10, acceptance 100 | `standard-1000` and `extended-10000` per track                                                           |
| Splits          | All `public-eval`        | A held-out test track                                                                                    |

Candidate sources for the missing languages are reviewed in
[docs/ocr-gold-set.md](ocr-gold-set.md). They include the NewsEye French, Finnish and Swedish
sets, the Austrian newspapers and NZZ. Each new language or label comes as a new release.

## Release v1.1.0: additional public multilingual tracks

v1.1.0 adds 52 CC BY 4.0 NewsEye validation-region image/GT pairs from 18
source pages: French 24 (8 pages), Finnish 18 (6), Swedish 10 (4). It does not
repeat or replace the 1,000 v1.0.0 samples. Sources, licences, exact counts,
selection and unmet targets are in the [source audit](notes/2026-10-18-ocr-bench-source-audit.md).

```sh
scripts/ocr-bench/fetch --version v1.1.0 --verify --subset multilingual/all-52
```

Content digest: `329c73b5fbaf56b91cd5cd5b1b0cdf131e15ec43391ac9e459e4a423a88eb01e`.
Archive SHA-256: `0a015eb4085af36b015566f1311411d12aef802d461442bd8c3e963bff932923`.
See `releases.json` for all six subset digests. Principal language labels are
inherited from the publisher; damage and language-mix status are **unannotated**.
This is a multilingual pool, not verified mixed-language samples, reviewed
damage strata, full pages, or per-language standard/extended tiers. Source
conventions are preserved, with no new diplomatic re-transcription claimed.

## Scoring

```sh
usnm-ocr-survey bench-select --release DIR --subset ID > jobs.jsonl
usnm-ocr-survey bench-score --release DIR --subset ID --outputs outputs.jsonl
```

`bench-select` lists a subset's image paths and checksums for an engine runner. It leaves the
references out unless `--with-references` is given, so they can't end up in a model prompt by
mistake. `bench-score` reads outputs as JSONL rows `{"id", "text"}` and reports the following,
in total and per source:

- `cer`, over the samples that have an output;
- `cer_missing_as_empty`, which scores each missing output as empty;
- the counts of samples scored and missing.

An output for an id outside the subset is an error.

| Rule           | Meaning                                                                                                                                                                                             |
| -------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `legacy-ws-v1` | The Iris pilot's rule. Whitespace runs collapse to one space and the ends are trimmed, as in Python's `" ".join(s.split())`. Then character (code point) edit distance. Case and punctuation count. |
| `raw-v1`       | Character edit distance with no normalization.                                                                                                                                                      |

`raw-masked-v1` is an opt-in ambiguity-aware lower-bound score using frozen
reference-side illegibility spans; [the policy](ocr-bench-annotation-policy.md)
defines alignment and denominators. Always report `raw-v1` alongside it. It does
not change either historical rule or any published release.

These are character-level rules. Word error rates depend on how a script is tokenized, so they
are left out until there is a definition for each script.

The separately versioned [extended evaluation measures v1](ocr-evaluation-measures-v1.md) add
historical-fidelity adjudications, paired page scoring, full-page region coverage/reading order,
truncation accounting and fixed search-relevance metrics. They consume generic reference/output
pairs and do not change this legacy scorer or any benchmark release.

## How a release is built

A release has two committed files: a recipe, `ocr/bench/<version>/spec.json`, and an inventory
of its inputs, `ocr/bench/<version>/inputs.sha256`. For v1.0.0 the inputs are the ten Iris
campaign roots on the BOINC project share. A copy of them is served from the project's public
download directory as `usnm-ocr-bench-inputs-v1.0.0.tar`.

The `ocr-bench` workflow runs on every pull request that touches the benchmark:

1. It downloads the inputs and checks every file against the inventory.
2. It runs `usnm-ocr-survey bench-build`, which checks each campaign's sha256, every job and
   every image against its campaign record.
3. It requires the built archive, digests and subsets to equal the release's entry in
   `releases.json`.

The build is reproducible, so a second build gives the same bytes. After merge, the workflow
publishes the GitHub release, fetches it back and verifies it. Once a release is published, its
spec and inventory must not change. A fix or an addition is a new version, so earlier results
stay valid for the bytes they were measured on.

To add a release:

1. Write `ocr/bench/<version>/spec.json` and `inputs.sha256`, and stage the inputs tar.
   A recipe may set an HTTPS `inputs_url` for a public immutable GitHub release
   asset; absent this, the original input host is used. The committed inventory
   still pins every input byte. Published release recipes must never change.
2. Open a pull request. The check job prints the release's `releases.json` entry.
3. Review the entry, commit it and merge.

## LoC published baselines

Legacy and refreshed LoC OCR are **separate pinned baselines**, not human
references. The [18 October inventory](notes/2026-10-18-loc-refreshed-baselines.md)
records current coverage, the original benchmark's coverage gap and unresolved
rights/history checks. Neither code release dates nor matching title names prove
which OCR artifact or scan a page uses.

Save the public guide HTML and fetch the benchmark with `--verify` above, then:

```sh
python3 scripts/ocr-bench/loc_inventory.py \
  --coverage-html improved-text.html --benchmark RELEASE/manifest.json \
  --retrieved-at 2026-10-18 --out inventory.json \
  --summary-out inventory-summary.json
```

Optionally add `--repair-pages repair-pages.jsonl`; its explicit export schema and
the measured manifest checksum are in the note. The full output contains
per-sample statuses and title/language/decade repair strata. The compact summary
omits these rows but preserves coverage, denominators and hashes. Candidate
title/year ranges and batch-base matches are **not confirmed page coverage**.
`not-listed-in-guide` is not proof of global absence; `outside-ndnp` distinguishes
non-LoC sources.

Use only the official public bulk endpoints, respecting the published request
budget (10 bulk requests per 10 minutes per IP), `Retry-After`, and access blocks.
Do not run parallel collection-wide retrievals or work around HTTP 403. The
sample can be retrieved as two bounded requests (metadata, then archive):

```sh
curl --fail --max-filesize 10000 --output metadata.json \
  https://chroniclingamerica.loc.gov/data/ocr/metadata/fu_eleven_ver02.tar.bz2.json
sleep 61
curl --fail --max-filesize 60000000 --output sample.tar.bz2 \
  https://chroniclingamerica.loc.gov/data/ocr/fu_eleven_ver02.tar.bz2
python3 scripts/ocr-bench/loc_archive.py \
  --archive sample.tar.bz2 --metadata metadata.json --retrieved-at 2026-10-18 \
  --out artifacts.json --summary-out artifacts-summary.json
```

Coordinate pacing with other users of the same IP. These commands do not retry
failed requests. Pin metadata SHA-256 as well as its archive SHA-256; the sample's
measured values are in the note. `loc_archive.py` checks archive bytes and page
count against metadata, bounds uncompressed content, never extracts paths and
records each ALTO/text hash, canonical page ID, coordinate units and embedded
software versions. An optional `--repair-pages` joins the actual archive page IDs
against the same repair export. It does **not** prove source-image registration.
All output paths are create-only.

### Registered crop adapter

`loc_align.py` emits the existing scorer's `{"id","text"}` JSONL plus a
checksummed alignment report. Choose `--baseline loc-legacy-v1` or
`--baseline loc-refreshed-v1` and a **new output directory for each artifact set**.
Keep historical baseline files and scores untouched.

For each available sample, supply one JSONL binding with the following fields:

```json
{
  "id": "SAMPLE-ID",
  "source_page_url": "EXACT-BENCHMARK-PAGE-URL",
  "source_image": "/local/full-resolution-page.jpg",
  "source_image_sha256": "EXACT-BENCHMARK-SOURCE-PAGE-HASH",
  "alto": "/local/pinned-page.alto.xml",
  "alto_sha256": "PINNED-ALTO-HASH",
  "scale": 0.25,
  "artifact_url": "PUBLIC-ARTIFACT-URL",
  "retrieved_at": "ISO-RETRIEVAL-DATE",
  "version_evidence": "Actual embedded version or explicit unknown; not latest software tag",
  "registration_evidence": "Evidence of same scan, orientation, no translation or deskew, and scale"
}
```

The example scale is **not a default**. Registration is an operator-reviewed
input, not something the adapter proves from a URL. Do not create a binding from
title/year overlap alone. The adapter verifies source-image bytes against the
benchmark's original full-page hash, crop/reference hashes, actual JPEG
dimensions, unresized crop geometry, supported ALTO units, explicit isotropic
scale and page extent agreement within one pixel. Rotated, offset, deskewed or
otherwise uncertain pairs require a separately validated registration policy;
omit their bindings here and retain the missing status.

Policy `loc-word-centres-xml-order-v1` reuses the survey ALTO helper, but supplies
the scale explicitly after validating all word/line coordinates. It selects
Strings whose centres are inside the exact crop, keeps XML line/word order,
counts intersecting boundary lines and excludes HYP elements. It never guesses
DPI, silently drops malformed coordinates or substitutes whole-page text for
a crop. This is a crop transcription adapter, **not whole-page reading-order
evaluation**. OCR inference cost is `unknown`, not zero.

```sh
python3 scripts/ocr-bench/loc_align.py \
  --release RELEASE --subset SUBSET --baseline loc-refreshed-v1 \
  --bindings refreshed-bindings.jsonl --out-dir refreshed-aligned &&
usnm-ocr-survey bench-score \
  --release RELEASE --subset SUBSET --outputs refreshed-aligned/outputs.jsonl \
  --rule legacy-ws-v1 --report refreshed-ws.json &&
usnm-ocr-survey bench-score \
  --release RELEASE --subset SUBSET --outputs refreshed-aligned/outputs.jsonl \
  --rule raw-v1 --report refreshed-raw.json
```

Inspect `alignment.json` **before either score**. Every selected sample has an
aligned, missing or error status. Missing bindings stay in the denominator;
alignment errors also produce a nonzero exit, rather than a success-shaped
fallback. Do not rank engines on different successful subsets. Use a common
verified paired subset and report all missing outputs and errors alongside it.
WER remains unavailable until the shared benchmark defines its tokenization
policy; no second scorer is introduced here.
