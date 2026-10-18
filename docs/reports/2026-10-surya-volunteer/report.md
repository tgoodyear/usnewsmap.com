# Surya 2 on BOINC volunteers

**Status: pilot not run; this is not a results report.** The project owner's
14 October update says the CPU app version was published, but the GPU app
version was not: production publishing was blocked by a deployment permission
check. No probe units had been staged or submitted. CI's CPU smoke test and
the worker's GPU diagnostics do not demonstrate that a volunteer's Docker or
Podman runtime can pass through an NVIDIA GPU.

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
