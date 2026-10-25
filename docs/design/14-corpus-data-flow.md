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
old source observations fresh.

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
`generated_at`. The flag stays until new evidence moves `observed_at`. BOINC
results are not cataloged until an adapter for them exists under
[#704](https://github.com/tgoodyear/usnewsmap.com/issues/704). To stop joining the
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
