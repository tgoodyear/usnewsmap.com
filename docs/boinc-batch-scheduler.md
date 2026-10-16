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
exporter still checks/reserves the ledger and refuses conflicting requests.
Unallocated pages remain on-prem work. There is no automatic queue consumption,
LoC fetching, scanfetch change, worker-image rollover, gold issuance, trust
override, client attachment, or retry of terminal failures.

The controller uses a single approved execution host with Python 3.9+, Azure CLI,
`jq`, Bash, an existing Azure login, the checkout's existing environment outputs,
and the existing hand-off/bridge jobs. The server/bridge images must contain the
new additive `batch-inventory` step; retain the existing immutable worker pin.
Do not deploy server/bridge images or run the controller while another release
owner is changing them.

The push command is a trusted literal argv prefix reaching the **existing**
`scripts/ocr-node/push-boinc-scan.sh` in CT 211. It receives normal `--scan`,
`--manifest`, account and root options. It reads cached scans and sidecars only;
it neither fetches missing scans nor modifies the fetcher's files/service.
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
worker image. Finish/drain, reconcile failures, and obtain operator approval
before migrating the configuration or allocating another cohort.

## One tick and backpressure

1. Take a local kernel lock and an infinite Azure blob lease on
   `boinc-handoff/scheduler/controller.lock`. A second controller fails closed.
   Manual bridge operations do not honor this lease: **one serialized operator
   still owns the two jobs**, whose launcher rejects running executions and
   verifies each execution's settings.
2. Run/wait for `batch-inventory`. It reads the bridge journal, complete
   assimilator outputs (or terminal markers), and two aggregate SQL counts with
   verified database TLS. It writes only `scheduler/inventory.json` in the
   hand-off container. A per-tick nonce rejects stale/concurrent snapshots.
3. Collect up to `batch_size` previously allocated results, even with no active
   hosts or a full queue. Never supply a host ID override or bypass `--trust`.
   Partial output sets wait; duplicate spool directories fail closed.
4. Count **all apps' unassimilated workunits**, plus unfinished dispatch entries
   not already in that SQL set and durable allocations not yet exported. This
   includes assimilated jobs with missing/partial outputs, not just reservations;
   a stalled collector cannot create unbounded result backlog. Target
   `min(max_pending, active_hosts * pages_per_host)` page workunits. Here
   `active_hosts` means hosts that made a scheduler RPC in the last 24 hours,
   not a promise that they can run this app. The existing scheduler/plan classes
   decide actual compatibility. Tune the bounded multiplier after measuring
   that pool; 0 recent hosts pauses feeding. Other apps' work consumes capacity,
   and an uncertain submission blocks allocation until explicit reconciliation.
5. Persist selected pages **before** push/export. Push/export at most
   `batch_size` cached pages, then submit at most `batch_size` reservations.
   Submissions, including recovered reservations, also respect the remaining
   observed all-app queue capacity. Export transfers only the selected scans and
   sidecars, their existing journal entries and the single manifest snapshot;
   collection transfers only one journal and its result/trust files. Retained
   scan history is never downloaded per result.
   On rerun, authoritative hand-off and bridge records recover partial export
   and interrupted pulls. Never replay `create_work` for a `submitting` record.
   A submitted reservation is not submitted twice.
6. Emit a structured `boinc_batch_tick` JSON event: selected/published counts,
   new publications, cumulative pages/hour, host/queue counts, oldest pending
   age, collection backlog and backpressure. Errors emit
   `boinc_batch_failure` and exit nonzero without dumping command output or
   credentials. Route stdout/stderr to the controller's existing log sink;
   this is controller telemetry, not a claim of live workbook ingestion.

Collection backlog consumes the next ticks before new allocation. A terminal
failure or failed/held collection stops that tick, leaves state intact and
does not manufacture a replacement task. Resolve trust/calibration/output
failures through the owning workflow, then rerun. A conservative snapshot can
delay refilling by one tick after collection; it cannot enlarge the queue.
The target counts page workunits rather than replicas; native quorum/deadline
settings are unchanged.

## Validation, scheduling and recovery

Offline validation makes no Azure/SSH calls and creates no state:

```sh
python3 scripts/boinc/batch_scheduler.py \
  --config /opt/usnm-controller/cohort.json \
  --state-dir /opt/usnm-controller/state
python3 -m unittest -v scripts/boinc/test_batch_scheduler.py \
  contrib-compute/images/server/test_batch_inventory.py
```

Only after serialized operator deployment/activation approval, run one actual
tick with the same command plus `--apply`. Verify its execution status, native
ledger reservations, canonical host IDs, trust decision, published ALTO/Parquet
and normal `ocr-sync` behavior. Verify a second identical tick creates no
duplicate work or publications. Then an operator may schedule that exact
command, for example every five minutes in the execution host's existing
scheduler, with stdout/stderr retained. The local lock rejects overlapping
ticks. No cron/systemd resource is installed by this change.

The state directory and hand-off/bridge journals are durable recovery state,
not disposable scratch. If a controller crashes, its infinite blob lease stays
held deliberately: stop the execution host's schedule, inspect both Azure job
executions and the journals, and reconcile any `submitting` jobs using the
existing explicit reconcile procedure. Only the serialized operator may break
the stale `scheduler/controller.lock` blob lease once the former owner is
confirmed stopped. Do not delete journals, reset state, or break a live lease.
A missing reply is not evidence that `create_work` failed. The inventory fails
closed above 10,000 jobs or 100,000 spool entries; archive only drained,
operator-verified history before resuming.

### Production acceptance remains separate

Merging code/tests is not a live deployment result. Production still requires:

- release-owner approval and reviewed immutable server/bridge images containing
  `batch-inventory`, without overwriting another release or its image pins;
- existing private hand-off reachability, identities and native plan/ledger
  access on the approved controller (no new grants or paid infrastructure);
- explicitly allocated cached pages, an approved worker pin, compatible
  attached hosts and the existing calibrated gold trust workflow;
- one serialized approved push/export/submit/collect run, observed
  backpressure, telemetry delivery and rerun evidence;
- operator-controlled scheduling/rollback (disable feeding first; retain
  collection and recovery state).

Until these are independently authorized and verified, leave the issue open
for operational acceptance and do not claim a continuous production feed.
