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
Podman/CDI if that is the target runtime. Record host evidence in the campaign
output before treating GPU support as accepted. The existing probe commands
and required digests are in [docs/boinc-surya.md](../../boinc-surya.md).

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
where page IDs overlap, CPU/GPU device agreement, all outcomes including
timeouts and no-GPU results, and the observed resource peaks. Set the
cross-host Dice threshold from the pilot pairs rather than treating the probe
floor as a production threshold.

## Acceptance blockers

- Publish the signed `docker_nvidia_cuda` app version; the owner reported this
  was blocked by production deployment authorization.
- Run a bounded probe on a real attached NVIDIA GPU host and verify device
  passthrough, including the intended container runtime.
- Add/use the stratified sample and Surya collection/scoring workflow to
  produce the report above before widening to a few hundred pages per group.
- Reassess resource bounds from volunteer measurements. The current shared
  workunit memory bound is 12 GiB to cover the CPU peak measured in round 1;
  the GPU container has an 8 GiB cap.
- Keep production OCR routing unchanged until the report supports a decision
  and the required Surya credit is present on the credits page.
