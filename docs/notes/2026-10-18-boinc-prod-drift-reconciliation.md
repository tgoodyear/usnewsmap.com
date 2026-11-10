# BOINC production drift reconciliation

**Status:** record; the Azure server it describes was replaced by the on-prem server on 21 October 2026 · **Date:** October 2026 · **Measured on:** the Azure-hosted prod BOINC stack's live images, jobs and settings, 18 October 2026 · **Code:** `boinc/infra/boinc/main.bicep`, `boinc/project/html/user/work_summary.php` · **Issues:** #464, #777, #856

How the live Azure server's images and settings, built from a commit outside main, were brought back into main, and the rollout gates set then. Moved here from the BOINC database TLS page on 10 November 2026. #777 and #464 are closed.

## Drift reconciliation

The source of the live lifecycle server/bridge (`9323a0d2…` / `b8fb643e…`)
was `09987e4732d631292031a4b82b82be8a194b72a7`, outside main. Its quality
metrics are now reconciled into main without importing unrelated resource-limit,
threading or alignment changes;
this does not adopt the separately proposed damage router (#777).
The additive web image `508a9256…` adds only the already-merged
`boinc/project/html/user/work_summary.php`. Optional `images.web`
preserves this web/daemon binding independently of lifecycle `server`/`bridge`.
A normal image build replaces an existing web override with the new bridge image,
which includes that endpoint and the reconciled metrics. Existing main bundle and
upload limits, linear-space alignment and single-threaded execution are unchanged. Thus the override cannot
silently strand production on the old additive image after a rebuild.

The existing production bridge default is `BRIDGE_STEP=iris-bundle-collect`,
`IRIS_CAMPAIGN=overproof-100-v12`; root Bicep preserves these values and the actual
200-GiB project-share quota. Signed app-version and worker pins are not changed.
The independently operated quality job is **not managed by this root stack**.
Its observed configuration is 2 CPU / 4 GiB, 86400-second timeout, zero retries,
manual parallelism/completion count 1, live lifecycle bridge image, and the
managed-identity-fetched `quality/v1-wave9/run_wave9.py` runner. Its runtime shard
settings are NUM_SHARDS=8, BUNDLE_SIZE=200, MAX_IN_FLIGHT_BUNDLES=45,
MIN_FREE_GB=35.0. Its storage credentials remain private, never committed here.
Provisioning this stack neither starts nor adopts that job. An active quality
execution and its template must remain unchanged; record its deferred image
update in #529 and reconcile it separately after that execution ends naturally.

Per maintainer policy #856, do not impose a global all-idle gate for a scoped
server/TLS rollout. Take the #529 claim and controller lease; take the per-job
start lease and verify that job has no nonterminal execution before updating its
template. Defer only busy job templates, not the entire rollout. Complete copied
project backup/migration/rollback and every-consumer TLS gates still apply;
stock tools/upgrade also invokes database upgrade, so never run it blindly on
production or mistake an image rollout for migration of copied files.

Normal provisioning registers applications with an execution-only override of the
complete bridge template, protected by the existing per-job start lease. Only that
execution receives `BRIDGE_STEP=register-apps`; persisted Iris defaults, secret
references, resources, and project mounts remain unchanged. Legacy quality results
that lack the added image measurements export null, not fabricated numeric zero.

## Mixed-schema collector rollout gate

**Do not deploy the reconciled collector until its assessment-database consumer
supports mixed old and new campaign schemas.** The additional page metrics add
Parquet columns. `read_parquet(..., union_by_name=true)` within each campaign does
not make a positional `UNION ALL` across campaigns compatible: missing columns
must be filled with null by a name-based union across campaign queries as well.

The compatibility change and regression belong to the assessment-database work
in PR #777, not this reconciliation. Before collector rollout, require a merged
fix for both page and crop campaign unions in
`scripts/data/build-quality-assessment-db.py` and a passing regression consolidating
old and new campaign files, with the older campaign's new measurements null.
Record the merged commit and test evidence in the #464/#529 rollout receipt.
Image builds may proceed, but neither the collector nor TLS activation may be
released while this compatibility gate is unmet. This is a code compatibility
gate, not a global all-idle requirement; #856 resource-specific deferrals still
apply to all later production operations.

## Mixed-schema collector rollout gate

**Do not deploy the reconciled collector until its assessment-database consumer
supports mixed old and new campaign schemas.** The additional page metrics add
Parquet columns. `read_parquet(..., union_by_name=true)` within each campaign does
not make a positional `UNION ALL` across campaigns compatible: missing columns
must be filled with null by a name-based union across campaign queries as well.

The compatibility change and regression belong to the assessment-database work
in PR #777, not this reconciliation. Before collector rollout, require a merged
fix for both page and crop campaign unions in
`scripts/data/build-quality-assessment-db.py` and a passing regression consolidating
old and new campaign files, with the older campaign's new measurements null.
Record the merged commit and test evidence in the #464/#529 rollout receipt.
Image builds may proceed, but neither the collector nor TLS activation may be
released while this compatibility gate is unmet. This is a code compatibility
gate, not a global all-idle requirement; #856 resource-specific deferrals still
apply to all later production operations.

## Revision 69 scheduler settings

The maintainer adopted revision 69's live scheduler settings on 2026-10-18:
`next_rpc_delay=3600` and its plan-policy template, sourced from PR #777
(`3bad37f5`). The template SHA-256 is
`c8ec0b9be5aded0f62204669bbe3b5855df04de8227d343d1b33718dc7b6083b`.
The native `mt` and `apple_gpu_8b` minimum client version is 8.2.11; existing
closed GPU workunit bounds remain closed. This adoption imports no router,
collector, assessment database, or other unmerged #777 work. The collector gate
above remains in force.

Production writers must use the documented exchange-account
`boinc-handoff/scheduler/controller.lock` lease. The nonstandard
`boinc-locks/prod-controller.lock` is not a replacement: respect an active lease
there as a live owner, wait briefly, but never acquire or break it. Continue with
the #529 claim, documented controller lease and per-job guards under #856.
