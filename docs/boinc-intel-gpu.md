# Intel and NVIDIA GPU measurements for usnm_qwen

Plan class `intel_vulkan_8b` runs the same Qwen3-VL-8B Q4_K_M reading as the CUDA and Metal
classes, through the pinned llama.cpp Vulkan build (`ubuntu-vulkan-x64` and `win-vulkan-x64` in
`ocr/boinc/qwen/llama.lock`). It shares the batch-8 workunits of `nvidia_cuda_8b` and
`apple_gpu_8b`, so its readings can join the same quorum
([#786](https://github.com/tgoodyear/usnewsmap.com/issues/786)). The class ships closed
(`min_wu_id=2`, `max_wu_id=1`). An operator opens one audited workunit at a time with
`scripts/boinc/native-qwen-cohort.php --plan-class intel_vulkan_8b`, as for the NVIDIA classes
([app releases](../contrib-compute/docs/app-release.md)).

### Persistent finite NVIDIA/Intel refill

`scripts/boinc/qwen-auto-cohort.py` is an operator-enabled, finite rolling pilot,
not general admission. The accompanying `usnm-qwen-cohort-auto.service` and
`.timer` run one short operation per minute under the normal **nonblocking**
project mutex. No collector, AMD acceptance, client restart or manual feed start
is required. Lock contention exits 75 normally; the 30-second service budget
bounds its own operation, not another owner's workload.

Each refill admits at most **ten contiguous** newly produced native-8B workunits
with two pristine ordinary replicas, quorum/target 2, no assignment, no app-version
binding and zero error/transition/HR flags. The existing pinned Windows/Linux
GPU artifacts, model/engine hashes, generation budgets, distinct-host gate and
validator are not relaxed. Only the already published Linux Intel67/NVIDIA66
1.14 pair is authorized. A superseding release or changed project config closes
the owned window instead of silently adopting a new release.

The controller reads the producer's durable
`/project/qwen/qwen-nonenglish-v1/audit.json` directly, not a human handoff or a
headline count of stale ready work. It binds each WU/job identity to the actual
downloaded job/page, native unit/submitted marker, source-quality page hash,
archive ALTO hash/provenance, and original sampled damage metadata. Existing
Yiddish and Serbian source cohorts are explicit bounded inputs; 1–12 crops and
8192 UTF-8 bytes per crop remain enforced. Selection is the oldest contiguous
eligible prefix, never easiest-page selection. Quality/provenance file hashes
and damage metadata are journaled; this is not a new whole-archive/GGUF readback
or a multilingual accuracy claim.

A window advances only when it has no **currently unsent replica eligible for
either pilot host**. Completed/inconclusive replicas already run by both hosts
are not fresh work. Pending normal peers/validation are left untouched; no
canonical/status/assignment row is rewritten, no fake third replica is made,
and no collector wait is added. Normal transitioner peers can remain pending
outside an advanced finite window: report that unresolved inventory honestly,
not as independently accepted jobs. An active AMD-first reserved ID is excluded;
later disjoint NVIDIA/Intel windows may continue on either side of it.

Before installation, coordinate #529 with exact source SHA, config/helper/initial
policy SHA, unique controller ID, minimum new WU, budget, deadline and rollback.
Install the reviewed controller and helper at
`/project/ops/qwen-cohort-auto/{qwen-auto-cohort.py,native-qwen-cohort.php}` on the
persistent project bind. Install the two reviewed units in the host's normal
systemd directory, and create `/etc/usnm-boinc/qwen-cohort-auto.env`:

```ini
HELPER_SHA256=REVIEWED_HELPER_SHA
CONFIG_SHA256=REVIEWED_CURRENT_CONFIG_SHA
INITIAL_POLICY_SHA256=REVIEWED_CURRENT_POLICY_SHA
CONTROLLER_ID=UNIQUE_32_HEX
MINIMUM_WORKUNIT=FIRST_NEW_WU
MAX_ADMISSIONS=500
ENABLED_UNTIL=REVIEWED_UNIX_DEADLINE
```

Default ramp budget is **500 unique new workunits**, not 500 replicas/readings or
10,000 accepted jobs; the hard budget limit is 10,000, and one reviewed enablement
lasts at most 24 hours. Increase only with measured source-eligible input,
storage floors, actual dispatch/completion and scientific-quality evidence.
Run a dry invocation under the same normal mutex before enabling the timer.
Verify the real ordinary timer's immutable input receipt and actual signed
assignments/returns afterwards; a policy change alone is not throughput.

The helper rechecks pristine DB state **inside the policy lock** and accepts an
expected policy SHA, so a stale preview cannot overwrite a concurrent operator.
Durable intent precedes publication; exact interrupted publications reconcile
without duplicate admission. Unknown ownership stops rather than clobbers.
Expiry/budget shutdown needs neither the producer audit nor DB. Rollback is to
disable only this owned timer and use the reviewed shared-cohort `--stop` after
checking the current window still belongs to this controller; in-flight work
continues normally. Raw candidate/generation/quality retention and independent
validation remain mandatory, and high backend agreement is not accuracy.

`usnm_iris` ([#703](https://github.com/tgoodyear/usnewsmap.com/issues/703),
[docs/notes/2026-10-16-boinc-iris-pilot.md](notes/2026-10-16-boinc-iris-pilot.md)) is a separate experiment with a different model
(4B INT4 on OpenVINO). Its readings are not comparable with the 8B quorum.

## How a host is chosen and checked

- **Scheduler.** BOINC finds Intel GPUs through OpenCL, so the class needs `gpu_type intel` and
  an OpenCL device with at least 7,000 MB.
- **Resource type.** Stock BOINC infers an app version's resource from its plan-class name and
  knows Intel only by `intel_gpu`, so it would tell clients this project has no Intel GPU apps
  (`no_rsc_apps intel_gpu`), and they would stop asking for Intel GPU work.
  `scripts/boinc/patch-server-intel-proc-type.py` maps every plan class naming `intel` to the
  Intel GPU. Each such class must therefore have `gpu_type intel` (#736).
- **App.** The app lists the Vulkan devices with its own `llama-server --list-devices` and takes
  the Intel device BOINC assigned. BOINC numbers Intel GPUs among themselves, so device 0 is the
  first Intel device in the list, whatever its `VulkanN` name. The app never takes a software
  device (llvmpipe, lavapipe, SwiftShader) or a device BOINC didn't assign. It runs `llama-server`
  with `--device VulkanN` for that device.
- **No CPU fallback.** With no hardware Vulkan device, or less free device memory than the model
  needs (7,000 MiB), the unit ends `no_gpu` with the reason in `diag.txt` and reads nothing.
- **Slow device probe.** The listing has a 300 s deadline. A probe that answers without a usable
  device ends `no_gpu` at once. A probe that does not answer in time is a cold driver load, not a
  missing GPU: on the RTX 2060 SUPER host the same signed listing took 242 s cold and 9.4 s warm
  under heavy disk load (#1252). The app then writes BOINC's `boinc_temporary_exit` file and
  exits without finishing, as `boinc_temporary_exit()` does. The client restarts the same task
  120 s later, with the driver libraries now in the page cache. A counter in the slot allows
  three such postponements, after which the timeout ends `no_gpu`. The deadline and device
  matching are unchanged.

OpenCL is not a reliable memory gate for integrated GPUs. On Computer 9, OpenCL reported
15,135,723,520 bytes (nearly all system RAM), while Vulkan reported a 7,917 MiB heap with
7,125 MiB free. An integrated GPU with less RAM behind it can pass the scheduler and still be
refused by the app.

## Volunteer setup

- **Linux:** Mesa's Vulkan driver (`mesa-vulkan-drivers` on Debian and Ubuntu) and Intel's
  OpenCL runtime (`intel-opencl-icd`), so that BOINC detects the GPU. `vulkaninfo --summary`
  should list the Intel device.
- **Windows:** The current Intel graphics driver, which includes both Vulkan and OpenCL.
- **Memory:** An integrated GPU's memory is system RAM. One 8B unit used up to 8,609 MiB of GPU
  buffers and process memory on Computer 9. The BOINC scheduler reserves no host memory for GPU
  classes, so on an integrated GPU, run one unit at a time and leave headroom for other work, in
  the project's `app_config.xml`:

  ```xml
  <app_config>
      <app>
          <name>usnm_qwen</name>
          <max_concurrent>1</max_concurrent>
      </app>
  </app_config>
  ```

## Supported hardware

| GPU                                    | Host                              | Status                                                                                              |
| -------------------------------------- | --------------------------------- | --------------------------------------------------------------------------------------------------- |
| Iris Xe (TigerLake-LP GT2), i7-1185G7  | 16 GB RAM, Debian 13, Mesa 25.0.7 | Measured (below)                                                                                    |
| Iris Xe with 8 GB RAM                  |                                   | Not measured; likely refused if its Vulkan heap is half of RAM, as on Computer 9                    |
| NVIDIA GeForce RTX 2060 SUPER, 8 GiB   | Proxmox host, CT 214              | CUDA and Vulkan-on-NVIDIA measured (below)                                                          |
| Arc A-series (A750 8 GB, A770 16 GB)   |                                   | Not measured; no Arc hardware was available for #829                                                |
| AMD GPUs through the same Vulkan build |                                   | [Separate automatic AMD admission](boinc-amd-gpu.md); RX 9070 XT hardware acceptance pending (#885) |

## Measured on Computer 9 (#787)

The production `usnm_qwen` app, with plan class `intel_vulkan_8b`, read the 100 crops of
`legacy/overproof-en-100` from OCR benchmark release `ocr-bench-v1.0.0`. That is the exact input
of the accepted Iris pilot. The crops ran as four 25-page bundle slots, staged with
`usnm-ocr-survey qwen-bench-stage` and collected with `qwen-bench-collect`.

|                     |           Iris pilot (4B INT4, OpenVINO) |                                    `intel_vulkan_8b` (8B Q4_K_M, Vulkan) |
| ------------------- | ---------------------------------------: | -----------------------------------------------------------------------: |
| Crops read          |                                100 / 100 |                                                                100 / 100 |
| CER, `legacy-ws-v1` |                                   1.975% |                                                    2.209% (614 / 27,795) |
| CER, `raw-v1`       |                                          |                                                    2.439% (678 / 27,795) |
| Seconds per crop    |         35.4 mean, model loaded per crop | 15.4 mean wall time with loads; 14.9 p50, 18.0 p95, 18.9 max per reading |
| Model load          |                               every crop |           3.0 to 3.5 s per 25-crop slot with the model cached (9 s cold) |
| Peak memory         | 8,496,205,824 bytes cgroup, largest crop |    8,609 MiB (7,020 GPU shmem + 1,589 anon), plus reclaimable page cache |

Memory was sampled once a second from the host side of the container's cgroup, so GPU shared
allocations are included and the page cache of the memory-mapped model file is counted
separately. The run used llama.cpp b8799, Mesa 25.0.7 and a 12 GiB container.

## Measured on NVIDIA GeForce RTX 2060 SUPER (#829)

On 2026-10-18, the pinned production `usnm-boinc-worker` CUDA image and its `work` path ran
Qwen3-VL-8B-Instruct-Q4_K_M on the 100 verified crops in release `ocr-bench-v1.0.0`, subset
`legacy/overproof-en-100` (subset digest `e99edaf91b5493fcd3eaf309cea9f31dbbeb23438bab865638ab92889cd14d04`).
The crops ran in four sequential 25-crop slots in a private container in CT 214; this did not
submit BOINC work or change shared BOINC, VM, or production configuration.

| Measurement                                      |                                                                                                                                                                                                                                                                                                                                               Result |
| ------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------: |
| Hardware / driver                                |                                                                                                                                                                                                                                                           NVIDIA GeForce RTX 2060 SUPER, Turing, compute capability 7.5, 8,192 MiB; driver 610.57.04 |
| Runtime                                          | `docker_nvidia_cuda_8b`, CUDA 12.8.1; worker image `sha256:6676d88226f050c63ad821c220aacd1205ac9f35c964c02dbdca8b93b031193e`; embedded llama.cpp base `sha256:b0baccbe2e00b5d74e63f85f9f023c93d61d0f329475269e1e7002e87eb875ea` (build 11610, commit `091b6a7fc`); weights `sha256:969266d1c24656977e06a6efacae04deac6d5093c77227fe55ac28975035cddd` |
| Crops read                                       |                                                                                                                                                                                                                                                                                                              100 / 100; all four slots `outcome: ok` |
| CER, `legacy-ws-v1`                              |                                                                                                                                                                                                                                                                                                           2.137% (594 / 27,795 reference characters) |
| CER, `raw-v1`                                    |                                                                                                                                                                                                                                                                                                           2.317% (644 / 27,795 reference characters) |
| Cold first reading after each slot's model start |                                                                                                                                                                                                                                                                                                          70.4–101.0 s (four readings; median 96.0 s) |
| Remaining 96 warm readings                       |                                                                                                                                                                                                                                                                                                                1.599 s p50, 1.898 s p95, 1.973 s max |
| Model load                                       |                                                                                                                                                                                                                                                                                                                         62.3–63.8 s per 25-crop slot |
| Total wall time                                  |                                                                                                                                                                                                                                             781 s for 100 crops; 7.81 s/crop including model loads. Summed crop-read time was 521.6 s (5.22 s/crop). |
| Peak VRAM                                        |                                                                                                                                                                                                                                                                             6,491 MiB of 8,192 MiB sampled; the 8B model fit and all crops completed |
| GPU utilization                                  |                                                                                                                                                                                                                                                                                  18.7% mean; 204 / 776 one-second samples (26.3%) above 0%; 95% peak |
| Board power                                      |                                                                                                                                                                                                                                                                                               66.0 W mean and 184.7 W peak over the benchmark window |
| Estimated electricity                            |                                                                                                                                                                                                                                 $0.26 per 1,000 pages at $0.15/kWh and 12 crops/page; GPU-board draw only, excluding host CPU and other system power |

The first reading in each slot is the first request after a fresh server/model start; no host or
filesystem caches were cleared. Thus “warm” is the other 24 readings in each slot, not a claim of
a cold-disk test. The full wall-time and energy estimate include all four model loads.

**Telemetry caveats.** All four worker result manifests report `device: cpu` and the worker did not
parse a llama-server device line. This is the known device-label defect tracked in [#794](https://github.com/tgoodyear/usnewsmap.com/issues/794), also observed in the NVIDIA runtime receipt on
[#559](https://github.com/tgoodyear/usnewsmap.com/issues/559). The manifests nevertheless record
6,465–6,491 MiB of GPU memory; independent host-side `nvidia-smi` sampling during the run saw up
to 95% GPU utilization and 184.7 W. The NVIDIA activity, CUDA plan class, and full 100-crop
completion are the measurement evidence; the worker's `device` string is not reliable here.

The private container requested 6 GiB memory and 2 CPUs, but CT 214 did not delegate cgroup v2
limits (`memory.max` and `cpu.max` read as `max`), so those limits were not enforced. The
observed worker peak RSS was about 5.23 GiB. The production CUDA image is a different pinned
llama.cpp build from the native Vulkan `llama.lock` route; the similar CER values do **not**
establish CUDA/Vulkan equivalence. The separate native b8799 pair below supplies that backend
comparison; it does not turn this b11610 production-image run into a same-build pair.
No Arc hardware was available, so Arc was not measured.

## Paired native b8799 CUDA / Vulkan on NVIDIA (#786, #829)

On 2026-10-18, the **same RTX 2060 SUPER**, existing driver 610.57.04 and native
`usnm_qwen` 1.6 executable read the same 100 verified `legacy/overproof-en-100` crops through
both backends. **This is Vulkan on NVIDIA, not Intel hardware.** It tests the pinned Vulkan
backend that Intel hosts use without needing the unavailable raw Computer 9 readings.

The Vulkan archive is the exact `ubuntu-vulkan-x64` b8799 asset in `llama.lock`
(SHA-256 `d962a8228c42c84583150e809d654f9ec995cdf9f4e02e37365293642210a309`). Because #849 did
not document a retained raw-output location and its CT scratch was removed, CUDA was run
again in the same window. To avoid a b11610/b8799 confound, this new CUDA run was built from
b8799 source commit `e1a9a6dcbefccb4b864d9385ce8494f2a7f2ffcd`, using CUDA 12.8.1, GCC 13.3.0
and architecture 75. **It is a source-pinned native benchmark build, not the production CUDA
image or a prebuilt Linux CUDA asset from `llama.lock`.** Its banner is `b0-unknown` because
its source archive/build container had no Git metadata; the source archive, resulting binaries,
compiler and commands are independently pinned in the [run receipt](../ocr/bench/results/qwen-b8799-cuda-vulkan-rtx2060s-100-v1/run.json).
Do not substitute its CER for #849's production-image 2.137%, or attribute the timing difference
between those two CUDA runs solely to a backend change.

Both native passes used four identical 25-crop jobs, the same Qwen3-VL-8B Q4_K_M and Q8_0
vision-projector bytes from `weights.lock`, prompt v1, context 4096, GPU layers 99, two CPU
threads, parallel 1, temperature 0, top-p 1, seed 42 and four output tokens per 1,000 square
pixels. The native plan classes were `nvidia_cuda_8b` and `nvidia_vulkan_8b`; the Intel class
would deliberately refuse the NVIDIA device. Both server logs confirm 37/37 layers offloaded
to the GPU. Vulkan used CT 214's existing NVIDIA ICD with no CT or driver configuration changes.

| Measurement                                          |                        Native CUDA b8799 | Pinned Vulkan b8799 on NVIDIA |
| ---------------------------------------------------- | ---------------------------------------: | ----------------------------: |
| Successful crops / missing                           |                                  100 / 0 |                       100 / 0 |
| CER, `legacy-ws-v1`                                  |                    2.256% (627 / 27,795) |         2.241% (623 / 27,795) |
| CER, `raw-v1`                                        |                    2.490% (692 / 27,795) |         2.446% (680 / 27,795) |
| Completed-pass wall / mean per crop, including loads |                       293.29 s / 2.933 s |            423.15 s / 4.232 s |
| All 100 readings, p50 / p95                          |                        1.523 s / 1.874 s |             1.693 s / 2.112 s |
| Remaining 96 warm readings, p50 / p95                |                        1.518 s / 1.874 s |             1.685 s / 2.047 s |
| First read after each model start                    |                            1.630–2.076 s |               34.476–35.585 s |
| Model load per slot                                  | 21.048 s first; 2.658–2.744 s next three |                 4.604–4.908 s |
| Sampled peak physical VRAM, of 8,192 MiB             |                                7,013 MiB |                     6,490 MiB |
| Sampled combined worker + server peak RSS            |                                 5.17 GiB |                      5.31 GiB |
| Sampled GPU utilization, mean / peak                 |                              51.0% / 97% |                   39.3% / 97% |
| Reported GPU-board power, mean / peak                |                        103.6 W / 209.1 W |              83.6 W / 185.9 W |
| Estimated GPU-board electricity per 1,000 pages      |                                    $0.15 |                         $0.18 |

The electricity estimates use 12 crops/page and $0.15/kWh, exclude host power and one-time
build/setup costs, and are not wall-meter measurements. RSS is sampled across the worker and
its server; the native result's `peak_rss_kib` alone describes only the small worker process.
These were native processes, not newly resource-limited inference containers. Filesystem and
private shader caches were not cleared: “first read” means a fresh model/server, not cold disk
or cold shader caches. A first Vulkan attempt yielded to a newly executing unrelated CPU BOINC
task after 75 completed readings; it was discarded, and all 100 Vulkan crops were rerun from
fresh slots after a 15-minute idle recheck. The table uses only the uninterrupted completed
passes, not the discarded attempt, waiting interval, CUDA build or coordination time.

### Pairwise agreement and retained evidence

`usnm-boinc qwen compare`, the implementation invoked by `usnm_qwen_compare`, validated the
unmodified readings/manifests/diagnostics and accepted **all four slots / all 100 crops** at
both the existing 0.5 probe Dice floor and an additional fixed 0.95 benchmark threshold.

- **Exact text equality:** 92/100; after whitespace normalization, 95/100.
- **Word-bag Dice:** mean 0.999792, median 1.0, minimum 0.979167; 99/100 word bags identical.
  The only Dice below 1 was `pub-overproof-ca-053`. None fell below 0.95.
- **Aggregate Vulkan minus CUDA CER:** −0.014391 percentage points (`legacy-ws-v1`, four fewer
  edits) and −0.043173 points (`raw-v1`, 12 fewer edits). These are edit-weighted aggregate
  differences, not averages of per-crop CERs.
- Exact differences were crops 007, 018, 044, 053, 072, 081, 088 and 106 (all with prefix
  `pub-overproof-ca-`). The [per-crop comparison](../ocr/bench/results/qwen-b8799-cuda-vulkan-rtx2060s-100-v1/crop-comparison.jsonl)
  retains exact/whitespace matches, Dice, both CER edit deltas and timings for every crop;
  [pairs.jsonl](../ocr/bench/results/qwen-b8799-cuda-vulkan-rtx2060s-100-v1/pairs.jsonl) and
  `strict-pairs.jsonl` retain the actual validator decisions.

The native jobs were staged once before either run. For this native-only benchmark,
`engine.image` fingerprints the identical native executable and `engine.llama` is the allowed
release-level `llama.lock` digest, **not a claim that CUDA/Vulkan executable bytes are identical
or that either ran inside the production image**. Backend binaries/source are pinned separately
in the receipt. Byte-identical result-manifest copies were given BOINC-safe upload filenames
solely for validator logging; no job, engine or output metadata was rewritten.

[All evidence](../ocr/bench/results/qwen-b8799-cuda-vulkan-rtx2060s-100-v1/) includes both
100-row output files, both scorer rules/reports, timing/power traces, comparison summary,
SHA-256 inventory and `raw-native-results.tar.gz` (the archive is in the `bench-assets` set: `scripts/ops/data-assets.sh --fetch --set bench-assets`). The archive preserves byte-exact native
outputs, server logs, staged job metadata, validator-name copies, build receipts and the
separately labelled discarded Vulkan attempt. For offline replay, extract it into a new local
project directory, then run `usnm-boinc qwen compare --files READINGS RESULT DIAG --files2
READINGS RESULT DIAG --min-dice 0.95 --log replay-pairs.jsonl`, selecting the corresponding
`native/{cuda,vulkan}/JOB/` readings/diagnostics and `native/validator-upload-names/` result
copies. Re-score `cuda-outputs.jsonl` and `vulkan-outputs.jsonl` with `bench-score` against the
verified v1.0.0 release under both rules.

This completes #829's remaining paired-comparison measurement and supplies bounded b8799
backend-agreement evidence for #786. It is **not** cross-vendor Intel validation, an
independent-host production quorum receipt, or permission to widen a cohort. Arc hardware was
unavailable and remains optional for #829; Intel rollout/audited work, Arc characterization
and integrated-GPU host-RAM reservation remain separate #786 follow-ups. The isolated CT
workspace/store/cache were removed and the GPU was explicitly released on #794 before any
PR/review work; shared BOINC processes, preferences, caches and other sessions were untouched.
