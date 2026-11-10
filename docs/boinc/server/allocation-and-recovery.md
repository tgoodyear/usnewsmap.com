# Allocation, recovery and acceptance

How `usnm-boinc export` allocates pages, the journal that tracks each job, and how failed work is recovered. The [batch controller](batch-scheduler.md) runs these commands in prod; this page covers what they guarantee and what an operator does by hand.

## Allocation and recovery

Export filters the manifest while decoding, retaining only selected batch and
page rows (the native decoder still validates all rows). Undetected page
languages use the title's first catalog language from the published reference
snapshot, using the same MARC-to-ISO mapping as native enqueue. BOINC
eligibility is stricter: only titles present in this version's `titles.json` are
eligible. The mutable `catalog/titles.json` cannot make an unpublished title
eligible. Published version IDs must be safe single path segments before any
snapshot objects are read.

Export stages all selected jobs locally before reserving the cohort under one
renewable `ocr-sync` lock. If another owner holds the lock (the hourly
`ocr-sync` pickup holds it for under a minute), export and claim try again every
10 seconds for up to 2 minutes before they fail. It reads the published metadata and native ledger
once, validates the whole cohort, and writes one create-only queued part for
fresh reservations. If any row conflicts, no new reservations are written. It
verifies that the published version has not changed since staging, checks batch,
version and title against that snapshot, and leaves Japanese titles to their
existing pipeline. It refuses pages with prior native work, except an exact
replay of its own reservation. "Fresh cohort" means different page keys without
prior native work. Both outstanding and completed native work make a page
ineligible; changing the plan, image pin, timeout or job ID does not bypass this
check. The ledger remains outstanding across BOINC processing; no Azure queue
lease is held or renewed. Allocate a fresh small cohort, and exclude it from
overlapping seeder plans: the existing seeder may reconsider old outstanding
work under its own policy. This pilot does not add a global long-lived BOINC
ownership field to that policy.

The local journal has `prepared`, `reserved`, `submitting`, `submitted`,
`published`, and `failed` states. Keep it on persistent local storage and back it
up. An advisory lock serializes bridge processes sharing that directory.

- Repeating export completes a reservation interrupted by a crash, without writing
  another request. A lost cohort-write response or partially updated local
  journals replay the exact existing rows and reserve only still-fresh pages.
  The original request time is retained at native microsecond precision.
- Before `create_work`, submission records `submitting`. After an error or crash
  it never automatically submits again. Inspect BOINC for `usnm_JOB_ID` to confirm
  whether it exists, then run `reconcile --root DIR --job ID --found
present` (mark submitted) or `--found absent` (permit submission again).
- The importer snapshots the first accepted bytes before external publication.
  Fixed object names and byte comparisons recover a lost upload response or a
  crash between ALTO and Parquet. Conflicting preexisting bytes stop publication.
  Native import outcomes recover a lost local commit after `ocr-sync` consumed
  the objects. A completed import is never republished from another callback.
- A terminal BOINC error sets `failed`. Preserve the native reservation and
  diagnose it with BOINC's server tools. The controller can dead-letter such a
  page (`--dead-letter-failed`, [a page that fails for good](batch-scheduler.md#a-page-that-fails-for-good)),
  and a reviewed page can be [retried deliberately](#deliberate-terminal-ocr-retry-1159);
  nothing reallocates it automatically. After recovering the same workunit on the server, preserve then
  remove its stale spool error marker and use `reconcile --root DIR --job ID
--found present` to permit collection of its recovered output. A failed job
  cannot be marked absent to create a duplicate workunit. Alternatively arrange
  a new native allocation through the existing pipeline. Do not reset a
  submitted or failed task blindly. There is no supported "re-export this failed
  page with a new BOINC job ID" command. To move the page back to the native
  pool, first ensure BOINC can no longer return or collect it, retain its
  journal and spool for audit, then use the existing [native enqueue/requeue
  procedure](../../operations/onprem-ocr.md#ocr-pickup-ocr-sync-296) with an operator-approved
  `ocr-enqueue --include-outstanding` retry for that page. `ocr-requeue` targets
  native dead letters, not BOINC spool error markers. Never delete ledger
  history to make the page appear fresh.
- Keep `keep/` scans. Do not delete `buffer/` scans from a volunteer claim or task
  completion. The exporter snapshots inputs centrally and never deletes originals.
- No in-page checkpointing: restart can repeat a page. BOINC resource estimates
  are initial bounds; calibrate them on real scans. One-hour maximum
  configurable OCR deadline, default 1800 seconds (#1159), one Tesseract thread, PSM 3,
  DPI 300. The fixed `rsc_fpops_est=1e12` is provisional, so client ETA and
  credit are rough; use `elapsed_ms` from returned manifests across real JPEG
  and JP2 scans to calibrate it. `target_nresults=1` requests one result.
  `max_success_results=3` is a failure ceiling, not a request for three copies;
  stock BOINC cancels unsent copies once it finds a canonical result. These
  retry ceilings remain unchanged pending live recovery measurements. The 1.5
  GiB memory bound is provisional and has only been exercised on a synthetic
  page; peak memory for large historical JP2 decode plus Tesseract is
  unmeasured. Record peak memory and time on the largest intended scans before
  widening enrollment or raising batch volume. A resource limit failure is a
  failed task, not evidence of a usable OCR result.

Result identity and provenance fields and checksums establish transport
consistency, not proof that a volunteer actually ran the declared image. Trusted
friends are a prerequisite; public enrollment requires stronger validation.
Raising quorum alone with `sample_trivial_validator` does not compare OCR
output. A future audit or comparative validator could rerun sampled pages and
compare text, after measuring determinism across supported hosts, or check a
trusted reference set. No hand-transcribed historical accuracy measurement is
implied by the synthetic smoke test. `usnm_ocr` still validates with
`sample_trivial_validator` (`boinc/apps/daemons.xml`); the optional fuzzy
validator from [#444](https://github.com/tgoodyear/usnewsmap.com/pull/444) is
not enabled for it. Publication is gated on [host trust](gold-check.md#host-trust-for-usnm_ocr) instead.

Replicas of one workunit go to different **hosts**
(`one_result_per_host_per_wu`), not different accounts. The project's own hosts
share one account, and with the per-account rule they could take only one of
each quorum-2 pair, so they idled while the second copies waited for another
user. In exchange, a volunteer with several hosts can supply both copies of a unit.
Gold units and spot checks ([Host trust](gold-check.md#host-trust-for-usnm_ocr)) are the guard against that, not
the quorum.

## Deliberate terminal OCR retry (#1159)

`usnm-boinc retry` prepares **one explicitly reviewed single-page retry**, not an
automatic recovery loop. Attempt 0 keeps its existing v1 JSON and job/workunit ID.
Retry contract v2 binds a new attempt to its parent ID. Never edit an old request,
signed app version, journal, assimilator marker or accepted result to force a replay.

Before activation, deploy the reviewed reader to **every** strict journal consumer
(controller, server, bridge, native sync and seeder), and sign/install a matching `usnm_ocr` worker
supporting contract v2. Old workers reject v2; a new CLI alone is insufficient.
Record the supporting BOINC worker's full released image digest (not its separate
Tesseract base image digest) and
BOINC app version number. Existing trust, routing, source budgets and validation
still apply. Do not retry a bundle or bypass its reconciliation.
Submission verifies its registered Dockerfile signature, requires an unambiguous
active app/version/platform/plan mapping, and compares the signed Dockerfile's
immutable worker `FROM` digest against `Job.engine.image`. No Podman socket or
host image store is required. Missing registration or signature proof refuses
submission before its uncertain state.

After confirming BOINC's actual terminal disposition and that native sync has
settled the exact previous native row as a permanent failure:

```sh
usnm-boinc retry --root /private/jobs --spool /private/assimilated \
  --job PARENT_JOB_ID --expected-error-mask 2 \
  --image sha256:SUPPORTING_WORKER_DIGEST --app-version-num SUPPORTING_APP_VERSION \
  --reviewed-reason 'Reviewed terminal 0x2; supporting worker release recorded' \
  --handoff-url /private/handoff \
  --state-file /private/state.json \
  --curated-url /private/curated --reference-url /private/reference
```

A page that ran out of time on every host fails again with the same deadline:
add `--timeout-secs <1 to 3600>` to give the child a longer one (`engine.timeout_secs`,
part of its identity). The parent keeps its own. Without it the child keeps the
parent's, and a resumed retry must give the same value.

Use `--expected-no-output-files` instead of `--expected-error-mask` only for an
actual `_no_output_files` marker. Unknown and zero masks are refused, not treated
as missing output. An error mask is decimal on the CLI (`2` reviews `0x2`).
Accepted/canonical/partial output, changed terminal proof, nonterminal work and a
newer native reservation all require reconciliation, not retry.
Historical imports always veto retry, even if a later dead letter became the
latest outcome. Legacy journals without terminal proof require collection of
the unchanged actual marker; a later collect cannot overwrite the first proof.
The permanent outcome must carry the exact parent message digest; historical
unbound outcomes cannot authorize retry. For queue-fed parents, the exact handoff
claim must be conclusively `dead_lettered`, not lease-lost or still outstanding.

The command prints the new child ID and reserves it without submission. It
preserves the original scan, failed journal and request-day quota attribution.
An identical invocation resumes its durable prepared/reserved child; it never
creates another workunit for that parent. Submit that child through the existing
single-page `submit` path, which pins `--app_version_num` from the retry journal.
Pass `--curated-url` and exactly one of `--state-file` or `--cosmos` again on
submission. For both `submit` and `submit-remote`, the on-prem bridge forwards
host-owned `COSMOS_ENDPOINT` and `CURATED_URL` from `bridge.env`; configure its
existing identity with the native lock/ledger permissions before activation.
Submission holds the native lease while revalidating the exact outstanding
child, verifying the signed worker, and crossing the write-ahead/create-work
boundary. A replaced or settled child is refused without submission.
Uncertain submission still requires authoritative BOINC lookup and explicit
`reconcile`; never delete its journal and repeat. A retry of a failed child needs
a fresh terminal review naming that child.
Preparation validates settlement under the native lease before persisting the
child timestamp, scan and journal, then writes its reservation. An unreserved
prepared child may refresh its timestamp after later settlement; a reserved
replay must still match the exact outstanding native row. The controller retains
the parent/child chain but never automatically exports or submits manual retries.

If that child fails, the existing handoff `claim dead-letter` step publishes the
child's terminal mask and new message timestamp without touching the old claim.
Its private durable payload/sent receipt makes a completed repeat a no-op; lost
send acknowledgement retains at-least-once delivery. Native sync fences duplicate
old-attempt events from subsequent retries.

First acceptance is one reviewed failed-page canary: preserve its original real
mask through collect/journal/dead letter, verify the one new workunit and supporting
app version, collect/import via ordinary validation, verify native settlement,
and record all identities. Code tests or a merged PR do not prove this live gate.

## Live acceptance exercise

Complete one real task on a stock client and server, then test keyboard activity
pause, sleep and reboot, a missing result file, a wrong checksum, an importer
restart and repeat collection. Confirm the wrapper applies `create_args` and all
three outputs are readable by the host client. Include a slot owned by the BOINC
service user with restrictive directory and input modes, not only a directory
owned by the runtime user; confirm `DAC_OVERRIDE` and the file modes work as
intended. Record the server, client, wrapper and image versions and which
platforms were tested. Local native-pipeline tests do not establish BOINC
scheduler or OS-specific idle behavior.

Upstream references:

- [Server setup](https://github.com/BOINC/boinc/wiki/ServerIntro)
- [Docker app cookbook](https://github.com/BOINC/boinc/wiki/Docker-app-cookbook)
- [Code signing](https://github.com/BOINC/boinc/wiki/Code-signing)
- [Batch collector](https://github.com/BOINC/boinc/blob/master/sched/batch_collect_assimilator.cpp)
- [Stock validator result and retry handling](https://github.com/BOINC/boinc/blob/master/sched/validator.cpp)
- [Stock Docker wrapper source](https://github.com/BOINC/boinc/blob/master/samples/docker_wrapper/docker_wrapper.cpp)
- [Stock scheduler plan-class sample](https://github.com/BOINC/boinc/blob/master/sched/plan_class_spec.xml.sample)
