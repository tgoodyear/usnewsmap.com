# Intel and NVIDIA GPU measurements for usnm_qwen

Plan class `intel_vulkan_8b` runs the same Qwen3-VL-8B Q4_K_M reading as the CUDA and Metal
classes, through the pinned llama.cpp Vulkan build (`ubuntu-vulkan-x64` and `win-vulkan-x64` in
`ocr/boinc/qwen/llama.lock`). It shares the batch-8 workunits of `nvidia_cuda_8b` and
`apple_gpu_8b`, so its readings can join the same quorum
([#786](https://github.com/tgoodyear/usnewsmap.com/issues/786)). The class ships closed
(`min_wu_id=2`, `max_wu_id=1`). An operator opens one audited workunit at a time with
`scripts/boinc/native-qwen-cohort.php --plan-class intel_vulkan_8b`, as for the NVIDIA classes
([app releases](../contrib-compute/docs/app-release.md)).

`usnm_iris` ([#703](https://github.com/tgoodyear/usnewsmap.com/issues/703),
[docs/boinc-iris-pilot.md](boinc-iris-pilot.md)) is a separate experiment with a different model
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

| GPU                                    | Host                              | Status                                                                           |
| -------------------------------------- | --------------------------------- | -------------------------------------------------------------------------------- |
| Iris Xe (TigerLake-LP GT2), i7-1185G7  | 16 GB RAM, Debian 13, Mesa 25.0.7 | Measured (below)                                                                 |
| Iris Xe with 8 GB RAM                  |                                   | Not measured; likely refused if its Vulkan heap is half of RAM, as on Computer 9 |
| NVIDIA GeForce RTX 2060 SUPER, 8 GiB   | Proxmox host, CT 214              | Measured (below)                                                                 |
| Arc A-series (A750 8 GB, A770 16 GB)   |                                   | Not measured; no Arc hardware was available for #829                             |
| AMD GPUs through the same Vulkan build |                                   | Not in scope                                                                     |

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
establish CUDA/Vulkan equivalence. Paired raw Vulkan readings and matching engine metadata are
still required for the equivalence item in [#786](https://github.com/tgoodyear/usnewsmap.com/issues/786).
No Arc hardware was available, so Arc was not measured.
