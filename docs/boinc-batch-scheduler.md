# BOINC volunteer batch controller

Issue [#525](https://github.com/tgoodyear/usnewsmap.com/issues/525). The opt-in
controller connects the existing CT 211 cache push, hand-off export, bridge
`submit-remote`, assimilator `spool-out`, and trust-checked hand-off `collect`.
It runs **one bounded tick**, not a daemon, and does nothing remotely without
`--apply`. Adding this code does **not** deploy it or enable a production feed.

## Allocation and boundaries

An operator allocates explicit page keys from one prefetch manifest snapshot,
one base batch and one native plan to BOINC. Reserve that allocation for this
controller; do not also dispatch it through an on-prem node plan. The native
exporter still checks and reserves the ledger and refuses conflicting requests.
LoC batch names are case-sensitive (`curiv_joshuaTree`, `lu_Cypher`); preserve
their spelling and use the unversioned base batch, not its `_verNN` directory.
Unallocated pages remain on-prem work. There is no automatic queue consumption,
LoC fetching, scanfetch change, worker-image rollover, gold issuance, trust
override, client attachment, or retry of terminal failures.

The controller uses a single approved execution host with Python 3.9+, Azure
CLI, `jq`, Bash, an existing Azure login, the checkout's existing environment
outputs, and the existing hand-off and bridge jobs. The server and bridge images
must contain the new additive `batch-inventory` step; retain the existing
immutable worker pin. Do not deploy server or bridge images or run the
controller while another release owner is changing them.

### Separate hand-off runtime

The hand-off job is in the **main** resource group, not the BOINC runtime
group. Updating the BOINC server and bridge pair does not update this job.
`USNM_BOINC_HANDOFF_IMAGE` in `infra/env/prod.json` must name a reviewed bridge
artifact containing the bounded export, exact-file reads and accepted-snapshot
collection code. The desired production pin is the scoped-trust v4 bridge
artifact built from merged #755 source `38893b3008708f6f25c2beab2dcba40ab7521fab`,
also selected for the BOINC bridge. It includes the current-worker Gold routing
registry and rejects unscoped legacy trust for publication. This pin alone is
not a runtime rollout or host promotion.

After the pin PR passes review and CI and merges, serialize a hand-off-only
update through #529. Verify the job and other lifecycle jobs are idle and retain
the entire before-template. Update only the existing `handoff` container's image;
verify the exact digest afterward and compare every other template,
configuration and identity property with the before-state. Do not start a job
or run a broad stack provision merely to update this pin. A code or image
update is not allocation, trust promotion or permission to start a feed.

The controller's existing local environment must contain
`AZURE_SUBSCRIPTION_ID`, `AZURE_RESOURCE_GROUP` and `BOINC_HANDOFF_JOB`.
Check the case-normalized main-stack outputs and actual job ID; older deployed
stacks can lack the hand-off output even while the job exists. Reconcile only
these local, nonsecret output values from the verified existing resource, in
the approved controller's settings directory. Do not provision resources,
change managed identities or add grants to repair local outputs.

The push command is a trusted literal argv prefix reaching the **existing**
`scripts/ocr-node/push-boinc-scan.sh` in CT 211. It receives normal `--scan`,
`--manifest`, account and root options. It reads cached scans and sidecars only;
it neither fetches missing scans nor modifies the fetcher's files or service.
The SSH user below is illustrative, not a new permission requirement.

Create an operator-owned JSON configuration outside version control (replace
all illustrative values and the image digest with the already approved pin):

```json
{
  "schema_version": 1,
  "env": "dev",
  "account": "exchange123",
  "plan": "volunteer-cohort-1",
  "batch": "example_batch",
  "manifest": "snapshot-1/manifest.parquet",
  "image": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "push_command": [
    "ssh",
    "-o",
    "BatchMode=yes",
    "operator@pve.example",
    "pct",
    "exec",
    "211",
    "--",
    "/opt/usnm/scripts/ocr-node/push-boinc-scan.sh"
  ],
  "scans_root": "/tank/ocr/scans",
  "manifests_root": "/tank/ocr/manifests",
  "batch_size": 10,
  "max_pending": 40,
  "pages_per_host": 2,
  "pages": [
    {
      "page": "sn12345678/1900-01-01/ed-1/seq-1",
      "scan": "/tank/ocr/scans/keep/example_batch/sn12345678/1900-01-01/ed-1/seq-1.jp2"
    }
  ]
}
```

The scan path must identify that page's actual manifest scan and sidecar; the
existing exporter verifies them. Allocate up to 10,000 explicit pages, with
1–100 pages per tick. Do not change configuration or delete controller state
mid-cohort: its configuration hash pins the complete allocation, limits and
worker image. Finish or drain, reconcile failures, and obtain operator approval
before migrating the configuration or allocating another cohort.
If any allocated page already has another plan's hand-off journal, the controller
refuses the overlap before writing allocation state or exporting anything. Keep
the original owner's journals intact; resolve ownership or choose different
pages rather than overwriting them, including for historical cohorts.

## One tick and backpressure

1. Take a local kernel lock and an infinite Azure blob lease on
   `boinc-handoff/scheduler/controller.lock`. A second controller fails closed.
   Manual bridge operations do not honor this lease: **one serialized operator
   still owns the two jobs**, whose launcher rejects running executions and
   verifies each execution's settings.
2. Run `batch-inventory`, or wait for it. It reads the bridge journal, complete
   assimilator outputs (or terminal markers), and two aggregate SQL counts with
   verified database TLS. It writes only `scheduler/inventory.json` in the
   hand-off container. A per-tick nonce rejects stale or concurrent snapshots.
   Hand-off journals are downloaded as metadata only, using one Azure CLI batch
   process rather than one CLI invocation per historical job. Each refresh
   clears the disposable metadata cache and checks the exact listed file set.
3. Collect up to `batch_size` previously allocated results, even with no active
   hosts or a full queue. Never supply a host ID override or bypass `--trust`.
   Partial output sets wait; duplicate spool directories fail closed.
4. Count **all apps' unassimilated workunits**, plus unfinished dispatch entries
   not already in that SQL set and durable allocations not yet exported. This
   includes assimilated jobs with missing or partial outputs, not just
   reservations; a stalled collector cannot create unbounded result backlog.
   Target `min(max_pending, active_hosts * pages_per_host)` page workunits. Here
   `active_hosts` means hosts that made a scheduler RPC in the last 24 hours,
   not a promise that they can run this app. The existing scheduler and plan
   classes decide actual compatibility. Tune the bounded multiplier after
   measuring that pool; 0 recent hosts pauses feeding. Other apps' work consumes
   capacity, and an uncertain submission blocks allocation until explicit
   reconciliation.
5. Persist selected pages **before** pushing or exporting. Push and export at
   most `batch_size` cached pages, then submit at most `batch_size`
   reservations. Submissions, including recovered reservations, also respect the
   remaining observed all-app queue capacity. Export transfers only the selected
   scans and sidecars, their existing journal entries and the single manifest
   snapshot; collection transfers only one journal, its optional `accepted/`
   publication recovery snapshot and result and trust files. Accepted bytes
   survive partial publication failures and are reused even if spool bytes
   change. Retained scan history is never downloaded per result.
   On rerun, authoritative hand-off and bridge records recover partial export
   and interrupted pulls. Never replay `create_work` for a `submitting` record.
   A submitted reservation is not submitted twice.
   Any existing SQL workunit, including completed work, with a missing or stale
   bridge journal also requires explicit reconciliation, even if capacity is
   available. Submission headroom includes other plans' assimilated, uncollected
   backlog; converting this cohort's reservations does not ignore that backlog.
6. Emit a structured `boinc_batch_tick` JSON event: selected and published
   counts, new publications, cumulative pages/hour, host and queue counts,
   oldest pending age, collection backlog and backpressure. Errors emit
   `boinc_batch_failure` and exit nonzero without dumping command output or
   credentials. Route stdout/stderr to the controller's existing log sink;
   this is controller telemetry, not a claim of live workbook ingestion.

Collection backlog consumes the next ticks before new allocation. A terminal
failure or a failed or held collection stops that tick, leaves state intact and
does not manufacture a replacement task. Resolve trust, calibration or output
failures through the owning workflow, then rerun. A conservative snapshot can
delay refilling by one tick after collection; it cannot enlarge the queue. The
target counts page workunits rather than replicas; native quorum and deadline
settings are unchanged.

## Validation, scheduling and recovery

### Drain without changing the allocation

For a controlled feed handoff or rollback ([#833](https://github.com/tgoodyear/usnewsmap.com/issues/833)),
add `--drain --apply` to the existing invocation. The controller still takes both
locks, checks inventory and collects existing results through the normal trust
gates, but selects no new pages, pushes no scans, exports no jobs and submits no
reservations, including reservations recovered from an interrupted tick.
Existing allocations and journals remain intact. Tick telemetry reports
`draining: true`; omit `--apply` to validate offline without remote calls.
Remove `--drain` to resume the same immutable allocation and state directory.
Uncertain submissions and terminal failures still require explicit reconciliation.
Drain mode does not cancel outstanding BOINC work or release reservations.

This switch controls only this BOINC controller. To drain the **direct OCR**
bulk feed, first preserve the configured `USNM_OCR_SEED_*` values and disable
seeding by clearing `USNM_OCR_SEED_PLAN` on the sync job through the normal
reviewed settings/deployment path. Keep `ocr-sync` running for imports and
dead-letter handling. Its user-hit priority feed is separate and remains enabled;
manual enqueue/requeue commands must also be coordinated. Do not transfer or
purge queue messages. Before assigning drained pages to BOINC, reconcile their
native ledger state and prove the selected worker image, German/Fraktur model
and calibrated trust scope match the intended cohort. Disabling a feed is not
evidence that all outstanding work has completed.

Offline validation makes no Azure or SSH calls and creates no state:

```sh
python3 scripts/boinc/batch_scheduler.py \
  --config /opt/usnm-controller/cohort.json \
  --state-dir /opt/usnm-controller/state
python3 -m unittest -v scripts/boinc/test_batch_scheduler.py \
  contrib-compute/images/server/test_batch_inventory.py
```

Only after the serialized operator has approved deployment and activation, run
one actual tick with the same command plus `--apply`. Verify its execution
status, native ledger reservations, canonical host IDs, trust decision,
published ALTO and Parquet, and normal `ocr-sync` behavior. Verify that a second
identical tick creates no duplicate work or publications. Then an operator may
schedule that exact command, for example every five minutes in the execution
host's existing scheduler, with stdout/stderr retained. The local lock rejects
overlapping ticks. No cron or systemd resource is installed by this change.

The state directory and the hand-off and bridge journals are durable recovery
state, not disposable scratch. If a controller crashes, its infinite blob lease
stays held deliberately: stop the execution host's schedule, inspect both Azure
job executions and the journals, and reconcile any `submitting` jobs using the
existing explicit reconcile procedure. Only the serialized operator may break
the stale `scheduler/controller.lock` blob lease once the former owner is
confirmed stopped. Do not delete journals, reset state, or break a live lease. A
missing reply is not evidence that `create_work` failed. The inventory fails
closed above 10,000 journals or existing native workunit IDs, or above 100,000
spool entries. Archive only drained, operator-verified history before resuming.

### Production acceptance remains separate

Merging code and tests is not a live deployment result. Production still
requires:

- release-owner approval and reviewed immutable server and bridge images
  containing `batch-inventory`, without overwriting another release or its
  image pins;
- existing private hand-off reachability, identities and native plan and ledger
  access on the approved controller (no new grants or paid infrastructure);
- explicitly allocated cached pages, an approved worker pin, compatible
  attached hosts and the existing calibrated gold trust workflow;
- one serialized approved push, export, submit and collect run, observed
  backpressure, telemetry delivery and rerun evidence;
- operator-controlled scheduling and rollback (disable feeding first; retain
  collection and recovery state).

Until these are independently authorized and verified, leave the issue open
for operational acceptance and do not claim a continuous production feed.
