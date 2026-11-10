# Release pipeline (#551)

`boinc/scripts/bootstrap_pipeline.py` runs a BOINC release end to end with the existing scripts: images, the worker, signing, publication and #548's client acceptance, stopping at two reviewed pull requests. Prod's server is on-prem, so prod releases use the [on-prem lifecycle](#on-prem-lifecycle); the [Azure lifecycle](#azure-lifecycle) remains for a `"server": "azure"` environment.

Both lifecycles share these rules:

- It is an **operator-invoked release**. Execution needs `--coordination-claim https://github.com/tgoodyear/usnewsmap.com/issues/529#issuecomment-N`: an acknowledgement of the #529 deploy protocol, not a lock, cost authorization, signing approval or permission to deploy. Code review and CI don't authorize a live restart, signing, dispatch or acceptance.
- Run from a clean checkout of current main. Before changing anything, the pipeline checks that #548's entrypoint `boinc/scripts/acceptance.py` exists on `origin/main` and in the checkout, and runs its offline config validator. A missing entrypoint fails; there is no fallback to registration-only acceptance.
- It never imports private signing keys, unlocks storage, merges a PR, resets Git, reinitializes a partial project, drops a database or deletes a resource.
- Operator inputs go in ignored `target/`, with private permissions. All fields shown are required and unknown fields are rejected. A `usnm_ocr` release signs with `--hold` by default (#1687): the version is published deprecated, so release it with `app-version-hold.php --action release` ([held versions](../apps/app-release.md#held-versions)) before the acceptance phase, which needs it sent. `"release_mode": "outrank-pinned"`, the one optional field, publishes it live instead; it is accepted only for `usnm_ocr` and `usnm_gold`.

## On-prem lifecycle

`--onprem-host HOST --onprem-ct CT` select this lifecycle (`boinc/scripts/onprem_pipeline.py`, #1258). It builds local Podman images and reviews an on-prem release manifest; it never uses the environment's old Azure image pins as deployment or signing identities.

Prerequisites: the existing on-prem MySQL/project setup, configured `server.env` and
`bridge.env`, server unit, Podman, Python 3 and installed `usnm-boinc-step`; root SSH to
the Proxmox host/CT; an already unlocked reachable isolated signing host; GitHub workflow
and reviewed-PR permissions; and independent, dedicated existing-client acceptance inputs
from #548. The pipeline does not install a runtime, initialize/reset a database, unlock a
key, copy private keys, or change topology. A missing prerequisite fails explicitly.

Place the operator configuration and #548 existing-client configuration below ignored
`target/` in a clean checkout of reviewed main. Container configuration:

```json
{
  "env": "dev",
  "app": "usnm_ocr",
  "version": "1.0",
  "platform": "x86_64-pc-linux-gnu",
  "plan_class": "docker",
  "signing_host": "signing.example",
  "acceptance_config": "existing-client.json"
}
```

For a native Linux x86_64 release, add `"kind": "native"`, `"binary": "worker"`,
`"binary_sha256": "<64 lowercase hex>"`, and `"binary_source": "<40 hex source commit>"`.
Supported native identities are `usnm_quality` with empty `plan_class`, or `usnm_qwen`
with `mt`, `nvidia_cuda_2b`, `nvidia_cuda_8b`, `intel_vulkan_8b`,
`nvidia_vulkan_8b`, `nvidia_vulkan_6gb_8b`, `nvidia_vulkan_2b`, or `amd_vulkan_8b`. Supply an already built binary; additional
platforms are not claimed. A native release reviews the
binary checksum/source instead of dispatching a container worker workflow. Native
auxiliary payloads use `"extra_files": {"llama-server": {"path": "llama-server",
"sha256": "<64 hex>", "source_commit": "<40 hex>"}}`; every payload checksum/source
is included in the reviewed manifest and checked before signing. Existing native
plan-specific required-file checks still apply. Container
support remains OCR `docker` and Qwen `docker_mt_2b` / `docker_nvidia_cuda_8b`; do not
select a deprecated production plan just to exercise the lifecycle.

Offline parser check (no SSH, builds, signing, installation or publication):

```sh
python3 boinc/scripts/bootstrap_pipeline.py --config target/release/config.json \
  --state-dir target/release/state --onprem-host pve.example --onprem-ct 230 --dry-run
```

Only in a separately authorized serialized window, invoke/resume the same command:

```sh
python3 boinc/scripts/bootstrap_pipeline.py --config target/release/config.json \
  --state-dir target/release/state --onprem-host pve.example --onprem-ct 230 \
  --authorize-runtime-install \
  --coordination-claim https://github.com/tgoodyear/usnewsmap.com/issues/529#issuecomment-CLAIM
```

The explicit install flag authorizes installation of the **reviewed** `SERVER_IMAGE`
pin and a server-unit restart. The runner shares the installed bridge runner's exclusive
lock. Existing runtime prerequisites are read first; base/server/bridge-bin/bridge are
built locally from an archived reviewed source, using the existing Containerfiles.
Like the installed step runner, these online builds use the CT's host network; they
do not require `/dev/net/tun` or an additional nested network stack inside an LXC.
This does not change the separate, network-disabled signing operation.
Each local image's immutable ID and source label is read back. Partial builds retain
private per-stage image-ID receipts on the CT and reuse only source-verified IDs.
An independent per-operation supervisor inherits that lock and passes it to child
commands, so a lost SSH/helper cannot release exclusion while an operation continues.
Installation waits for the expected container to be running. A submitted systemd job
must finish before the supervisor releases its lock; if systemd cannot confirm this,
exclusion remains held for operator reconciliation rather than permitting overlap.
The rollback receipt and replacement env pin are file- and directory-fsynced before
restart submission; a failed artifact/source check cannot modify the privileged config.

Two independent review checkpoints remain mandatory:

1. A version-1 manifest at
   `boinc/infra/boinc/releases/ENV-onprem-APP-PLAN.json` (`default` for an empty
   plan) records target host/CT, environment, exact release, source commit, local
   `base`/`bin`/`server`/`bridge` IDs, and the public worker digest with successful
   run/attempt/model/source receipt provenance (or native checksum/source).
   Only this manifest may change. The pipeline opens a PR and stops. A maintainer
   must review and merge it with successful CI and no unresolved findings.
   The pipeline never approves or merges its own candidate.
2. After reviewing the first manifest, installation writes `images.bridge` (the bridge
   contains both server and bridge tools) into `/etc/usnm-boinc/server.env`, preserving
   other host settings and a private old-pin rollback receipt. Both the server unit
   and installed step runner consume that file. The running image is checked, not
   just a tag. Signing transfers only this public image to the isolated signing host;
   no Azure pull/pin fallback is allowed. `boinc-app-release.sh --prepare-only
--onprem-manifest PATH --artifact-receipt FILE` emits a versioned signed local
   appver-ID receipt. Its source and runtime must match the candidate.
   A second manifest-only PR adds `images.appver` and `signing_source`.
   Publication waits for its reviewed merge and successful CI too.

Publication uses the installed `usnm-boinc-step --target publish --step publish
--image sha256:...`, never a moving tag. Runtime/env/source identities are rechecked
before publication and acceptance. The pipeline binds #1246's root-local acceptance
transport to the reviewed running bridge ID and the staged reviewed adapter source;
the operator's existing-client command and expected outputs are retained. The final
durable #548 receipt must prove canonical acceptance **and cleanup**, and is checksummed
on subsequent resumes. Fake tests are not live #548/#551 closure evidence.

Exit 3 means a workflow/review checkpoint is pending; rerun the identical command
after it completes. Exit 3 also means another step held the production step lock when
a build, installation or verification was due to start. Nothing ran, so rerun the
identical command later. Publication waits up to five minutes for that lock
(`USNM_BOINC_STEP_LOCK_WAIT_SECS=300`), because the verification before it has just
released the lock and queued steps take it first. If the lock is still held,
publication fails like any other phase. Interrupted/failed phases persist an ambiguous intent and require
operator reconciliation followed by `--approve-retry PHASE`. Completed phases never
repeat automatically. A crash after `server.env` replacement can be reconciled by the
runtime retry, which restarts and reads back the already-reviewed pin. There is no
automatic rollback, key unlock or destructive recovery. Do not delete state to evade
an ambiguous publication or acceptance reservation.
Native binary/auxiliary inputs are rehashed after copying into private staging;
only those verified copies are transferred for signing and appver construction.

## Azure lifecycle

For a `"server": "azure"` environment ([the Azure server](azure.md)). Without `--onprem-host` and `--onprem-ct`, preflight rejects an on-prem or unknown server mode from freshly fetched main, including on resume and `--dry-run`, before any phase. Don't change an environment's server mode to get past this.

### Interface

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
fields above are required, and unknown fields are rejected. The Azure lifecycle
supports Linux x86_64 container releases: `usnm_ocr/docker`,
`usnm_qwen/docker_mt_2b`, and `usnm_qwen/docker_nvidia_cuda_8b`. Other app,
platform and native build contracts fail before deployment instead of silently
selecting the wrong worker.

```sh
python3 boinc/scripts/bootstrap_pipeline.py \
  --config target/bootstrap-config.json --state-dir target/bootstrap-run --dry-run
```

Dry-run fetches main and invokes **only** #548's offline validation, never any
provision/build/sign/publish/client action. It returns the ordered phase plan. It
may create a private local state directory/lock, but does not advance state.
Tests use fake GitHub/Azure/SSH/registry interfaces; Docker is unnecessary.

Execution uses the same command without `--dry-run`, plus `--coordination-claim`, for dev
as well as prod.

### Durable phases

| Phase            | Action and completion gate                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| ---------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `base`           | Invoke `provision-boinc.sh` only when server/bridge are both absent. Existing immutable runtime pins skip this phase; they are never erased to force bootstrap. Existing script owns safe initialization, secret isolation and partial-project refusal.                                                                                                                                                                                                        |
| `server`         | `build-boinc-images.sh` builds server/bridge from a clean, main-pinned isolated worktree. Existing content-key build caches are retained.                                                                                                                                                                                                                                                                                                                      |
| `worker`         | Dispatch existing main-only `boinc-worker.yml` or `boinc-qwen.yml` with a unique durable correlation; resume by that run, require terminal success, download the immutable final-digest receipt for that exact run/attempt/model/source, verify its public GHCR digest, and record that identity. Mutable commit tags are never promotion inputs. Partial reruns resolve the selected model's latest successful job attempt, not an unrelated model's attempt. |
| `review-runtime` | Commit only allowed server/bridge/worker pin changes, push a deterministic candidate branch, create a PR, then block. No deployment until that PR is merged into main, has recorded approval and successful CI, and the entire main settings document still equals the candidate.                                                                                                                                                                              |
| `runtime`        | Provision from a fresh main-pinned checkout with exactly those reviewed settings.                                                                                                                                                                                                                                                                                                                                                                              |
| `sign`           | Invoke `boinc-app-release.sh --prepare-only` against the configured signing host, pinned worker and exact app/version/platform/plan. The existing isolated SSH/network-disabled signing implementation owns keys. Any signing failure, including locked/unreachable storage, is fatal. It is never bypassed or unlocked. No publication occurs here.                                                                                                           |
| `review-app`     | Open the second immutable pin PR; require the same reviewed-main/CI gates for `images.appver`. Only that field may change; the reviewed app-specific worker pin is immutable during signing.                                                                                                                                                                                                                                                                   |
| `publish`        | Run the existing provisioner from the reviewed main settings; immutable app version replay/registration/public-health checks remain its responsibility.                                                                                                                                                                                                                                                                                                        |
| `acceptance`     | Invoke #548 with exact release identity and a private evidence path. Require accepted canonical client execution **and** completed cleanup; registration alone cannot complete this pipeline.                                                                                                                                                                                                                                                                  |

Qwen CPU/GPU digests are reviewed in app/plan-specific manifests under
`boinc/infra/boinc/releases/<env>-<app>-<plan_class>.json`. Their runtime
PR preserves the shared `images.worker` Tesseract default; signing uses the
reviewed app-specific digest explicitly. The runtime review allowlist permits
only server/bridge changes plus that app pin for Qwen, or server/bridge/default
worker pins for OCR.

Candidate PRs deliberately require review outside the automation; the pipeline
requests their initial Copilot review but never approves or auto-merges its own
artifacts. Promotion accepts recorded GitHub approval, an exact-head explicit
approval event from a verified write/maintain/admin reviewer, or an exact-head
Copilot GREEN/BLUE outcome under the repository policy when no aggregate review
requirement is configured. `REVIEW_REQUIRED`/`CHANGES_REQUESTED` cannot be
overridden by fallback approval. Promotion always requires no unresolved review
threads and successful CI. This does not assume GitHub's aggregate review decision
is configured on the repository. An open PR, running worker build
or pending CI is durable `blocked` status (exit **3**). A scheduler can repeat the
same invocation to advance after these gates without by-hand build/sign/publish
handoffs. Successful completion exits **0**; missing inputs, locked signing,
unexpected dirty files, changed reviewed settings and failed operations exit
**1** with fixed diagnostics. Subprocess output is suppressed to prevent secret
leakage. Cancellation forwards SIGTERM and allows bounded child cleanup before
SIGKILL; acceptance receives its explicit 600-second cleanup budget plus a
60-second grace margin, with repeated parent interrupts masked during teardown.
Inspect the upstream build/PR runs through their normal protected
interfaces; this pipeline does not copy their logs into its receipts.

## State and recovery

Base provisioning and server builds compare the entire workspace settings with
freshly fetched main before any operation, even if local settings edits were
committed cleanly. Promotion's field allowlist is anchored to the original
reviewed source, not a later local settings commit.

State must live below this checkout's ignored `target/`. The private directory
contains atomically written `state.json`, a process lock, managed Git worktrees,
and #548 evidence. Keep it between invocations. The successful acceptance receipt
is checksum-bound to state and revalidated even when resuming a completed run;
missing or altered evidence cannot produce another complete claim. State binds
the complete pipeline
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
python3 -m unittest boinc/scripts/test_bootstrap_pipeline.py boinc/scripts/test_onprem_pipeline.py
```

The dedicated `boinc-bootstrap-pipeline` workflow runs only these offline tests
and syntax checks. It has read-only repository permission and no Azure/signing
credentials or publication jobs. The acceptance runner's tests run with the other
script tests in `ci.yml`'s `script-tests` matrix (`scripts/ci/run-unit-tests.py`).

A real end-to-end run needs reviewed operator inputs, an approved
reachable/unlocked signing service, explicit resource authorization, the pin PR
reviews/CI and a live serialized #529 execution window. Its acceptance phase also
needs #548's isolated test account, dedicated client and staged fixture. In the
Azure lifecycle, native and other worker/platform contracts and subscription
bootstrap are outside the supported interface; they fail clearly instead of being
claimed as tested. Live canonical execution/cleanup evidence is not established
by fakes.
