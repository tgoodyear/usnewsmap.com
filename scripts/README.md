# Scripts

Shell and Python for the work around the code: standing an Azure environment up and tearing it down, deploying and operating the BOINC server, running the on-prem OCR nodes, building generated data files, checking data and versions, and the checks CI runs. The table of subdirectories gives each one's status.

Scripts take named flags (`--env prod --key K`), never positional arguments. `scripts/ci/no-positional-args.py` fails CI on a script that reads them. The examples assume the repository root. The Azure scripts use your own `az` sign-in.

The 61 files directly in this directory are grouped below by purpose. The subdirectories follow.

## Azure environments

An environment is a deployment stack with its resource groups, settings and GitHub Environment. [Operations](../docs/operations.md) and the [infrastructure README](../infra/README.md) describe the flow; each script's header has its flags.

| Script                | What it does                                                                                                                                                                           |
| --------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `bootstrap.sh`        | Stands up or updates one environment in any subscription and wires this repository to deploy it. Safe to run again.                                                                    |
| `provision.sh`        | Deploys an existing environment's stack from its current settings.                                                                                                                     |
| `stand-up.sh`         | Stands an environment up for one experiment with this checkout's images (#275); refuses `prod`. Creates no GitHub Environment, so no push to main rolls it.                            |
| `teardown.sh`         | Deletes an environment: its stack, groups and GitHub Environment. Keeps the settings file as `.env.deleted-<time>`.                                                                    |
| `dev-status.sh`       | Reports whether `dev` is up, for which experiment, and what it has cost.                                                                                                               |
| `settings.sh`         | Reads or changes an environment's settings. Public ones live in `infra/env/<env>.json`, private ones in the settings store.                                                            |
| `settings-store.sh`   | Deploys the App Configuration store that holds the private settings, in its own stack.                                                                                                 |
| `archive-store.sh`    | Deploys, or unlocks, the archival storage account, in its own stack.                                                                                                                   |
| `build-quickwit.sh`   | Builds the patched Quickwit image ([ADR-0013](../docs/design/adr/0013-quickwit-from-our-fork.md)) in the environment's registry and records its digest in `infra/quickwit-image.json`. |
| `start-job.sh`        | Starts one execution of a Container Apps job with other arguments, keeping the job's image and settings.                                                                               |
| `titles-seed.sh`      | Seeds an environment's title records and catalog from the archive's newest copy. `stand-up.sh` runs it.                                                                                |
| `logs.sh`             | Runs a saved query from [`ops/queries/`](../ops/README.md) against the environment's Log Analytics workspace.                                                                          |
| `searches.sh`         | Summarizes the anonymous search log: top queries, searches per day, searches that found nothing, date spans.                                                                           |
| `log_analytics.py`    | A read-only Log Analytics query client with bounded retries, used by `qwcluster-report.py` and `validate-boinc-workbook.py`.                                                           |
| `qwcluster-report.py` | Gathers the Quickwit search cluster experiment's results (#239, status: experimental) into a Markdown report.                                                                          |

## BOINC

The volunteer pilot's server and apps ([docs/boinc.md](../docs/boinc.md); status: see that page). The files that define the apps are in [`ocr/boinc/`](../ocr/README.md), the server's hosting in [`contrib-compute/`](../contrib-compute/README.md), and more scripts in `boinc/` below.

| Script                                                                               | What it does                                                                                                         |
| ------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------- |
| `provision-boinc.sh`                                                                 | Deploys the BOINC server from its committed settings ([ADR-0019](../docs/design/adr/0019-boinc-server-hosting.md)).  |
| `build-boinc-images.sh`                                                              | Builds the server and bridge images in ACR and writes their digests to `contrib-compute/infra/boinc/env/<env>.json`. |
| `start-boinc-bridge.sh`                                                              | Starts one run of the bridge job with its step and that step's settings.                                             |
| `boinc-app-release.sh`                                                               | Builds, stages, signs and publishes one app version on the live server.                                              |
| `boinc-quality-campaign.py`                                                          | Prepares, launches, checks and restores large `usnm_quality` campaigns.                                              |
| `boinc-test-client.sh`                                                               | Creates or deletes the ephemeral BOINC test client VM (#484).                                                        |
| `validate-boinc-workbook.py`                                                         | Runs the source panels of the BOINC workbook and writes aggregate-only acceptance receipts.                          |
| `test_boinc_provision.py`, `test_boinc_quality_campaign.py`, `test_boinc_scripts.py` | Tests of the scripts above, offline.                                                                                 |

Prod's BOINC server has run on an on-prem host since 21 October 2026 ([docs/boinc-onprem.md](../docs/boinc-onprem.md)), which changes five of these. With `"server": "onprem"` in the settings, `provision-boinc.sh` keeps only the identities, Key Vault, VNet and static site, and deletes the rest of the Azure server, including the bridge job and the workbook. `start-boinc-bridge.sh` then starts only the hand-off job (`--target handoff`); `boinc/onprem-step.sh` runs the bridge steps on the host with the same arguments. `boinc-app-release.sh` also takes `--onprem-host` and `--onprem-ct`. `boinc-quality-campaign.py` and `validate-boinc-workbook.py` act on the Azure bridge job, quality wave job and workbook, so they apply to a server with `"server": "azure"`.

## Generated data

Each script writes a committed file. Regenerate it with the script, never by hand, and commit the result.

| Script                         | Writes                                                                        | Notes                                                                     |
| ------------------------------ | ----------------------------------------------------------------------------- | ------------------------------------------------------------------------- |
| `build-gazetteer.py`           | `catalog/gazetteer/us-places.csv`                                             | From the Census Bureau's national places file. `--year`, `--zip`.         |
| `build-english-words.py`       | `crates/usnm-core/data/english-words.txt.zst`, `english-shared-words.txt.zst` | From wordfreq 3.1.1; needs the `zstd` tool. `--check` compares.           |
| `build-damage-words.py`        | `crates/usnm-core/data/damage-words/<code>.txt.zst`                           | The word lists the damage metric scores against. `--check` compares.      |
| `build-page-language-words.py` | `crates/usnm-core/data/page-language-words.txt`                               | The function words a page's language is found by. `--check` compares.     |
| `page-language-samples.py`     | `fixtures/page-language/samples.jsonl`                                        | Text of known language from Wikipedia and Wikisource. `--check` compares. |
| `page-language-sample.py`      | `crates/usnm-core/tests/data/page-language-parity.jsonl.zst`                  | Real pages for the page-language detector's parity test.                  |

The word-list licences are in [`crates/usnm-core/data/README.md`](../crates/usnm-core/data/README.md). The scripts that write other committed files are listed elsewhere: [Index, version and load checks](#index-version-and-load-checks) for `ops/`, `settings.sh` and `lib/env_settings.py` for `infra/env/*.json`, and `ocr-survey/golden_score.py` for the OCR scorer's golden files.

`build-quality-assessment-db.py` is a different kind of builder: it joins the full-corpus damage manifest with the BOINC quality campaign tables into Parquet and SQLite files under a directory you choose. It is analysis tooling and writes to the directories you name with `--out-dir` and `--sqlite-out`; `test_build_quality_assessment_db.py` tests it.

## Index, version and load checks

| Script                                                                                 | What it does                                                                                                                                                                                         |
| -------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `compare-versions.py`                                                                  | `capture` asks the live site a fixed set of searches and writes `ops/version-snapshots/<version>.json`; `diff` compares two captures.                                                                |
| `check-version-snapshots.py`                                                           | Checks every capture against `ops/version-snapshots/schema.json` and the sums a schema cannot say. Needs `fastjsonschema`.                                                                           |
| `data-assets.sh`                                                                       | Packs, publishes (through the `publish data assets` workflow) and fetches the data sets kept outside the tree ([docs/reports/README.md](../docs/reports/README.md)); `test_data_assets.py` tests it. |
| `reconstruct-index-history.py`                                                         | Rebuilds the build records of versions released before #161 into `ops/index-history.json`.                                                                                                           |
| `check-index-history.py`                                                               | Checks `ops/index-history.json` against its schema, with sorted keys. Needs `fastjsonschema`.                                                                                                        |
| `check-ja-search.py`                                                                   | Checks that the Japanese OCR is searchable on the live version after a release (#139).                                                                                                               |
| `bench-cold-searches.py`                                                               | Times a few known-slow searches, cold then warm, one request at a time.                                                                                                                              |
| `load-cold-searches.py`                                                                | Runs cold searches at rising concurrency and reports how the API copes. Run against staging, or production once at a quiet hour.                                                                     |
| `test_check_version_snapshots.py`, `test_check_index_history.py`, `test_date_spans.py` | Tests of the checkers and of `lib/date-spans.jq`.                                                                                                                                                    |

The `ops` job in `.github/workflows/ci.yml` runs the two checkers and their tests. [`ops/README.md`](../ops/README.md) has the commands.

## Local development

| Script                 | What it does                                                                                                               |
| ---------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| `fixture-batches.py`   | Turns the synthetic fixture corpus back into LoC-style batch archives, to try the ingest pipeline locally.                 |
| `quickwit-fixtures.sh` | Loads the fixture indexes into a local Quickwit node and serves them as production does. Used by the Quickwit parity test. |

Both are described in the [development guide](../docs/development.md).

## Corpus data flow

Tools for the catalog of corpus data, runs and processing flow ([design 14](../docs/design/14-corpus-data-flow.md)). The catalog and the observer use only the Python standard library. The catalog, the observer and the evidence sync run as systemd units from `ocr-node/`.

| Script                                                                                                               | What it does                                                                                                                                    |
| -------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| `data_catalog.py`                                                                                                    | A local, rebuildable SQLite catalog of evidence about pages, artifacts and runs. Ingests evidence files and serves bounded queries on loopback. |
| `data_flow_observer.py`                                                                                              | Joins the catalog snapshot to local OCR and public BOINC counters and writes the dashboard's snapshot.                                          |
| `scan_cache_inventory.py`                                                                                            | A bounded, resumable inventory of the local scan cache. File stats are not checksum verification.                                               |
| `worker_evidence_sync.py`                                                                                            | Moves workers' run-metadata captures to the catalog over a restricted SSH command and ingests them.                                             |
| `test_data_catalog.py`, `test_data_flow_observer.py`, `test_scan_cache_inventory.py`, `test_worker_evidence_sync.py` | Their tests. `fixtures/data_catalog/` holds sample captures.                                                                                    |

## Other script tests

| Script                                           | What it tests                                                                                                           |
| ------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------- |
| `test_env_scripts.py`                            | The dev lifecycle pieces of `lib/env.sh` and `dev-status.sh`, against a stub `az`                                       |
| `test_rename_guard.py`, `test_resource_names.py` | The protected-resource removal gate and the script-owned names ([ADR-0017](../docs/design/adr/0017-resource-naming.md)) |
| `test_host_boundary.py`                          | That application onboarding does not own lab OS subscriptions or hypervisors                                            |
| `test_engine_spike.py`                           | The process-cgroup lookup of `ocr-node/engine-spike.sh`                                                                 |
| `test_qwcluster_report.py`                       | `qwcluster-report.py`, on `testdata/qwcluster/`                                                                         |

## Subdirectories

| Directory                       | What it holds                                                                                                                                                                                                                                                                                                                                                                                                    | Status                                                                                                                                                        |
| ------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`archive/`](archive/README.md) | Scripts kept only because dated notes and reports cite them. Not maintained and not run by CI.                                                                                                                                                                                                                                                                                                                   | archived                                                                                                                                                      |
| `boinc/`                        | The BOINC pilot's tooling: volunteer setup and preflight (Linux, macOS, Windows, a Proxmox container), patches applied to the stock server, the batch scheduler, the Iris and quality campaign runners, acceptance and smoke tests, wrapper checks, `db-tls/` (a disposable database to test verified TLS) and `onprem/` (the BOINC server on an on-prem host, [docs/boinc-onprem.md](../docs/boinc-onprem.md)). | status: see [docs/boinc.md](../docs/boinc.md)                                                                                                                 |
| `ci/`                           | What `.github/workflows/ci.yml` runs: change selection (`changes.sh`), the guards (`no-private-addresses.sh`, `no-shared-keys.sh`, `no-positional-args.py`, `check-names.py`), Bicep install and lint, image checks, deploy helpers (`roll-api.sh`, `quickwit-image.sh`, `indexnow.sh`), the OCR worker release and the unit-test runner.                                                                        | live                                                                                                                                                          |
| `fixtures/`                     | `data_catalog/`: sample worker run captures and an `ocr-sync` import that `test_data_catalog.py` reads. Not the synthetic corpus; that is [`fixtures/`](../fixtures/README.md) at the repository root.                                                                                                                                                                                                           | live                                                                                                                                                          |
| `ja-ocr/`                       | The engine-choice tools for the Japanese OCR (`sample.py`, `crops.py`, `run.py`, `score.py`), `build-wordlists.py`, and `start-quality.sh` and `quality-rows.sh`, which start the job's audit commands and read their results. The job itself is [`ja-ocr/`](../ja-ocr/README.md) at the repository root.                                                                                                        | `start-quality.sh`, `quality-rows.sh` and `build-wordlists.py`: live. The four engine-choice tools: see [the note](../docs/notes/2026-10-05-japanese-ocr.md). |
| `lib/`                          | Helpers the other scripts source or import: `env.sh` (settings and stack functions), `env-name.sh`, `env_settings.py` (checks `infra/env/*.json`), `guardrails.sh`, `names.sh`, `rename_guard.py`, `acr-build.sh`, `boinc.sh`, `boinc-start-guard.sh` and `date-spans.jq`.                                                                                                                                       | live                                                                                                                                                          |
| `local-azure/`                  | Local stand-ins for Azure: Azurite for Blob, `shim.py` (a managed identity endpoint and an HTTP proxy) and `cosmos.py` (the Cosmos DB REST calls the pipeline makes). `up.sh` and `down.sh` start and stop them. See the [development guide](../docs/development.md).                                                                                                                                            | live                                                                                                                                                          |
| `ocr-bench/`                    | The OCR benchmark's tools: `fetch` and `release-ci.sh` for releases, the LoC baseline tools (`loc_inventory.py`, `loc_archive.py`, `loc_align.py`), `measures_v1.py` and `serialset-pilot.py`. Tested by the `ocr-bench` workflow. Data is in [`ocr/bench/`](../ocr/README.md).                                                                                                                                  | live                                                                                                                                                          |
| `ocr-node/`                     | Installing and running the on-prem OCR nodes ([Operations](../docs/operations.md#on-prem-ocr-nodes)): `onboard.sh`, `offboard.sh`, `group.sh`, `node-setup.sh`, `install-worker.sh`, `update-worker.sh`, the scan archive scripts, the quality JPEG cache, and the systemd units and timers the nodes run.                                                                                                       | live                                                                                                                                                          |
| `ocr-survey/`                   | The OCR gold set: select pages, cut crops, validate transcriptions and score CER (`gold.py`, `transcription.py`, `crops.py`, `altotext.py`, `locfetch.py`), the golden test of the Rust scorer (`golden_score.py`), and the October 2026 engine survey's runner and model locks (`engine_runner.py`, `fetch-models.sh`, `models.lock`). See [docs/ocr-gold-set.md](../docs/ocr-gold-set.md).                     | gold set: live (CI, the bridge image). Engine survey tools: see [the survey report](../docs/reports/2026-10-ocr-engine-survey/report.md).                     |
| `onprem/`                       | A full release on an on-prem host (`release.sh`, `install-quickwit.sh`; [Operations](../docs/operations.md#full-rebuild-on-prem)) and `queue/`, the PostgreSQL and pgmq setup for the OCR work queues ([README](onprem/queue/README.md)).                                                                                                                                                                        | live                                                                                                                                                          |
| `testdata/`                     | `qwcluster/`: `qwcluster report` log lines that `test_qwcluster_report.py` reads.                                                                                                                                                                                                                                                                                                                                | live                                                                                                                                                          |

## Running the tests

`scripts/ci/run-unit-tests.py` runs every `scripts/**/test_*.py`, each in its own `python3 -m unittest` process from the repository root, so a new test file runs in CI without being added to a list. It is the "script unit tests" step of the `infra` job in `ci.yml`.

```sh
scripts/ci/run-unit-tests.py --list                          # what it runs and what it leaves out
scripts/ci/run-unit-tests.py                                 # run them all
python3 -m unittest -v scripts/boinc/test_install.py         # one file
```

`python3 -m unittest discover -s scripts` is not a substitute: with no `__init__.py` under `scripts/` it finds only the files directly in this directory. The runner leaves out `archive/`, `ocr-bench/`, `ocr-survey/`, two `ocr-node` tests that need Pillow, the two ops checkers that need `fastjsonschema` and one test that compiles C++. The reason for each is in its `EXCLUDE` table, and each runs in a CI step of its own. The [development guide](../docs/development.md#script-tests) has more.

Other checks that touch these scripts: `shellcheck -S warning` runs on the BOINC shell scripts and the OCR node install scripts, `scripts/ci/no-private-addresses.sh` fails on a private IPv4 address written into any tracked file, and `scripts/ci/no-shared-keys.sh` fails on anything that authenticates with a shared secret ([CONTRIBUTING](../CONTRIBUTING.md)).
