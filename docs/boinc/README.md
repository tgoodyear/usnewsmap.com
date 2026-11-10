# BOINC

The volunteer worker pool. Since the direct queue worker was retired (#966), it is the only way
the project runs OCR ([#438](https://github.com/tgoodyear/usnewsmap.com/issues/438)). The server,
its database, the bridge steps and the controller run on an on-prem host
([BOINC server on-prem](server/onprem.md)).
Production and experimental tasks, including Qwen, share the same account,
CPU and GPU choices and resource limits. No per-experiment profile update is
required. Operators bound campaigns with the [native NVIDIA workunit window
and stop procedure](apps/app-release.md).

The pilot uses stock BOINC, one cached page per task, pinned Tesseract 5.5.0,
the model lock and language routing. It publishes native ALTO and Parquet to
`ocr-out`; `ocr-sync` and release decisions are unchanged. Other apps:

- `usnm_assess`: [JP2 versus IIIF JPEG comparison](../notes/2026-10-16-boinc-assessment.md) (#456).
  Retired on 2 November 2026; it never sent work.
- `usnm_gold`: [score a host's engine on public ground truth](server/gold-check.md#gold-check).
- `usnm_surya`: [Surya 2 on CPUs, NVIDIA GPUs, and Intel GPUs](apps/surya.md) (#516, #842).
  Retired on 2 November 2026; it sends no work.
- `usnm_ndlocr`: [Japanese NDLOCR-Lite/default-route CPU work](apps/ndlocr-lite.md),
  two-reading character-similarity validation and collection into `ocr-ja`.
- `usnm_iris` (Iris Xe 4B pilot, #703) and `usnm_qwen_train` (LoRA training, #583): retired on
  2 November 2026; they send no work. `usnm_qwen`'s `intel_vulkan_8b` class replaced the Iris pilot.
- `usnm_quality` and `usnm_qwen`: [per-page quality scoring without ground
  truth and Qwen3-VL deterministic word-crop transcription](apps/quality.md) (#522).

The opt-in [batch controller](server/batch-scheduler.md) schedules allocated
cached pages with bounded queue backpressure; operators activate it separately.
The [post-training pilot](../notes/2026-10-16-boinc-transcription.md) evaluates public-gold
baselines and adapted models with `usnm_qwen`.

## Volunteers

- [Volunteer guide](volunteer/README.md): what volunteers run, the apps, limits and troubleshooting.
- Quick starts: [Linux](volunteer/linux.md), [Windows](volunteer/windows.md), [macOS](volunteer/macos.md).

## Apps

- [OCR quality](apps/quality.md): `usnm_quality` and `usnm_qwen`.
- [OCR bundles](apps/ocr-bundles.md): several pages in one `usnm_ocr` workunit.
- [Native OCR](apps/native-ocr.md): `usnm_ocr` and `usnm_gold` without a container runtime.
- [Japanese OCR with NDLOCR-Lite](apps/ndlocr-lite.md): `usnm_ndlocr`.
- [AMD GPUs](apps/amd-gpu.md) and [Intel and NVIDIA GPUs](apps/intel-gpu.md) for `usnm_qwen`.
- [Surya 2](apps/surya.md): retired; kept for reading its results.

## Server

- [BOINC server on-prem](server/onprem.md): what runs on the on-prem host and the cutover order.
- [Batch controller](server/batch-scheduler.md): the bounded tick that chains the pipeline steps.
- [Cross-host start guard](server/start-guard.md), [local pipeline documents](server/local-state.md), [local download host](server/file-host.md) and [BOINC beside the retired OCR worker](server/ocr-node.md).

## Pipeline pages

- [Boundaries and monitoring](server/boundaries-and-monitoring.md): the public dataset-page counts, workbook query validation and the pilot's boundaries.
- [The worker image](apps/worker-image.md): building and publishing the worker image, and the local synthetic demonstration.
- [The server and the apps](server/server-and-apps.md): connecting a stock BOINC project, installing app versions and plan classes, and moving the project share.
- [Feeder-only refresh](server/feeder-refresh.md): when and how to restart only the feeder so its `--allapps` slot allocation matches the app registry, with the readbacks, retry limit and rollback (#1462).
- [The bridge steps](server/bridge-steps.md): starting a bridge step, registering the apps, two October 2026 campaign records, and running the server and maintainer on different hosts.
- [The scan-cache hand-off](server/scan-cache-handoff.md): getting one page from the scan cache to the BOINC server, with the infrastructure, the runbook, the scan fetcher's identity and the quality wave's page JPEG cache.
- [Allocation, recovery and acceptance](server/allocation-and-recovery.md): how a cohort is allocated, how to recover failed work, and the live acceptance exercise.
- [The gold check](server/gold-check.md): scoring a host's engine on public ground truth, host trust for `usnm_ocr`, building the gold bundle and engine routing.
- [Releasing an app version](apps/app-release.md): signing and publishing app versions, the feeder sizing, the native GPU windows and client acceptance.
- [The AMD #885 release](apps/amd-885-release.md): the record of the scoped AMD Vulkan production release.
- [The bootstrap pipeline](server/bootstrap-pipeline.md): the reviewed orchestration of the existing BOINC scripts (#551).
- [Database TLS acceptance](server/db-tls-acceptance.md): the local BOINC database TLS checks.
