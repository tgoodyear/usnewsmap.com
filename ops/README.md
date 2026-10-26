# Ops

Operations data kept in git: the saved Log Analytics queries, the build records of earlier index versions, and captures of what a fixed set of searches returned on a version. Status: live. It holds data only; the scripts that read and check it are in [`scripts/`](../scripts/README.md).

| Path                            | What it holds                                                                                                            | Read or checked by                                                                                            |
| ------------------------------- | ------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------- |
| `queries/*.kql`                 | Saved Kusto queries, one per file, named for what they show. The first comment line is the query's one-line description. | `scripts/azure/logs.sh`; the workbooks in `infra/workbooks/` read the same log lines                          |
| `index-history.json`            | Build records of the index versions released before releases recorded their own (#161), by version name.                 | `usnm-api` compiles it in for `/v1/versions`; `scripts/ops/check-index-history.py`; the `ops` job in `ci.yml` |
| `index-history.schema.json`     | The JSON Schema of `index-history.json`.                                                                                 | `scripts/ops/check-index-history.py`                                                                          |
| `version-snapshots/*.json`      | One capture per index version, named `{index_version}.json`: the fixed searches, asked of the live version.              | `scripts/ops/compare-versions.py diff`; `scripts/ops/check-version-snapshots.py`; the `ops` job in `ci.yml`   |
| `version-snapshots/schema.json` | The JSON Schema of a capture.                                                                                            | `scripts/ops/check-version-snapshots.py`                                                                      |
| `data-assets.json`              | The release tag and tarball sha256 of each data set kept outside the tree (`report-data`, `bench-assets`).               | `scripts/ops/data-assets.sh --fetch`; `scripts/ops/test_data_assets.py` checks its shape                      |

## Saved queries

Run one against an environment's Log Analytics workspace:

```sh
scripts/azure/logs.sh --list                                          # names and one-line descriptions
scripts/azure/logs.sh --env prod --query-name release-progress        # the last day
scripts/azure/logs.sh --env prod --query-name curation-throughput --span 6h
```

`scripts/azure/logs.sh` needs `az` signed in with access to the workspace and `jq`. The `ocr-node-*` queries run against the OCR nodes' own workspace, not the environment's. A name is lowercase letters, digits and hyphens, and the file must exist here. To add a query, add `queries/<name>.kql` with a first comment line that says what it shows.

| Group              | Queries                                                                                                                                                        |
| ------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| API and site       | `api-errors`, `api-requests`, `backend-latency`, `traffic`, `warm-up`                                                                                          |
| Searcher           | `searcher-caches`, `searcher-health`, `searcher-threads`                                                                                                       |
| Ingest and release | `curate-replicas`, `curation-throughput`, `errors-by-batch`, `index-layout`, `ingest-endings`, `job-executions`, `prune`, `release-progress`, `titles-archive` |
| OCR                | `ocr-node-errors`, `ocr-node-health`, `ocr-node-updates` (the on-prem nodes), `ocr-quality` (the OCR quality audit of `ocr/ja-ocr/quality.py`)                 |

Three queries are kept in step with workbook panels by a test: `ocr-node-health`, `searcher-caches` and `searcher-threads` (`scripts/ci/test_check_workbooks.py`). `ocr-node-health.kql` also holds the nodes' workspace cap (`cap_gb`), which `infra/modules/ocr-node-monitoring.bicep` repeats.

## Index history

`index-history.json` maps a version name (`pages-v20260928-1`) to what built it: the commit, the ingest and Quickwit engine versions, the feature versions, the index templates, whether the run was a full build, the image digest and push time, the execution and the run's start time. Each entry has a `reconstructed` note saying how its values were found. Releases since #161 record their own build, so the file holds the versions released before that.

`scripts/ops/reconstruct-index-history.py` writes it. It reads the runs from the live `/v1/versions`, the executions and image tags with `az`, and git from your checkout, so it needs `az` signed in as the operator and a fetched checkout. Check the output before replacing the file:

```sh
scripts/ops/reconstruct-index-history.py --env prod --merge ops/index-history.json > /tmp/h.json \
  && scripts/ops/check-index-history.py --path /tmp/h.json && mv /tmp/h.json ops/index-history.json
```

The checker needs `fastjsonschema` (`scripts/ci/requirements-ops.txt`, installed in a hash-pinned venv in the [development guide](../docs/development.md#json-data-files)). Besides the schema it requires sorted keys at every level and an image pushed before its run started. The note [Index versions: what built each one](../docs/notes/2026-10-05-index-versions-and-what-built-them.md) explains the reconstruction.

## Version snapshots

The API serves one index version at a time, so versions are compared through captures. A capture records, for each search of a fixed set, the HTTP status, the totals, the per-bucket hits, the hits by language, the backend's timing and the wall time. The set is the ten benchmark searches of 05 §5.8, the home page examples (`web/src/examples.json`) and three Japanese searches (06 §6.3.8 in the [API design](../docs/design/06-api-design.md)). The two committed captures hold different numbers of searches (63 and 113); `scripts/ops/compare-versions.py` defines the set it takes now.

Take a capture while the version is live, before a release replaces it, at a quiet hour:

```sh
scripts/ops/compare-versions.py capture > ops/version-snapshots/<live version>.json
scripts/ops/compare-versions.py diff --before ops/version-snapshots/<older>.json --after ops/version-snapshots/<newer>.json
scripts/ops/check-version-snapshots.py                 # as CI does
```

`diff` flags a search whose hits changed by more than 1% (`--threshold`), or whose status or query changed. `check-version-snapshots.py` checks every file here against `schema.json`, that the file is named after its `index_version`, and that each answered search's per-bucket hits add up to its total. A capture is laid out one search per line (`compare-versions.py fmt --path FILE` rewrites one), so a diff between captures shows the searches that changed.

## CI

The `ops` job in `.github/workflows/ci.yml` runs `check-version-snapshots.py`, `check-index-history.py` and the tests of both checkers and of the date-span tables of `scripts/azure/searches.sh`. `scripts/ci/changes.sh` starts it when a snapshot, the index history or its schema, those scripts or `scripts/lib/date-spans.jq` change. A change to `queries/` does not start it.
