# Taking Azure out of the hand-off's critical path (#1367)

Status: proposal for review. Nothing here is implemented. The retry change in
#1883 is the stopgap that ships first.

## Problem

The controller (CT 211) and the BOINC server (CT 230) are both on the home
LAN, but a tick still depends on Azure Blob in three places:

1. **Spool-out and collect** put and get objects in `boinc-handoff`
   (`spool/...`, `alto/...`, `dispatch/...`, `gold/trust.json`) through
   `handoff::put_spool`, `handoff::put` and the get functions in
   `crates/usnm-boinc/src/handoff.rs`. On 12 Nov (about 8:45 PM ET) and 13 Nov
   (2:08 AM ET) a connect timeout on one of these PUTs paused native-feed-4;
   host 13 sat idle until a manual `--resume-native`. The NUC logged no NIC
   fault, so the cause is upstream of the LAN.
2. **The controller lock.** `Transport.lock` in `boinc/scripts/batch_scheduler.py`
   takes an infinite Azure blob lease on `scheduler/controller.lock`
   (`az storage blob lease acquire --lease-duration -1`). A killed tick leaves
   it held forever. It went stale twice on 12 Nov (the pve-big power cycle,
   then a CT 211 stop) and every plan's tick failed until it was broken by
   hand. #1872 added a boot-time breaker as a workaround (see #1698).
3. **Azure CLI calls in `Transport`** for inventory, journals and
   `exists` checks, unless the plan sets `handoff_root`.

A WAN blip should not be able to stop a LAN-only pipeline, and a stale lock
should not need a person.

## What already exists

- `handoff_root` ("Local handoff journals" in [onprem.md](onprem.md#local-handoff-journals)):
  the controller reads inventory and journals from a local directory mounted at
  `/handoff-state`, takes `fcntl.flock` on `.controller.lock` instead of the
  lease, and refuses Azure handoff reads. Every handoff step must be routed
  on-prem and carries `EXPECTED_HANDOFF_URL`. It requires the controller and
  the root on one filesystem (cross-host mounts are not qualified).
- `--onprem-handoff claim|collect|export` sends those steps over
  `step_command` (ssh to `usnm-boinc-step` on CT 230).
- `LocalStore` in `usnm-store` implements the same `ObjectStore` trait as
  `BlobStore`, so `handoff::put_spool` and friends work unchanged on a local
  directory.

So most of the transport already exists. The gaps are: the lock has no expiry
and `handoff_root` is not qualified across CT 211 and CT 230; spool-out and
`alto/` writes still resolve `HANDOFF_URL` to Azure unless the root is set; and
nothing copies the handoff to Azure afterwards.

## Design

### 1. Every step over the existing path

- The authoritative handoff root is a directory on CT 230 (the BOINC host),
  `/srv/usnm-boinc/handoff-state`, mode `0700`, owned by the step runner's
  service user. `HANDOFF_URL=/handoff-state` in `handoff.env`, as the existing
  local mode specifies.
- Spool-out, collect, claim, release, dead-letter, export and the inventory all
  already run as `usnm-boinc-step` on CT 230. With the root local they read and
  write it through `LocalStore`, with no network.
- The controller on CT 211 stops reading the root directly. Where it reads
  inventory or journals now (`Transport.blob`, `_local_object`), it asks a
  read step over the same ssh runner (`--target handoff --step inventory`,
  `--step read-object` with a validated relative path and a size cap). This
  keeps the "no cross-host filesystem" rule and adds one ssh round trip per
  read; the inventory is one read per tick and journals are listed in the
  existing batched steps.
- The on-disk spool layout is the contract and does not change:
  `results/<batch>/<wu>__file_<logical>`, `<wu>_error`,
  `<wu>_no_output_files`, plus the objects `put_spool` and `collect` already
  write. The Cloudflare BOINC server's `usnm-boinc cloud pull` (#1882) writes
  that layout, so a future cloud-hosted BOINC server plugs in on the
  BOINC-server side and no handoff interface is added.

Alternative considered: move the controller to CT 230 so the existing flock
and `handoff_root` work as qualified. It removes the read steps but
co-locates the controller with the BOINC database host and the CT 211 scan
fetch/push roles (CT 211 also holds `ocr-out` and the priority controller).
Not recommended unless the read-step cost proves too high.

### 2. A local expiring lock with a heartbeat

Replace the Azure lease with a lock held on CT 230, in the handoff root, taken
and renewed through the step runner.

- **Record**, JSON in `scheduler/controller.lock.json`, written atomically
  (temp file, fsync, rename):
  `holder` (random id plus plan name), `host`, `pid`, `generation`,
  `acquired_at`, `renewed_at`, `expires_at`.
- **Acquire** (`lock-acquire` step): under a short kernel `flock` on a sidecar
  file, take it if absent or expired; otherwise fail with the holder and
  expiry in the error. Each acquire increments `generation`.
- **Heartbeat.** The controller renews every 30 s (`lock-renew`, checking
  holder and generation) with a TTL of 5 minutes. TTL is about missed
  heartbeats, not tick length: a refill tick runs 11 to 13 minutes and stays
  held. A failed renewal after the TTL has passed ends the tick before its next
  mutating step.
- **Release** on normal exit and in the tick's finally path (`lock-release`,
  holder-checked). A killed tick expires by itself within the TTL; the
  boot-time breaker of #1872 is no longer needed for correctness.
- **Fencing.** Each mutating handoff step gets `LOCK_HOLDER` and
  `LOCK_GENERATION` and refuses to run if the stored record has moved on to
  another holder or generation. This covers a controller that was paused
  (VM stop, ssh stall), expired, and then resumed beside its successor. The
  existing `/run/usnm-boinc-handoff.lock` still serialises steps on CT 230.
- **Diagnosable.** `usnm-boinc-step --target handoff --step lock-status` prints
  holder, host, pid, generation, acquired, renewed and expiry (in the tick
  summary too, and in US Eastern for operators). A stale lock reads as
  "expired 14 min ago, holder X on host Y" instead of a lease error.
- **LAN only.** No lock dependency leaves the LAN. This also drops the
  Surya-plan special case in `Transport.lock`: each plan keeps its own tick
  lock, and the controller lock record is per environment.
- The clock is CT 230's (`expires_at` is written and compared there), so CT 211
  and CT 230 clock skew cannot shorten a lease. The renew step returns the
  server-side expiry.

### 3. An optional, asynchronous R2 backup

- A separate timer on CT 230 (not a tick step) copies the handoff root's
  durable objects (journals, spool, trust, alto) one way to a Cloudflare R2
  bucket, using a bucket-scoped R2 API token (the repo's R2 docs pattern).
  Never the account-wide keys.
- Failure of the copy raises a feed-health alert and never fails or delays a
  tick, a claim or a collect. Nothing on-prem reads the copy back.
- Durable Objects or a Worker API are not
  proposed for the handoff: the BOINC Workers session advised against putting a
  cloud API in the controller's hot path, since it recreates this failure, and
  there is no switch date for the cloud server.
- The copy is a rollback and disaster-recovery aid, consistent with the
  maintainer's 11 Nov plan (the Azure failover spare receives a one-way push).

## Migration

Each phase has one writer at a time. The batch scheduler's dry-run output shows
`onprem_handoff` and the lock mode.

1. **Tools, no behaviour change.** Add the lock steps, `read-object`, fencing
   arguments and `lock-status` behind a plan flag (`--local-lock`), default
   off. Tests: two acquirers (second fails with the holder named), expiry
   takeover with a lower-generation renew refused, kill during a tick then
   fresh acquire after the TTL, heartbeat loss ends the tick, `LocalStore`
   spool-out/collect with injected failures, duplicate-free replay.
2. **Seed.** With the controller timers stopped and the current tick finished,
   copy the Azure `boinc-handoff` state to the CT 230 root (dispatch requests,
   journals including live pop receipts, claims, spools, trust, inventory,
   alto), verify counts and hashes, and keep Azure read-only. This reuses the
   11 Nov plan's `azcopy sync` receipts for `boinc-handoff`.
3. **Cutover on one plan first** (native-feed-4, which already runs
   `--onprem-handoff` and has been the one pausing). Set `HANDOFF_URL` to the
   local root, run the controller with `--local-lock`, run a real
   claim, export, submit, result, collect, clean tick, restart and replay.
   Acceptance: zero requests to `stusnmoffapzy` from the tick.
4. **Remaining plans**, then the Azure CLI exists/lease code paths are removed
   from `Transport` and the boot-time breaker is retired.
5. **Backup timer** enabled last.

No cutover is done by the code PRs; each phase is a separate operator action
and needs the usual controller pause (stop timer, wait for the tick to exit,
verify no writer remains).

## Rollback

- Until phase 4 the Azure objects are untouched and read-only, so rolling back
  is: stop the timer and wait for the tick to end; stop the backup timer;
  restore `HANDOFF_URL` to the Azure URL and drop `--local-lock`; copy the
  changed handoff state back as a complete set, inputs and retained results
  before journals (the reverse of the phase 2 copy, with the same hash check
  and the verification in the review follow-ups), preserving job ids and stage history; start the
  timer. Never run one writer on each copy.
- After a pull has already settled a page (dead-letter, collect), never restore
  its submitted journal; reconcile from the receipt, as the scheduler doc
  already says.
- The expiring lock is a flag: turning it off returns to the kernel flock plus
  the Azure lease with the boot-time breaker still in place until phase 4.

## Out of scope

- Queue backend, Cosmos and lake storage (the rest of #1367/#895).
- Changing spool names, layout or the BOINC MySQL read path.
- SAS tokens or shared keys for any remaining Azure access.

## Review follow-ups

- **Held pages.** `Transport.publish_held` writes and deletes `scheduler/held-pages/` parts, so it
  is a remote, fenced step too (`write-held-pages`, JSON on stdin, other plans' parts untouched).
  The controller's digest marker is written only after the step succeeds.
- **Heartbeat during long steps.** The `lock-*` steps do not take `/run/usnm-boinc-handoff.lock`.
  They take only a short flock on the lock record's sidecar, and the heartbeat runs on its own
  thread beside the controller's blocking step calls, so a collect longer than the TTL does not
  starve renewal.
- **Fencing.** Every controller step that writes the root or dispatches work carries the holder and
  generation: claim, release, dead-letter, collect, export, `write-held-pages`, `bridge spool-out`,
  `bridge batch-inventory` (it writes `scheduler/inventory.json`) and `bridge submit-remote`. The
  host checks at entry and again after waiting for the step lock, immediately before launching, and
  each step's commit (the rename that publishes an object) happens inside the step lock, so a step
  that waited out its lease is refused rather than run. The controller also stops dispatching once its
  own conservative renewal deadline has passed, and caps each renewal attempt by the time left on
  the lease.
- **Takeover drains old mutators.** `lock-acquire` of a new generation, after it sees an expired
  record, takes the step lock once before returning. A mutator that already passed its check holds
  that lock until it finishes (every step has a hard timeout well under the TTL), so the successor
  starts only after the old holder's last write has landed or the step was killed. A killed step
  leaves temp files only, because objects are published by atomic rename.
- **Offsite copy consistency.** The R2 copy is a one-way disaster-recovery aid. A collect writes
  kept results before its settled journal, so copying the live root can hold a journal without its
  result. The backup timer therefore stages a local snapshot first: under the step lock it takes a
  filesystem snapshot (or hard-link copy) of the root, which is quick, then releases the lock and
  uploads the snapshot to R2 outside any writer lock. A completion manifest (object list, sizes,
  hashes, generation) is uploaded last; a copy without a manifest is not a recovery point.
- **Rollback copies the complete root.** Reverse copy covers inputs for newly exported jobs,
  retained results before the journals that depend on them, and the journals themselves, with all
  writers stopped, then verifies (every settled journal has its retained results, every exported
  job has its inputs) before switching back to Azure.

## Decisions

The maintainer decided the earlier open questions (PR #1884 comment):

1. The controller stays on CT 211 and reads journals and inventory through
   new ssh read steps on CT 230.
2. The lock uses a 30 s heartbeat and a 5 min TTL.
3. The optional offsite async copy goes to Cloudflare R2, not Azure. R2 does
   not take Entra ID or managed identity, so the copy uses the scoped R2 API
   token pattern from the repo's R2 docs, never shared account keys.

Still noted: `put_new` is not retried in #1883 because a retried create reads
as "already exists". That is moot after this change, but applies to any step
that stays on Azure meanwhile.
