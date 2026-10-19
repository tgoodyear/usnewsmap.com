# AMD native Qwen Vulkan onboarding

`usnm_qwen` plan class `amd_vulkan_8b` prepares AMD volunteers, including a possible
Radeon RX 9070 XT (16 GB), for **bounded hardware acceptance**, not general dispatch.
No RX 9070 XT was available for this change. Driver compatibility, correctness,
memory use and performance on that card remain unverified in
[#885](https://github.com/tgoodyear/usnewsmap.com/issues/885).

This is a native application, not Docker/Podman, ROCm or a CUDA fallback. Neither
the NVIDIA CUDA Qwen containers nor the Surya CUDA images support AMD through this
change. The existing Intel/NVIDIA measurements are not AMD acceptance evidence.

## Volunteer prerequisites

- **OS/artifact:** x86-64 Linux (`x86_64-pc-linux-gnu`) or x64 Windows
  (`windows_x86_64`) only. ARM Linux and both macOS platforms are excluded.
  The native driver comes from `boinc-quality-native.yml`; its existence alone
  is not a published AMD app version. The operator must assemble and sign the
  matching Vulkan bundle before inviting a canary.
- **Linux driver:** a kernel/firmware stack and real AMD Vulkan ICD supporting
  the card, plus a compatible AMD OpenCL runtime so BOINC detects the AMD/ATI
  resource. Vulkan availability alone does not make a host scheduler-eligible.
  Confirm `vulkaninfo --summary`, `clinfo`, and the BOINC event log all identify
  the real card, including under the BOINC service account. Mesa RADV may provide
  Vulkan, but does not by itself establish working OpenCL detection. No specific
  RX 9070 XT Linux driver/runtime combination has been accepted yet.
- **Windows driver:** a current AMD graphics driver supporting the card, with
  Vulkan and OpenCL available to BOINC. The driver supplies `vulkan-1.dll`; do not
  copy an arbitrary loader into the application bundle. The signed native bundle
  must include its app-local Microsoft CRT and pinned llama runtime DLLs.
- **BOINC:** stable 8.2.11 or newer, reporting an AMD/ATI GPU and at least
  7,000 MB device RAM. Keep ordinary GPU-use preferences under the volunteer's
  control; no account flag, attachment change or preference edit enables this class.
- **Memory/disk:** the 16 GB device capacity is not a guarantee of free memory.
  The app refuses below 7,000 MiB free Vulkan memory. Leave room for the desktop
  and other workloads, run one Qwen task at a time for acceptance, and leave
  adequate host RAM headroom (16 GiB host RAM is a conservative starting point,
  not an AMD measurement). The native 8B cold disk envelope is 18 GiB;
  normal BOINC free-space reserves still apply.

## Operator prerequisites and bounded admission

Use the unchanged `ocr/boinc/qwen/llama.lock` b8799
`ubuntu-vulkan-x64` or `win-vulkan-x64` asset, verified by its pinned SHA-256.
Do not substitute the CPU or CUDA archive. Bundle `llama-server[.exe]`, all its
runtime shared libraries, `llama.lock`, and `weights.lock` as signed native extra
files. Windows requires `ggml-vulkan.dll`, the common llama/ggml/mtmd DLLs,
`ggml-cpu-x64.dll`, `libomp140.x86_64.dll`, and the Microsoft CRT enforced by
`scripts/boinc/install.py`. Keep the complete pinned Linux library set as well.
Verify that the bundled server launches and lists the real AMD Vulkan device
before admission. Retain checksums and the signed publication/readback receipt.

Publish through the normal [reviewed app release path](../contrib-compute/docs/app-release.md)
with `--app usnm_qwen --kind native --plan-class amd_vulkan_8b` and the matching
platform. Stage the unchanged Qwen3-VL-8B Q4_K_M model and Q8_0 projector from
`weights.lock` as digest-qualified sticky inputs, using `usnm_qwen_8b_in`,
native-8B batch 8, 18 GiB disk bound, quorum 2 and target replicas 2.
Use the existing submission CLI and idempotency/provenance safeguards; do not
edit scientific rows or relax independent/distinct-host validation.

The reviewed scheduler policy ships **closed** (`min_wu_id=2`, `max_wu_id=1`).
Only after signed publication and explicit operator authorization, preview and
apply one newly staged, audited, unaccepted workunit with two unsent results:

```sh
php scripts/boinc/native-qwen-cohort.php --project /project/usnmocr \
  --plan-class amd_vulkan_8b --workunit ID
php scripts/boinc/native-qwen-cohort.php --project /project/usnmocr \
  --plan-class amd_vulkan_8b --workunit ID --apply
# Close only AMD; allow in-flight results to report.
php scripts/boinc/native-qwen-cohort.php --project /project/usnmocr \
  --plan-class amd_vulkan_8b --stop --apply
```

AMD has an **independent exact-one-workunit** window; `--workunit-end` cannot
widen it. It is deliberately not a member of the existing `shared_8b`
Intel/NVIDIA pilot. Opening/stopping AMD preserves all old windows; legacy/shared
commands (including an unqualified `--stop`) preserve AMD. Stop AMD explicitly.
Both paths share the policy lock and durable journal. Image policy refresh
introduces AMD closed on upgrade and preserves valid existing windows on restart.

The scheduler uses `<gpu_type>ati</gpu_type><opencl/>` and reserves one AMD GPU
plus two host CPUs. The pinned BOINC source accepts `ati`/`amd` in
[`plan_class_spec.cpp`](https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/sched/plan_class_spec.cpp#L690-L750)
and maps `amd` plan names to `PROC_TYPE_AMD_GPU` in
[`sched_util_basic.cpp`](https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/sched/sched_util_basic.cpp#L313-L332).
No new scheduler source patch is needed. Inference uses Vulkan, not OpenCL.

The app requires an explicit ATI/AMD assignment and nonnegative
`gpu_device_num`. This is BOINC's **per-vendor ordinal**, not the global
`VulkanN` index. It selects that hardware AMD device among AMD/ATI/Radeon or
Advanced Micro Devices names, skipping llvmpipe/lavapipe/SwiftShader and all
other vendors. A missing assignment, invisible assigned device, failed probe or
insufficient free memory ends `no_gpu`; it never selects another adapter or
falls back to CPU.

## Pending actual-device acceptance

On a dedicated, explicitly authorized test client, retain evidence of:

1. Signed AMD app download, exact driver/OS/BOINC versions, archive/model hashes,
   and BOINC's AMD resource assignment.
2. The bundled server's hardware listing and actual `--device VulkanN`
   selection, layer offload/GPU activity, free/peak VRAM and host RAM.
3. Successful native inference, checkpoint/return and output collection; not
   merely device enumeration or app-version registration.
4. Independent validation against another host and canonical collection under
   the existing quorum/distinct-host rules, without accepting failures as reads.
5. Correctness on retained benchmark inputs/readings, elapsed/load/inference
   times, memory evidence, and any mismatch or driver failure.

The acceptance runner uses `gpu: "amd"` in a reviewed fixture and maps BOINC's
`coproc_ati` resource accordingly. Use existing-client mode; Azure ephemeral
acceptance supports CPU/NVIDIA only and must not be repurposed for AMD.
Its scheduler preflight refuses the closed class and never opens cohorts.
Close the AMD window after the canary. Do not claim speed, equivalence or general
RX 9070 XT support until the real-card evidence is reviewed in #885.
