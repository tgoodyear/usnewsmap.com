# BOINC volunteer batch controller

Issue [#525](https://github.com/tgoodyear/usnewsmap.com/issues/525). The opt-in
controller connects the existing CT 211 cache push, hand-off export, bridge
`submit-remote`, assimilator `spool-out`, and trust-checked hand-off `collect`.
It runs **one bounded tick**, not a daemon, and does nothing remotely without
`--apply`. Adding this code does **not** deploy it or enable a production feed.

## Recovering a contract-invalid canonical output

BOINC validity alone does not establish native output integrity. Do not repair a
checksum in `result.json`, replace scientific output with empty files, reset the
controller journal, or release a failed page to rediscover the same workunit.
Trust holds, missing files and transport errors are not permanent output failures.

After an operator reconciles the exact job, canonical result and original
assimilator spool, `usnm-boinc reject-output` provides an explicit, local-only
disposition:

```sh
usnm-boinc reject-output --root "$dispatch_root" --spool "$canonical_spool" \
  --job "$job" --expect-result-sha256 "$result_sha256" \
  --expect-text-sha256 "$text_sha256" --expect-alto-sha256 "$alto_sha256"
```

All three hashes must come from the reviewed canonical bytes, including an empty
file's hash. The command refuses missing/changed files, accepted snapshots,
bundles, unsubmitted jobs and outputs with a valid native result contract.
Only invalid result JSON or a failed native provenance/checksum/UTF-8 contract
can be rejected. It preserves those exact bytes plus a content-free receipt in
`quarantine/<job>/rejected/` **before** marking the job `failed`. Replaying the
same disposition is idempotent; conflicting receipts or quarantined bytes fail.
Ordinary collection and all trust checks are unchanged.

For a remote hand-off, first back up both authoritative dispatch journals and
the canonical files under the exclusive operator/controller locks. Run the
command on a private working copy reconciled with the existing hand-off helpers:

1. Read the BOINC bridge's `handoff/jobs/<job>/record.json` and the collection
   container's `dispatch/jobs/<job>/record.json`. Remote submission can leave the
   latter **reserved** while the bridge is **submitted**; do not manually advance
   its stage. Require matching complete job identity, plan, stamp and native
   ledger, a confirmed submitted canonical workunit, no bundle and no uncertain
   submission. A mismatch requires reconciliation, not disposition.
2. Use the verified **submitted bridge record** as the private working record.
   Fetch the collection-side `dispatch/jobs/<job>/accepted/` into that working
   job's `accepted/`, and check the bridge-side accepted directory too. Any
   accepted manifest makes the command refuse; do not discard accepted data to
   pass its guard. Preserve existing quarantine files in the working copy as
   well, so a retry cannot overwrite conflicting evidence.
3. After the command succeeds, persist the exact failed record to **both**
   bridge `handoff/jobs/<job>/record.json` and collection
   `dispatch/jobs/<job>/record.json`. Persist its quarantine to collection
   `dispatch/quarantine/<job>/` through the existing `handoff put` workflow and
   the bridge's corresponding local quarantine path. Verify all hashes before
   resuming the controller. A changed local working copy alone is not recovery.

With `--dead-letter-failed`, the existing controller
then uses the failed journal's reason to send only that page to `ocr-deadletter`,
preserving the native reservation and scientific database. Confirm message
settlement and a clean subsequent ordinary tick; a lost queue lease still needs
explicit reconciliation. Requeue is deliberately not automatic.

Rollback before message settlement restores the backed-up journals under the
same locks. After settlement, do not restore a submitted journal or replay work:
retain the receipt and reconcile the existing dead letter/reservation explicitly.

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
outputs, and the existing hand-off job. Its bridge steps run through
`scripts/boinc/start-boinc-bridge.sh` (the Azure bridge job) or, with `step_command` in
the configuration, on the on-prem BOINC host through `usnm-boinc-step`, which is how
the on-prem server's bridge steps are reached ([boinc-onprem.md](boinc-onprem.md), cutover
step 5). The server and bridge images
must contain the new additive `batch-inventory` step; retain the existing
immutable worker pin. Do not deploy server or bridge images or run the
controller while another release owner is changing them.

### On-prem controller host (CT 211)

Production runs the queue-fed priority controller in the scan-fetch container
(CT 211, #866), signed in as its Arc machine identity, the same
`USNM_BOINC_HANDOFF_RUNNER` that pushes scans. `USNM_BOINC_CONTROLLER_ONPREM`
(`infra/modules/boinc-controller.bicep`) gives that identity Reader on the
subscription, for the stacks' outputs, and a custom role on the main and BOINC
groups that reads, updates and starts the hand-off and bridge jobs (with the
environment join and identity assign checks an update makes). That role can
change those jobs, so the host can act as their identities; this was accepted
for CT 211. CT 211 holds a checkout of main at `/opt/usnm`, the non-secret
outputs in `/root/.config/usnewsmap/prod/.env`, the configuration in
`/etc/usnm/boinc-priority-controller.json` (`claim_queues ["ocr-priority"]`,
`claim_languages ["eng"]`, `lease_hours` 36), its state in
`/var/lib/usnm-priority-controller`, and the push command
`/opt/usnm/scripts/ocr-node/push-boinc-scan.sh` (local, no SSH).

The service passes `--server-status-url
https://boinc.usnewsmap.com/server_status.php?xml=1` and
`--max-global-pending 1000`. Without per-app status, conservative inventory
accounting treats every application's pending work as OCR and can prevent
priority claims while unrelated GPU queues are full. Keep `max_pending: 40`,
`pages_per_host: 2`, and `batch_size: 10`: OCR admission is bounded by
`min(40, active_hosts * 2)` independently of the global safety cap. Status fetch
or parsing failures stop the tick rather than admitting unmeasured work.
Use these existing runtime flags rather than changing an initialized
configuration's immutable hash; never delete or reset controller state to
change admission settings.

The systemd service `usnm-boinc-priority-controller.service` runs one tick with
`--apply` and sets `Environment=USNM_SETTINGS_HOME=/root/.config/usnewsmap`.
Set this explicitly: a system service need not inherit `HOME`, and the
hand-off launcher otherwise cannot resolve its local environment file even
though the bridge launcher's stack discovery works. Verify hand-off
`--what-if` under systemd, not only an interactive shell.
`usnm-boinc-priority-controller.timer` is enabled with
`OnBootSec=5min` and `OnUnitInactiveSec=5min`. Read its structured events with
`journalctl -u usnm-boinc-priority-controller.service`; stop the timer and wait
for any active service to finish before changing its configuration or performing
exclusive lifecycle maintenance. Each tick takes the shared controller lease.
Successful empty-queue ticks are not end-to-end priority-page acceptance:
the real-page receipt for #866 is recorded below.

### Batched steps (#932)

A tick submits, spools out and collects one page per job execution unless told otherwise.
Each execution costs one to two minutes of Container Apps start and stop, so a 10-page tick
spends about 40 minutes on them (measured 20 October 2026, when the bridge steps were still Azure jobs; on-prem bridge steps start no Container Apps execution), whatever the volunteers' capacity.
Four runtime flags put a tick's pages into one execution per step instead. They aren't part
of the configuration, so turning them on or off changes no hash:

- `--batch-submit`: `submit-remote` with `JOB_IDS=<id>,...`. It stops at the first job that
  fails, as one execution per job would; the next tick reconciles that job before any other
  submission.
- `--batch-spool-out`: `spool-out` with `HANDOFF_SPOOLS=<id>=<spool>,...`.
- `--batch-collect`: the hand-off `collect` with `JOB_IDS=<id>,...`.
- `--batch-export`: the hand-off `export` for pages from many base batches in one execution
  (up to 20 batches; more go in several), with `HANDOFF_BATCHES` naming each page's batch in
  `HANDOFF_PAGES`' order. The job exports each batch on its own. Without it, a queue-fed tick
  runs one execution per batch, and user-requested pages come from many batches (#1055).

Spool-out, collect and export go on past a job (or batch) that fails and fail at the end, so one held page doesn't
hold up the others; the tick then stops as before. Each job in a batch runs the step's
single-job path, with the same checks. A flag needs the image whose step takes the batch
variable: the bridge image for the first two (usnm-boinc-prod), the hand-off image for the
third and fourth (usnm-prod's `USNM_BOINC_HANDOFF_IMAGE`). Add a flag to the service's `ExecStart` only
once that image is live, with the timer stopped.

These flags batch the server-side steps; each page is still its own BOINC workunit. A plan can
also put several pages of one queue into one workunit with `pages_per_unit` and
`bundle_app_version` (#1059, [boinc-ocr-bundles.md](boinc-ocr-bundles.md)). Unlike the flags,
those are configuration, so turning them on makes a new plan.

### On-prem claim, collect and export (#967)

By default the hand-off steps run as the Azure hand-off job. `--onprem-handoff claim`,
`--onprem-handoff collect` and `--onprem-handoff export` (any of them) send them through
`step_command` instead, to `usnm-boinc-step --target handoff` on the on-prem BOINC host
([boinc-onprem.md](boinc-onprem.md#hand-off-steps-on-prem)). `claim` takes its `release`
with it. `texts-export` runs with `scripts/boinc/texts-export-onprem.sh` on the host with a local copy of curated. With `--onprem-handoff export`, the controller's push
sends the claimed pages' scans straight to the BOINC host (`usnm-boinc-step --target scans
--step receive`, a tar over the step key into `/srv/usnm-boinc/project/handoff-scans`), not to
the hand-off container. Export reads them there, and the bridge's `submit-remote` takes each
job's page from them, so no scan goes through Azure (#1116). Like the batch flags, these are runtime
flags, not configuration, so turning them on or off changes no hash. They need a plan with
`step_command`, and the host needs `/etc/usnm-boinc/handoff.env`. A dry run prints the
choice as `onprem_handoff`.

### Production priority canary

On 19 October 2026, cached English page
`sn99066040_1865-08-12_ed-1_seq-2` completed the queue-fed path under plan
`priority-feed-1`, job `a9ba35b339d479442d0efcfeefa9bbab2f31ab4989b88db569cd3c0c0f0761bb`.
The existing ledger-aware operator command `ocr-enqueue --reason user` queued
it at 04:36:52 UTC; this tests downstream priority handling, not browser
request capture or the daily request batch.

The normal bounded scan claimed it at 05:10:14, pushed its cached scan,
exported it and submitted it. Host 10 finished at 05:18:33 with 29,826 text
bytes and 181,376 ms worker elapsed time. Collection initially quarantined it
because that host lacked exact-image English gold evidence. One normal targeted
challenge using the existing frozen 30-crop calibration then passed: CER 0.0716
against the unchanged 0.08734328574534386 threshold, with no skips or complete-loss
crops. Native canonical reports and complete outcome snapshots for hosts 1, 8
and 10 produced the v5 trust list; the image, challenge, language, model, PSM,
DPI and threshold were unchanged. No trust override was used.

The resumed normal collector published at 06:25:06, and the claim's persisted
`ended` became `finished`. An operator-triggered execution of the unchanged
`ocr-sync --max-runtime-secs 3000` stage recorded the page's `imported` outcome
at 06:28:01. Enqueue-to-claim was 33 min 22 s, enqueue-to-volunteer-output
41 min 41 s, and enqueue-to-import 1 h 51 min 9 s. Calibration bootstrap and a
container-exec throttling wait dominate this single observation; it is not a
steady-state latency or page-accuracy guarantee.

Node 4's direct worker was active, contrary to the original issue's assumption:
it consumed the first canary. The second used a bounded graceful pause, with an
automatic restart armed before stopping; the worker was restored immediately
after the BOINC claim. Both it and CT 211's timer were active afterward.
Competing consumers and Azure queue ordering mean bounded scanning does not
promise ownership or a fixed number of ticks. Inventory and lifecycle overhead
also mean five minutes between inactive services is not five-minute polling.
Language admission alone does not guarantee the returning host is calibrated;
retain normal gold onboarding and fail-closed quarantine.

### Gold onboarding (#901)

Language admission doesn't decide which host returns a page. A result from a
host with no gold evidence for the scope is held at collection, and since the
hand-off `collect` step fails on a held result, the tick stops there and no new
work goes out until the host is calibrated. On 21 October 2026 that took a
manual targeted gold campaign. With `gold_onboarding` in the controller's
config, the controller does this itself:

```json
"gold_onboarding": [
  {"campaign": "current247c-amd64-calibration-20261017", "language": "eng", "profile": "eng",
   "engine_base": "sha256:<the worker's TESSERACT_IMAGE digest>",
   "interval_minutes": 60, "max_hosts": 10}
]
```

`campaign` must be the scope's calibrated challenge: its `prepared.json` has the
`manifest_sha256` that `gold/trust.json` records for the scope (for eng/eng with worker
247c, `current247c-amd64-calibration-20261017`, manifest `d8cd7279…`), or `gold-promote`
refuses it. `engine_base` is a bare `sha256:` digest, as the gold reports record it, or
`name@sha256:`.

When a scope is due, the tick runs two bridge steps before it collects:

1. `gold-onboard`: the existing host-trust onboarding pass. Every host BOINC
   knows that isn't trusted for the scope, the calibrated challenge and the
   worker image (`image`) gets one targeted gold unit, at most one outstanding
   per host and at most `max_hosts` new units per pass. Gold units count
   against the global safety cap, computed as the refill computes it (from
   the same per-app status, and counting the same retained backlog:
   uncollected journals of every plan, recovered reservations and selected
   pages not yet exported). A pass issues at most what the cap leaves
   (shared across scopes), is skipped when nothing is left (`gold-promote` still runs, with the
   configured `max_hosts`), and the units it
   may have issued count as in flight for the tick's OCR refill. A failed unit
   gets a fresh attempt. A unit that ran and scored over the threshold is a verdict and
   isn't reissued.
2. `gold-promote` (`usnm-boinc host-trust promote`): collects those units with
   the native result query, judges each untrusted host's complete evidence
   against the scope's calibration in `gold/trust.json` (its `max_cer` and
   `manifest_sha256`), and adds the hosts that pass to that scope. It never
   sets or changes a threshold or challenge, refuses when the prepared campaign
   isn't the calibrated challenge, keeps every other scope, image and host, and
   never removes a host. Each promotion is recorded in
   `host-trust/promotions.jsonl` with the evidence's workunit and result ID.
   A recorded workunit is never used again, so a later trust rebuild that drops
   the host stands. The record is written ahead of the trust change and
   committed after it; the next pass finishes an uncommitted record only while
   `trust.json` is still exactly as it was, and otherwise settles it without
   adding the host. The ledger is read again under the controller lock before
   any change. An uncommitted record is replayed only if it was judged by
   the scope's current threshold and challenge, and the list version is
   compared again under the trust lock as hosts are added, so a rebuild in
   between is never undone.

A host admitted this way has its held results published by the same tick's
collection. A host that fails or hasn't returned its unit stays untrusted, and
its results stay held, as before.

A scope is due `interval_minutes` (10 to 1440, default 60) after its last run.
While results are held it's due after 10 minutes at most. The run goes into the
controller state before its steps start, so a crash or a failing step waits for
the next due time instead of running every tick. A failed step adds a
`gold_onboarding_failed` alert to the tick line and doesn't stop the tick. The
tick line reports `gold_onboarding` (the runs this tick), `collection_held` and
`oldest_held_seconds`. A tick that stops at a held result reports the same
counts and the last runs on its `boinc_batch_failure` line. Drain ticks run no
gold steps.

`gold_onboarding` isn't part of the allocation digest, so turning it on needs
no migration. Up to four scopes, one per language and profile. Over ssh,
`onprem-step.sh` lets the controller set only the `GOLD_CAMPAIGN`,
`GOLD_LANGUAGE`, `GOLD_PROFILE`, `GOLD_ENGINE_IMAGE`, `GOLD_ENGINE_BASE`,
`GOLD_ENGINE_VERSION` and `GOLD_MAX_HOSTS` inputs of these two steps; never a
threshold, a challenge hash, a target host or a spool. `gold-promote` needs a
bridge image built from this change.

### Separate hand-off runtime

The hand-off job is in the **main** resource group, not the BOINC runtime
group. Updating the BOINC server and bridge pair does not update this job.
`USNM_BOINC_HANDOFF_IMAGE` in `infra/env/prod.json` must name a reviewed bridge
artifact containing the bounded export, exact-file reads and accepted-snapshot
collection code. PR #888 initially pinned both lifecycle jobs to bridge digest
`sha256:91539ee197e3de29ac5e1eb3ccbee1d25237553d25b519feabb90aef9436100d`,
built from main `1ac92dcb91fcc0b3bc4b1c900163d427df63aed1`, including
language-scoped claims (#867) and priority user work (#868). Both job images
were rolled out image-only under #529; server, web, worker, and TLS settings
were unchanged. PR #898 subsequently advanced only the hand-off job to
`sha256:b4f2f00b2128b7eb32206f210e0673be7f9edc7017133e1684e493ed2922b877`,
containing #897's bounded priority scan. The bridge remained at `91539ee1...`;
the hand-off template, configuration and identity were otherwise unchanged.
A future pin change alone is not a runtime rollout or host
promotion; it requires a reviewed rebuild and separately coordinated deployment.

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
queues, `ocr-bulk` and optionally `ocr-priority` (#296). Since the direct
on-prem worker was retired (#966) it is their only consumer. The damage order
and the user-requested pages are the queues', and there is no separate list to
keep in step with them.

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
(#866), so a host that sits on it hands it on within a day. Its results inherit
the priority, and the feeder's default order (`priority desc, workunitid`) puts
them ahead of other `usnm_ocr` work in its candidate window (no app uses
homogeneous redundancy, which would ignore the order). A
controller that claims `ocr-priority` should set `lease_hours` to cover that
deadline plus a collect (36 hours), so a lost page returns to the queue sooner.

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
deleted), so it starts again at a dequeue count of zero for the node to read.
The queues are read in `claim_queues` order, and `CLAIM_MAX` is one limit
across them: the first queue fills what it can and the next fills the rest.
Bulk queues are peeked first, and one whose front holds nothing usable isn't
touched. Priority queues instead scan past an unusable front, sending unusable
messages back. This lets later cached, trusted pages progress across ticks when
the front contains only uncached or untrusted pages. Each queue has its own
receive budget (`--max-receives`, three times the claim limit by default), so
unusable messages on one queue can't use up the next queue's reads (#1057). A
tick therefore receives at most the budget times the number of queues. A
repeated sent-back message stops that queue's scan. No eligibility check is
relaxed, and an unusable message is never deliberately left hidden. The step's
log gives the claimed, sent-back and deleted counts for each queue.

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
which finds the scan's format (`.jp2` or `.jpg`) on the scan server, in the fetcher's
keep set and then its buffer set, where a queued OCR request's scan is (#1055). A
buffer scan is copied up under its keep name, so the export reads it like any other,
and evicting it from the buffer later doesn't affect the hand-off. A page with no
cached scan, despite the list, is given back with the `release` step and dropped.
Export runs once per batch with `HANDOFF_CLAIMS=1`.

With claims, `export` builds each job from the page's claim, not from the
manifest, so a page someone asked for (#1055), which no prefetch manifest lists,
is exported like any other. The batch version is the claimed ledger row's, the
language the message's, and the scan, in the fetcher's keep or buffer set, must
match its sidecar's doc id, batch and batch version. A cohort export still takes
its pages from the manifest.

With claims, `export` does not write a ledger row. The job's message is the
queue's own (reason, score, `queued_at`), the journal records the claimed ledger
row, and the export checks under the `ocr-sync` lock that the row is still the
outstanding one (`ocr_local::boinc::adopt`). `collect` then imports against that
row exactly as it does for the node's pages.

### A page that fails for good

Each job gives the volunteer's Tesseract a page deadline (`engine.timeout_secs`, export's
`--timeout-secs`, 1800 s by default and at most 3600). Before 25 Oct it was 600 s, and two dense
1860s pages ran past it on every host they were sent to (#1159). The deadline is part of the job's
identity, so the same page exported with another deadline is a new job and workunit.

When a queue-fed page's workunit fails for good (BOINC's error mask is set: too many error
results, or no output), `collect` marks its journal `failed`. It can't simply go back to its
queue: the copy would be the same job, with the same workunit name, which BOINC already has.
By default the tick stops and names the page and its job. With `--dead-letter-failed` (#1154)
the controller runs the hand-off `dead-letter` step instead (`JOB_IDS`). That step puts a v2 `permanent`
dead letter on `ocr-deadletter`, keeping the message and taking its `error` from the job's journal (the workunit's error mask and the page deadline, #1159), deletes the message with the claim's pop
receipt, and marks the claim `ended: dead_lettered`. The controller records the job in its
state and passes over its journal after that; the journal stays in `dispatch/`. `ocr-sync`
drains the dead letter into the ledger. The flag needs the on-prem host's Message Sender role
on `ocr-deadletter`.

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

## Surya waves

A plan with `"app": "usnm_surya"` keeps a Surya campaign's queue topped up
([#968](https://github.com/tgoodyear/usnewsmap.com/issues/968),
[boinc-surya.md](boinc-surya.md)) so a wave needs no hand-staging. It is a
separate kind of plan: it has no `manifest`, `batch`, `pages`, `source`, `account`
or scan push, takes nothing off the OCR queues, and uses no hand-off job, inventory
or journal. Stage the quality campaign's pages first (`quality-stage`); the plan
cuts the crops from them.

```json
{
  "schema_version": 1,
  "env": "prod",
  "plan": "surya-wave1",
  "app": "usnm_surya",
  "low_watermark": 100,
  "high_watermark": 400,
  "server_status_url": "https://boinc.usnewsmap.com/server_status.php?xml=1",
  "step_command": ["ssh", "-o", "BatchMode=yes", "usnm-boinc-step@boinc.example"],
  "surya": {
    "campaign": "surya-wave1",
    "quality_campaign": "quality-2026",
    "engine_image": "sha256:<64 hex>",
    "engine_llama": "sha256:<64 hex>",
    "engine_weights": "sha256:<64 hex>",
    "timeout_secs": 600,
    "stage_pages": 50,
    "submit_units": 200
  }
}
```

`campaign` and `quality_campaign` are `[a-z0-9-]` names (the `SURYA_CAMPAIGN` and
`QUALITY_CAMPAIGN` of the bridge steps). `timeout_secs` (60 to 14400, default 600),
`stage_pages` (1 to 500, default 50) and `submit_units` (1 to 2000, default 200) are
optional. Both watermarks are required.

One tick reads `usnm_surya`'s unsent count from the server status
(`server_status_url`, `--server-status-url` or `--server-status-file`; a status
without the app stops the tick). When it is above `low_watermark` the tick does
nothing. At or below it, the tick runs `surya-stage-crops` (`QUALITY_CAMPAIGN`, the
three engine pins, `SURYA_MAX=stage_pages`, `SURYA_TIMEOUT_SECS`) and then
`surya-submit` with `SURYA_MAX` the smaller of `submit_units` and half of
`high_watermark` minus the unsent count (the count is results, and every unit makes
two), so the queue never goes past the high mark; with less than two results of
room it runs nothing. A failed step stops the tick. The state file records the last
check and the steps of the last run, with the time. `--drain` runs nothing.

Without `--apply` the controller reads and runs nothing and prints the steps a tick
would run at an empty queue. The host lock and `state.json` are as for the OCR plan,
but a Surya plan takes no Azure lease and its state is not tied to the
configuration's hash, so a later tick may change the watermarks and caps. The engine
pins are held to the campaign, persisted before staging even if the step fails:
staged units keep the pins they were cut with, so a
tick whose pins differ from the campaign's recorded ones refuses to run, and a new
engine goes under a new `campaign` name. Collecting results (`surya-collect`, which needs the assimilator's spool) is still an
operator step. Over ssh the controller's key may set only `SURYA_CAMPAIGN`,
`QUALITY_CAMPAIGN`, the three `SURYA_ENGINE_*` pins, `SURYA_MAX` and
`SURYA_TIMEOUT_SECS` on `surya-stage-crops`, and only `SURYA_CAMPAIGN` and
`SURYA_MAX` on `surya-submit`.

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
