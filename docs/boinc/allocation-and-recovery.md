# Allocation, recovery and acceptance

Part of the [BOINC batch pilot guide](README.md).

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
renewable `ocr-sync` lock. It reads the published metadata and native ledger
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
  it never automatically submits again. Inspect BOINC for `usnm_JOB_ID`. Only
  after verifying whether it exists, run `reconcile --root DIR --job ID --found
present` (mark submitted) or `--found absent` (permit submission again).
- The importer snapshots the first accepted bytes before external publication.
  Fixed object names and byte comparisons recover a lost upload response or a
  crash between ALTO and Parquet. Conflicting preexisting bytes stop publication.
  Native import outcomes recover a lost local commit after `ocr-sync` consumed
  the objects. A completed import is never republished from another callback.
- A terminal BOINC error sets `failed`. Preserve the native reservation and
  diagnose it with BOINC's server tools. This first pilot has no automatic
  reallocation. After recovering the same workunit on the server, preserve then
  remove its stale spool error marker and use `reconcile --root DIR --job ID
--found present` to permit collection of its recovered output. A failed job
  cannot be marked absent to create a duplicate workunit. Alternatively arrange
  a new native allocation through the existing pipeline. Do not reset a
  submitted or failed task blindly. There is no supported "re-export this failed
  page with a new BOINC job ID" command. To move the page back to the native
  pool, first ensure BOINC can no longer return or collect it, retain its
  journal and spool for audit, then use the existing [native enqueue/requeue
  procedure](../operations/onprem-ocr.md#ocr-pickup-ocr-sync-296) with an operator-approved
  `ocr-enqueue --include-outstanding` retry for that page. `ocr-requeue` targets
  native dead letters, not BOINC spool error markers. Never delete ledger
  history to make the page appear fresh.
- Keep `keep/` scans. Do not delete `buffer/` scans from a volunteer claim or task
  completion. The exporter snapshots inputs centrally and never deletes originals.
- No in-page checkpointing: restart can repeat a page. BOINC resource estimates
  are initial bounds; calibrate them on real scans. One-hour maximum
  configurable OCR deadline, default 600 seconds, one Tesseract thread, PSM 3,
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
implied by the synthetic smoke test. The optional comparative validator is being
developed separately in
[#444](https://github.com/tgoodyear/usnewsmap.com/pull/444); it is not part of
this pilot's default validator or a claim of public-volunteer readiness.

Replicas of one workunit go to different **hosts**
(`one_result_per_host_per_wu`), not different accounts. The project's own hosts
share one account, and with the per-account rule they could take only one of
each quorum-2 pair, so they idled while the second copies waited for another
user. The cost: a volunteer with several hosts can supply both copies of a unit.
Gold units and spot checks ([Host trust](gold-check.md#host-trust-for-usnm_ocr)) are the guard against that, not
the quorum.

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
