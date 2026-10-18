# Surya 2 on BOINC volunteers

**Status: bounded 23-unit BOINC trial completed, collected and scored.**
This supersedes the historical preflight-only receipts in #848/#852. Do not
widen the sample or change production OCR routing on this evidence. The
maintainer's 18 October 10:32 EDT instruction was “use boinc to orchestrate a
trial”; all inference here was dispatched by BOINC, not a standalone substitute.

## Signed releases and campaign

| App version | Plan class | Production app_version ID | Host | Units |
|---|---|---:|---|---:|
| 1.2 | docker_mt | 54 | CT100 / BOINC host 2, CPU | 3 |
| 1.3 | docker_nvidia_cuda | 55 | CT214 / BOINC host 10, RTX 2060 SUPER | 20 |

Campaign IDs: `probe812-20261018-cpu` and `probe812-20261018-gpu`.
The 23 workunits use exact-host assignment, quorum one, one result, no retries
or fallback hosts. Stock assignment cannot guarantee one CPU and one GPU
replica in the same quorum-two unit; this authorized probe measures each host
separately. Per-result validation remains unchanged. There is no quorum-pair
acceptance: `pairs.jsonl` is absent, as expected for quorum one.

Normal isolated signing and signature-verified ACR builds ch98/ch99 preceded
successful scoped publishers `2yzdzhu` (CPU) and `wu0meav` (GPU). Production DB
rows and all six anonymous signed downloads were verified byte-for-byte.
Retired 1.0/1.1 remain unregistered, immutable and pinned to older `84c1069f…`;
they were not overwritten. Public GHCR serves volunteer workers; private ACR
serves signed app-version/server artifacts. No worker mirror into private ACR
was necessary. The anonymous apps.php login redirect is not app inventory.

Worker and engine pins:

- Worker: `ghcr.io/tgoodyear/usnm-boinc-worker@sha256:e326a2a53818685d068312743f9cd12960bb818a1f33dad47a0cc28ff1d9e01a`.
- Surya: 0.22.1.
- Weights lock: `sha256:557aca6f56e5844007dbb04fe69a638f40e8d55823e89de07a1d4906c291b701`.
- llama image: `sha256:b0baccbe2e00b5d74e63f85f9f023c93d61d0f329475269e1e7002e87eb875ea`.
- CPU signed appver: `sha256:7bd422a344d792d9fc48fcc2e887a6a482399195340471bbe4171ebd7c3a8ae0`.
- GPU signed appver: `sha256:8db09402135495f1c9384c87e344586842632053fc01c57f72050f0168ebd2e2`.

Publication and dispatch used the #529 claim/controller lease and the maintainer's
#856 add-only/per-job policy. Persisted job templates, running quality execution,
OCR routing, scheduler preferences and existing app payloads were not changed
by this trial. All publication locks/leases were released normally.

## Immutable inputs and collection

The GPU leg uses exactly the published ocr-bench v1.0.0
`overproof-en/smoke-10` (10 English crops) and
`reichsanzeiger-de/smoke-10` (10 German line pairs), not full newspaper pages.
The CPU repeats are `pub-overproof-ca-013`, `pub-overproof-ca-017` and
`pub-reichsanzeiger-lines-028`. No transformation or benchmark-release mutation
was performed. All image/reference hashes, upstream manifest and licenses were
checked before staging; references remained private server-side.

- Full 20-input manifest SHA256: `147b3141bb68e0e7ea7ed559f03f9e36a47cdf8d7a087b4071efb11e3844fd82`.
- CPU three-input manifest SHA256: `03d80fc7cc9229bb6cfb143ab6d98a7249db219c113afbdd7e2ef1bdcc760396`.
- Input transport archive SHA256: `e7e6cf9e1469a4a9723c9259ffab5d86149b1ef14647520019fac80ebe3e04d8`.
- English references: CC BY-NC-SA 4.0; German references: CC BY 4.0;
  images: public domain. The maintainer authorized this noncommercial research.

The existing LoC fetch HOLD was preserved without retries. Therefore no
Serbian/Russian/worst-English full-page LoC sample is included; the maintainer
explicitly authorized the pinned-subset-only fallback. No LoC CER or full-page
throughput is inferred from these crops.

Results pass the production script validator and assimilator, are retained under
`surya/seen`, and are exported only after the unchanged `surya check` succeeds
again. #859 fixes authentic randomized `_r<digits>_3` output-directory matching;
otherwise a collector could silently omit real kept results. Failed and empty
outputs are retained, not filtered out as quality outliers.

## Results

All **23/23** BOINC results finished with server state 5, outcome 1, validate
state 1 and exit status 0; all 23 workunits have assimilate state 2. This means
the result protocol is valid, not that every OCR mode succeeded. There are 63
mode records: 60 GPU-assigned modes are `ok`; CPU has two `ok` and one timeout.

| Assignment / group | Mode | n | Mean / median s/input | CER legacy / raw | Peak RSS / GPU MiB | Mode failures / empty-ok |
|---|---|---:|---:|---:|---:|---:|
| CPU / english | tiled | 2 | 4355.655 / 4355.655 | 57.77% / 58.66% | 2125.8 / — | 1 / 0 |
| CPU / german | tiled | 1 | 619.005 / 619.005 | 38.03% / 38.03% | 2071.4 / — | 0 / 0 |
| RTX assigned* / english | tiled | 10 | 154.775 / 154.767 | 44.93% / 45.69% | 1746.2 / 1853 | 0 / 4 |
| RTX assigned* / german | tiled | 10 | 219.580 / 154.442 | 1344.24% / 1351.21% | 2309.3 / 1845 | 0 / 1 |
| RTX assigned* / english | whole | 10 | 20.649 / 19.806 | 44.93% / 45.69% | 1746.3 / 1855 | 0 / 4 |
| RTX assigned* / german | whole | 10 | 86.035 / 19.534 | 32.82% / 39.69% | 2072.2 / 1853 | 0 / 1 |
| RTX assigned* / english | capped | 10 | 20.098 / 20.011 | 44.93% / 45.69% | 1746.5 / 1855 | 0 / 4 |
| RTX assigned* / german | capped | 10 | 85.876 / 18.297 | 54.40% / 61.28% | 2259.3 / 1853 | 0 / 1 |

\*All runs report `device=cpu`; see the device caveat below. CPU English mean
includes the timed-out attempt (5400.317 s) and the successful attempt
(3310.994 s). CPU German took 619.005 s. CPU whole/capped were not scheduled;
they are not missing results. Empty-ok counts are per mode, not additional units.
The tiny CPU repeat sample is not comparable to the complete 20-input GPU leg.

Machine-readable [results.json](results.json) records unrounded values, edit
counts, reference lengths, outcomes and medians. It contains metrics only, not
images, reference text, returned OCR text, credentials or full host diagnostics.

Engine seconds/page are measured per mode, including model setup, not BOINC
wrapper wall time. GPU wrapper wall time for all three modes averages 311.531 s
(median 208.650 s, range 197.399–969.446 s). GPU campaign start includes image
pull/container overhead; do not confuse this with per-mode steady-state latency.
RSS/GPU peaks are maximum sampled values, not claims of complete system peak.

CER is micro-averaged edit distance/reference characters, with both existing
`legacy-ws-v1` and `raw-v1` scorers. Empty successful texts incur deletion errors;
failed/missing outputs count as empty for the allocated-set headline CER.
CER is uncapped: insertion-heavy output can exceed 100%. CPU aggregation uses
only its three allocated IDs from the immutable subset per-sample scorer rows;
the 17 unallocated inputs are not counted as CPU failures. No reference text
normalization beyond the named scorer rule is introduced.

### Device interpretation and limits

Every GPU-assigned mode reports `device=cpu` and `gpu_layers=null`, despite
visible NVIDIA devices, CUDA libraries, GPU allocations around 1.8 GiB, and a
GPU plan/cgroup verified in the kept diagnostics. torch is intentionally the CPU
layout fallback; `torch_cuda=false` alone does not identify the llama OCR device.
The llama device-log sections contain no offload evidence accepted by the
worker parser. The original parser treated any nonempty log lacking an offload
count as CPU; corrected telemetry now reports `none` (unknown) unless an explicit
valid zero-layer count proves CPU or a positive count proves GPU offloading.
This changes future reporting only: signed 1.2/1.3 and these historical raw
results remain immutable. CUDA device discovery alone is not offload proof. Consequently these are **RTX-assigned, CPU-reported** results,
not proven CUDA acceptance and not proof of CPU-only fallback. Diagnosing that
instrumentation/runtime ambiguity requires a separate reviewed follow-up, not
relabeling these measurements or altering this immutable trial.

Successful execution is not successful OCR: five of twenty inputs are empty in
each GPU mode, and the tiled German output includes severe insertion/hallucination.
These data do not justify widening, tuning the production agreement threshold,
reducing resource bounds or changing routing. Quorum-one yields no scientific
CPU/GPU agreement pairs. The independent #810 LoC comparison remains unresolved.

### Artifact reconciliation and reproduction

Private canonical archive: `boinc-handoff/surya/probe-812-20261018/results-rechecked.tar.gz`,
SHA256 `3dfee8f6144fc97bed92c9cdf6a10b71411993e69e00443a50a06608ccded762`.
The original full archive is retained separately at SHA256
`ce78c081a43f121417a3fa4f1e04897a635ad4d4fd0b8e889903fff6602accbc`.
An export audit found its GPU CPU-labeled tiled-English JSONL was zero bytes
although all ten raw result records/text checksums were valid. Both campaigns
were re-exported from the archived validator-kept files using the reviewed
collector and per-result check; only this one JSONL changed. It is now
byte-identical to the independently collected completed GPU-leg export
(SHA256 `6b3e254665224e9fa4a2da1fd2a056cd97291070c95aa236fde222b8a302be83`).
The raw results, campaign inputs and DB records were not changed. The cause of
the truncated server export is not established. Both original and corrected
artifacts plus `export-reconciliation.json` are retained; no silently missing
output stream was scored as if it were the returned OCR.

For each campaign, export with the reviewed helper and the existing validator:

```sh
python3 scripts/boinc/surya-benchmark.py collect \
  --root CAMPAIGN --seen SEEN --out EXPORT --binary USNM_BOINC \
  --intended-device cpu  # use cuda for the GPU-target campaign
usnm-ocr-survey bench-score --release VERIFIED_V1_0_0 \
  --subset overproof-en/smoke-10 --outputs EXPORT/cpu-tiled-english.jsonl \
  --rule legacy-ws-v1 --report SCORE.json
```

Repeat for `raw-v1`, both source subsets and each scheduled mode. Actual
CPU-reported GPU outputs use `cpu-*` streams; empty intended `cuda-*` streams
are diagnostic, not the actual quality denominator. For CPU, aggregate only
the three allocated IDs from each scorer's per-sample rows, summing edits and
reference characters (missing predictions contribute reference-length deletions).
The metrics artifact pins the scorer binary SHA256 and per-subset results.

## Resource cleanup and review receipts

CT100's complete original config was saved before increasing memory from 8192
to 14336 MiB (swap 1024, cores 12 unchanged). BOINC saw 15032385536 bytes;
its unchanged 90% memory allowance admitted the existing 12 GiB CPU bound.
After all three CPU tasks finished, `pct set 100 --memory 8192` ran at
2026-10-18T21:25:31.604529+00:00. The **entire** config equaled the original, not just
three fields. The idle client was then refreshed under the explicit authorization;
at 2026-10-18T21:26:40.457287+00:00 BOINC reported 8589934592 bytes RAM
and 1073741824 bytes swap. No unrelated active task was restarted.
**CT100 reverted: YES**, including the client RAM cache. No claim, controller
lease, local mutex or GPU reservation remains held by this trial.

The GPU reservation was 18:45–20:45 UTC. All twenty GPU tasks finished naturally
by 20:33 UTC; a fresh check at 20:34 UTC showed 0 MiB, 0% and no compute processes.
Explicit handoff: #794 comment 6254821772 and #829 comment 6254822073. No active
unit was aborted, no pending GPU work remains, and no extension was taken.

- #848 merged `ff795cb4`: staging, exact targeting, scoped normal publication.
  Four allowed review requests were exhausted; maintainer subsequently approved
  and merged. Actual post-merge CI and CodeQL passed.
- #852 merged `e160d9ec`: earlier preflight/rollback receipt; two GREEN rounds,
  required CI and post-merge CI passed. Its no-results status is superseded here.
- #859 merged `4208f0eb`: authentic-result collection and scoped ownership walk;
  two GREEN rounds, required CI, post-merge CI 39159390589 and CodeQL 39159390617
  all passed. No runtime deployment was required for the collector export.
- This report PR: final review/CI receipt is recorded on #812 after merge.

#812 remains OPEN for the remaining comparison/acceptance work. #516/#515 remain
historically CLOSED; their state is not a new CUDA acceptance. No production OCR
routing change is proposed. The [runbook](../../boinc-surya.md) documents the
staging/targeting/export commands and unchanged validator contract.
