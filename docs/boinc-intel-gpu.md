# Intel GPUs for usnm_qwen

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
- **App.** The app lists the Vulkan devices with its own `llama-server --list-devices` and takes
  the device BOINC assigned. It never takes a software device (llvmpipe, lavapipe, SwiftShader).
  It pins `llama-server` to that device with `GGML_VK_VISIBLE_DEVICES`.
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
  buffers and process memory on Computer 9, and the class reserves 9 GiB. Run one unit at a time
  until a host's headroom is measured.

## Supported hardware

| GPU                                    | Host                              | Status                                                                           |
| -------------------------------------- | --------------------------------- | -------------------------------------------------------------------------------- |
| Iris Xe (TigerLake-LP GT2), i7-1185G7  | 16 GB RAM, Debian 13, Mesa 25.0.7 | Measured (below)                                                                 |
| Iris Xe with 8 GB RAM                  |                                   | Not measured; likely refused if its Vulkan heap is half of RAM, as on Computer 9 |
| Arc A-series (A750 8 GB, A770 16 GB)   |                                   | Not yet measured                                                                 |
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
