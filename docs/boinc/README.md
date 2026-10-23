# BOINC batch pilot

A volunteer worker pool alongside the on-prem OCR nodes
([#438](https://github.com/tgoodyear/usnewsmap.com/issues/438)).
Production and experimental tasks, including Qwen, share the same account,
CPU and GPU choices and resource limits. No per-experiment profile update is
required. Operators bound campaigns with the [native NVIDIA workunit window
and stop procedure](../../contrib-compute/docs/app-release.md).

The pilot uses stock BOINC, one cached page per task, pinned Tesseract 5.5.0,
the model lock and language routing. It publishes native ALTO and Parquet to
`ocr-out`; `ocr-sync` and release decisions are unchanged. Other apps:

- `usnm_assess`: [JP2 versus IIIF JPEG comparison](../notes/2026-10-16-boinc-assessment.md) (#456).
- `usnm_gold`: [score a host's engine on public ground truth](gold-check.md#gold-check).
- `usnm_surya`: [Surya 2 on CPUs, NVIDIA GPUs, and Intel GPUs](../boinc-surya.md) (#516, #842).
- `usnm_ndlocr`: [Japanese NDLOCR-Lite/default-route CPU work](../boinc-ndlocr-lite.md),
  two-reading character-similarity validation and collection into `ocr-ja`.
- `usnm_quality` and `usnm_qwen`: [per-page quality scoring without ground
  truth and Qwen3-VL deterministic word-crop transcription](../boinc-quality.md) (#522).

Start with the [volunteer guide](../boinc-volunteer.md).
The opt-in [batch controller](../boinc-batch-scheduler.md) schedules allocated
cached pages with bounded queue backpressure; operators activate it separately.
The [post-training pilot](../notes/2026-10-16-boinc-transcription.md) evaluates public-gold
baselines and adapted models with `usnm_qwen`.

## Pages

- [Boundaries and monitoring](boundaries-and-monitoring.md): the public dataset-page counts, workbook query validation and the pilot's boundaries.
- [The worker image](worker-image.md): building and publishing the worker image, and the local synthetic demonstration.
- [The server and the apps](server-and-apps.md): connecting a stock BOINC project, installing app versions and plan classes, and moving the project share.
- [The bridge steps](bridge-steps.md): starting a bridge step, registering the apps, two October 2026 campaign records, and running the server and maintainer on different hosts.
- [The scan-cache hand-off](scan-cache-handoff.md): getting one page from the scan cache to the BOINC server, with the infrastructure, the runbook, the scan fetcher's identity and the quality wave's page JPEG cache.
- [Allocation, recovery and acceptance](allocation-and-recovery.md): how a cohort is allocated, how to recover failed work, and the live acceptance exercise.
- [The gold check](gold-check.md): scoring a host's engine on public ground truth, host trust for `usnm_ocr`, building the gold bundle and engine routing.
