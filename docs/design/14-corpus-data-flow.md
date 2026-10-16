# 14: Corpus data, runs and processing flow

Status: implementation in progress. Tracking: [#704](https://github.com/tgoodyear/usnewsmap.com/issues/704).

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

BOINC project downloads are Azure-hosted, but container images come from GHCR.
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
