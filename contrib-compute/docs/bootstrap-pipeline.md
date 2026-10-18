# Reviewed BOINC bootstrap pipeline (#551)

`scripts/boinc/bootstrap_pipeline.py` orchestrates the existing BOINC scripts. It
never imports private signing keys, unlocks storage, merges a PR, resets Git,
reinitializes a partial project, drops a database, or deletes a resource. It does
not replace the proposed subscription/dev bootstrap in PR #634: the isolated
BOINC resource group, registry, policy/waiver prerequisites and credentials must
already exist. It never invokes the full application provisioner.

## Interface

Run from a clean, current-main checkout containing this pipeline and **the merged
#548 entrypoint** `scripts/boinc/acceptance.py` (PR #807). Before any resource
mutation the pipeline verifies that entrypoint exists on `origin/main` and invokes
its offline config validator. An unmerged #807 is an explicit failure, not a
fallback to registration-only acceptance or a copy of another branch's code.

Put operator inputs in ignored `target/`, with private directory/file permissions:

```json
{
  "env": "dev",
  "app": "usnm_qwen",
  "version": "2.0",
  "platform": "x86_64-pc-linux-gnu",
  "plan_class": "docker_mt_2b",
  "signing_host": "approved-signing-host.example",
  "acceptance_config": "acceptance-config.json"
}
```

`acceptance_config` is resolved relative to this config, and is exactly #548's
reviewed config format. No passwords, authenticators, key contents or arbitrary
command overrides are accepted by this pipeline's own config. #548 owns private
account configuration and client cleanup; do not put that config in Git. All
fields above are required, and unknown fields are rejected. Initial support is
Linux x86_64 container releases: `usnm_ocr/docker`,
`usnm_qwen/docker_mt_2b`, and `usnm_qwen/docker_nvidia_cuda_8b`. Other app,
platform and native build contracts fail before deployment instead of silently
selecting the wrong worker.

```sh
python3 scripts/boinc/bootstrap_pipeline.py \
  --config target/bootstrap-config.json --state-dir target/bootstrap-run --dry-run
```

Dry-run fetches main and invokes **only** #548's offline validation, never any
provision/build/sign/publish/client action. It returns the ordered phase plan. It
may create a private local state directory/lock, but does not advance state.
Tests use fake GitHub/Azure/SSH/registry interfaces; Docker is unnecessary.

Execution uses the same command without `--dry-run`, plus
`--coordination-claim https://github.com/tgoodyear/usnewsmap.com/issues/529#issuecomment-N`.
A claim is required for both environments to prevent accidental dev/prod
concurrency. It is an acknowledgement, **not** an automatic distributed lock,
resource/cost authorization, signing approval or permission to deploy. The
operator must hold the current #529 deployment-coordination protocol and all
necessary approvals for the entire execution window. No live execution is part
of this implementation's acceptance evidence.

## Durable phases

| Phase            | Action and completion gate                                                                                                                                                                                                                                                                                                                           |
| ---------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `base`           | Invoke `provision-boinc.sh` only when server/bridge are both absent. Existing immutable runtime pins skip this phase; they are never erased to force bootstrap. Existing script owns safe initialization, secret isolation and partial-project refusal.                                                                                              |
| `server`         | `build-boinc-images.sh` builds server/bridge from a clean, main-pinned isolated worktree. Existing content-key build caches are retained.                                                                                                                                                                                                            |
| `worker`         | Dispatch existing main-only `boinc-worker.yml` or `boinc-qwen.yml` with a unique durable correlation; resume by that run, require terminal success, resolve and checksum the public GHCR manifest, record its immutable digest. CPU/GPU Qwen tags are selected explicitly.                                                                           |
| `review-runtime` | Commit only allowed server/bridge/worker pin changes, push a deterministic candidate branch, create a PR, then block. No deployment until that PR is merged into main, has recorded approval and successful CI, and the entire main settings document still equals the candidate.                                                                    |
| `runtime`        | Provision from a fresh main-pinned checkout with exactly those reviewed settings.                                                                                                                                                                                                                                                                    |
| `sign`           | Invoke `boinc-app-release.sh --prepare-only` against the configured signing host, pinned worker and exact app/version/platform/plan. The existing isolated SSH/network-disabled signing implementation owns keys. Any signing failure, including locked/unreachable storage, is fatal. It is never bypassed or unlocked. No publication occurs here. |
| `review-app`     | Open the second immutable pin PR; require the same reviewed-main/CI gates for `images.appver`. Only that field may change.                                                                                                                                                                                                                           |
| `publish`        | Run the existing provisioner from the reviewed main settings; immutable app version replay/registration/public-health checks remain its responsibility.                                                                                                                                                                                              |
| `acceptance`     | Invoke #548 with exact release identity and a private evidence path. Require accepted canonical client execution **and** completed cleanup; registration alone cannot complete this pipeline.                                                                                                                                                        |

Candidate PRs deliberately require review outside the automation; the pipeline
never approves or auto-merges its own artifacts. An open PR, running worker build
or pending CI is durable `blocked` status (exit **3**). A scheduler can repeat the
same invocation to advance after these gates without by-hand build/sign/publish
handoffs. Successful completion exits **0**; missing inputs, locked signing,
unexpected dirty files, changed reviewed settings and failed operations exit
**1** with fixed diagnostics. Subprocess output is suppressed to prevent secret
leakage. Inspect the upstream build/PR runs through their normal protected
interfaces; this pipeline does not copy their logs into its receipts.

## State and recovery

State must live below this checkout's ignored `target/`. The private directory
contains atomically written `state.json`, a process lock, managed Git worktrees,
and #548 evidence. Keep it between invocations. The successful acceptance receipt
is checksum-bound to state and revalidated even when resuming a completed run;
missing or altered evidence cannot produce another complete claim. State binds the complete pipeline
config and acceptance-config checksum; changing either cannot silently resume an
old run. It records phase intent **before** side effects and commits successful
completion **after** gates pass. A completed phase is not rerun. The state lock
serializes the same run locally; existing BOINC provision/signing locks and #529
remain required across runs/hosts.

Interrupted/failed phases are ambiguous and cannot automatically replay. After
reconciling their external effects and approving that exact retry, supply
`--approve-retry PHASE`. This is permission to retry the existing non-destructive
phase, not permission to delete/reset/reinitialize anything. A dirty build or
signing workspace still fails the underlying clean-tree gate, and a prior
acceptance evidence file still fails #548's exclusive-create protection. Those
cases need separately reviewed recovery; the pipeline does not remove evidence,
wipe partial trees or guess that a timed-out side effect never happened. Worker
dispatch intent without a visible matching run similarly blocks rather than
publishing a duplicate. No automatic destructive-recovery mode exists.

Review waits and successful phases resume automatically. Stale/main-divergent
candidates fail closed instead of deploying historical pins. Never edit state to
mark phases complete or bypass a review/signing/cleanup gate. Retain run state,
PR links and acceptance receipts for audit; resource teardown is not inferred
from deleting local state.

## Validation and outstanding operational acceptance

```sh
python3 -m unittest scripts/boinc/test_bootstrap_pipeline.py
```

The dedicated `boinc-bootstrap-pipeline` workflow runs only these offline tests
and syntax checks. It has read-only repository permission and no Azure/signing
credentials or publication jobs. No files owned by PR #807 or #634 are changed.

A real end-to-end run is still gated on #807 merging, reviewed operator inputs,
an approved reachable/unlocked signing service, explicit resource authorization,
required pin PR reviews/CI and a live serialized #529 execution window. Native
and additional worker/platform contracts and subscription bootstrap are outside
this initial supported interface; they fail clearly instead of being claimed as
tested. Live canonical execution/cleanup evidence is not established by fakes.
