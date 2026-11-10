# AMD GPUs for usnm_qwen

`usnm_qwen` plan class `amd_vulkan_8b` reads Qwen3-VL-8B Q4_K_M crops on AMD GPUs through the pinned llama.cpp Vulkan build, as a native app for x86-64 Linux and x64 Windows. It is not Docker or Podman, ROCm or a CUDA fallback. Measurements and progress are in [#885](https://github.com/tgoodyear/usnewsmap.com/issues/885), which is open.

AMD has had general admission since 1 November 2026: `amd_vulkan_8b` has no workunit bounds, and compatible hosts get ordinary native-8B batch-8 work through normal BOINC scheduling, without per-volunteer cohorts or host approval. GPU, memory and validation checks are unchanged. The maintainer approved it after a dry run; it followed the pause in [home-infra#81](https://github.com/tgoodyear/home-infra/issues/81), which came after signed Windows 1.10 failed before inference on an RX 9070 XT.

The acceptance evidence is one ordinary canary. On workunit 23296 (30 October 2026), signed Windows 1.14 on an RX 9070 XT read 12 of 12 crops, an Apple host's reading agreed under the strict validator, and the AMD result became canonical. Workunit 21128 (1.11, 27 October) stayed inconclusive ([validation note](../../notes/2026-11-01-qwen-cross-vendor-validation.md)). That is one validated unit on one card, not an accuracy or equivalence claim. Other AMD cards and Linux drivers haven't run it, and correctness, memory use and speed are unmeasured.

The published AMD versions read request version 1 only. Request versions 2 to 4 ([OCR quality](quality.md)) need a newer signed AMD release; until then the refill controller closes its window rather than admit jobs an open AMD class can't read ([Intel and NVIDIA GPUs](intel-gpu.md#persistent-finite-nvidiaintel-refill)).

| Versions                                                      | Published       | Record                                                                                                                     |
| ------------------------------------------------------------- | --------------- | -------------------------------------------------------------------------------------------------------------------------- |
| 1.7 Linux, 1.8 Windows                                        | 19 October 2026 | [First AMD Vulkan release](../../notes/2026-10-19-amd-vulkan-release.md)                                                   |
| 1.9 Linux (app version 61), 1.10 Windows (app version 62)     | 26 October 2026 | [GPU release](../../notes/2026-10-26-boinc-gpu-release.md): bounded failure diagnostics and the Windows embedded-lock fix |
| 1.11 (workunit 21128), 1.14 Windows (workunit 23296)          | by 30 October   | the canaries above                                                                                                         |

Which AMD versions are live now is UNVERIFIED: anonymous `apps.php` redirects to the login page, and the repository records only the 6 GB NVIDIA release (`boinc/infra/boinc/releases/`).

## Volunteer prerequisites

- **Platform:** x86-64 Linux (`x86_64-pc-linux-gnu`) or x64 Windows (`windows_x86_64`). ARM Linux and macOS have no AMD version.
- **Linux driver:** a kernel and firmware stack and a real AMD Vulkan ICD supporting the card, plus an AMD OpenCL runtime so BOINC detects the AMD/ATI GPU; Vulkan alone doesn't make a host eligible. `vulkaninfo --summary`, `clinfo` and the BOINC event log should all show the card, including under the BOINC service account. Mesa RADV can provide Vulkan but not OpenCL detection. No RX 9070 XT Linux driver combination has been accepted yet.
- **Windows driver:** a current AMD graphics driver supporting the card, with Vulkan and OpenCL. The driver supplies `vulkan-1.dll`.
- **BOINC:** 8.2.11 or newer, reporting an AMD/ATI GPU with at least 7,000 MB. No account flag, attachment change or preference enables the class; ordinary GPU-use preferences apply.
- **Memory and disk:** the app refuses below 7,000 MiB of free Vulkan memory, so leave room for the desktop and other work, and run one Qwen task at a time. 16 GiB of host RAM is a conservative starting point, not an AMD measurement. A cold native-8B unit can need 18 GiB of disk.

## How the device is chosen

The scheduler uses `<gpu_type>ati</gpu_type><opencl/>` and reserves one AMD GPU and two host CPUs; the pinned BOINC source accepts `ati`/`amd` in [`plan_class_spec.cpp`](https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/sched/plan_class_spec.cpp). Inference uses Vulkan, not OpenCL.

The app needs an explicit ATI/AMD assignment, a nonnegative `gpu_device_num` and `gpu_opencl_dev_index`, and does not assume BOINC's device number is the Vulkan index. The signed helper `usnm-amd-device-map[.exe]` resolves the assigned OpenCL GPU on AMD platforms (following the pinned `boinc_get_opencl_ids_aux`), takes its PCI identity (`cl_khr_pci_bus_info` or AMD's PCI topology) and matches it against the bundled ggml Vulkan backend's PCI identities, returning exactly one `VulkanN`. Multiple PCI domains, conflicting platform mappings, missing PCI support, duplicate identities or an invisible assigned adapter are refused; there is no name or ordinal fallback. The app also requires that device in `llama-server`'s listing with a real AMD/ATI/Radeon name, skipping software devices and other vendors.

The helper runs under the same 300-second enumeration deadline and the same bundled backend and loader environment as `llama-server --list-devices`. It loads the registry from `libggml.so` / `ggml.dll` and the device API from `libggml-base.so` / `ggml-base.dll`, so both, and the Vulkan backend, must be in the signed bundle. A missing helper or assignment, an unverifiable identity, a failed probe or too little free memory ends the unit `no_gpu`; it never picks another adapter or falls back to CPU.

### Windows drivers without Vulkan PCI bus information

Signed Windows 1.10 failed on an RX 9070 XT with OpenCL `????:03:00.0` and Vulkan `[]`: baseline b8799 ggml exports no `device_id` when `VK_EXT_pci_bus_info` is absent. A source patch exports `luid:` plus the same Vulkan device's eight LUID bytes, only on Windows and only when PCI information is missing. The helper opens that LUID with `D3DKMTOpenAdapterFromLuid`, queries `KMTQAITYPE_ADAPTERADDRESS` and compares the PCI bus, device and function with BOINC's OpenCL assignment. D3DKMT gives no PCI domain, so the helper refuses an OpenCL assignment that names one rather than assume domain zero.

Build the patched DLL on Windows with MSVC, CMake, Python and Vulkan SDK 1.4.313.2 (`VULKAN_SDK` set), in an operator build directory:

```powershell
git clone --config core.autocrlf=false --no-checkout https://github.com/ggml-org/llama.cpp.git amd-llama-source
git -C amd-llama-source checkout e1a9a6dcbefccb4b864d9385ce8494f2a7f2ffcd
.\boinc\scripts\build_amd_vulkan_runtime.ps1 `
  -Source "$PWD/amd-llama-source" -Build "$PWD/amd-llama-build" `
  -Output "$PWD/amd-llama-identity"
```

The builder rejects a dirty or wrong checkout and checks the original Vulkan source's SHA-256. It builds only `ggml-vulkan`, checks the DLL's `ggml_backend_init` export, and writes `ggml-vulkan.dll` and `AMD_RUNTIME_BUILD.json` (source commit, patch digest, output digest, SDK). These are unsigned build artifacts. CI tests the patched function against synthetic Vulkan responses on both platforms and compiles and self-tests the helper on Windows.

## Releasing an AMD version

Use the unchanged `boinc/apps/qwen/llama.lock` b8799 `ubuntu-vulkan-x64` or `win-vulkan-x64` asset, checked against its pinned SHA-256; never the CPU or CUDA archive. Bundle `llama-server[.exe]`, all its runtime libraries, `llama.lock`, `weights.lock` and the matching native CI artifact's `usnm-amd-device-map[.exe]` as signed extra files. Windows needs `ggml-vulkan.dll`, the llama, ggml and mtmd DLLs, `ggml-cpu-x64.dll`, `libomp140.x86_64.dll` and the Microsoft CRT, which `boinc/scripts/install.py` enforces. For Windows, unpack the verified baseline archive and replace only `ggml-vulkan.dll` with the patched build, and add `--extra-file AMD_RUNTIME_BUILD.json=PATH`; the bundle then no longer matches the archive hash. Test the bundled `llama-server` with the real AMD device before signing.

Release through the [normal path](app-release.md) with `--app usnm_qwen --kind native --plan-class amd_vulkan_8b` and the platform, signing every replaced and existing extra file. Models are the sticky `usnm_qwen_8b_in` inputs; native-8B work is batch 8 with 6 GiB host-memory and 18 GiB disk bounds, quorum 2 and two replicas. Coordinate on #529 before publication or any admission change, and record the new app-version IDs, commit and public download hashes.

## Admission

`boinc/scripts/native-qwen-cohort.php` sets AMD's admission in the plan-class policy. The installation default is closed (`min_wu_id=2`, `max_wu_id=1`). Every mode previews by default, applies with `--apply`, and shares the policy lock and durable journal with the other classes. The deployed copy is `/usr/local/lib/usnm-boinc/amd-885/native-qwen-cohort.php` (in the `amd-885` and `live-overlay` server images).

```sh
php boinc/scripts/native-qwen-cohort.php --project /project/usnmocr \
  --plan-class amd_vulkan_8b --general --apply   # general admission
php boinc/scripts/native-qwen-cohort.php --project /project/usnmocr \
  --plan-class amd_vulkan_8b --stop --apply      # close AMD only
```

`--general` removes AMD's `min_wu_id` and `max_wu_id` and writes the comment `usnm-amd-general-admission-v1`; startup's policy refresh keeps that mode. Removing the bounds without the marker doesn't opt in: the refresh restores them. `--stop` removes the marker and restores `2/1`, which stays closed across restarts. AMD's window is independent of the Intel and NVIDIA ones: an unqualified `--stop` doesn't close AMD, and neither AMD mode changes another class.

To diagnose a release, close AMD and admit one workunit at a time:

- **Ordinary canary.** `--plan-class amd_vulkan_8b --ordinary-canary --workunit ID` admits AMD to one ordinary native-8B workunit; its second replica goes to any eligible native-8B host and the normal validator compares them. The workunit must be ordinary (batch 8, 18 GiB disk bound, quorum and target 2, not canonical, no error, `need_validate=0`, `transitioner_flags=0`, `app_version_num=0`, no `assignment` rows) with exactly two unsent results that no host has been sent yet. The proof records `admission_mode: ordinary_canary`. If the canary and its peer come back inconclusive, a [tiebreaker](intel-gpu.md#admission) admits Intel or NVIDIA to the third replica.
- **Staged workunit.** `--plan-class amd_vulkan_8b --workunit ID` admits a workunit staged without results ([below](#bounded-acceptance-with-a-staged-workunit)).
- **AMD-first reservation.** Opening AMD and another vendor on the same pair doesn't reserve the AMD replica. `boinc/scripts/qwen-amd-reservation.py reserve`, `tick` and `release` lease one newest untouched ordinary pair to AMD by opening that workunit and temporarily bounding the Apple class below it, which also holds later new work from Apple. It needs AMD closed and Intel and NVIDIA closed or bounded before the reserved workunit, a reviewed policy SHA and helper SHA, an independent per-workunit receipt, and a separately armed rollback unit that calls `release --apply` at the lease deadline (default 90 minutes, at most two hours). It never writes science, result or assignment rows; record the lease on #529 before `--apply`.

Close AMD after any diagnostic, and if the result ran an older app version (`result.app_version_id`), the device mapping failed, the GPU didn't offload, or no independent peer agreed, leave general admission closed.

### Bounded acceptance with a staged workunit

When a dedicated assignment-only account and a peer host eligible for the same release exist, acceptance can use a new targeted workunit instead of production work. Prepare a server fixture with the exact release, platform and plan class, the account, one distinct peer host, all four native-8B input names and hashes, `batch: 8`, `min_quorum: 2` and profile `gpu: "amd"`, `memory_bytes: 6442450944`, `disk_bytes: 19327352832`, without `audited_workunit_id`. While AMD is closed, stage it:

```sh
python3 boinc/scripts/acceptance_server.py --project /project/usnmocr \
  --fixtures /project/acceptance --stage-fixture REVIEWED_FIXTURE
```

This hashes the inputs, checks the native template bounds and release, journals intent and runs `create_work --no_new_results` (added by `patch-server-stage-work.py`; an older base image rejects the flag and leaves an uncertain-stage journal to reconcile, not retry). Audit the workunit: zero results, `transitioner_flags=2` (`TRANSITION_NO_NEW_RESULTS`), a future `transition_time`, nothing waiting for validation, and the expected name, payload, template, batch, bounds and version. Add its ID as `audited_workunit_id`, open it with `--plan-class amd_vulkan_8b --workunit ID --apply`, and run the [existing-client acceptance runner](app-release.md#client-acceptance-548). Dispatch rechecks the window under the policy lock and database locks and adds only the client and peer assignments; stock targeted scheduling then creates one result for each, keeping quorum and target 2. Close AMD with `--stop --apply` afterwards. Never delete or relabel existing scientific rows to turn an old workunit into a fixture; stage a new one.

The runner maps BOINC's `coproc_ati` to `gpu: "amd"`. The Azure ephemeral client supports CPU and NVIDIA only.

## What is left

On a dedicated, authorized test client, #885 still needs: the driver, OS and BOINC versions with the archive and model hashes; the chosen `VulkanN`, layer offload, free and peak VRAM and host RAM; successful inference, return and collection with independent validation; and correctness and timings on retained benchmark crops. Don't claim speed, equivalence or general RX 9070 XT support until that evidence is reviewed in #885.
