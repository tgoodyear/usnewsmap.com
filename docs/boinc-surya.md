# Surya 2 on BOINC

Issue: [#516](https://github.com/tgoodyear/usnewsmap.com/issues/516), [#842](https://github.com/tgoodyear/usnewsmap.com/issues/842). Round 1 of the engine survey ([report](reports/2026-10-ocr-engine-survey/report.md), #370) ran Surya 2 on one CPU node, on 8 pages. This app, `usnm_surya`, runs it on volunteer hosts, on CPU, on NVIDIA GPUs (CUDA), and on Intel Xe / Iris GPUs (Vulkan, #842), to read a larger sample and to measure CPU and GPU time per page. The other apps are described in [boinc.md](boinc.md) and [boinc-assessment.md](notes/2026-10-16-boinc-assessment.md).

Status: a probe of a few pages, to find out whether volunteers' container runtimes can give the job a GPU, how long a page takes, and how far two hosts' texts agree. The larger sample comes after it.

## What a unit is

One page per workunit: the page's full-size IIIF JPEG from LoC (`page.jpg`) and the job (`surya.json`). Staging also keeps LoC's ALTO as `loc.xml` (or records `loc.xml.missing`) on the server for later quality scoring; it is never sent to a volunteer. The ALTO request uses the shared LoC pacer and the same hold-on-block behavior as scan staging. The host reads the page in up to three input modes:

| Mode     | Input                                                                                                                                      |
| -------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `tiled`  | 1,280 px tiles overlapping by 256 px, as round 1 read pages, with each overlap split at its midpoint so a block is kept from one tile only |
| `whole`  | the whole page in one request; the model scales it to its own image budget                                                                 |
| `capped` | the whole page, scaled first so its long side is at most 2,048 px                                                                          |

A host with a usable GPU runs all three, each with a 1,800 s deadline. A CPU-only host runs `tiled` only, with a 5,400 s deadline: round 1 took a median 2,411 s per tiled page with about 8 cores busy.

Every result has five files: `text-tiled.txt`, `text-whole.txt`, `text-capped.txt` (empty for a mode that didn't run), `result.json` and `diag.txt`. The types are in `crates/usnm-ocr-contract/src/surya.rs`.

## The image

`ocr/boinc/surya/Containerfile` builds two variants from the same pinned `llama.cpp` revision (`b11610`, commit `091b6a7fc561b78da90a0ed2d524ec995458c657`):

- `LLAMA_VARIANT=cuda` (default): on `ghcr.io/ggml-org/llama.cpp:server-cuda`, which loads its CUDA backend when the container can see an NVIDIA GPU and its CPU backends otherwise (`docker_mt` and `docker_nvidia_cuda`).
- `LLAMA_VARIANT=vulkan` (#842): on `ghcr.io/ggml-org/llama.cpp:server-vulkan`, which runs the OCR model on Intel Xe / Iris GPUs via Vulkan (`docker_intel_vulkan`).

On top of the base image are surya-ocr 0.22.1 in a Python virtual environment, with the CPU build of torch (`TORCH_DEVICE=cpu`; torch only runs Surya's layout fallback, while the OCR model runs in llama-server), and `usnm-boinc`, which runs the unit.

The weights are in the image: Surya 2's GGUF model and vision projector, and the layout model, at the revisions round 1 used. `ocr/boinc/surya/weights.lock` pins each file's URL and sha256, and the build checks them. Datalab's licence (a modified Open RAIL-M) requires a copy with every distribution of the weights, and notice of its use-based restrictions, so the image carries `LICENSE` beside the weights (`/opt/surya/models/LICENSE`) and a `NOTICE`. The image records its llama.cpp base, Surya version and weights lock digest as `USNM_SURYA_LLAMA_IMAGE`, `USNM_SURYA_VERSION` and `USNM_SURYA_WEIGHTS`; a unit carries the same three values and the host refuses a unit staged for another build.

The image is several GB (the weights alone are 1.6 GB), which a volunteer downloads once per image version. `.github/workflows/boinc-surya.yml` builds both `cuda` and `vulkan` variants, checks that each reads a rendered line on CPU, and, when started by hand on main, pushes them to the public package `ghcr.io/tgoodyear/usnm-boinc-worker` with the tags `surya-<commit>` and `surya-vulkan-<commit>`. The run's summary gives each digest and the values to stage with.

## App versions and plan classes

| Version | Plan class            | Sent to                                                                                                                                                                      | Container                                                                                         |
| ------- | --------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------- |
| 1.0     | `docker_mt`           | hosts with Docker or Podman and at least 4 usable CPUs; 4 to 8 threads                                                                                                       | `--cpus` from the thread count, 12 GiB, no swap (`ocr/boinc/surya/job-cpu.toml`)                  |
| 1.1     | `docker_nvidia_cuda`  | hosts with Docker or Podman whose client reports an NVIDIA GPU with CUDA 12.0 or later, compute capability 5.0 or later and 3,000 MB of memory                               | `--gpus all`, 2 CPUs, 8 GiB, no swap (`ocr/boinc/surya/job-gpu.toml`)                             |
| 1.x     | `docker_intel_vulkan` | hosts with Docker or Podman whose client reports an Intel OpenCL GPU with at least 4,096 MB of memory (#842; closed by default with `min_wu_id=2, max_wu_id=1` until staged) | `use_gpu = false`, `--device=/dev/dri`, 2 CPUs, 8 GiB, no swap (`ocr/boinc/surya/job-intel.toml`) |

Both classes need client 8.2.13 or later and are in `contrib-compute/boinc/plan_class_spec.xml`. The scheduler sends a host the version it expects to be fastest, so a host with a qualifying GPU gets the GPU version.

### How the GPU reaches the container

The stock `docker_wrapper` adds `--gpus all` to `docker create` (or `podman create`) when the version's `job.toml` sets `use_gpu = true` (`samples/docker_wrapper/docker_wrapper.cpp` at the pinned BOINC commit). The scheduler only checks the GPU the client detected; on Linux it does not check that the container runtime can pass it through. (On Windows it does, from the GPU list the client reports for each WSL distribution, since BOINC's commit 1635c05 of June 2026.) So the host needs, beyond the NVIDIA driver:

- **Podman** (including rootless): a CDI specification for the GPUs, made with the NVIDIA Container Toolkit (`nvidia-ctk cdi generate --output=/etc/cdi/nvidia.yaml`). Podman turns `--gpus all` into the CDI device `nvidia.com/gpu=all`. With SELinux enforcing, the container may also need `container_use_devices` (`setsebool -P container_use_devices on`).
- **Docker**: the NVIDIA Container Toolkit installed and configured as a Docker runtime (`nvidia-ctk runtime configure --runtime=docker`).

Without these, the container either fails to start (the wrapper's stderr, which BOINC returns with the result, holds the runtime's error) or starts without a GPU. In the second case the unit returns `no_gpu` for each mode, quickly, with the diagnostics.

The wrapper passes every GPU, not only the one BOINC assigned (`gpu_device_num`). The worker runs llama-server on the assigned one (`CUDA_VISIBLE_DEVICES`, taking BOINC's number as nvidia-smi's index, which may differ on a host with mixed GPUs) and measures that GPU's memory; the diagnostics record which GPU that was. llama.cpp's CUDA build needs a driver that supports CUDA 12.

## Diagnostics

`usnm-boinc work` always writes all five outputs, so a host that can't run Surya still returns why, as a valid result rather than a lost one. Before reading, it records in `diag.txt` (at most 64 KiB) and `result.json`:

- from BOINC's `init_data.xml`, only these fields: client version, app version, plan class, GPU type and device number, CPU count and memory bound (the file also holds the account key and host name, which are never read out);
- the container runtime and whether it is rootless (Podman's `/run/.containerenv`; Docker's `/.dockerenv`), the cgroup limits the container got, and its memory;
- the NVIDIA device nodes, the CUDA and NVIDIA variables, the driver version and whether `libcuda` is there;
- `nvidia-smi`'s GPU name, driver, memory, compute capability and full output, or its error;
- Python's probe: torch's version and CUDA check, surya-ocr's version and its inference settings.

Each mode then runs in its own process group with its deadline. The worker samples the group's memory and CPU time every 0.5 s and, on a GPU, the GPU's memory every 2 s. A mode in which Surya reports a block it could not read counts as `failed`, whatever text the rest gave. It records the outcome (`ok`, `timeout`, `failed`, `no_gpu`), exit code or signal, time, CPU seconds, peak memory, peak GPU memory above what was in use before, and the device llama-server actually used: `cuda` or `vulkan` when its log says it offloaded layers to the GPU, `cpu` when it offloaded none or said it found no usable GPU, and `none` when the log gives no such evidence. `diag.txt` also gets llama-server's device lines and last lines, and the end of Surya's stderr.

A unit that can't run at all (an input whose checksum doesn't match, an engine that differs from the job's) returns `unit_error` with the reason and no runs.

## Redundancy and validation

`create_work` makes each unit with `min_quorum` 2 and `target_nresults` 2, and at most 4 results. `script_validator` runs:

- `usnm_surya_check` (`usnm-boinc surya check`) on each result: shape, sizes, UTF-8 and checksums. It also copies the result to `<project>/surya/seen/<result>/`, so every host's timings and diagnostics are kept, not only the canonical result's.
- `usnm_surya_compare` (`usnm-boinc surya compare`) on each pair. Two results agree when they have the same job and engine; when, in each mode both ran, both read the page or neither did; and when, where both read it, their word-bag Dice (the measure in [boinc-assessment.md](notes/2026-10-16-boinc-assessment.md)) is at least the threshold. A mode only one of them ran is not compared, and two results with no mode in common don't agree. Two results that both could not run the unit (`unit_error`) do agree. Every pair is appended to `<project>/surya/pairs.jsonl` with its Dice per mode and both hosts' devices.

Vision-language output can differ between GPUs, and between CPU and GPU, so the threshold is measured, not assumed. Until the probe has measured it, `compare` accepts any Dice of 0.5 or more (`PROBE_MIN_DICE`), which only rejects unrelated text, and the logged pairs give the distribution to set it from.

`rsc_fpops_est` is 1e14 (round 1's median 18,753 CPU-seconds per tiled page at a few GFLOPS per core) and `rsc_memory_bound` 12 GiB, rounded up from the measured 11.8 GB CPU peak. A workunit can be sent to either version, so the shared admission bound must cover the CPU version even though the GPU container is capped at 8 GiB. This conservatively excludes hosts whose BOINC memory allowance is below 12 GiB; revise it only after pilot measurements.

The ALTO staged with each page is retained so the Surya outputs can be scored against LoC with the existing report tooling. There is not yet a Surya collector that produces the large-sample quality report. The attached-host pilot and its output are prerequisites to setting the agreement threshold and widening the sample.

## Running a campaign

The server must have the `usnm_surya` app (`register-apps`), the two plan classes and daemons (server and bridge images built from a commit that includes `usnm_surya`, `scripts/build-boinc-images.sh` then `scripts/provision-boinc.sh`), and both app versions:

```sh
gh workflow run boinc-surya.yml --ref main -f correlation="surya-$(date -u +%Y%m%dT%H%M%S)"
# From the run's summary: the image, its digest, SURYA_ENGINE_LLAMA and SURYA_ENGINE_WEIGHTS.
scripts/boinc-app-release.sh --env prod --app usnm_surya --version 1.0 --plan-class docker_mt \
  --worker-image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:... --signing-host HOST
scripts/boinc-app-release.sh --env prod --app usnm_surya --version 1.1 --plan-class docker_nvidia_cuda \
  --worker-image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:... --signing-host HOST
```

Then, under `<share>/surya/<campaign>/`:

```sh
# Gets each page's JPEG from the scan archive, else from tile.loc.gov through its batch (see
# below); a 403, 429 or 5xx, or an HTML challenge page, writes HOLD and stops.
scripts/start-boinc-bridge.sh --env prod --step surya-stage --var SURYA_CAMPAIGN=probe1 \
  --var SURYA_PAGES=serbian:srp:sn85055101_1917-04-03_ed-1_seq-3:dlc_arlington_ver01,... \
  --var SURYA_ENGINE_IMAGE=sha256:... --var SURYA_ENGINE_LLAMA=sha256:... --var SURYA_ENGINE_WEIGHTS=sha256:... \
  --var SURYA_MAX=8 --wait-mins 15
scripts/start-boinc-bridge.sh --env prod --step surya-submit --var SURYA_CAMPAIGN=probe1 --var SURYA_MAX=8 --wait-mins 15
# As results come in:
scripts/start-boinc-bridge.sh --env prod --step surya-summary --var SURYA_CAMPAIGN=probe1 --wait-mins 15
```

Each page is `group:lang:doc_id:batch[+batch...]`. Staging never asks www.loc.gov where a page is (its item API answers Azure with a Cloudflare challenge), so the page names the batches that may hold it, as `<name>_ver<NN>`. Take them from the published set's `batches.json` in the lake's `reference` container (the version `current.json` names): every batch whose `lccns` include the page's LCCN and whose `first` to `last` dates include its date. For example, with the file downloaded:

```sh
jq -r --arg l sn82015743 --arg d 1931-04-10 '.[] | select((.curated.lccns | index($l)) and .curated.first <= $d and $d <= .curated.last)
  | "\(.batch)_ver\(.curated.version | tostring | if length < 2 then "0" + . else . end)"' batches.json | paste -sd+ -
```

For each page, staging (`crates/usnm-boinc/src/scans.rs`, shared with the assessment) takes the first of:

1. the scan archive's full-size JPEG of the page in one of its batches (`keep/<batch>/<lccn>/<date>/<edition>/<seq>.jpg`), checked against its stored sha256, when the bridge job has `SCAN_ARCHIVE_URL`;
2. the archive's JP2 of the page: its stored IIIF base, then the JPEG from tile.loc.gov (one request; the JP2 itself is not read);
3. each batch in turn on tile.loc.gov's storage service: the bag manifest and the issue's METS (kept in `<campaign>/batch-cache`) give the IIIF base, then the JPEG;
4. none: the page is marked `missing`.

Requests to LoC go one at a time, at least 5 s apart, with no retries, as for the assessment (docs/notes/2026-10-16-boinc-assessment.md, "Where the inputs come from"). The job's `input.url` is always the page's tile.loc.gov JPEG URL, wherever the bytes came from.

To clear a hold, read `<campaign>/HOLD`, deal with the cause, and delete it (on the share, for example with `az storage file delete`).

### Where units live

A campaign started after ADR-0026 (docs/design/adr/0026-source-qualified-page-ids.md) keeps each unit under its page id's path: `units/loc/<doc_id>` for a page, `units/loc/<doc_id>.<crop_id>` for a crop, and `units/sample/<dataset>/<sample_id>` for a benchmark sample, so a page and a sample can't share a directory. Staging marks such a campaign with `<campaign>/units-layout`. A campaign staged before that has no marker and keeps `units/<doc_id>`, `units/<doc_id>.<crop_id>` and `units/<sample_id>`: resumed staging writes there, and submit, collect, summary and `gold-hyp` read either layout. Workunit names stay `usnm_surya_<job id>`, a sha256, whatever the unit's path.

### The same pages and crops as the quality campaign (#968)

To compare Surya with Qwen, Tesseract and American Stories on the same text, stage from the quality campaign rather than a hand-written page list:

- `usnm-boinc surya stage --sample <sample.jsonl> --group <name>` takes the rows `quality sample` drew (one page per row, in its batch) as pages of `<name>`, and keeps each row in its unit as `sample.json`, so a collector can weigh the page as the quality campaign does. `--page` still works, alone or alongside.
- `usnm-boinc surya stage-crops --quality-root <root> --root <campaign>` writes one unit per crop of every staged `usnm_quality` page: the crops the page's `usnm_qwen` unit reads (`crop_seed` and `k_crops` from its `quality.json`, the recipe in `crates/usnm-ocr-quality/src/crops.rs`), cut here from the staged `page.jpg` as JPEGs at quality 95. The unit is `units/loc/<doc_id>.<crop_id>`; its job carries `crop` (`id`, `box`, `stratum`), reads `whole` only, with one deadline (`--timeout-secs`, 600 s by default), and its `input.url` is the page's source with the box as an `#xywh=` fragment. A page whose ALTO has no line the recipe can cut gets a `loc/<doc_id>.no_crops` marker.

On the bridge, the steps are `surya-stage` with `SURYA_SAMPLE=<share>/quality/<campaign>/sample.jsonl SURYA_GROUP=<group>` (instead of, or beside, `SURYA_PAGES`) and `surya-stage-crops` with `QUALITY_CAMPAIGN=<campaign>` (`SURYA_MAX` pages, `SURYA_TIMEOUT_SECS`); both then go through `surya-submit` as any unit. The batch controller can run `surya-stage-crops` and `surya-submit` for you whenever the `usnm_surya` queue runs low: see [Surya waves](boinc-batch-scheduler.md#surya-waves).

`usnm-boinc surya collect --root <campaign> --spool <assimilator results> --texts-dir <campaign>/texts` reads the canonical results back, page units and crop units alike, and writes `<doc_id>.surya.json` for every page with a read: the page's text as the reader `ours-surya`, from the first of `--modes` (`whole,capped,tiled` by default) that read the page, with `surya:<mode>:<job id>` as its source. `quality stage --surya-texts <that directory>` then joins it into each unit's `texts.json` when `--readers` names `ours-surya`, so the hat on the host estimates Surya's page error alongside the other readers'. `quality collect --surya-root <campaign> --surya-spool <results>` joins the crop units' texts into `crops.parquet` as the reader `surya`, in the crop-level hat with LoC, the vision-language reading and every reader with boxes. On the bridge: `surya-collect` with `SURYA_SPOOL`, and `SURYA_CAMPAIGN` on `quality-stage` and `quality-collect` (with `SURYA_SPOOL`).

### Surya on the gold crops (#968)

The OCR gold set ([ocr-gold-set.md](ocr-gold-set.md)) is 60 pages with one seeded box each. The 40 Czech, Serbian, Polish and Russian crops are out for transcription, and Surya reads the same boxes so that, when the transcriptions come back, it gets a character error rate against them.

`usnm-boinc surya stage-gold --manifest fixtures/ocr-gold/cze-srp-pol-rus/manifest.jsonl --root <campaign>` writes one crop unit per manifest row, in file order, up to `--max-crops` (50 by default). The unit is `units/loc/<doc_id>.<crop_id>`, with `doc_id` in the project's form (`sn83045348_1876-03-07_ed-1_seq-1` for the manifest's `sn83045348_1876-03-07_ed1_seq1`). It holds `page.jpg` (the box, cut as `stage-crops` cuts, JPEG at quality 95), `surya.json` and `gold.json` (the manifest row, as it was). The job has group `gold`, the row's language, `crop` with the row's `crop_id`, its box and the stratum `gold/<lang>` (`gold/cze`), `whole` mode only and one deadline (`--timeout-secs`, 600 s by default), and its `input.url` is the row's `page_image_url` with the box as an `#xywh=` fragment.

The pages are fetched from `page_image_url` on tile.loc.gov, once per page however many rows share it, one request at a time and at least `--gap-secs` (5 or more) apart, with no retries; a 403, 429, 5xx or an HTML answer writes `<campaign>/HOLD` and stops, as `surya stage` does. `--pages-dir <dir>` reads `<dir>/<page_id>.jpg` instead, for anyone who already has the pages. Each page's sha256 is checked against the row's `page_image_sha256`, because the gold set pins its pixels by that checksum. A page that differs gets a `<doc_id>.<crop_id>.mismatch` marker with one line saying so, and its crops are not cut; a page LoC answers 404 for gets a `.missing` marker. Neither marker is tried again until a person deletes it. A unit with a `surya.json` is left as it is, so a second run stages only what is still to do. The command prints how many crops it `staged`, found `already` staged, found `mismatch` and how many pages were `missing`.

On the bridge, `surya-stage-gold` takes `SURYA_GOLD_MANIFEST=<a manifest.jsonl on the share>`, the three `SURYA_ENGINE_*` pins, `SURYA_MAX` (crops, 50 by default) and `SURYA_TIMEOUT_SECS` (600 by default), and then goes through `surya-submit` as any unit.

`usnm-boinc surya gold-hyp --root <campaign> --spool <assimilator results> --out <file.jsonl>` writes one `{"crop_id", "text"}` line per gold crop unit with a successful `whole` read, sorted by `crop_id`, and prints how many crops had a read and how many did not (still out, given up on, or failed). On the bridge, `surya-gold-hyp` with `SURYA_SPOOL` writes `<campaign>/gold/surya.jsonl`. Once the transcriptions are filled in, score it with:

```sh
scripts/ocr-survey/gold.py score --crops fixtures/ocr-gold/cze-srp-pol-rus/crops.jsonl \
  --hyp surya=<campaign>/gold/surya.jsonl
```

### Wave 1 runbook (22 October 2026)

The first wave on the on-prem server, in order. Everything runs through `scripts/boinc/onprem-step.sh --env prod --target bridge` on the BOINC host (`--wait-mins 30` unless noted); the controller's ssh key may run only the two steps the [Surya plan](boinc-batch-scheduler.md#surya-waves) takes.

Before the first step, three things must hold:

1. The bridge image (`SERVER_IMAGE` in `/etc/usnm-boinc/server.env`) is built from `main` at `6fdf525` or later: `surya stage --sample`, `surya stage-crops` and `surya collect` came in [#970](https://github.com/tgoodyear/usnewsmap.com/pull/970) and [#973](https://github.com/tgoodyear/usnewsmap.com/pull/973), and `surya-submit` dropping the status page's cache in [#978](https://github.com/tgoodyear/usnewsmap.com/pull/978).
2. `usnm_surya` 1.4 (`docker_mt`, app version 58) and 1.5 (`docker_nvidia_cuda`, app version 59) are the active versions, both from worker image `sha256:bccf7cba49878481d394f94f9ca313787d8f28886e369009ae4cfe94859fb494` (`main` at `614756c`, with the device fix of [#965](https://github.com/tgoodyear/usnewsmap.com/pull/965)). 1.2 and 1.3 are deprecated.
3. A quality campaign `<Q>` is staged on the share with `quality/<Q>/sample.jsonl` and its units (the quality-wave staging of [#962](https://github.com/tgoodyear/usnewsmap.com/issues/962)); the Surya pages and crops are cut from those units, so nothing is staged for Surya before it.
4. For the crop comparison against the vision-language reading, a Qwen campaign `<QW>` staged from the same `<Q>` units (`qwen-stage` with `QUALITY_CAMPAIGN=<Q>`), with results back. Without it `quality-collect` reads an empty Qwen root and the crop table has no `vlm` reader.

The engine pins are the same for every CUDA Surya worker image published since 14 October, so units staged with them run on 1.4 and 1.5 alike (the worker checks the llama image, the Surya version and the weights against its own environment, not the image digest):

```sh
SURYA_ENGINE_IMAGE=sha256:bccf7cba49878481d394f94f9ca313787d8f28886e369009ae4cfe94859fb494
SURYA_ENGINE_LLAMA=sha256:b0baccbe2e00b5d74e63f85f9f023c93d61d0f329475269e1e7002e87eb875ea
SURYA_ENGINE_WEIGHTS=sha256:557aca6f56e5844007dbb04fe69a638f40e8d55823e89de07a1d4906c291b701
```

Do not take the pins from a `surya-vulkan-*` image: its llama image differs, and no Vulkan app version is published.

The steps, with `wave1` as the Surya campaign:

```sh
# 1. Page units for the quality campaign's pages (the page-level hat gets the ours-surya reader).
onprem-step.sh --env prod --target bridge --step surya-stage --var SURYA_CAMPAIGN=wave1 \
  --var SURYA_SAMPLE=/share/quality/<Q>/sample.jsonl --var SURYA_GROUP=sample \
  --var SURYA_ENGINE_IMAGE=$SURYA_ENGINE_IMAGE --var SURYA_ENGINE_LLAMA=$SURYA_ENGINE_LLAMA \
  --var SURYA_ENGINE_WEIGHTS=$SURYA_ENGINE_WEIGHTS --var SURYA_MAX=50 --wait-mins 30
# 2. Crop units at the Qwen boxes (the crop-level hat compares surya, vlm, loc and the rest on the same text).
onprem-step.sh --env prod --target bridge --step surya-stage-crops --var SURYA_CAMPAIGN=wave1 \
  --var QUALITY_CAMPAIGN=<Q> --var SURYA_ENGINE_IMAGE=$SURYA_ENGINE_IMAGE \
  --var SURYA_ENGINE_LLAMA=$SURYA_ENGINE_LLAMA --var SURYA_ENGINE_WEIGHTS=$SURYA_ENGINE_WEIGHTS \
  --var SURYA_MAX=50 --var SURYA_TIMEOUT_SECS=600 --wait-mins 30
# 3. Submit. Either once by hand, or leave it to the controller (below).
onprem-step.sh --env prod --target bridge --step surya-submit --var SURYA_CAMPAIGN=wave1 --var SURYA_MAX=200 --wait-mins 30
# 4. As results come back: read them, then join the crop units into the quality campaign's
#    crops.parquet beside the Qwen reading (QWEN_CAMPAIGN and QWEN_SPOOL, or there is no vlm reader).
onprem-step.sh --env prod --target bridge --step surya-collect --var SURYA_CAMPAIGN=wave1 \
  --var SURYA_SPOOL=/project/usnmocr/results/0 --wait-mins 30
onprem-step.sh --env prod --target bridge --step quality-collect --var QUALITY_CAMPAIGN=<Q> \
  --var QUALITY_SPOOL=/project/usnmocr/results/0 --var QWEN_CAMPAIGN=<QW> \
  --var QWEN_SPOOL=/project/usnmocr/results/0 --var SURYA_CAMPAIGN=wave1 \
  --var SURYA_SPOOL=/project/usnmocr/results/0 --wait-mins 30
# 5. The page-level hat with ours-surya. quality-collect joins crops only, and quality-stage skips a
#    unit that exists, so the page reader needs a fresh quality campaign over the same sample, staged
#    after surya-collect has written wave1/texts: <Q>-surya reads <Q>'s sample.jsonl with the Surya
#    texts joined into each unit's texts.json.
onprem-step.sh --env prod --target bridge --step quality-stage --var QUALITY_CAMPAIGN=<Q>-surya \
  --var QUALITY_SAMPLE=/share/quality/<Q>/sample.jsonl --var QUALITY_READERS=loc,as,ours-tesseract,ours-surya \
  --var SURYA_CAMPAIGN=wave1 --var QUALITY_ENGINE_IMAGE=sha256:<quality worker digest> --var QUALITY_MAX=50 --wait-mins 30
onprem-step.sh --env prod --target bridge --step quality-submit --var QUALITY_CAMPAIGN=<Q>-surya --wait-mins 30
onprem-step.sh --env prod --target bridge --step quality-collect --var QUALITY_CAMPAIGN=<Q>-surya \
  --var QUALITY_SPOOL=/project/usnmocr/results/0 --wait-mins 30
```

`/share` stands for the bridge's `SHARE_DIR`; `SURYA_SAMPLE` and `QUALITY_SAMPLE` must be paths under it. Page units take 40 minutes each on CPU, so `SURYA_MAX=50` on step 1 is a day of CPU work for the queue; crop units take seconds. Steps 1 and 5 are optional for the crop comparison: without them the `ours-surya` page reader is simply absent and the crop-level hat (step 4) still runs. `<Q>-surya` holds the same pages as `<Q>` with one more reader, so the assessment database carries both campaigns and the rollup counts a page once per campaign.

To keep the crop queue topped up instead of running step 3 by hand, give the controller this plan (`step_command` ends with `--`, as [boinc-onprem.md](boinc-onprem.md) explains):

```json
{
  "schema_version": 1,
  "env": "prod",
  "plan": "surya-wave1",
  "app": "usnm_surya",
  "low_watermark": 100,
  "high_watermark": 400,
  "server_status_url": "https://boinc.usnewsmap.com/server_status.php?xml=1",
  "step_command": ["ssh", "-i", "<key>", "-o", "BatchMode=yes", "usnm-boinc-step@<host>", "--"],
  "surya": {
    "campaign": "wave1",
    "quality_campaign": "<Q>",
    "engine_image": "sha256:bccf7cba49878481d394f94f9ca313787d8f28886e369009ae4cfe94859fb494",
    "engine_llama": "sha256:b0baccbe2e00b5d74e63f85f9f023c93d61d0f329475269e1e7002e87eb875ea",
    "engine_weights": "sha256:557aca6f56e5844007dbb04fe69a638f40e8d55823e89de07a1d4906c291b701",
    "timeout_secs": 600,
    "stage_pages": 50,
    "submit_units": 200
  }
}
```

The watermarks count results (two per unit): at or below 100 unsent the controller stages crops for 50 more pages and submits up to 150 units, never past 400 unsent. Run `batch_scheduler.py --config surya-wave1.json --state-dir <dir>` once without `--apply` to see the steps, then with `--apply` on the controller's schedule. The pins are held to the campaign: a new engine needs a new `campaign` name.

After steps 4 and 5, `scripts/build-quality-assessment-db.py` over the campaign's `pages.parquet` and `crops.parquet` writes `reader_language_summary`, one row per language and reader, which is the apples-to-apples table [#968](https://github.com/tgoodyear/usnewsmap.com/issues/968) asks for.

A crop unit takes seconds rather than the 40 minutes a tiled page takes on CPU, so every plan class can run it. `submit` gives it a hundredth of a page's fpops estimate and bound, an 8 GiB memory bound and a one-day delay bound, at the same quorum of two. `check` and `compare` treat it as any unit: `text-whole.txt` is the crop's text. A page unit's job is unchanged, and keeps the identity it had before crops existed.

`surya-summary` prints a table of every kept result (page, client, plan class, runtime, GPU, and per mode the outcome, device, time, memory) and every compared pair, and writes it to `<campaign>/summary.md`. A unit left at `submitting` is resolved as for the assessment.

To check a host or the image without LoC or BOINC, make a unit from any image and run it:

```sh
docker run --rm -v "$PWD/u:/work" IMAGE usnm-boinc surya local-unit --image /work/in.jpg --dir /work --modes whole
docker run --rm --network=none --read-only --tmpfs=/tmp:rw,size=1g --gpus all -v "$PWD/u:/work" IMAGE
```

## Not verified

- That rootless Podman with a CDI specification gives the container the GPU through the stock wrapper (the probe's question).
- That llama.cpp's CUDA build falls back to its CPU backends on a host without a GPU other than the CI runner.
- Peak memory and time on volunteers' hosts, and so the plan classes' limits.
- Quality against LoC and the gold set for a stratified multi-hundred-page sample.

## Bounded immutable-input probe (#812)

The registries serve different purposes, just as for Qwen and Iris. Volunteers
pull the **public GHCR worker** by digest. The private production ACR
`crusnmprodffapzy` holds signed **app-version payloads** and server/bridge images.
Do not mirror the worker into private ACR: anonymous volunteers cannot pull it.
`images.appver` is the last publisher replay image, not a catalog of all registered
versions. An older app's artifact need not equal that pin. Anonymous `apps.php`
redirects to login; an empty 302 body is not evidence that an app is absent.
Check the production `app`/`app_version` rows and their signed Dockerfile downloads
before releasing either version. Also inspect existing version/platform directories:
absence from the DB does not permit overwriting an immutable signed payload.
On 18 October both Surya 1.0/1.1 directories existed with the older `84c1069f…`
worker while neither version was registered. A new worker requires an unused,
reviewed version pair; do not delete or replace the old signed directories. Use the normal signer; stop if locked or absent.

`surya-benchmark-stage` accepts a pinned bundle, without fetching or transforming
its images. Prepare the same two v1.0.0 smoke subsets used by the candidate trials:

```sh
scripts/ocr-bench/fetch --version v1.0.0 --verify --dest benchmark
python3 scripts/boinc/surya-benchmark.py prepare \
  --release benchmark/usnm-ocr-bench-v1.0.0 --out surya-inputs
```

The helper checks the committed release-manifest and subset digests and every
selected image/reference hash. It keeps upstream provenance and `LICENSES`,
including the English reference NC attribution. Copy the bundle, retaining its
layout, to `<share>/surya-inputs/<campaign>/` through the existing authorized
operator transport. Keep references private. `inputs.json` is an array of
`sample_id`, `dataset` (the release's `source`, such as `overproof-ca`), `group`,
`language`, relative `image`/`reference` paths and their
`image_sha256`/`reference_sha256`, `source_url`, and `license`. Each unit's doc id
is `sample:<dataset>/<sample_id>`; staging refuses a bundle with no `dataset`, so
prepare older bundles again. The SHA256 of its
exact bytes, printed by prepare, is required by staging:

```sh
scripts/start-boinc-bridge.sh --env prod --step surya-benchmark-stage \
  --var SURYA_CAMPAIGN=probe-812-gpu --var SURYA_BENCHMARK_SHA256=MANIFEST_SHA256 \
  --var SURYA_ENGINE_IMAGE=sha256:WORKER --var SURYA_ENGINE_LLAMA=sha256:LLAMA \
  --var SURYA_ENGINE_WEIGHTS=sha256:WEIGHTS --wait-mins 15
```

The equivalent local command is `usnm-boinc surya benchmark-stage --manifest FILE
--manifest-sha256 SHA256 --root ROOT --engine-image sha256:WORKER --engine-llama
sha256:LLAMA --engine-weights sha256:WEIGHTS`. Staging validates all inputs before
writing, refuses an existing campaign, and writes a completion marker. Submission
rechecks the pinned manifest, every reference/image and job. Only `page.jpg` (the
original image bytes, never re-encoded) and `surya.json` enter the download tree.
The existing v1 worker's `https://tile.loc.gov/local` sentinel is retained for
local inputs; it is **not a provenance URL and is never fetched**. Actual source
URLs and license evidence remain in `benchmark-inputs.json` and the bundle.

For a small exact-host probe, set `SURYA_TARGET_HOST` on `surya-submit` (or
`surya submit --target-host ID`). This reuses stock `create_work --target_host`
and requires the real `enable_assignment=1` server setting from #719. It uses
**quorum one**, target one, maximum one result, no fallback/retry hosts, and at
most 30 units per campaign. The campaign lock, durable target policy and existing
`submitting` marker prevent concurrent dispatch, retargeting and blind retries.
Targeted workunit names include the host ID so separate CPU and GPU probe
campaigns can share immutable job identities. Untargeted campaigns retain the
original quorum-two behavior.

This fallback deliberately does **not** claim GPU+CPU quorum: the stock single
`--target_host` option cannot specify one replica per device, while ordinary
quorum-two scheduling cannot guarantee the pair. The per-result Surya check
script remains unchanged and keeps every returned result in `surya/seen`.
Quorum-one results do not generate validator comparison pairs; report
`pairs.jsonl` as absent/empty rather than claiming cross-host consensus. Separate
validated CPU/GPU outputs can be compared explicitly with `surya compare --log`
and labeled as post-collection comparison, not quorum validation.

Keep the total dispatch around 20–30 units: for example 20 GPU smoke inputs,
three LoC pages (one each Serbian, Russian and worst-English, through ordinary
`surya-stage` into a **separate** campaign), and three pinned repeat inputs on a
known eligible CPU host, each with its own target/campaign. The CPU repeat bundle
must explicitly pin its three selected members and retain original attribution.
Do not append LoC units to an immutable benchmark campaign. Recheck the #529
claim, current GPU reservations on #794, host idleness, signed-image equality,
client plan eligibility, memory/disk bounds and existing preferences before
submission. A prepared bundle or passing CI is not dispatch authorization.

Export only validator-kept results for the existing scorers:

```sh
python3 scripts/boinc/surya-benchmark.py collect --root CAMPAIGN \
  --seen PROJECT/surya/seen --out collected --binary usnm-boinc --intended-device cuda
usnm-ocr-survey bench-score --release RELEASE --subset overproof-en/smoke-10 \
  --outputs collected/cuda-tiled-english.jsonl --rule legacy-ws-v1
usnm-ocr-survey bench-score --release RELEASE --subset overproof-en/smoke-10 \
  --outputs collected/cuda-tiled-english.jsonl --rule raw-v1
```

Repeat for each device/mode and `reichsanzeiger-de/smoke-10`. Collection runs the
same structural/checksum check script again, verifies engine equality and rejects
duplicate successful outputs instead of silently selecting a favorable repeat.
The receipt retains every failure, timing, measured RSS/GPU peak and missing
sample. Use scorer missing-as-empty metrics as well as successful-output CER.
LoC outputs are baseline comparisons, not independent gold CER. No routing,
queue, volunteer preference, trust, or account policy is changed by this probe.

The authorized current-worker probe version pair is **1.2 (`docker_mt`) and
1.3 (`docker_nvidia_cuda`)**, subject to a fresh unused-version check. The old
1.0/1.1 payloads remain retired/unregistered and immutable; do not publish or
replace them. Use the same normal `boinc-app-release.sh` commands above with
these new version numbers and the requested worker digest.

### Scoped signed publication without runtime rollback

When reviewed main differs from the deployed stack, do not run a full-stack
provision to release this probe. Under the #529 claim and ordinary local mutex /
controller lease, first verify **all six lifecycle jobs idle**. Prepare each new
version with `scripts/boinc-app-release.sh --prepare-only` and the normal signing
host. Review and merge the resulting immutable appver pin before publication.
Do not reuse a locked signer or change existing signed payloads.

For publication, save the existing publish job's complete `properties.template`;
copy it as an execution-only YAML/JSON document (JSON is YAML), change only its
container image to the approved signed appver digest, and add
`APPVER_REUSE_PROJECT=1` to that container's existing environment. Preserve all
other environment, identity/secret references, resources and volume definitions.
Start the existing publish job with `az containerapp job start --yaml FILE`;
**never update its persisted template** or deploy the stack. The reviewed scoped
publisher fetches credentials into its own execution-local memory and creates a
minimal registration-only config there. It leaves shared project config/key
symlinks, scheduler configuration and `project.xml`/other app registration untouched, while preserving signature checks, immutable-directory
checks, stock `update_versions` and exact app-version DB verification. The app
must already exist; this mode does not create it. This handles the deployed
layout where shared config/key symlinks point into each container’s private
`/dev/shm/boinc`, not the web replica’s memory.

Check the actual execution's image, environment and volumes match the intended
full template, wait for success, then independently verify DB registration,
Dockerfile worker digest/signature and anonymous downloads. If execution is
uncertain, retain the serialization lease and reconcile rather than retrying.
Release the claim/lease only after terminal status and verification. Keep current
live server/bridge pins/settings unchanged; the appver release pin alone does not
authorize replacing them.

The NVIDIA-only exact-host variant also sets `SURYA_GPU_PROBE=1` (CLI
`--gpu-probe`): it pins `--app_version_num 103` (1.3) and reduces the shared
memory admission bound to **8 GiB**, matching `job-gpu.toml`. This cannot fall
back to CPU version 1.2. Untargeted/CPU campaigns retain the 12 GiB CPU bound.
CT214 reports 12 GiB RAM and two CPUs; the CPU plan needs four usable CPUs.
On 18 October at 14:10 EDT the maintainer explicitly authorized temporarily
raising CT100 from 8 to 12 GiB for this probe, with restoration afterwards;
its original configuration is memory 8192 MiB, swap 1024 MiB and 12 cores.
Record the original configuration and verify both the cgroup and BOINC client's
reported memory before dispatch. This authorization does not lower the CPU
workunit bound or change BOINC memory preferences. CT100 currently allows BOINC
90% of RAM, so even 12 GiB physical memory provides only 10.8 GiB for admission,
below the unchanged 12 GiB bound. Stock targeted delivery skips ordinary server
feasibility checks, but does not bypass the client's memory gate: for a new app
version with no RSS history, `cpu_sched.cpp` uses the workunit memory bound as
its initial estimated RSS. A running client may also retain its original
8 GiB host-info cache after a live cgroup increase. Never restart a client with
active unrelated tasks to force a refresh. If safe CPU eligibility cannot be
established, restore the original memory and report CPU measurements unavailable
rather than fabricate cross-device acceptance. Any subsequent change needs its
own authorization and rollback receipt.

Scoped app-version publication does not recursively change ownership of the live
`download/` tree. Stock registration creates the new public, readable downloads;
walking unrelated historical downloads on Azure Files can outlast the publisher
timeout after successful registration. Scoped ownership normalization covers only
the released payload and templates; ordinary full publication retains its prior
ownership behavior. Always reconcile exact DB registration and downloaded signed
payloads before retrying an execution that times out after registration.

Actual validator-kept directories include BOINC's optional output-template
random token, for example `HOST_SUFFIX_r2014260292_3`. Collection accepts that
numeric `r` token after the exact host/job and PID/timestamp/sequence prefix;
it does not broaden matching to another host or arbitrary suffix text.
