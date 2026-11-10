# Surya 2 on BOINC (retired)

Issues: [#516](https://github.com/tgoodyear/usnewsmap.com/issues/516), [#842](https://github.com/tgoodyear/usnewsmap.com/issues/842), [#968](https://github.com/tgoodyear/usnewsmap.com/issues/968). `usnm_surya` ran Surya 2 on volunteer hosts, on CPU, on NVIDIA GPUs (CUDA) and on Intel Xe / Iris GPUs (Vulkan), to read a larger sample than round 1 of the engine survey ([report](../../reports/2026-10-ocr-engine-survey/report.md)) and to measure CPU and GPU time per page.

The app was retired on 2 November 2026 and the server sends no `usnm_surya` work ([Retirement](#retirement-2-november-2026)). This page describes what its results contain and how they were made, for anyone reading them or bringing the app back. The results are in two reports: [the October volunteer trial](../../reports/2026-10-surya-volunteer/report.md) and [the November GPU studies](../../reports/2026-11-gpu-studies/report.md).

The November GPU studies ([#1428](https://github.com/tgoodyear/usnewsmap.com/pull/1428)) are why it was retired. Surya's best pooled CER was 23.0% (Study 6, line mode), against Tesseract's 9.0% on the same 612 crops. Tiled pages cost about $36 per 1,000 on a rented RTX 3090, against about $3.40 per 1,000 pages for Qwen (12 crops a page). Study 6 also scored Surya's native weights on vLLM at 61.4% CER against 18.9% for the GGUF on 50 crops. That gap is unexplained and may come from our server settings, so the native-weights result is unresolved.

## What a unit is

A page unit holds the page's full-size IIIF JPEG from LoC (`page.jpg`) and the job (`surya.json`). Staging also kept LoC's ALTO as `loc.xml` (or a `loc.xml.missing` marker) on the server for scoring; it was never sent to a volunteer. The host read the page in up to three input modes:

| Mode     | Input                                                                                                                                      |
| -------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `tiled`  | 1,280 px tiles overlapping by 256 px, as round 1 read pages, with each overlap split at its midpoint so a block is kept from one tile only |
| `whole`  | the whole page in one request; the model scales it to its own image budget                                                                 |
| `capped` | the whole page, scaled first so its long side is at most 2,048 px                                                                          |

A host with a usable GPU ran all three, each with a 1,800 s deadline. A CPU-only host ran `tiled` only, with a 5,400 s deadline: round 1 took a median 2,411 s per tiled page with about 8 cores busy.

A crop unit (from `surya stage-crops` or `surya stage-gold`) holds one crop cut as a JPEG at quality 95 and reads `whole` only, with one deadline (600 s by default). Its job carries `crop` (`id`, `box`, `stratum`), and its `input.url` is the page's URL with the box as an `#xywh=` fragment. `submit` gave a crop unit a hundredth of a page's fpops estimate and bound, an 8 GiB memory bound and a one-day delay bound.

Every result has five files: `text-tiled.txt`, `text-whole.txt`, `text-capped.txt` (empty for a mode that didn't run), `result.json` and `diag.txt`. The types are in `crates/usnm-ocr-contract/src/surya.rs`.

### Where units live

A campaign staged after [ADR-0026](../../design/adr/0026-source-qualified-page-ids.md) keeps each unit under its page id's path: `units/loc/<doc_id>` for a page, `units/loc/<doc_id>.<crop_id>` for a crop and `units/sample/<dataset>/<sample_id>` for a benchmark sample, and has a `<campaign>/units-layout` marker. An older campaign has no marker and keeps `units/<doc_id>`, `units/<doc_id>.<crop_id>` and `units/<sample_id>`; submit, collect, summary and `gold-hyp` read either layout. Workunit names are `usnm_surya_<job id>`, a sha256, whatever the unit's path.

## The image and engine identity

`boinc/apps/surya/Containerfile` builds two variants from the same pinned `llama.cpp` revision (`b11610`, commit `091b6a7fc561b78da90a0ed2d524ec995458c657`):

- `LLAMA_VARIANT=cuda` (default): on `ghcr.io/ggml-org/llama.cpp:server-cuda`, which loads its CUDA backend when the container can see an NVIDIA GPU and its CPU backends otherwise (`docker_mt` and `docker_nvidia_cuda`).
- `LLAMA_VARIANT=vulkan` (#842): on `ghcr.io/ggml-org/llama.cpp:server-vulkan`, for Intel Xe / Iris GPUs (`docker_intel_vulkan`).

On top are surya-ocr in a Python virtual environment, with the CPU build of torch (torch only runs Surya's layout fallback; the OCR model runs in llama-server), and `usnm-boinc`, which runs the unit. The image carries Surya 2's GGUF model and vision projector and the layout model, pinned by URL and sha256 in `boinc/apps/surya/weights.lock`. Datalab's licence (a modified Open RAIL-M) requires a copy with every distribution of the weights, so the image carries `LICENSE` beside them (`/opt/surya/models/LICENSE`) and a `NOTICE`.

The image records its llama.cpp base, Surya version and weights-lock digest as `USNM_SURYA_LLAMA_IMAGE`, `USNM_SURYA_VERSION` and `USNM_SURYA_WEIGHTS`. A unit carries the same three values, and the host refused a unit staged for another build. The values are written once, in `boinc/apps/surya/engine.lock` (`SURYA_VERSION`, `LLAMA_DIGEST`, `LLAMA_VULKAN_DIGEST`, `SURYA_WEIGHTS`, the last being the sha256 of the non-comment lines of `weights.lock`). `usnm-boinc` compiles the lock in and uses `SURYA_VERSION` as the default of `--engine-surya`, and `scripts/ci/check-engine-pins.py` fails CI when `SURYA_WEIGHTS` doesn't match `weights.lock` or the Containerfile gains a copy of a pin. `.github/workflows/boinc-surya.yml` builds both variants and, when started by hand on `main`, pushes them to `ghcr.io/tgoodyear/usnm-boinc-worker` as `surya-<commit>` and `surya-vulkan-<commit>`.

The CUDA campaigns used `SURYA_ENGINE_LLAMA` = `LLAMA_DIGEST` and `SURYA_ENGINE_WEIGHTS` = `SURYA_WEIGHTS` as they still stand in `engine.lock`; a `surya-vulkan-*` image has a different llama image.

## App versions and plan classes

| Version | Plan class            | Sent to                                                                                                                                         | Container                                                                                          |
| ------- | --------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| 1.4     | `docker_mt`           | hosts with Docker or Podman and at least 4 usable CPUs; 4 to 8 threads                                                                          | `--cpus` from the thread count, 12 GiB, no swap (`boinc/apps/surya/job-cpu.toml`)                  |
| 1.5     | `docker_nvidia_cuda`  | hosts with Docker or Podman whose client reports an NVIDIA GPU with CUDA 12.0 or later, compute capability 5.0 or later and 3,000 MB of memory  | `--gpus all`, 2 CPUs, 8 GiB, no swap (`boinc/apps/surya/job-gpu.toml`)                             |
| 1.6     | `docker_intel_vulkan` | hosts with Docker or Podman whose client reports an Intel OpenCL GPU with at least 4,096 MB of memory; the class stayed closed and sent nothing | `use_gpu = false`, `--device=/dev/dri`, 2 CPUs, 8 GiB, no swap (`boinc/apps/surya/job-intel.toml`) |

All three classes need client 8.2.13 or later and are in `boinc/project/plan_class_spec.xml`. 1.4 and 1.5 (app versions 58 and 59) were the active pair from 22 October 2026, from worker image `sha256:bccf7cba49878481d394f94f9ca313787d8f28886e369009ae4cfe94859fb494`. 1.2 (`docker_mt`, app version 54) and 1.3 (`docker_nvidia_cuda`, app version 55) ran the [#812 probe](#the-812-probe-18-october-2026) on 18 October and were deprecated on 22 October. 1.0 and 1.1 were never registered. 1.6 (app version 60) was published on 26 October with its class closed ([release note](../../notes/2026-10-26-boinc-gpu-release.md)).

`rsc_memory_bound` was 12 GiB for a page unit, rounded up from round 1's 11.8 GB CPU peak, since a workunit could go to either version. `rsc_fpops_est` was 1e14, from round 1's median 18,753 CPU-seconds per tiled page.

### How the GPU reached the container

The stock `docker_wrapper` adds `--gpus all` to `docker create` (or `podman create`) when the version's `job.toml` sets `use_gpu = true`. On Linux the scheduler checks only the GPU the client detected, not whether the container runtime can pass it through. So the host needed, beyond the NVIDIA driver, a CDI specification from the NVIDIA Container Toolkit for Podman (`nvidia-ctk cdi generate --output=/etc/cdi/nvidia.yaml`), or the toolkit configured as a Docker runtime. Without it the container either failed to start (the runtime's error is in the wrapper's stderr) or started without a GPU and returned `no_gpu` for each mode.

The wrapper passes every GPU. The worker ran llama-server on the one BOINC assigned (`CUDA_VISIBLE_DEVICES`, taking BOINC's number as nvidia-smi's index, which can differ on a host with mixed GPUs), and the diagnostics record which GPU that was.

## Diagnostics

`usnm-boinc work` always writes all five outputs, so a host that couldn't run Surya still returned why. `diag.txt` (at most 64 KiB) and `result.json` hold:

- from BOINC's `init_data.xml`, only the client version, app version, plan class, GPU type and device number, CPU count and memory bound (never the account key or host name);
- the container runtime and whether it is rootless, the cgroup limits and memory;
- the NVIDIA device nodes, CUDA and NVIDIA variables, driver version and whether `libcuda` is there, and `nvidia-smi`'s output or its error;
- torch's version and CUDA check, surya-ocr's version and its inference settings.

Each mode ran in its own process group with its deadline, sampling the group's memory and CPU time every 0.5 s and, on a GPU, the GPU's memory every 2 s. Per mode the result records the outcome (`ok`, `timeout`, `failed`, `no_gpu`), exit code or signal, time, CPU seconds, peak memory, peak GPU memory above what was in use before, and the device llama-server used: `cuda` or `vulkan` when its log says it offloaded layers to the GPU, `cpu` when it offloaded none or found no usable GPU, and `none` when the log gives no evidence. A mode in which Surya reported a block it could not read counts as `failed`.

The runner used in the October campaigns discarded block errors like skips, so those campaigns' results can't show whether any occurred; the GPU studies report treats the affected scores as provisional.

A unit that couldn't run at all (a checksum mismatch, an engine that differs from the job's) returns `unit_error` with the reason and no runs.

## Redundancy and validation

`create_work` made each unit with `min_quorum` 2, `target_nresults` 2 and at most 4 results. `script_validator` ran:

- `usnm_surya_check` (`usnm-boinc surya check`) on each result: shape, sizes, UTF-8 and checksums. It copied every result to `<project>/surya/seen/<result>/`, so every host's timings and diagnostics are kept, not only the canonical result's.
- `usnm_surya_compare` (`usnm-boinc surya compare`) on each pair. Two results agree when they have the same job and engine; when, in each mode both ran, both read the page or neither did; and when, where both read it, their word-bag Dice is at least 0.5 (`PROBE_MIN_DICE`), which only rejects unrelated text. Two `unit_error` results agree. Every pair is in `<project>/surya/pairs.jsonl` with its Dice per mode and both hosts' devices.

Kept result directories can carry BOINC's optional output-template token, for example `HOST_SUFFIX_r2014260292_3`; `surya-benchmark.py collect` accepts it.

## Reading the results

`usnm-boinc surya collect --root <campaign> --spool <assimilator results> --texts-dir <campaign>/texts` reads the canonical results and writes `<doc_id>.surya.json` for every page with a read: the page's text as the reader `ours-surya`, from the first of `--modes` (`whole,capped,tiled` by default) that read it, with `surya:<mode>:<job id>` as its source. `quality stage --surya-texts <dir>` joins it into each unit's `texts.json` when `--readers` names `ours-surya`, and `quality collect --surya-root <campaign> --surya-spool <results>` joins crop units into `crops.parquet` as the reader `surya` ([OCR quality](quality.md)). `surya summary` (bridge step `surya-summary`) prints every kept result and every compared pair to `<campaign>/summary.md`.

### Surya on the gold crops (#968)

`usnm-boinc surya stage-gold --manifest fixtures/ocr-gold/cze-srp-pol-rus/manifest.jsonl --root <campaign>` writes one crop unit per row of the [OCR gold set](../../ocr/gold-set.md)'s Czech, Serbian, Polish and Russian manifest, with group `gold`, the row's language and the stratum `gold/<lang>`. It checks each page's sha256 against the row's `page_image_sha256` and marks a page that differs `.mismatch` (its crops aren't cut) and a page LoC answers 404 for `.missing`. `usnm-boinc surya gold-hyp --root <campaign> --spool <results> --out <file.jsonl>` writes one `{"crop_id", "text"}` line per crop with a successful `whole` read, which the gold scorer takes:

```sh
scripts/ocr-survey/gold.py score --crops fixtures/ocr-gold/cze-srp-pol-rus/crops.jsonl \
  --hyp surya=<campaign>/gold/surya.jsonl
```

### The #812 probe (18 October 2026)

Campaigns `probe812-20261018-cpu` and `probe812-20261018-gpu`: 23 units on versions 1.2 and 1.3, staged from the `ocr-bench-v1.0.0` smoke subsets with `surya-benchmark.py prepare` and `surya benchmark-stage`. They used exact-host assignment (`surya submit --target-host`) with quorum one, so there are no validator pairs and `pairs.jsonl` is absent. Each unit's doc id is `sample:<dataset>/<sample_id>`. Its `input.url` is the `https://tile.loc.gov/local` sentinel, not a source URL; the bundle's `benchmark-inputs.json` holds the real source and licence. `surya-benchmark.py collect` exports kept results for `usnm-ocr-survey bench-score`. Results are in [the volunteer trial report](../../reports/2026-10-surya-volunteer/report.md).

### Wave 1 (22 to 26 October 2026)

Campaign `wave1` on versions 1.4 and 1.5: page and crop units cut from a quality campaign's sample and units (`surya-stage` with `SURYA_SAMPLE`, `surya-stage-crops` with `QUALITY_CAMPAIGN`), kept topped up by the [batch controller](../server/batch-scheduler.md#surya-waves). The last workunit was created on 26 October. Results are in [the GPU studies report](../../reports/2026-11-gpu-studies/report.md).

## Retirement (2 November 2026)

In the server container, the app's `app` row and every `app_version` row were saved as JSON under `/project/ops/surya-retire-20261102/`, `deprecated=1` was set on the app and on 1.4, 1.5 and 1.6 (app versions 58, 59 and 60), and `reread_db` was touched so the feeder reloaded. Nothing was deleted: workunit and result rows, signed app versions, images and staged units all stay. No result was in progress; four results stayed unsent, the third replicas of 4 workunits from 24 October whose first two results disagreed.

Images built from #1449 on drop the `usnm_surya` validator and assimilator from `render_config`. The bridge steps, the `usnm_surya` plan kind in `batch_scheduler.py`, the image workflow and `boinc/scripts/surya-benchmark.py` stay in the repository, so the studies can be rerun.

To roll back: restore `deprecated=0` on the app and app versions 58, 59 and 60 and touch `reread_db`; the 4 unsent results become sendable again. On an image built from #1449, first put the validator, the assimilator and the `usnm_surya_check` and `usnm_surya_compare` links back in `render_config` and deploy that image, before any result is sent. A new worker image needs an unused version pair, released as in [Releasing an app version](app-release.md).
