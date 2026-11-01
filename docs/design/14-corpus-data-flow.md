# 14: Corpus data, runs and processing flow

Status: initial catalog and dashboard deployed; source coverage remains partial.
Tracking: [#704](https://github.com/tgoodyear/usnewsmap.com/issues/704).

## Goal and boundaries

Improve transcription quality across original sources without tying the data
model to one OCR engine, experiment or compute provider. Answer four questions:

1. What page and artifact is this, and where are verified copies?
2. What processing ran, with which inputs, engine and configuration?
3. What remains eligible to fetch, transfer, compute, validate or publish?
4. What evidence supports the answer, and how old is it?

The catalog describes evidence. It cannot grant training or redistribution
rights, allocate pages, establish volunteer trust, accept OCR, or publish text.
Existing owners and checks retain those responsibilities. Missing observations
mean **unknown**, not missing files or zero work.

## Existing systems of record

| Evidence                                               | Authority                                                     |
| ------------------------------------------------------ | ------------------------------------------------------------- |
| Corpus text, accepted overlays and reference snapshots | Immutable Blob/Parquet objects and their commit markers       |
| Ingest batches, issues, index releases and leases      | Existing Cosmos documents                                     |
| Intended scan population and fetch order               | Versioned prefetch manifest                                   |
| Local scans                                            | Scan cache index, per-file sidecars and verified bytes on ZFS |
| OCR requests and import outcomes                       | Existing append-only queued/outcome Parquet ledger            |
| BOINC workunits, assignments and validation            | BOINC MySQL                                                   |
| Export, submission and collection recovery             | Existing hand-off/bridge journals                             |
| Individual engine executions                           | Versioned run records, independently of acceptance            |

Bulk bytes stay in Blob, Azure Files, ZFS or the existing package registry.
MySQL and Cosmos are not file inventories. A successful process exit is not
evidence of validation, import or inclusion in a released search index.

BOINC project downloads are served from the on-prem BOINC host (they were Azure-hosted until 21 October 2026), and container images come from GHCR.
Legacy gold inputs may still reference upstream sources. Mirror readiness must
be established for each artifact and campaign, not inferred from a deployment.

## Identities and observations

Use a source-qualified page identity. Existing Chronicling America `doc_id`
values map to `loc:<doc_id>` without changing their native keys. Other source
namespaces retain their original page identifiers. Source versions, crops,
image renditions and text readings are related artifacts, not replacement page
identities. A crop records its parent and coordinate convention.

An artifact identity includes its kind and immutable version or content hash.
Locations are independent observations: local cache, private Blob staging,
public project download, volunteer-return spool and curated output are not
interchangeable. Record byte size, checksum algorithm/value, observation time,
observer and evidence reference. Inventory presence alone does not verify bytes.
Record deletion or failed verification explicitly; do not remove history.

One page may have many runs and attempts. Reuse
`usnm_ocr_contract::runs::RunRecord` for engine execution metadata: pinned engine,
model and configuration identities, input hashes, output hashes, timings,
language and measured quality/layout features. Do not store raw page text,
images, account keys or unrestricted diagnostic logs in this metadata plane.
Unknown measurements remain unknown; do not invent zeros to fit a schema.

Keep execution, validation, acceptance, import and indexing as separate facts.
Quality metrics retain their metric version and population. A Qwen score,
engine agreement or fluent transcription is not human ground truth. Refining a
quality rule creates a new versioned evaluation, not an edit of historical runs.

An observation carries an immutable identity and source timestamp. Replaying
identical evidence is idempotent; reusing an identity for different bytes is an
error. Older observations cannot regress the current read model. Ties resolve
deterministically. A source's absence from one partial scan is not a deletion.
A complete-inventory watermark is required before inferring missing copies.

## Storage and online queries

Use two layers:

- **Durable evidence:** immutable metadata batches alongside their outputs.
  Existing Parquet ledgers stay authoritative. New local engine evidence can
  start as bounded JSON records, with explicit import adapters; compact/archive
  to columnar batches as volume grows. Retain schema, row count, checksums and
  time bounds in batch manifests. Do not claim an archive is committed until
  its data and manifest are durably written.
- **Rebuildable read model:** indexed SQLite on existing local persistent
  storage, populated transactionally from evidence. A single writer publishes
  bounded, read-only page/run queries and an aggregate dashboard snapshot.
  Never put SQLite WAL on SMB or share its writable directory across hosts.

SQLite avoids a Cosmos request per page and another always-on paid cloud
database. It is not the corpus archive or a new scheduler ledger. Retain
immutable input batches so it can be rebuilt; back up a consistent database
snapshot rather than copying a live WAL database piecemeal.

Online queries first filter by page, run, source, engine, outcome or time and
use capped pagination. No client-supplied SQL or arbitrary object-path reads.
Keep the initial service loopback-only; expose only sanitized aggregate data
through the LAN dashboard. Do not add public API access, cloud credentials or
new grants to the dashboard.

At corpus scale, compact page histories into partitioned Parquet and keep only
measured hot/query indexes locally. Choose partitions from actual query
selectivity; avoid both one object per page and one giant file per corpus.
Measure database size, ingest rate, query p95 and scanned bytes before choosing
larger infrastructure. Cosmos may hold low-cardinality checkpoints or run
summaries later, not every page observation by default.

## Flow and division of labor

```text
Original sources -> fetch/cache -> explicit allocation
                                      | local OCR
                                      | BOINC prepare -> dispatch -> return
                                      v
                         validation and acceptance
                                      v
                         collect -> import -> index release

Each boundary -> immutable evidence -> catalog/read model -> LAN dashboard
```

Fetching and input preparation are not OCR. Local OCR is not a prerequisite for
volunteer full-page OCR; quality-comparison jobs may require existing text.
Experiments and production lanes share identities and evidence conventions,
but retain separate eligibility, budgets and acceptance policies.

| Component                    | Responsibility                                             |
| ---------------------------- | ---------------------------------------------------------- |
| Source fetcher               | Respect source limits and rights; cache verified originals |
| Allocation owner             | Reserve disjoint page cohorts through existing ledgers     |
| Preparation/controller       | Stage only needed inputs; bounded submit and collect       |
| Local workers and volunteers | Run immutable, eligible workloads; return evidence         |
| Validators and collectors    | Check identity, content, quorum and calibrated trust       |
| Import/release jobs          | Publish accepted overlays and record index inclusion       |
| Catalog/dashboard            | Reconcile and explain; never mutate those authorities      |

Compute the next action from an approved workload's input requirements and
verified locations. Distinguish **fetch**, **copy**, **compute**, **validate** and
**publish** backlogs. A missing public mirror of a locally cached scan needs a
transfer, not another upstream fetch. No useful job is defined merely by having
an idle GPU.

Keep a global backlog ceiling, then per-workload limits based on measured
eligible throughput and collection capacity. A future starting target is
2–4 hours of dispatchable work per accepted lane, not an enabled default.
Count workunits and replica results separately. Recent scheduler contact does
not establish application compatibility. Closed campaigns, missing apps, trust
holds and uncollected results must remain visible blocking reasons.

## Dashboard contract

The dashboard reads a local bounded snapshot, not production databases.
The snapshot includes schema version, producer, generation time, source
observation time, coverage, stage counts, artifact/run aggregates and issues.
Unknown stages have null counts. Successful snapshot generation does not make
old source observations fresh. A stage may carry `max_age_secs`, its own age limit
when its evidence is slower than the snapshot's, and `historical`, explained under
[Overall evidence age](#overall-evidence-age).

Show data flow separately from local host utilization. Each panel identifies
coverage and age; retain the last valid reading with a visible stale/error state
when a source fails. Never label worker-reported upload as imported or indexed.
Show BOINC-only hosts as compute inventory, not failed retired OCR workers.

Initial coverage is explicit and incomplete. Cached-scan observations, engine
run records, BOINC task counts and curated/index publication each need their own
adapter and timestamp. Until connected, the corresponding stage is unknown.

## Delivery and acceptance

1. Add tested catalog ingestion, indexed read-only queries and snapshot export.
2. Emit actual page-level run evidence through an opt-in worker path, retaining
   failures and execution/acceptance separation.
3. Add dashboard snapshot support and remove intentionally retired node 4 from
   active OCR polling. Preserve host monitoring, TLS and access restrictions.
4. Review and merge scoped PRs; deploy the dashboard on existing CT 212 with a
   preserved binary/config rollback. Seed only evidence observed from real
   sources, showing all unconnected stages as unknown.
5. Activate adapters and worker emission on approved hosts without interrupting
   allocated work. Reconcile one page through artifact, execution, validation,
   import and index release before claiming complete pipeline coverage.

Production BOINC changes remain serialized through #529. This design does not
open a GPU cohort, promote trust, enable the periodic batch controller, resolve
mirror licensing, or bypass existing quality/download/validator defects.

Acceptance includes malformed/oversized evidence, idempotent replay, conflicting
identities, interrupted transactions, out-of-order events, missing sources,
future/stale timestamps, bounded filtered queries, HTML escaping and explicit
error states. Deployment must verify live JSON and HTML, unchanged TLS/access
controls, current node inventory, and restart persistence.

## Initial operator interface

The catalog and observer use Python's standard library. Evidence ingestion and
query serving are separate commands:

```bash
python3 scripts/corpus/data_catalog.py --db catalog.sqlite ingest --batch evidence.jsonl
python3 scripts/corpus/data_catalog.py --db catalog.sqlite snapshot --out catalog.json
python3 scripts/corpus/data_catalog.py --db catalog.sqlite serve --host 127.0.0.1 --port 8764
```

Retain the imported evidence files: rebuilding means replaying them into a new
database, not querying the upstream source again. Keep evidence, SQLite and its
WAL on local persistent storage. The optional `usnm-data-catalog.service` exposes
only loopback queries; use an administrative SSH tunnel rather than opening a
public port.

`data_flow_observer.py` joins the catalog snapshot to local OCR and public BOINC
counters. `usnm-data-flow-observer.timer` (in `scripts/ocr-node/`) runs it every
five minutes and writes `/var/lib/usnm-data-flow/pipeline.json`; the dashboard
reads that file and serves `/api/pipeline.json`. Nothing else refreshes the file,
so a host installed without the timer shows a stale panel. A fresh aggregate
refresh does not refresh old page observations. The timer does not ingest page
records or allocate work.

The existing Cold-tier scan archive is a separate recovery copy, not the public
BOINC input mirror. Its documented complete verification at
2026-10-16T11:26:03Z covered 148,127 scans. A later partial verification or local
file stat does not replace that dated acceptance or prove current cloud presence.

## Deployed baseline and run ingestion

CT 212 runs `usnm-data-catalog.service` on `127.0.0.1:8764`, beside the
dashboard and five-minute observer. The database and retained evidence are under
`/var/lib/usnm-data-flow`; executables are under `/usr/local/lib/usnm-data-flow`.
The query listener is not exposed through the LAN dashboard. Administrators can
query it through the existing host access:

```bash
ssh root@pve-big.tjg.fyi \
  'pct exec 212 -- curl --fail --silent http://127.0.0.1:8764/health'
```

`/page?page_id=loc%3A<doc_id>` returns page evidence; `/runs` and `/executions`
support bounded filters and pagination. `/health` means the catalog is readable,
not that the pipeline is ready.

The initial bootstrap observes 128 real cached scans and independently hashes
one scan. It also includes one isolated local worker run to check metadata
capture/import. That instrumentation run used the available English model,
not a calibrated target-language experiment; its source is
`instrumentation-smoke-eng-not-quality`. No output was submitted for acceptance,
curated import or indexing.

For subsequent worker runs, enable
`USNM_OCR_RUN_METADATA_DIR` on a persistent, worker-writable local directory when
deploying a compatible worker at a safe workload boundary. This is opt-in; no
active worker was restarted for this rollout. The
[worker contract](../../crates/usnm-ocr-worker/README.md#local-page-run-metadata-opt-in)
describes success, failure, retry and durability semantics.

Copy completed `*.json` captures through the administrative path, retaining the
originals. Import each with a new evidence filename and stable batch/source IDs:

```bash
python3 scripts/corpus/data_catalog.py --db catalog.sqlite ingest-worker \
  --input capture.json --evidence-out run-001.jsonl \
  --batch-id run-001 --source worker-node3
python3 scripts/corpus/data_catalog.py --db catalog.sqlite snapshot --out catalog.json
```

The adapter accepts captures up to 64 KiB; larger captures fail explicitly even
though the worker can publish larger records. Never truncate them or silently
drop fields. Replay a previously exported JSONL with `ingest --batch`, rather
than regenerating it. Rebuilding a new database from the retained batches must
preserve run/artifact counts and page-query results. Keep a consistent SQLite
backup or rebuild from evidence; do not copy live database fragments.

The dashboard binary rollback and prior environment are retained under
`/var/backups/usnm-data-flow-20261016-102e948f-attempt2`. To remove catalog
integration, stop its query service and restore the saved `aggregate-only.conf`
to the observer's service drop-in directory, then reload systemd and rerun the
observer. This does not stop OCR or BOINC work.

Corpus-wide inventory, BOINC and `ocr-file` capture, and authoritative
validation/import/index exporters remain separate rollout steps. Do not enable
an uncalibrated corpus-wide job merely because the control plane is online.

## Retired worker evidence transport

The direct OCR worker is retired (#966, #1053) and local OCR runs through BOINC
only, so no host writes run-metadata captures any more. The SSH receiver that
collected them (`worker_evidence_sync.py`, `usnm-worker-evidence-sync.service`
and its timer) and its tests are removed from the repository; the history keeps
them. `data_catalog.py ingest-worker` still imports a single capture by hand.

A catalog host that still runs the receiver should retire it. Once the worker's
host is gone, the unit fails on every tick and `systemctl is-system-running`
reports `degraded`. Run these as root, in this order:

```bash
systemctl disable --now usnm-worker-evidence-sync.timer
systemctl stop usnm-worker-evidence-sync.service
systemctl reset-failed usnm-worker-evidence-sync.service
```

Stopping the timer keeps it from starting another tick, but does not stop a tick
it already started. That tick can fail after `reset-failed` and leave the host
degraded again, and would fail the same way on a file removed under it, so stop
the service before clearing its failed state and before removing anything.

Then remove the two unit files, `/etc/usnm/worker-evidence-sync.env`,
`worker_evidence_sync.py` from `/usr/local/lib/usnm-data-flow`, and the receiver's
key pair, `known_hosts` and `sync.lock` under `/var/lib/usnm-data-flow/worker-sync`,
and run `systemctl daemon-reload`. Keep
`worker-sync/retained/`: its captures and derived JSONL (source
`worker-node3-live`) are what the catalog is rebuilt from, by replaying them with
`ingest --batch`.

The receiver was the only scheduled writer of `catalog.json`, so without it the
snapshot stops changing while the observer keeps joining it. The observer
therefore adds a `catalog-frozen` issue when the snapshot's `generated_at` or its
newest evidence (`observed_at`) is more than three days old;
`--catalog-max-age-days` changes the limit, and the issue text gives both ages.
The counts stay in the snapshot as history and the observer run still succeeds,
so a frozen catalog does not fail its unit.

Regenerating the snapshot with
`data_catalog.py --db catalog.sqlite snapshot --out catalog.json` moves only
`generated_at`. The flag stays until new evidence moves `observed_at`, which
[`refresh-live`](#current-evidence) does every hour. To stop joining the
catalog, drop `--catalog-snapshot` from the observer's unit. A unit that
regenerates the snapshot needs more than the observer unit's 10% CPU quota: at
112,640 inventory rows the snapshot took about 0.35 seconds unthrottled, missed its
two-second query deadline under a 10% quota, and the scheduled run succeeded under
a 50% quota.

Snapshot counters remain partial. The observer joins explicit catalog
acceptance, import and index observations with their original source timestamps
and does not infer those stages from successful execution. A one-page import
proof is one observed import, not the total imported corpus. Unconnected stages
remain unknown.

## Current evidence

`usnm-data-catalog-refresh.timer` runs `data_catalog.py refresh-live` once an hour,
as the observer's service user. It reads `throughput.json`, the two weeks of hourly
counts the dashboard already keeps from its collectors
([throughput sources](../operations/onprem-ocr.md)). It writes those counts as one
immutable evidence batch of aggregate stages under
`/var/lib/usnm-data-flow/live-evidence/`, imports the batch, and regenerates
`catalog.json`. A batch is about 4 KB, so a year of them is about 35 MB. Besides
the catalog, it reads only that file: no Postgres, cloud access or new credential.
On a copy of the deployed catalog under the unit's limits, a run took 2.3 seconds
and 35 MB of memory.

| Panel row        | Source       | Evidence                                                                                                                                                      |
| ---------------- | ------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `queued`         | `batch`      | `ocr-bulk`'s approximate depth at ocr-sync's last run in the hour, leased messages included. `ocr-priority`'s depth is not logged, so it is not in this count |
| `allocated`      | `controller` | `cohort_pending` at the hour's last tick: pages at BOINC, held for collection, or selected with no job yet                                                    |
| `validated`      | `boinc`      | `usnm_ocr` workunits with a canonical result, last 336 hours                                                                                                  |
| `collected`      | `controller` | Jobs the hand-off marked published, last 336 hours: results that passed its collection checks. Includes jobs superseded by an earlier import                  |
| `imported`       | `batch`      | Pages in ocr-sync's `import written` log lines, which follow the committed import record, last 336 hours                                                      |
| `rejected`       | `batch`      | Parts rejected and dead letters archived, last 336 hours. Invalid dead letters are not counted                                                                |
| `quality_loaded` | `ship`       | Quality and Qwen pages loaded into the quality database, last 336 hours                                                                                       |

Each row is a separate fact from its own source. None is derived from another, and
a row that has no known hour stays unknown. A row built from two series counts only
hours both cover. A row's time is the end of its newest
known hour, so a source that stops keeps its last count and ages until it is
flagged. These are aggregates: `trace` still reports a page's stages only from
page-level evidence, and the rows are not cumulative corpus totals.

`queued` reads the depth of the Azure Storage queue that ocr-sync feeds. The
dashboard's pgmq metrics cover the on-prem queues of
[ADR-0025](adr/0025-onprem-database-client-certificates.md), which read zero
because producers and the claim step still use Azure, so they are not used. The
adapter adds no Postgres reads: quality load counts come from the loaders' logs,
not the quality database.

`accepted` stays release selection, as in
[Release selection and search provenance](#release-selection-and-search-provenance).
Hand-off collection is not that decision, so it has its own row. Release decisions
reach the catalog only through `ingest-release`, and no scheduled export feeds it
yet. Engine runs and per-file verified locations have no current source either:
BOINC results are not cataloged as run records, and no job verifies scans file by
file on a schedule.

### Overall evidence age

The overall age and the panel's warning used to follow the oldest stage, so one
source that had stopped for good set both for the whole panel. They now work as
follows:

- `HISTORICAL_SOURCES` in `data_catalog.py` lists sources that will never report
  again: one-off proofs and the retired worker receiver. Adding a source takes a
  reviewed change. A source not on the list counts as live.
- A stage is `historical` only if every source feeding it is on that list. A live
  source sets the age of a stage that also has historical ones.
- The overall `observed_at` is the oldest non-historical stage with a timestamp.
- The panel warns if any non-historical stage is unknown or past its limit: its own
  `max_age_secs`, else the dashboard's `--pipeline-max-age-secs`.
- A historical stage keeps its count and original age, is marked historical in its
  row and in the `historical-stages` issue, and never counts as current in the
  summaries. The `historical` flag in `/api/pipeline.json` and the
  `stage_freshness` entry say the same.

The live rows' limit is three hours. They cover whole hours and refresh hourly, so a
healthy row reaches about 90 minutes old. Other stages keep the 15-minute default.
`catalog-frozen` still fires at three days from either timestamp.

## Release selection and search provenance

Committed `ocr-sync` import records (`ocr-local/import/<run>.json`) prove
structural import into `ocr-local` (`stage="imported"`), never release
acceptance or search publication. Export committed imports, authoritative
release decisions (`accepted`, `rejected`, `revoked`, `unchanged_baseline`) and
public `/v1/hits` verification receipts as independent evidence batches:

```bash
python3 scripts/corpus/data_catalog.py --db catalog.sqlite ingest-import \
  --input import.json --evidence-out import-001.jsonl \
  --batch-id import-001 --source ocr-sync-import
python3 scripts/corpus/data_catalog.py --db catalog.sqlite ingest-release \
  --input release-decisions.json --evidence-out release-001.jsonl \
  --batch-id release-001 --source release-builder
python3 scripts/corpus/data_catalog.py --db catalog.sqlite ingest-search \
  --input hits.json --evidence-out search-001.jsonl \
  --batch-id search-001 --source public-hits-check \
  --page-id loc:<doc_id> --verified-at 2026-10-18T12:00:00Z --run-id <run_id>
python3 scripts/corpus/data_catalog.py --db catalog.sqlite trace --page-id loc:<doc_id>
```

`trace` (and `GET /trace?page_id=loc%3A<doc_id>`) reconciles a page across
`artifact`, `execution`, `imported`, `release_decision` and `search_provenance`
while keeping `accepted` and `published` explicitly `false` until authoritative
release selection and published `/v1/hits` replacement provenance are observed.
When `/v1/hits` returns a page without replacement `ocr` metadata, `ingest-search`
records `decision="unchanged_baseline"` and `selected_source="loc"` with a
checksummed `search_receipt` and no raw OCR text.

### Fixed-cohort delivery acceptance

Freeze a JSON array of 1–100 unique pages with explicit candidate run IDs before
checking delivery, for example
`[{"page_id":"loc:sn123_1900-01-01_ed-1_seq-1","run_id":"node.20260101T000000Z"}]`.
After importing the original producer receipts above, observe the currently served
index version independently from `/v1/meta` and run:

```bash
python3 scripts/corpus/data_catalog.py --db catalog.sqlite acceptance-report \
  --cohort cohort.json --index-version pages-v20261018-1 \
  --as-of 2026-10-18T13:00:00Z --max-receipt-age-seconds 3600 \
  > delivery-report.json
```

This read-only report keeps every cohort page in the denominator, pins its run,
reuses `trace`'s complete artifact/execution/import/release/search proof, and
requires the search receipt to match the supplied index and freshness window.
Missing, future-dated, stale or mismatched evidence cannot count as verified
delivery. Receipt timestamps and response hashes remain visible; report generation
does not refresh them. The cohort hash identifies the exact input denominator.
Exit zero means a valid report was produced, not that acceptance passed: inspect
`delivery_verified`, `delivery_incomplete` and each page's `delivery_blockers`.
The command neither fetches live data nor establishes that the operator-supplied
index is still current, and its individual page traces are not a transactional
cross-page snapshot. Retain the cohort, catalog evidence and dated metadata
response together; rerun after importing new search receipts or a version change.
The cutoff applies to observation and source timestamps across artifacts,
execution, runs, imports, release decisions and search proof, including nested
engine observations. Post-cutoff evidence is excluded and reported as a blocker.
This filters the current catalog; it does not reconstruct superseded historical
states that are no longer in its latest-observation table.

**Verified delivery is not independently verified better transcription.**
`quality` remains `unknown` and `verified_better_published_searchable` remains
zero until independent quality evidence is connected to these same candidate
page/run identities. Release selection and damage scores are not human ground
truth. Use the existing [evaluation measures](../ocr-evaluation-measures-v1.md)
for paired reference-based accuracy and fixed-query retrieval comparisons;
production search evaluation also needs a pinned production adapter.
This bounded cohort report is not corpus-wide coverage or automatic acceptance,
does not change publication pointers, and does not authorize scan reclamation.
