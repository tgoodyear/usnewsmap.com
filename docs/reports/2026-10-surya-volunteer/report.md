# Surya 2 on BOINC volunteers

**Status: pilot not run; this is not a results report.** An authoritative
18 October production DB query found the `usnm_surya` app but no registered app
versions. Existing signed 1.0/1.1 directories point at a different, older worker;
they cannot be overwritten with the requested digest through the immutable
release path. CI's CPU smoke and GPU diagnostics are not volunteer acceptance.

Do not widen the sample or draw a production-routing conclusion from this
document. First publish the GPU app through the normal signed release process,
then run the four-page probe on an attached NVIDIA host, including rootless
Podman with CDI if that is the target runtime. Record host evidence in the
campaign output before treating GPU support as accepted. The existing probe
commands and required digests are in
[docs/boinc-surya.md](../../boinc-surya.md).

## Results

No BOINC volunteer results are available. The fields below must be populated
from `surya-summary` and quality scoring after the pilot; no values are inferred
from the local CI runner or the 8-page engine survey.

| Group | Input mode | Pages | LoC quality | Gold quality | Device | Seconds/page | CPU seconds/page | Peak host/GPU memory | Failures |
|---|---|---:|---:|---:|---|---:|---:|---:|---:|
| Serbian | tiled / whole / capped | — | — | — | — | — | — | — | — |
| Russian | tiled / whole / capped | — | — | — | — | — | — | — | — |
| German Fraktur | tiled / whole / capped | — | — | — | — | — | — | — | — |
| Worst English | tiled / whole / capped | — | — | — | — | — | — | — | — |

The pilot must report per-mode word-bag quality against LoC, gold-set metrics
where page IDs overlap, agreement between the CPU and GPU devices, all outcomes
including timeouts and no-GPU results, and the observed resource peaks. Set the
cross-host Dice threshold from the pilot pairs rather than treating the probe
floor as a production threshold.

## Acceptance blockers

- Publish the signed `docker_nvidia_cuda` app version; the owner reported this
  was blocked by production deployment authorization.
- Run a bounded probe on a real attached NVIDIA GPU host and verify device
  passthrough, including the intended container runtime.
- Add or use the stratified sample and the Surya collection and scoring
  workflow to produce the report above before widening to a few hundred pages
  per group.
- Reassess resource bounds from volunteer measurements. The current shared
  workunit memory bound is 12 GiB to cover the CPU peak measured in round 1;
  the GPU container has an 8 GiB cap.
- Keep production OCR routing unchanged until the report supports a decision
  and the required Surya credit is present on the credits page.

## 18 October BOINC-only follow-up

The maintainer required “use boinc to orchestrate a trial” at 10:32 EDT. No
standalone inference substitutes for the requested BOINC results. The public
`apps.php` check in the earlier blocker receipt was inconclusive: on 18 October
at 15:21 UTC it returned an empty HTTP 302 to login. GHCR is the intentional
public worker registry; ACR stores signed publisher artifacts. Neither this
redirect nor a Surya artifact differing from the last `images.appver` replay pin
proves missing app registration or a registry mismatch.

The normal `boinc-app-release.sh --what-if` succeeded for the requested worker
digest `sha256:e326a2a53818685d068312743f9cd12960bb818a1f33dad47a0cc28ff1d9e01a`:
anonymous pull worked, and the signing dataset was unlocked with key and Podman
present. This is **preflight only**, not signature verification, publication or
execution. Production registration still requires an authoritative DB/payload
check. The first project-host probe used the wrong config path; subsequent ACA
exec transport returned HTTP 429 with Retry-After 600; that hold was honored.

The bounded staging/targeting implementation is described in
[the runbook](../../boinc-surya.md#bounded-immutable-input-probe-812). It preserves
the worker and validator protocol and adds exact-host, quorum-one dispatch with
no retry/fallback hosts. This is an explicitly experimental fallback, not a
GPU/CPU quorum claim. Inputs are exactly pinned before staging, references stay
server-side, and results are exported only after the existing check script.

Local staging verified all 20 real v1.0.0 smoke inputs (10 English crops, 10
German line pairs), using input-manifest SHA256
`147b3141bb68e0e7ea7ed559f03f9e36a47cdf8d7a087b4071efb11e3844fd82`.
These are preparation tests, **not a production campaign or OCR results**.
At 15:20 UTC CT214 showed 0 MiB and 0% GPU use, but the #829 reservation on #794
covers 15:15–17:15 UTC; it was preserved, not preempted. No Surya GPU slot was
claimed. No Surya app was published, BOINC work dispatched, or routing changed
by this follow-up at this report revision. The results table remains unpopulated
until real validator/assimilator outputs exist; #812 remains open for #810 too.


### Authoritative release gate, 18 October 15:35–15:38 UTC

A TLS-protected production DB query returned `usnm_surya` with NULL app-version
fields: the app exists, neither 1.0 nor 1.1 is registered. Read-only payload
inspection then found both immutable version directories already on the share,
with `Dockerfile_1` pointing to the older worker
`ghcr.io/tgoodyear/usnm-boinc-worker@sha256:84c1069f844e0c46e357dca0078ffb1f809340e36523ab5950294f82feeb66b2`.
The real `enable_assignment` setting is 1 and both Surya validator scripts exist.

This is a **signed-version identity collision**, not a worker-registry mismatch.
The normal publisher refuses a different payload in an existing version/platform
directory, even if registration previously failed. The requested worker digest
cannot safely replace either directory. No directories, signed files, registry
tags, app rows, account flags, or live queues were changed. Safe continuation
requires a separately reviewed unused version pair (or maintainer-directed
recovery preserving old signed artifacts), followed by normal signing and scoped
publication. This receipt does not authorize removal/re-signing of old versions,
a full-stack production redeploy, or use of the older worker as the requested
engine. No production campaign ID or per-mode inference measurements exist.

### Authorized unused-version continuation

The maintainer's continuation explicitly authorizes an unused pair, preserving
retired/unregistered 1.0/1.1. Fresh read-only DB and share checks found **1.2 and
1.3 unused**. Normal `boinc-app-release.sh --prepare-only` successfully staged,
signed on the isolated unlocked signing host, and built signature-verified ACR
artifacts under the #529 artifact-only claim:

| Version | Plan class | ACR build | Prepared signed app-version digest |
|---|---|---|---|
| 1.2 | `docker_mt` | ch96 | `sha256:0376b394a0b37833dabc6d8a1d57065bd1a96235ed1dda618e10bb6080342b3b` |
| 1.3 | `docker_nvidia_cuda` | ch97 | `sha256:7f36f808cb5d9bb680e5040cb499540a1bdc2927c6bf7f00cd6603941844d032` |

Both embed the requested public worker `e326a2a5…`; these are **prepared**, not
published/registered or client-accepted. The last replay pin selects 1.3;
publication must use the reviewed execution-only scoped path, not provision a
main-based stack over the divergent live runtime. #829 explicitly released its
GPU reservation at 15:54:37 UTC; no Surya GPU slot is held pending review.

CPU inventory found CT100/host2 has 8 GiB RAM, below the 12 GiB CPU contract;
CT214/host10 has two CPUs, below the CPU plan's four-core minimum. Neither will
be forced into CPU eligibility by changing limits/preferences. Exact-host
NVIDIA-only probing pins 1.3 and its existing 8 GiB GPU bound instead.

Local full-page staging respectfully fetched one Serbian page and its ALTO.
The subsequent Russian JPEG body could not be decoded, so the existing stager
wrote HOLD and stopped without retries; the hold is retained. The full-page
sample will remain explicitly partial unless an existing verified archive copy
can satisfy the blocked input. The 20 immutable smoke inputs are unaffected.

The round-two review caught the deployed config/key symlink layout. Scoped
publication now initializes execution-local credentials and a minimal
registration-only config without replacing live links or scheduler settings.
The corresponding replacement signature-verified artifacts are **ch98 / 1.2**
`sha256:7bd422a344d792d9fc48fcc2e887a6a482399195340471bbe4171ebd7c3a8ae0`
and **ch99 / 1.3**
`sha256:8db09402135495f1c9384c87e344586842632053fc01c57f72050f0168ebd2e2`.
These supersede ch96/ch97 as publication candidates; old artifacts are retained.
No version has yet been registered or dispatched by this continuation.


### Four-round review-policy receipt

PR #848 received four Copilot requests: round one had three findings (fixed),
round two had two findings (fixed), round three returned a service error rather
than a review, and round four identified the stock targeted-result naming shape.
The fourth-round finding is now fixed: collection accepts the scheduler's
numeric PID/timestamp/sequence components after the exact host-qualified prefix,
with a regression using an authentic assignment-style suffix. It still rejects
results from another target host.

All allowed review requests are exhausted; no fifth request was made. The last
completed review is yellow, not a merge authorization. The fixes are pushed for
maintainer disposition, with CI tracked separately. No signed version was
published, no campaign dispatched and no validator/assimilator inference results
were produced. The corrected 1.2/1.3 signed artifacts remain prepared candidates;
retired 1.0/1.1 and all live routing/settings are unchanged. No production claim,
controller/start lease or GPU reservation is held. The observed existing quality
job was preserved, not stopped to manufacture an idle publication window.


### Post-merge authorized execution preflight, 18 October

The maintainer approved continuation at 14:10 EDT; PR #848 merged as
`ff795cb4`, followed by the independent CHURRO report PR #838. This supersedes
the earlier review-disposition gate, not the production serialization protocol.
The actual post-merge runs **both completed successfully**: `39153022399`
(CI, verified at 18:21 UTC) and `39153022490` (CodeQL, verified at 18:22 UTC).
Pre-merge CI success was not substituted for these outcomes.

The corrected, normally signed app-version candidates ch98/ch99 above remain
the intended 1.2/1.3 artifacts. A fresh normal release preflight verified the
public worker pull and unlocked signer/key/Podman. No signing bypass is needed.
An additive **operator tooling** artifact, ch9d,
`sha256:19e22c93f68857dacf225876e5f855b5612b03dee75927e473548d13a4ba94a8`,
preserves the exact live `b8fb643e…` bridge base and adds the reviewed Surya
binary/collector. It is prepared only, not deployed or selected in persisted
settings. Its Surya Rust/contract sources match merged #848; its pinned scorer
is the existing v1.0.0 legacy/raw scorer, predating #738's additional rules.

The locally prepared immutable campaign allocation is 20 GPU smoke inputs plus
three CPU repeats (the first two pinned English smoke members and first German
member), 23 units total if host eligibility and publication pass. The CPU-only
manifest SHA256 is
`03d80fc7cc9229bb6cfb143ab6d98a7249db219c113afbdd7e2ef1bdcc760396`.
The reviewed staging CLI successfully staged all three repeats locally; original
image/reference hashes and the completion marker were independently rechecked.
This is not production staging or inference.
The full-page LoC HOLD remains in place; no retries or subsequent full-page
fetches were made. The maintainer permits the pinned benchmark subset alone.

The original CT100 configuration was recorded before mutation: memory 8192 MiB,
swap 1024 MiB, 12 cores. The explicitly authorized `pct set 100 --memory 12288`
succeeded, and the container saw 12288 MiB, but BOINC retained cached host memory
8589934592 bytes. It then naturally received two unrelated quality tasks at
18:17 UTC. The client and tasks were not restarted, suspended or preempted.
Memory was promptly restored with `pct set 100 --memory 8192`; cgroup and
configuration verification confirmed 8192 MiB, swap 1024 MiB and 12 cores.
**CT100 reverted: yes.** Even after refreshing the client cache, its unchanged
90% BOINC memory preferences leave only 10.8 GiB usable at a 12 GiB host limit,
below the unchanged 12 GiB Surya CPU workunit bound. Pinned upstream
`client/cpu_sched.cpp` uses that bound as the initial estimated RSS for a new app
version with no RSS history; `sched/sched_assign.cpp`'s targeted delivery does
not bypass this client-side gate. No preference or admission bound was silently
changed, and no Surya CPU work was submitted.

The historical pre-[#856](../../boinc-start-guard.md#scope-boundaries-and-per-resource-deploy-gates) idle check found quality execution
`usnm-boinc-quality-caj-prod-t8yp9m0` running since 16:43:04 UTC (24-hour timeout).
The other five lifecycle jobs were idle. Under [#856](../../boinc-start-guard.md#scope-boundaries-and-per-resource-deploy-gates), a running quality execution does not block app-version publication on `usnm-boinc-publish-caj-prod`; no job was stopped or queue changed to manufacture a window.
The #529 request is coordinated with the separately authorized #464 TLS work;
neither request is evidence of a held controller lease. CT214's GPU was idle
with an empty BOINC task list at preflight; no Surya GPU window was claimed while
publication remained pending. An ACA exec HTTP 429 at 18:20 UTC specified
Retry-After 600; that hold is respected rather than bypassing the transport.

At this preflight receipt there is still **no published version, production
campaign ID, validator/assimilator inference output or scored result**. The
unpopulated results table remains intentional. #812 remains open and historical
#516/#515 closure is not new acceptance.
