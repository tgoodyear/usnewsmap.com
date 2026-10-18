# OCR benchmark releases

A benchmark release is one versioned archive of newspaper images and their reference
transcriptions. Any OCR engine can be run and scored on it, and the scores can be compared
because everyone uses the same bytes. Each release is a GitHub release of this repository,
tagged `ocr-bench-<version>`. It has a content digest to cite and named subsets that resolve to
the same files ([#738](https://github.com/tgoodyear/usnewsmap.com/issues/738)).

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

These are character-level rules. Word error rates depend on how a script is tokenized, so they
are left out until there is a definition for each script.

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
2. Open a pull request. The check job prints the release's `releases.json` entry.
3. Review the entry, commit it and merge.
