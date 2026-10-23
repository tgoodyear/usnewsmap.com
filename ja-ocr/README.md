# Japanese OCR

The Python job that reads, with [NDLOCR-Lite](https://github.com/ndl-lab/ndlocr-lite), the pages of Japanese-language titles that the Library of Congress ships without usable text, and the audit and measurement commands that share its image. Design: [04 §4.8](../docs/design/04-data-sources-and-ingestion.md). The write-up of the work and its measurements is [Japanese pages: OCR of the pages LoC ships without text](../docs/notes/2026-10-05-japanese-ocr.md).

Status: live. CI builds the job into the `usnewsmap-ja-ocr` image and tests it. In `prod` the Azure job is off and the OCR runs on an OCR node (`USNM_JA_OCR_JOB` is `false` and `USNM_JA_OCR_ONPREM` is `true` in `infra/env/prod.json`; [Operations](../docs/operations.md#the-japanese-ocr-on-prem)). How many pages have been read so far is not recorded in this directory: see the note and `/v1/status`.

## What it writes and who reads it

The job writes to the curated store under `ocr-ja/`: the page list (`targets-v3.jsonl`), claims, one ALTO file per page and one Parquet overlay part per issue. The Parquet schema is `usnm-overlay`'s ([`crates/usnm-overlay`](../crates/README.md)). Every release reads the parts (`crates/usnm-ingest/src/ocr_ja.rs`) and builds a separate Japanese index from them. It also writes `reference/status/ocr-ja.json`, which `usnm-api` reads for the status page.

## Files

| File                        | What it is                                                                                                                                                                               | In the image |
| --------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------ |
| `jaocr.py`                  | The entry point, `python3 jaocr.py <command>`. Commands: `targets` and `run` (the OCR), `audit`, `quality`, `damage-parity`, `mixed`, `american-stories` and `american-stories-write`.   | yes          |
| `alto.py`                   | NDLOCR-Lite's XML to ALTO 3 in `inch1200` units, so it can stand in for the empty ALTO LoC ships                                                                                         | yes          |
| `audit.py`                  | `audit`: which titles have pages LoC ships without OCR text                                                                                                                              | yes          |
| `quality.py`                | `quality`: how good LoC's OCR is by language and decade, from a fixed sample of pages (OCR quality audit, metric v2)                                                                     | yes          |
| `damage_v2.py`              | The scoring of one page by `quality.py`. The Rust port `usnm_core::damage` is checked against this file alone.                                                                           | yes          |
| `damage_parity.py`          | `damage-parity`: scores a sample of pages for the Rust port's parity fixture (`crates/usnm-core/tests/data/damage-parity.jsonl.zst`)                                                     | yes          |
| `mixed.py`                  | `mixed`: how much English and Japanese the Japanese titles' pages hold that search cannot reach                                                                                          | yes          |
| `american_stories.py`       | `american-stories`: American Stories' text against LoC's on our pages                                                                                                                    | yes          |
| `american_stories_write.py` | `american-stories-write`: writes American Stories' text for our pages to the curated store                                                                                               | yes          |
| `solo.py`                   | Lets one replica of an execution do a one-off command's work (`mixed`, `american-stories`)                                                                                               | yes          |
| `wordlists/`                | Word lists for languages wordfreq lacks (Hawaiian, Yiddish). [README](wordlists/README.md).                                                                                              | yes          |
| `requirements.txt`          | The job's own pinned dependencies                                                                                                                                                        | read by pip  |
| `bench_quality.py`          | Times `quality.py`'s per-page scoring on synthetic pages, to compare changes to it                                                                                                       | no           |
| `testdata/`                 | One Rocky Shimpo headline (1945-01-01, page 4), used by `scripts/ci/check-ja-ocr-image.sh` and `scripts/boinc/smoke-ndlocr.py`                                                           | no           |
| `tools/`                    | Tools that work on the job from outside: the engine-choice scripts, `build-wordlists.py`, and the two operator scripts `start-quality.sh` and `quality-rows.sh`. [Below](#related-code). | no           |
| `test_*.py`                 | Unit tests of the modules above                                                                                                                                                          | no           |

## Who runs it

- **CI.** `docker/Dockerfile.ja-ocr` copies the modules marked above into `/opt/jaocr` and sets `jaocr.py` as the entry point. The `image` job in `.github/workflows/ci.yml` builds it, runs the tests inside it and runs `scripts/ci/check-ja-ocr-image.sh`, which reads the headline in `testdata/` and requires its known text. The `publish` job pushes the image on `main`.
- **Azure.** When `USNM_JA_OCR_JOB` is on, a Container Apps job (`caj-usnm-jaocr-<env>`, `infra/modules/ingestjobs.bicep`) runs the image, started by hand. `ja-ocr/tools/start-quality.sh` starts the audit commands as executions of it, and `ja-ocr/tools/quality-rows.sh` reads their results back from Log Analytics.
- **On-prem.** `scripts/ocr-node/ja-ocr-onprem.sh` builds the image on an OCR node from a pinned commit and installs `usnm-ja-ocr@.service`, one unit per replica. The grants are in `infra/modules/jaocr-runner.bicep`.
- **Settings.** The job reads `USNM_REFERENCE_URL` and `USNM_CURATED_URL` (blob container URLs or local directories) and the `JAOCR_*` variables documented in `jaocr.py`.

## Related code

- [`tools/`](tools/) holds the four tools of the engine choice described in [the note](../docs/notes/2026-10-05-japanese-ocr.md) (`sample.py`, `crops.py`, `run.py`, `score.py`; status: see the note), `build-wordlists.py`, which builds `wordlists/`, and the two operator scripts above. The Dockerfile copies the job's modules by name, so none of them is in the image, and they have no tests of their own.
- `scripts/data/build-damage-words.py` copies `wordlists/haw.txt` and `yi.txt` into `crates/usnm-core/data/damage-words/`.
- The BOINC app `usnm_ndlocr` reads Japanese pages by a different path, the engine contract: `ocr/engines/ndlocr-lite/`, `crates/usnm-ocr-adapter` and [docs/boinc-ndlocr-lite.md](../docs/boinc-ndlocr-lite.md). Its smoke test compares its ALTO conversion with `alto.py`.
- [Check Japanese search](../docs/operations.md#check-japanese-search) describes `scripts/ops/check-ja-search.py`, which tests the live site after a release.

## Tests

The tests need the job's pinned dependencies and, for `test_page_language_parity.py`, the rest of the checkout. CI runs them inside the built image with the checkout mounted read-only:

```sh
docker build -f docker/Dockerfile.ja-ocr -t usnm-ja-ocr-check .
docker run --rm -e PYTHONDONTWRITEBYTECODE=1 -v "$PWD:/repo:ro" -w /repo/ja-ocr --entrypoint python3 usnm-ja-ocr-check \
  -m unittest discover -v -p 'test_*.py'
```

`scripts/ci/run-unit-tests.py` does not run them: it looks only under `scripts/`.
