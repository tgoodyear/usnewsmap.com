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
Unallocated pages remain on-prem work, unless the controller is set to take pages
off the work queues itself ([Queue-fed work](#queue-fed-work)). There is no LoC
fetching, scanfetch change, worker-image rollover, gold issuance, trust
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
collection code. The existing production pin is the scoped-trust v4 bridge
artifact built from merged #755 source `38893b3008708f6f25c2beab2dcba40ab7521fab`,
also selected for the BOINC bridge. It includes the current-worker Gold routing
registry and rejects unscoped legacy trust for publication. The v5 source changes
in #756 do not update this image; a reviewed image rebuild and separately
coordinated rollout are required. This pin alone is not a runtime rollout or host
promotion.

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
4. Separate the runnable `usnm_ocr` cohort queue (`app` / `platform` / `resource`,
   default `usnm_ocr` / `all` / `cpu`; only `usnm_ocr`, `all`, and `cpu` are
   accepted as cohort selectors because `Transport.submit()` and
   `server_status` / `batch-inventory` queue accounting are application-scoped
   CPU workunits across all platforms) from unrelated other-app queues (such as
   `usnm_qwen`, `usnm_quality`, or `usnm_surya`), while enforcing an overall
   global safety cap (`max_global_pending` / `--max-global-pending`, up to
   100,000 workunits across all apps and collection backlog). When
   `server_status.php?xml=1` (or a
   structured status snapshot via `--server-status-url` / `--server-status-file`
   or `inventory.json`) is supplied, `parse_server_status` reads per-app
   `<unsent>` and `<in_progress>` counts so `usnm_ocr` admission checks
   `usnm_ocr`'s own runnable queued/in-progress work against its cohort low/high
   watermarks rather than letting unrelated GPU/other-app queues starve
   CPU/native hosts. Global and cohort headroom (`cohort_pending`,
   `other_app_pending`, `global_pending`) are reconciled in SQL workunit units,
   while result-replica counts (`cohort_pending_results`,
   `other_app_pending_results`, `global_pending_results`) are retained separately
   for queue diagnostics.
   Cohort watermarks (`low_watermark` / `--low-watermark` and `high_watermark` /
   `--high-watermark`, or nested under `cohorts.<cohort>` / `cohort`) implement
   hysteresis: when `cohort_pending <= low_watermark` (and `active_hosts > 0`),
   the controller enters refilling mode (`refilling: true`) and refills up to
   `high_watermark` (capped by `min(max_pending, active_hosts * pages_per_host)`
   when `high_watermark` is not explicitly configured), then holds above
   `low_watermark` (`backpressure_reason: "cohort_hysteresis"`) until the runnable
   queue drains back to `low_watermark`. When the global backlog reaches
   `max_global_pending`, refilling halts with `backpressure_reason:
"global_safety_cap"` (distinct from `"cohort_high_watermark"`,
   `"cohort_hysteresis"`, `"collection_backlog"`, `"no_active_hosts"`, and
   `"draining"`). Assimilated jobs with missing or partial outputs still count
   toward `collection_backlog`; a stalled collector cannot create unbounded
   result backlog, and an uncertain submission blocks allocation until explicit
   reconciliation.
5. Persist selected pages **before** pushing or exporting. Push and export at
   most `batch_size` cached pages, then submit at most `batch_size`
   reservations. Submissions, including recovered reservations, respect both the
   cohort high watermark and `max_global_pending`. Export transfers only the selected
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
   counts, new publications, cumulative pages/hour, host and queue counts
   (`cohort_pending`, `other_app_pending`, `global_pending`, `low_watermark`,
   `high_watermark`, `max_global_pending`, `refilling`, `app_queues`), oldest
   pending age, collection backlog, `backpressure` and `backpressure_reason`,
   plus `starvation_alert` and structured `alerts` (`cohort_starvation` when
   active hosts face `0` unsent or `<= low_watermark` runnable `usnm_ocr` work,
   and `zero_work_starvation` when scheduler logs or status report hosts
   receiving zero tasks). Errors emit `boinc_batch_failure` and exit nonzero
   without dumping command output or credentials. Route stdout/stderr to the
   controller's existing log sink; this is controller telemetry, not a claim of
   live workbook ingestion.

Collection backlog consumes the next ticks before new allocation. A terminal
failure or a failed or held collection stops that tick, leaves state intact and
does not manufacture a replacement task. Resolve trust, calibration or output
failures through the owning workflow, then rerun. A conservative snapshot can
delay refilling by one tick after collection; it cannot enlarge the queue. The
target counts page workunits rather than replicas; native quorum and deadline
settings are unchanged.

## Queue-fed work

By default the controller works through pages an operator named. With
`"source": "queue"` it instead takes them off the OCR exchange's Azure Storage
queues, `ocr-bulk` and optionally `ocr-priority`, as a **second consumer**
beside the on-prem worker (#296). A page has one place in line whichever pool
reads it, so the damage order and the user-requested pages are the queues' and
there is no separate list to keep in step with them.

```json
{
  "schema_version": 1, "source": "queue", "env": "dev", "account": "exchange123",
  "plan": "queue-feed-1", "manifest": "snapshot-1/manifest.parquet", "image": "sha256:...",
  "push_command": ["ssh", "-o", "BatchMode=yes", "operator@pve.example", "pct", "exec", "211",
                   "--", "/opt/usnm/scripts/ocr-node/push-boinc-scan.sh"],
  "scans_root": "/tank/ocr/scans", "manifests_root": "/tank/ocr/manifests",
  "batch_size": 10, "max_pending": 40, "pages_per_host": 2,
  "claim_queues": ["ocr-bulk"], "lease_hours": 96
}
```

There is no `batch` or `pages`: the pages are whatever the queue gives, from any
number of batches. `claim_queues` is `ocr-bulk` by default. A bulk workunit's
`delay_bound` is three days. A page someone was shown (`reason` `user`, from
`ocr-priority`) is submitted with a one-day `delay_bound` and workunit priority 10
(#866), so a host that sits on it hands it on within a day. The priority takes
effect only if the feeder orders by priority; the default feeder does not. A
controller that claims `ocr-priority` should set `lease_hours` to cover that
deadline plus a collect (36 hours), so a lost page returns to the queue sooner.
The on-prem node reads `ocr-priority` first when it runs.

```
ocr-bulk (and ocr-priority) --claim--> claims/<doc_id>.json in boinc-handoff
  -> push the page's cached scan from CT 211 -> export --claims -> submit
  -> BOINC -> spool-out -> collect (ocr-out) -> delete the queue message
```

### The claim

The hand-off job's `claim` step (`usnm-boinc claim pull`, in the main
environment, which reaches the queues, the ledger and `ocr-out`) receives up to
`CLAIM_MAX` messages with a hide of `lease_hours` (96 by default: the three-day
`delay_bound` and a day to collect; Azure allows seven days). It holds the `ocr-sync`
lock while it reads the ledger and receives, so no producer changes a row between
the two: every producer writes a page's row before it sends the page's message, and a
ledger read under the lock has the row of every message already queued. It uses a
message only when all of these hold, and otherwise puts it back:

- it decodes, and the ledger has the page **outstanding** with the same batch,
  reason and language (the seeder, `ocr-enqueue` and the requests step write the
  row before they send the message, and `ocr-sync` imports against it);
- the batch is published at the version the row names, the title isn't Japanese
  and the language routes to an installed Tesseract model;
- the page is on the newest fetched-scans list for the controller's manifest
  version, because volunteers never fetch from LoC;
- with `claim_languages` (`CLAIM_LANGUAGES`, `--language`), the page's source
  language is one of them (#866).

Set `claim_languages` to the trust calibration's language (`["eng"]` today:
`trust.json` has one `scope.language`, and German failed its gate in #835).
Without it the claim takes any language the worker image can read, and
`collect --trust` holds back a result outside the calibrated scope, so that
page's message would stay hidden for the whole lease instead of going back to
a node. The key is optional and changes nothing when absent. Deploy a hand-off
image that knows `CLAIM_LANGUAGES` before setting it: an older image ignores
the variable and claims every language.

A page whose ledger row is already answered is **deleted** from the queue. Any
other message it can't use is **sent back** (a copy is sent, then the original is
deleted), so it starts again at a dequeue count of zero for the node to read. The
queue is peeked first, and one whose front holds nothing usable isn't touched.

Each usable message becomes `claims/<doc_id>.json`, holding the message text, its
pop receipt, the lease end and the ledger row. The files go up **before** the
summary (`scheduler/claims.json`, with the run's nonce), even when the run then
fails, because the pop receipts exist nowhere else. A message whose claim file
could not be written comes back when its hide runs out.

### The rest of the tick

The tick claims only what it can start: `min(batch_size, target - pending)` less
the pages already claimed and not yet started, so backpressure holds back claims
and the messages stay visible to the node. Every claim is saved to the
controller's state before its scan is pushed. The controller then pushes each
page with `push-boinc-scan.sh --keep-page <batch>/<lccn>/<date>/<edition>/<seq>`,
which finds the scan's format (`.jp2` or `.jpg`) on the scan server. A page with no
cached scan, despite the list, is given back with the `release` step and dropped.
Export runs once per batch with `HANDOFF_CLAIMS=1`.

With claims, `export` does not write a ledger row. The job's message is the
queue's own (reason, score, `queued_at`), the journal records the claimed ledger
row, and the export checks under the `ocr-sync` lock that the row is still the
outstanding one (`ocr_local::boinc::adopt`). `collect` then imports against that
row exactly as it does for the node's pages.

### The queue message's life

| What happens                                               | The message                                                                                                                                                                                                                                 |
| ---------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Claimed                                                    | Hidden for the lease; a node doesn't see it                                                                                                                                                                                                 |
| `collect` published the page                               | Deleted, by `collect` itself (`usnm-boinc claim finish`)                                                                                                                                                                                    |
| `collect`'s delete failed, or the lease ended first        | Visible again. A claim finds the page answered and deletes the message; a node would read the page again, and the newest row per page wins                                                                                                  |
| The job failed, or was never collected                     | Visible again when the lease ends, for a node or a later claim. The controller still stops on a terminal failure for an operator, as before                                                                                                 |
| The same message again after its lease (job still pending) | Kept as the same claim: the summary carries the message's hash, the controller recognizes it and keeps its scan, selection and job, and the new delivery is hidden for a fresh lease. A different message for a claimed page stops the feed |
| A second delivery of a page within one pull                | Deleted; the first claim covers the page and keeps its pop receipt                                                                                                                                                                          |

This is at-least-once, like a node's. `ocr-sync`'s import and the ledger already
tolerate the same page being read twice. Nothing in a claim carries search text:
the message is the same page-id-only message the nodes read.

### Access

The hand-off job's identity gets Storage Queue Data Message Processor and
Message Sender on `ocr-priority` and `ocr-bulk` only (`handoffQueueIds` in
`infra/modules/ocr-exchange.bicep`). That is the nodes' roles plus the ability to
put a message back; it has no Update Message (ADR-0015) and nothing on
`ocr-deadletter`. The job's `QUEUE_URL` setting is optional for the launcher, so a
job deployed before this change still runs export and collect until it is
provisioned. `claims/` expires after 30 days.

### Rollout order and edge cases

- A queue-fed job's journal record carries one more field, the claimed ledger row, which
  the BOINC bridge reads when it pulls the job. Deploy the hand-off and bridge images
  built from the same revision (#529) before the first claim; explicit cohorts' records
  are unchanged, so nothing changes for them meanwhile.
- A message queued again after its claim lapsed (the seeder re-queues a page outstanding
  for 14 days) is a different message and so a different job. The controller then sees two
  jobs on one page and stops for an operator to reconcile, as it does for any two.
- A claim can lapse while the job is pending, and a node can then import the page. When
  the BOINC result arrives, `collect` finds the page answered by another producer and marks
  the job `published` with a `superseded` note, without publishing anything, so the feed
  goes on. This applies only to queue-fed jobs; an explicit cohort's page is not read by a
  node, so the same finding there remains an error.
- A claim that failed after taking messages leaves their claim files in the container.
  The controller records those it finds in the summary and stops the tick; any other
  message comes back when its lease runs out.

### Not changed

A page that fails on a volunteer is not dead-lettered by BOINC; the node and
`ocr-sync` own that path. Gold issuance, host trust, the worker pin and
operator approval are as above. Merging this is not a deployment: provision the
hand-off job and its roles, reconcile, and approve a first tick with `--apply`
before scheduling it.

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
