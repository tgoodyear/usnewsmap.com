# OCR quality on BOINC

Issues: [#522](https://github.com/tgoodyear/usnewsmap.com/issues/522), [#533](https://github.com/tgoodyear/usnewsmap.com/issues/533), [#559](https://github.com/tgoodyear/usnewsmap.com/issues/559). Two apps estimate, for every page and with no transcription, how accurate and complete each of our texts is and what the scan looks like. `usnm_quality` (CPU) compares the texts; `usnm_qwen` (GPU, with a CPU fallback) adds a vision-language reading of sampled crops as one more reader. Both ship as native executables (no container runtime needed) and as container versions; see [App versions and plan classes](#app-versions-and-plan-classes). The [transcription post-training pilot](../../notes/2026-10-16-boinc-transcription.md) reuses `usnm_qwen` for held-out public-gold inference. Other apps: [boinc/README.md](../README.md), [boinc-assessment.md](../../notes/2026-10-16-boinc-assessment.md), [boinc-surya.md](surya.md).

Status: implemented and tested locally; new split-payload releases are UNVERIFIED on the live server and volunteer hosts.

## Repeatable German comparison report (#926)

`boinc/scripts/german-quality-report.py` replaces the external experiment's `perpage.py`, `order.py` and the reporting part of `crops.py`, reusing the collector's metrics. Standard-library Python, offline:

```bash
python3 boinc/scripts/german-quality-report.py \
  --sample german-sample.jsonl \
  --pages pages.jsonl --crops crops.jsonl \
  --reader loc --reader ours-tesseract --reader tess-deulatf \
  --max-crops-per-page 12 --out german-report.json
```

`--sample` is the frozen `quality sample` JSONL (`doc_id`, `lang=deu`), kept apart from results (a labelled retrospective run may use the original **input** cohort; deriving it from result rows hides missing pages). `--pages` and `--crops` are the collector's tables exported to JSONL unchanged. `--max-crops-per-page` must match the plan's `k_crops`, which is only an upper bound: unused crop capacity is **not missing work**, and unobserved staged crops stay unknown without the staging manifests. Name every expected reader, including `loc` or `loc-hist` for reference counts; use `--crop-reader surya` for a crop-only reader.

The create-only report binds sample, exports and reporter source by SHA-256 and gives per-title/year and per-page word-bag Dice, CER/WER and crop CER/Dice against the VLM. These are **model agreement, not human CER or calibrated accuracy**, and crop scores keep the collector's normalization, not old `crops.py`'s. Rules:

- Distinct comparator models and distinct sources of one reader (`by_source`) are never pooled; model IDs like `qwen3-vl-8b-q4km` are metadata, **not file digests**.
- Conflicting page job IDs are rejected; both may be absent in older exports, but one known and one unknown are not the same job. Crop rows must match the page's job, source, rectangles and strata, or they are **unverified** and excluded.
- Absent reader rows are **unknown outcomes** (failed and timed-out units emit none), so keep a separate unit-outcome inventory. The collector keeps completed whitespace-only readings and exports `vlm_reading_present` (not used in scoring or hats); legacy null text without it is **ambiguous**.
- Means use only rows with known nonempty reference counts (`vlm_reference_chars`/`vlm_reference_words` for crops, nullable Int64); older exports have **unknown denominators**. Summaries are unweighted; reader `total_ms` is kept as reported, not summed into a campaign duration; `loc-hist` self-scores are dropped when `loc` is present. Layout and hat-derived accuracy metrics are omitted.
- Languages are reported per reader row; a historical `und` score is not relabelled German.

This is reporting only, **not `build_units.py` or a finished 100-page benchmark**: stage with the native `quality sample`, `texts-export`, `stage` and `collect`, and never copy our ALTO to `loc.xml` as LoC's layout (the external script did); curated LoC text without original ALTO needs an explicitly provenanced native path.

To close #926: freeze about 100 pages across titles, years and layouts (with #925's reading-order cases), pin native app, model and llama artifacts independently of Docker identities, keep source and input hashes and measured wall/CPU/memory, rerun and review missing coverage. The 12-page, one-title reproduction proves only the reporting path; accuracy claims need human-labelled crops.

## What a unit is

### usnm_quality

One page per workunit. The host checks every input's checksum before running, and the unit always writes all three outputs. Floats are rounded to six decimals and the computation is single-threaded over ordered maps, so two hosts' records are identical bytes.

| File                    | Contents                                                                                                                                                                                                                                                                                                                               |
| ----------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `page.jpg` (in)         | full-size IIIF JPEG                                                                                                                                                                                                                                                                                                                    |
| `loc.xml` (in)          | LoC's ALTO; used for features and crops                                                                                                                                                                                                                                                                                                |
| `texts.json` (in)       | `quality::Texts`: `{"page": {"width", "height"}, "readers": [{"name", "source", "text", "regions": [{"box": [x0, y0, x1, y1], "kind", "legibility"}], "alto": false}]}`; every reader the job names once, same source; every reader's text (LoC's too) comes from here                                                                 |
| `places.txt` (in)       | the campaign's, one sticky file for all units, `usnm_quality_places_<sha256>`                                                                                                                                                                                                                                                          |
| `quality.json` (in)     | `usnm_ocr_contract::quality::Job`: `v` (1), `id` (sha256 of the fields), `doc_id`, `language`, `group`, `inputs` (`page`, `alto`, `texts`, `places`, each `{bytes, sha256, source}`), `engine {image}`, `readers` (`[{name, source}]`, report order), `function_words`, `crop_seed`, `k_crops`                                         |
| `result.json` (out)     | `quality::ResultManifest`, <= 256 KiB: `v`, `job_id`, `engine`, `outcome` (`ok`, `timeout`, `failed`, `unit_error`), `unit_error`, `page {image, layout}`, `readers`, `hat`, `legibility`, `timings {decode_ms, features_ms, text_ms, total_ms, cpu_ms, peak_rss_kib}`, `host`, the other outputs' checksums and sizes, `completed_at` |
| `confusions.json` (out) | reader name to its top 50 confusion pairs, <= 256 KiB                                                                                                                                                                                                                                                                                  |
| `diag.txt` (out)        | <= 64 KiB                                                                                                                                                                                                                                                                                                                              |

Per reader in `result.json`: `name`, `source`, `chars`, `words`, `est_cer` (clipped at 0), `est_cer_raw`, `est_cer_low_bound_note`, `completeness`, `precision`, `findable_content`, `findable_places` (`content_found`, `content_total`, `places_found`, `places_total`), and `pairs` against every other reader: `cer_a` (this reader as hypothesis), `cer_b`, `cer_sym` (edits over mean length), `char_edits`, `chars_a`, `chars_b`, `wer_a`, `wer_b`, `word_edits`, `words_a`, `words_b`, `dice`. Rates without a denominator are null.

### usnm_qwen

One page per workunit.

- `qwen.json` (`usnm_ocr_contract::qwen::Job`): `v`, `id`, `doc_id`, `language`, `group`, `input`, `crops` (`[{id, box, stratum, loc_text}]`, <= 64; the quality unit's crops, LoC's words in visual line order), `engine {image, llama, models: [{name, file, mmproj, min_gpu_mb}], weights}`, `prompt_version`, `gpu_timeout_secs`, `cpu_timeout_secs`, `max_tokens_per_1000px2`. Plus `page.jpg`.
- `readings.json` (`qwen::Readings`): `v`, `job_id`, `outcome`, `device`, `model`, `prompt_version`, `readings: [{id, text, tokens, ms}]`, `diag`.
- `result.json` (`qwen::ResultManifest`): outcome, device, model, times, crops read, host, checksums of the other two. Plus `diag.txt`.
- Outcomes: `ok`, `timeout` (crops read so far kept), `failed`, `no_gpu` (GPU version, no visible GPU), `unit_error`.

`prompt_version` fixes the whole request:

| Version | Encoding                                                                                   | Loop guard |
| ------- | ------------------------------------------------------------------------------------------ | ---------- |
| 1       | llama.cpp default: native resolution, one token per 32x32 px (about 200-400 tokens a crop) | none       |
| 2       | `llama-server --image-min-tokens 1024` (Qwen-VL's minimum); crop upscaled to it            | none       |
| 3       | as 1                                                                                       | `stop`     |
| 4       | as 2                                                                                       | `stop`     |

On #529 version 2 raised image-checked Yiddish words from 23% to 59% and left English, Serbian and repeated-print crops as they were. The loop guard (Study 5's `stop` rule) streams the request, checks `tail_repetition` every eight tokens from the twelfth, and on a hit closes the connection (which stops llama-server), keeping the text before the repeated run plus one period. Cut readings, including printed repeats at a natural end, record the raw digest and length and `repetition_loop_guard_cut`; `tokens` is then the count of text events (`loop_guard.stream_deltas`, `completion_tokens_reported` empty). Versions 3 and 4 need app version 1.17; older versions refuse them.

`qwen stage --prompt-version` and `qwen_submit.py --prompt-version` choose it (default 1). `qwen_submit.py` defers a chunk (exit 75, READY kept) while any dispatchable app version can't read the version, and refuses a second version in one root. The feed stages 3, and 4 for Yiddish ([Request versions on the feed](#request-versions-on-the-feed)).

The refill reads `qwen-nonenglish-v1` unless `CAMPAIGN_ARGS` in its env names roots as `--campaign NAME:VERSION` (repeatable; each needs an `audit.json`, which can start as `{}`). Bundled roots (`qwen stage --bundle-size` above 1) are bound a bundle at a time (unit, `membership.json`, each page's job, image slice, quality record, provenance, sample) and only if every page's language is admitted. `--max-admissions` and `--max-inflight-per-host` count units. A unit passed over for its language is bound once that language is admitted. Admitted languages: Yiddish and Serbian, plus repeatable `--cohort-language LANG`, capped by the feed's budget.

Under versions 1 and 2 readings are kept as read. `diag.txt` holds each crop's generation record (`generation_v1/<crop>`, schema 2, or 3 with `loop_guard`) with `tail_periodic_repetition` (output ends in a run repeated three or more times over at least 30 characters) and `repetition_loop_at_token_budget` (that run also hit the budget; printed text can repeat, a looping model runs out of tokens). The readings' `diag` gives `loop_guard` (`stop`/`none`), `repetition_loop_crops` (names, or a count if they don't fit), `repetition_loop_crop_bits` (hex, always complete; bit `i % 8` of byte `i / 8` is crop `i`) and `image_min_tokens`.

## The estimator

Everything is in `crates/usnm-ocr-quality`, a library with no BOINC in it.

- `align`: edit script with backtrace in linear space (Hirschberg; a full table under 2^18 cells), fixed tie order. Texts are normalised as the gold set does (`usnm_ocr_survey::text::normalize`); character edits use the bit-parallel distance, word edits whitespace-split words, word-bag Dice lower-case alphanumeric tokens.
- `agreement`: two readers with no third (#962): the hat's pair, word-bag cover each way and length ratio, versioned. `usnm-ingest ocr-agreement` runs it for LoC against American Stories ([Text agreement](../../ocr/text-agreement.md)).
- `hat`: `d_ij = e_i + e_j` by least squares (normal equations, Gaussian elimination with partial pivoting); with three readers `e_A = (d_AB + d_AC - d_BC) / 2`. Estimates clipped at 0 (raw kept), residual is the RMS of `d_ij - e_i - e_j`, fewer than three readers or too few pairs gives nulls. The unit solves on whole-page `cer_sym` (hat version 1); `collect` re-solves as [version 2](#the-page-hat-version-2-962) and writes that into `pages.parquet`.
- `consensus`: tokens normalised, whitespace-split, edge punctuation stripped, case-folded; word-level alignment of every reader pair links occurrences, and components spanning at least two readers are consensus words. Completeness: share of consensus words a reader has. Precision: share of its tokens that are consensus occurrences. Substitutions against a missing consensus word feed the confusion counts.
- `findability`: content words have four or more letters and are not function words; places are `places.txt` tokenised the same way, matched as consecutive sequences. Rate: share of the consensus's distinct content words (and places) the reader contains.
- `confusion`: per disagreeing word pair, consecutive non-matching alignment steps merged (`rn` for `m` is one), top 50 per reader.
- `features`: on a grey copy downscaled by an integer factor to <= 1,024 px long side: skew (-3 to 3 degrees in 0.1 steps, maximising projection-profile variance; positive descends right), blur (3x3 Laplacian variance), contrast (95th less 5th percentile), noise (mean absolute 3x3 median residual), bleed-through (share in the middle third of grey between ink and background modes), ink density (share below Otsu). From ALTO: blocks, lines, words, median line height, columns (x-centre clustering with a gap of half the median block width), mean word confidence.
- `crops`: the gold set's recipe (`scripts/ocr-survey/crops.py`): a line of >= 4 words spanning >= 600 px; box from its left and top less 15 px to its right (<= 1,600 px from the left) plus 15 px, down to 6% of page height below the line's top. Six strata (block word-confidence terciles by top/bottom half); `k` crops (12 by default) round-robin over strata, each shuffled by SplitMix64 from the seed.
- `report`: the record, its JSON Schema (`report::schema()`), the rounding.

Unit legibility is `1 - min est_cer` (`basis: page-hat`). The crop-level estimate and the join with `usnm_qwen` readings happen in `quality collect`, since the apps run on different hosts. UNVERIFIED: how far page- and crop-level estimates agree.

## Dual native and container distribution

### The `boinc_finish_called` contract for native binaries

Stock BOINC restarts a task that exits `0` without `boinc_finish_called` in its slot and aborts it after 100 such exits with `ERR_TOO_MANY_EXITS` (`client/app_control.cpp`). Our Rust binaries (`crates/usnm-quality-app`, the native `usnm_qwen` driver) meet the contract without the C++ API or `samples/wrapper`:

- Page and control files carry `<copy_file/>`, so the slot holds real files. Native Qwen's sticky GGUFs are the exception: slot `<soft_link>` aliases to the shared project cache.
- The binary writes `fraction_done` (`0o644`) while running.
- After flushing all outputs and setting `fraction_done` to `1.000000\n`, it atomically writes `boinc_finish_called` (`0\n`) and exits `0`.

### Native `usnm_quality` (`crates/usnm-quality-app`)

Depends only on `usnm-ocr-quality` and the contract types in `usnm-ocr-contract` (no `tokio`, `reqwest`, `arrow`/`parquet`, `rustix`, `usnm-store`). `.github/workflows/boinc-quality-native.yml` builds it `--locked` for `x86_64-pc-linux-gnu` (`x86_64-unknown-linux-musl`, static), `aarch64-unknown-linux-gnu` (`aarch64-unknown-linux-musl`, static), `arm64-apple-darwin` (`aarch64-apple-darwin`), `x86_64-apple-darwin` and `windows_x86_64` (`x86_64-pc-windows-msvc`), runs the golden determinism test on each, and uploads `usnm_quality_<ver>_<platform>[.exe]` plus `SHA256SUMS`. On container hosts (`x86_64-pc-linux-gnu__docker`) it runs in the worker image (`boinc/apps/Containerfile`) under `boinc/apps/job.toml` (1 CPU, 1.5 GiB).

### Native and container `usnm_qwen` (`boinc/apps/qwen/llama.lock` and `<sticky/><no_delete/>` models)

1. **Native driver.** Starts `llama-server` on `127.0.0.1`, ephemeral port (`--no-webui --ctx-size 4096 --seed 42 --temp 0`; `--n-gpu-layers 99` on Metal and CUDA, `0` on `mt`), polls `/health`, posts each crop as a PNG data URL to `/v1/chat/completions`. `boinc/apps/qwen/llama.lock` pins the `llama.cpp` tag (e.g. `b11630`) and per-platform archive URLs and SHA-256s (`macos-arm64`, `macos-x64`, `win-cuda-12.4-x64`, `ubuntu-x64`, `ubuntu-arm64`), verified before staging and signed with the version.
   - Weights are not in app versions. Model and projector (8B: `Qwen3VL-8B-Instruct-Q4_K_M.gguf` 4.68 GiB + `mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf` 0.70 GiB; 2B: `Qwen3-VL-2B-Instruct-Q8_0.gguf` 1.71 GiB + `mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf` 0.41 GiB) are staged in `download/` as `usnm_qwen_model_<sha256>` with Apache-2.0 `LICENSE` and `NOTICE`. The templates (`boinc/apps/usnm_qwen_8b_in`, `boinc/apps/usnm_qwen_2b_in`) mark them **`<sticky/>`** (a host downloads them once) and **`<no_delete/>`** (`file_deleter` never unlinks them).
   - The on-prem server serves them outside Azure, with the four Qwen model files redirected to R2 at `models.usnewsmap.com` ([Qwen model files on R2](../server/onprem.md#qwen-model-files-on-r2)); from Azure they cost about $0.50 of egress per new 8B volunteer.
2. **Container image** (`boinc/apps/qwen/Containerfile`, `weights.lock`, `NOTICE`): on `ghcr.io/ggml-org/llama.cpp:server-cuda` at `sha256:b0baccbe2e00b5d74e63f85f9f023c93d61d0f329475269e1e7002e87eb875ea` (build 11610, `0.6.0-dev`), no Python. **Separate payloads**: `QWEN_MODEL=2b` (default) holds only the 2B Q8_0 pair, `QWEN_MODEL=8b` only the 8B Q4_K_M pair; `USNM_QWEN_MODEL` and the image label name the payload (the full `weights.lock` lists what is allowed, not installed). Both keep the pinned CUDA base (with CPU backends), immutable Hugging Face revisions and SHA-256 checks. The 8B image refuses a missing or too-small GPU rather than fall back to CPU; smaller NVIDIA GPUs can use the native 2B class.
   - `.github/workflows/boinc-qwen.yml` builds and checks both; a PR touching only the crates or their embedded files outside `boinc/apps/qwen/` builds 2b alone, since 8b shares its `usnm-boinc` build (#1585). Only a manually authorized run on `main` publishes `ghcr.io/tgoodyear/usnm-boinc-worker:qwen-2b-<commit>` and `:qwen-8b-<commit>`, each a **different digest**. Releases use anonymous digest-pinned pulls and signed wrapper, Dockerfile and job files; release preflight checks the anonymous config metadata against the plan class's model, base, weights identity and compressed-size envelope.

## App versions and plan classes

Plan classes are in `boinc/project/plan_class_spec.xml`. Since 2 November 2026 the live `usnm_qwen` versions are GPU only (`nvidia_vulkan_8b`, `nvidia_vulkan_6gb_8b`, `amd_vulkan_8b`, `intel_vulkan_8b`, `apple_gpu_8b`). The table lists the platforms each class can be released for; on Windows only `amd_vulkan_8b` has a live version. "All five" platforms: `x86_64-pc-linux-gnu`, `aarch64-unknown-linux-gnu`, `arm64-apple-darwin`, `x86_64-apple-darwin`, `windows_x86_64`. Every 8B class runs `Q4_K_M`, every 2B class `Q8_0`; GPU classes take 2 host CPUs unless noted.

| App            | Kind        | Plan class              | Platforms             | Requirements                                                                                                                                                           |
| -------------- | ----------- | ----------------------- | --------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `usnm_quality` | `native`    | _(default CPU)_         | all five              | `crates/usnm-quality-app`, 1 CPU                                                                                                                                       |
| `usnm_quality` | `container` | `docker`                | all five              | `boinc/apps/job.toml`: 1 CPU, 1.5 GiB, no swap                                                                                                                         |
| `usnm_qwen`    | `native`    | `apple_gpu_8b`          | `arm64-apple-darwin`  | `<gpu_type>apple</gpu_type>`, `min_metal_support` 1, >= 7,000 MB unified RAM (~8 GiB Mac)                                                                              |
| `usnm_qwen`    | `native`    | `nvidia_cuda_8b`        | Linux x86_64, Windows | `<gpu_type>nvidia</gpu_type><cuda/>`, CUDA 12+, compute capability >= 5.0, >= 7,000 MB GPU RAM                                                                         |
| `usnm_qwen`    | `native`    | `intel_vulkan_8b`       | Linux x86_64, Windows | `<gpu_type>intel</gpu_type><opencl/>`, Vulkan, >= 7,000 MB GPU RAM and 7,000 MiB free; integrated GPUs use about 8.6 GB host RAM ([Intel GPUs](intel-gpu.md))          |
| `usnm_qwen`    | `native`    | `nvidia_vulkan_8b`      | Linux x86_64, Windows | BOINC CUDA detection, Intel's pinned Vulkan runtime, >= 7,000 MB GPU RAM and 7,000 MiB free                                                                            |
| `usnm_qwen`    | `native`    | `nvidia_vulkan_6gb_8b`  | Linux x86_64          | Closed: >= 5,500 MB GPU RAM and 5,120 MiB free, 24 GPU layers, CPU projector, no fallback                                                                              |
| `usnm_qwen`    | `native`    | `amd_vulkan_8b`         | Linux x86_64, Windows | AMD/ATI OpenCL detection, Vulkan, >= 7,000 MB and 7,000 MiB free; [automatic admission](amd-gpu.md) after signed publication and promotion, hardware unverified (#885) |
| `usnm_qwen`    | `native`    | `nvidia_vulkan_2b`      | Linux x86_64          | Closed (#1234): >= 4,500 MB GPU RAM and 4,096 MiB free (estimates), BOINC >= 8.2.9, 2B fully on GPU                                                                    |
| `usnm_qwen`    | `native`    | `nvidia_cuda_2b`        | Linux x86_64, Windows | `<gpu_type>nvidia</gpu_type><cuda/>`, CUDA 12+, compute capability >= 5.0, >= 3,000 MB GPU RAM                                                                         |
| `usnm_qwen`    | `native`    | `mt`                    | all five              | CPU, `min_ncpus` 4, `max_threads` 8, 2B                                                                                                                                |
| `usnm_qwen`    | `container` | `docker_mt_2b`          | Linux x86_64          | `boinc/apps/qwen/job-cpu.toml`: 2B, `--cpus` from the thread count (4–8), 6 GiB, no swap                                                                               |
| `usnm_qwen`    | `container` | `docker_nvidia_cuda_8b` | Linux x86_64          | `boinc/apps/qwen/job-gpu.toml`: 8B, `--gpus all`, 2 CPUs, >= 7,000 MB GPU RAM, compute capability >= 6.0, 6 GiB, no swap                                               |

### NVIDIA userspace in an LXC

Native apps need no Docker. Prerequisite: the host operator has already passed the NVIDIA character devices through to an NVIDIA-equipped Debian/Ubuntu LXC. Run inside the LXC, **not on Proxmox**:

```sh
python3 boinc/scripts/setup_nvidia_lxc.py                 # show the userspace package plan
sudo python3 boinc/scripts/setup_nvidia_lxc.py --apply    # install it
sudo python3 boinc/scripts/setup_nvidia_lxc.py --check-only  # diagnose without APT changes (also .run installs)
```

It matches userspace packages to the **host kernel driver** in `/proc/driver/nvidia/version` down to the patch version (Ubuntu's branch-suffixed packages, NVIDIA's unversioned Ubuntu 590+ packages, Debian's separate Vulkan/EGL ICD packages), installs the Vulkan loader, diagnostics and runtime libraries, pins them, and verifies `nvidia-smi`, `libcuda.so.1` and a hardware Vulkan device **as `boinc`** (`--boinc-user NAME` for another non-root account). `nvidia-smi`'s CUDA version is driver compatibility, not an installed toolkit; `nvcc` is not needed.

It never installs kernel drivers, creates devices or changes their permissions, adds repositories or changes BOINC preferences; it refuses to mix runfile and package installs and stops if no exact package set exists. Use the container distribution's packages or the [official NVIDIA repository](https://docs.nvidia.com/datacenter/tesla/driver-installation-guide/ubuntu.html#network-repository-enablement-amd64), never a repository for another distribution. After a host driver update rerun it (it updates `/etc/apt/preferences.d/usnm-nvidia-lxc`), then restart `boinc-client` at a checkpoint to load the repaired libraries and group membership. Passing proves userspace readiness only; signed app publication, client-version admission, model memory limits and successful GPU results are separate.

### Native 8B dispatch windows

Native Intel and NVIDIA Vulkan classes share batch-8 inputs; publish signed Linux versions with the same locked Vulkan bundle first. `boinc/scripts/native-qwen-cohort.php --project PATH --plan-class shared_8b --workunit FIRST --workunit-end LAST` (preview, then `--apply`) opens a shared window over at most ten contiguous native-8B workunits, each unaccepted with two unsent replicas and quorum two. It closes other classes in its scope and pauses the NVIDIA container batch-18 queue without changing its input format or aborting assigned work, so NVIDIA fetches from the same bounded native pool as Intel instead of the older container version. `--stop --apply` closes it, leaving AMD and 6 GB windows alone. It changes eligibility only. With DB credentials in a separate tmpfs project, pass `--db-project PATH` with its `config.xml` and private `html/inc` copy; policy and journal stay under `--project`, and credentials never go on the shared filesystem. Run one-off queue operators with the committed single-replica bridge configuration.

**6 GB profile** (`nvidia_vulkan_6gb_8b`): closed, and the only 8B class allowing BOINC 8.2.9 (others 8.2.11). One [8.2.9 run](https://github.com/tgoodyear/usnewsmap.com/issues/1234#issuecomment-6537940240) read all 12 crops at 3,725 MiB peak GPU memory; a real 6 GB card is unproven. Before admission, publish a signed native version with the pinned Vulkan bundle, verify the volunteer's CUDA/Vulkan access and measure a successful output. Then qualify one workunit: preview `native-qwen-cohort.php --project PATH --plan-class nvidia_vulkan_6gb_8b --workunit ID --fresh-pair --admit-until UNIX` and apply with its `--expected-policy-sha256`. This needs a pristine ordinary quorum-two pair and enforces distinct-host validation, rechecked under the policy lock. Arrange the peer in a reviewed shared window, since opening only the low-memory class does not guarantee one. The deadline does not close the window: schedule `--plan-class nvidia_vulkan_6gb_8b --stop --apply` for it. Shared refills and server restarts keep this window, and its stop leaves other classes alone. Do not enable general admission or alter quorum or result state to force a pass.

**2B NVIDIA profile** (`nvidia_vulkan_2b`, #1234): closed, BOINC >= 8.2.9, own window (`--plan-class nvidia_vulkan_2b --workunit ID`, then `--stop`) for batch-2 work from `usnm-boinc qwen submit --target native-2b`. No signed version until a maintainer releases one ([app release](app-release.md#linux-nvidia-2b-nvidia_vulkan_2b)).

The server patch disables app-ID-only version memoization when plan classes have workunit restrictions, so a paused batch can't block later eligible work. A no-work reply mentioning 15,360 MB can come from the separate training app; the native 8B classes still need 7,000 MB. Server startup keeps existing Qwen admission fields (including a paused container pool) while refreshing image-owned batch, resource and client requirements; change windows with the cohort operator, not a restart.

### Registering and releasing

`boinc/scripts/install.py` takes `--kind container` (default) or `--kind native --binary <FILE>`. `create_work` bounds (UNVERIFIED): `usnm_quality` 1e12 flops estimated, 1e14 bound, 1.5 GiB; `usnm_qwen` 1e13, 1e16, 6 GiB. Register apps and platforms on the on-prem host (`boinc/scripts/onprem-step.sh --env prod --target bridge --step register-apps --wait-mins 15`), then release with `boinc/scripts/boinc-app-release.sh` ([docs/boinc/apps/app-release.md](app-release.md)), adding `--onprem-host HOST --onprem-ct ID` for prod:

```sh
# Container versions:
boinc/scripts/boinc-app-release.sh --env prod --app usnm_quality --version 1.0 --signing-host <host>
boinc/scripts/boinc-app-release.sh --env prod --app usnm_qwen --version 1.0 --plan-class docker_mt_2b \
  --worker-image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:<qwen-2b-digest> --signing-host <host>
boinc/scripts/boinc-app-release.sh --env prod --app usnm_qwen --version 1.1 --plan-class docker_nvidia_cuda_8b \
  --worker-image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:<qwen-8b-digest> --signing-host <host>

# Native usnm_quality binary (from .github/workflows/boinc-quality-native.yml artifacts):
boinc/scripts/boinc-app-release.sh --env prod --app usnm_quality --version 1.1 --kind native \
  --platform arm64-apple-darwin --binary /path/to/usnm_quality_1.1_arm64-apple-darwin \
  --binary-sha256 <sha256> --signing-host <host>
```

## Qwen disk admission and cold-cache acceptance (#559)

[`boinc/apps/qwen/disk-policy.json`](../../../boinc/apps/qwen/disk-policy.json) is the versioned policy for submission, release checks, host preflight and tests. **GiB = 1,073,741,824 bytes.** The scheduler always advertises the cold bound, since a present image or tag does not prove every shared layer is complete; this deliberately over-reserves per concurrent unit.

| Target (`qwen submit --target`) | Payload                         | Cold `rsc_disk_bound`, bytes (GiB) | Warm incremental disk envelope                |
| ------------------------------- | ------------------------------- | ---------------------------------- | --------------------------------------------- |
| `container` (default)           | CPU 2B only                     | 34,359,738,368 (32)                | 2 GiB slot/workspace; no new image layers     |
| `container-8b`                  | GPU 8B only                     | 51,539,607,552 (48)                | 2 GiB slot/workspace; no new image layers     |
| `native-2b`                     | sticky 2B model + native bundle | 12,884,901,888 (12)                | up to 4 GiB: app bundle slot copy + workspace |
| `native-8b`                     | sticky 8B model + native bundle | 19,327,352,832 (18)                | up to 4 GiB: app bundle slot copy + workspace |

Warm figures assume the **exact digest-qualified payload is resident**, are incremental and exclude a 2 GiB free-space reserve; warm hosts still need the cold bound free. Nothing prunes volunteers' caches, so extra retained revisions use more disk.

Model bytes (LFS metadata matching `weights.lock`): 2B **2,279,480,352** (model 1,834,427,296 + projector 445,053,056), 8B **5,780,074,528** (5,027,784,800 + 752,289,728); the pinned CUDA base's compressed layers are **2,592,304,104 bytes** (manifest `sha256:cb7f872937c65952258e5670d8c1da7bae7394aef50c1c90a8bb251bde46c32e`). Native and container paths share model bytes, not filesystem overhead. [Measured image sizes](#split-image-disposable-docker-acceptance) are below.

Envelopes (conservative, **not measured**): compressed image <= base compressed + selected GGUF bytes + 512 MiB overhead; unpacked <= 8 GiB base + GGUF bytes + 512 MiB. Cold container admission covers compressed storage **plus two unpacked copies** (extraction and copy overlap), 2 GiB workspace and 2 GiB reserve, rounded to 32 and 48 GiB; CI fails rather than publish an image over it. Native cold admission covers one model set, **two copies of a native app bundle bounded at 2 GiB** (cache plus slot; `install.py` rejects larger), 2 GiB workspace and 2 GiB reserve. Native templates soft-link GGUFs from the shared project cache instead of copying them into slots; revisit the bounds if that changes.

The scheduler rejects `rsc_disk_bound > disk_available` (`sched/sched_check.cpp::check_disk`) and subtracts the bound per dispatched unit; the old 2 GiB selection criterion is gone, and BOINC disk preferences and quota must allow the bound. The managed rootless-Podman preflight checks free disk on slots, image graph storage and extraction scratch **before any pull**, and requires all three on BOINC's filesystem so scheduler reports reflect runtime use: the managed profile puts graph storage and image-copy scratch under the BOINC data directory and rejects a separate mount. A cached image does not bypass the check. Startup pull budget: 30 minutes inside a 40-minute service startup, not an inference deadline. Manual Docker installs need equivalent co-location and free-disk checks before enabling work; the server cannot inspect a remote Docker store.

### Activation and acceptance (operator authorization required)

1. Build and publish **both** split images with the reviewed manual workflow on `main`, keeping the anonymous manifest and config check and the signed release procedure. Release CPU with the 2B digest and `docker_mt_2b`, GPU with the 8B digest and **new** `docker_nvidia_cuda_8b`; never reuse one digest. The release script refuses legacy dual-model images and old GPU plan classes.
2. Roll out the server's batch routing, plan-class XML, bridge and templates **before** submitting: batch 12 CPU container, 18 GPU container, 2 native 2B, 8 native 8B. Legacy work keeps batch 1's `docker_mt`/`docker_nvidia_cuda`, which cannot get batch 12 or 18 units. Retire old Qwen versions through the reviewed release process; do not reset nodes or rewrite queued workunits, which keep their old bounds and must drain or be handled explicitly by the operator.
3. Stage separate CPU and GPU campaigns with each image digest, the base digest and the lock's weights identity from the build summary. GPU submits with `--target container-8b` (bridge `--var QWEN_TARGET=container-8b`); `container` is CPU. Native targets keep their model-specific sticky templates, and the workunit engine identity carries the campaign image digest. Benchmark NOTICE inputs keep the container target's batch and bound; native benchmark targets fail before submission (no native NOTICE/model templates yet).
4. On a **disposable runner**, `qwen_cold_cache.py` streams a locally built image into a new isolated Docker daemon, store and network namespace, without pulling, pruning or touching the existing daemon. CI artifacts `qwen-disk-2b`/`qwen-disk-8b` record image ID, local gzip bytes (not registry compressed bytes), unpacked size, graph `du`, filesystem delta and peak, warm increment and load time, and assert bounds and exact GGUF membership (do not sum shared-layer `du`). The build also checks all three error outputs and runs the synthetic 2B CPU reading under the released memory, swap, PID and isolation limits. Native fake-model and loopback tests cover shared aliases and outputs.
5. Live acceptance needs authorized publication and rollout plus an eligible >=7 GB NVIDIA host: anonymous cold pull, all three outputs, upload and validation, a warm repeat, and an insufficient-disk host rejected before selection or pull. Record digest, platform, runtime, storage driver, free bytes per filesystem before/after/peak, and transfer/extraction, model load and inference times **separately**.

Cold transfer and extraction are startup overhead; the historical ~513-second acceptance must not calibrate per-crop inference or FLOPS. `load_ms`, `engine_ms` and per-crop timings keep their meaning.

## Diagnostics

Both runners write `diag.txt` as the Surya worker does: from `init_data.xml` only client version, app version, plan class, GPU type and device number, CPU count and memory bound (never the account key or host name); container runtime and whether rootless; cgroup limits and memory. `usnm_qwen` adds NVIDIA device nodes, variables, driver, `nvidia-smi` output, the chosen model and llama-server's lines; `usnm_quality` adds the job's readers and timings. A unit that cannot run (checksum mismatch, missing or different engine build) returns `unit_error` with the reason and no record.

## Redundancy and validation

Both apps: `min_quorum` 2, `target_nresults` 2, at most 4 results. Daemons are in `boinc/apps/daemons.xml` and `boinc/images/server/lib.sh`, wrappers in `boinc/images/bridge/`. `script_validator` runs:

- `usnm_quality_check` (`usnm-boinc quality check`): shape, sizes, UTF-8, checksums of `confusions.json` and `diag.txt`, record consistent with outcome. Every result is copied to `<project>/quality/seen/<result>/`.
- `usnm_quality_compare` (`usnm-boinc quality compare`): same job, engine and outcome, and equal canonical records (keys sorted, floats to six decimals, without host, timings, time and diagnostics). Every pair goes to `<project>/quality/pairs.jsonl` with the verdict and, if different, which part differs.
- `usnm_qwen_check` (`usnm-boinc qwen check`): shape, sizes, checksums of `readings.json` and `diag.txt`, readings against the result (count, device, model, outcome). Every checked result, not only the canonical one, is kept in `<project>/qwen/seen/<uploaded-result-manifest-name>/` before validation succeeds, as a complete fsynced directory: `capture-v1.json` binds upload names, byte counts and SHA-256s (including exact `result.raw.json` and versioned generation sections in `diag.txt`), and `readings-metrics-v1.json` holds per-reading uncertainty/repetition/token metrics, not accuracy. Legacy normalized `result.json` files are completed with the raw manifest and receipt; replay never overwrites mismatches. Capture/storage conflicts return transient exit **3** (deferred, volunteer not marked invalid); malformed science fails non-transiently. Source-image/damage measurements are unavailable until the offline `candidates` join. Quorum, thresholds, canonical selection and deletion are unchanged, and JSON formatting an older validator already normalized can't be recovered.
- `usnm_qwen_compare` (`usnm-boinc qwen compare`): same job and engine, both read the page or neither, and per crop both read or neither with word-bag Dice at least the threshold: 0.5 (`PROBE_MIN_DICE`) until the probe measures it, which rejects only unrelated text. Every pair goes to `<project>/qwen/pairs.jsonl` with per-crop Dice and both hosts' devices and models. The line's `wordless` lists crops neither reading has a letter or digit in (Dice 1.0 regardless), with `id`, `identical` (equal once whitespace is collapsed) and `looped` (loop-guard bit or a trailing repeated run, as `crop-consensus-v2` marks it), for measurement only ([#1463 note](../../notes/2026-11-04-qwen-wordless-crops.md)).

### Bundle pages

For a bundle unit the rule also runs per page (`<doc_id>:` crop prefix) when both results finished (`ok`): the pair line gains `pages`, and with `--pages` (`<project>/qwen/pages` in the bridge wrapper) each page's verdict and readings go to `<pages>/<job_id>/<doc_id>/<pair>.json` (`page_agreed` or `page_disagreed`, `accuracy_established: false`). An all-blank page is not agreed. Under the default bundle verdict (`unit`) the unit still needs every page, so a bundle with one disagreeing page stays inconclusive in BOINC while its agreed pages are kept for `quality collect --qwen-pages` (#1435).

**Page-level verdicts (`page-v1`, #1435).** With `--bundle-verdict page-v1` (the wrapper passes it while `<project>/qwen/bundle-verdict` holds `page-v1`), a bundle pair whose results both finished settles the unit when both read the same crops (as many as each says the unit has), every page's record was written and at least half of the pages (and at least one) agreed. The pair then passes: BOINC makes the unit canonical, credits both results and sends no third replica, and the pair writes `<pages>/<job_id>/unit-verdict.json` (the pair's result names and each page's verdict, `accuracy_established: false`; the first settling pair keeps it). The page ledger still decides each page: `quality collect --qwen-pages` joins only a settled bundle's `accepted` pages, never its canonical result's reading of a disagreed page. `qwen stage --page-records <pages>` (`--qwen-page-records` on `quality-ship.sh`) packs a page that every one of its bundles settled without accepting it (`disagreed`, or `pending` when no record of it counts) into a new bundle with a fresh pair, within what is left of the run's `--max-pages` after its new pages, so a run publishes no more pages than it was admitted for; a page is packed into at most two bundles (`MAX_PAGE_ATTEMPTS`), so it gets one retry, like the single third replica the [tie-breaker note](../../notes/2026-11-05-qwen-yiddish-tiebreak-policy.md) found settles a minority of disputes. A pair under half, a page record that can't be written, or a result that did not finish leaves the unit's own rule in force, so it gets more replicas as before. Yiddish is staged single-page, so the `yid:1` tie-breaker stop never meets a bundle. Removing the file restores `unit`; a binary that does not know `--bundle-verdict` keeps `unit` too.

### The crop-consensus rule

`usnm-boinc qwen compare --rule crop-consensus-v2` judges crop by crop (default `strict-v1`). A crop is `looped` if either side loops, `no_text` if neither side has a letter or digit, `disagree` if only one side has text, else `agree` when character similarity (`1 - edits / longer length`, whitespace collapsed, Hebrew points ignored, readings unchanged) is at least 0.7. A pair agrees when no crop disagrees and `agree` crops are over half of the crops with text and hold at least 100 characters; a page with no text agrees, as under the strict rule. Results that did not both finish keep the strict verdict. The pair log gains `rule`, `basis`, `strict_reason`, `crop_states`. `--crop-states <dir>` (required) keeps each pair's states at `<dir>/<job_id>/<pair>.json` (record version 2, `basis` `crop-consensus-v2` or `strict-v1`, `readings_sha256` naming both results); if the record can't be written the strict verdict stands, so a result accepted without a record was accepted crop for crop. A pair vouches only for its `agree` crops, and `quality collect --qwen-crop-states <dir>` joins only those (#1434). Evidence: [validation note](../../notes/2026-11-01-qwen-cross-vendor-validation.md), [floor check against human transcriptions](../../notes/2026-11-04-crop-consensus-floor-check.md) (reproduced by `usnm-boinc qwen grounded`).

**Turning the crop rule on and off.** The wrapper reads `<project>/qwen/validator-rule`: `crop-consensus-v2` turns the rule on for the next pair; removing the file or writing `strict-v1` turns it off, with no restart. A `usnm-boinc` beside the wrapper's target overrides the image's, so a build newer than the server image can validate: install wrapper and binary in one directory and point `bin/usnm_qwen_compare` at it. Every server start and every bridge step (each `quality-stage`, so each shipped chunk) relinks `bin/` to the image's wrapper (`boinc/images/server/lib.sh`), so such a link lasts only until the next step. Between image rolls, put the wrapper and the binary in the running server container's `/usr/local/lib/usnm-boinc/` instead, where the relinked `bin/` entry points; recreating the container restores the image's. Order: run every Qwen collector with `--qwen-crop-states <project>/qwen/crop-states` first (`CROP_STATES_ARGS` on `usnm-qwen-results.service`, `QWEN_CROP_STATES` on the bridge's `quality-collect`), then add the rule file; a collector without it joins every crop of a canonical result. Turning the rule off keeps the records, and results accepted under it still join only their vouched crops.

### Stopping tie-breakers by language and request version (#1477)

When a Qwen first pair disagrees, the validator raises `target_nresults` and the transitioner sends a tie-breaker, up to `max_total_results` 4. For Yiddish request version 1 a tie-breaker rarely settles the unit ([replica cost note](../../notes/2026-11-04-qwen-replica-cost.md), [tie-breaker policy note](../../notes/2026-11-05-qwen-yiddish-tiebreak-policy.md)), so `boinc/scripts/qwen-tiebreak-policy.py` stops them for named groups: `<project>/qwen/tiebreak-policy.json` (`{"schema": 1, "stop_tiebreakers": [{"language": "yid", "request_version": 1}]}`) or `--stop-tiebreakers yid:1` (repeatable). With neither, nothing changes. The language is the unit's language set as `qwen-replica-cost.py` labels it, matched exactly (`yid` does not match `heb+yid`).

A unit is settled once its first pair has disagreed: no canonical result or error, at least two inconclusive returns, no replica running, nothing waiting for the validator, not due for the transitioner. Settling is one transaction under row locks (`SELECT … FOR UPDATE`, then a recheck): unsent replicas become OVER/DIDNT_NEED (as `cancel_job` does), `max_total_results` = result count, `error_mask |= 8` (too many total results), `transition_time` now. The transitioner then marks the returns NO_CHECK and assimilates with no canonical result, as for a capped unit; the page keeps its Tesseract readings and the router still reads Yiddish with `yid`. Units with a running replica, or not lockable within 5 s, wait for a later pass. No image change is needed. No new Yiddish v1 units are made, so repeat until `plan` reports nothing. Settled units count as error workunits in `qwen_submit.py`'s source status and leave the feed's `unresolved_native` count (freeing room under `--max-unresolved`). A settled unit can't be reopened; re-reading its page needs a new unit.

CI fixtures `boinc/scripts/db-tls/full.py` (server daemons, MariaDB) and `boinc/scripts/db-tls/mysql84.py` (production's MySQL, no daemons) cover row-locked units, running replicas and a concurrent `mark_as_sent`, and check that `mark_as_sent` then matches no row for a cancelled replica, so a stale feeder cache can't send it; with no daemons in the second, the first production run is bounded to one unit.

Run as root on the BOINC host after the #1449 server-image roll:

1. `plan` (read-only: one `READ ONLY` transaction of `LIMIT`-bounded `SELECT`s):

   ```sh
   pct exec 230 -- podman exec -i usnm-boinc-server python3 - plan \
     --since 2026-10-21T00:00:00Z --until 2026-11-06T00:00:00Z --stop-tiebreakers yid:1 \
     < boinc/scripts/qwen-tiebreak-policy.py
   ```

2. `settle` with the same arguments and no `--apply` previews units and replicas to cancel.
3. Apply under the project mutex with an append-only journal (prior state, every unit settled or skipped), starting with `--max-units 1`. After the transitioner's next pass, check its returns are NO_CHECK (`validate_state` 3) and `assimilate_state` is 1 or later, then raise the bound (for example `--max-units 500`).

   ```sh
   pct exec 230 -- flock -x -w 120 /run/usnm-boinc-step.lock podman exec -i usnm-boinc-server python3 - settle \
     --since 2026-10-21T00:00:00Z --until 2026-11-06T00:00:00Z --stop-tiebreakers yid:1 \
     --apply --max-units 1 --journal /project/usnmocr/qwen/tiebreak-journal.jsonl \
     < boinc/scripts/qwen-tiebreak-policy.py
   ```

## CLI subcommands

### usnm-boinc quality

- `sample`: stratified `sample.jsonl` from the prefetch manifest (`--manifest`, `--out`, `--pages` 500, `--min-per-stratum` 2, `--seed`, optional `--title-languages` JSON of LCCN to MARC codes, `--with-as` doc ids with American Stories text, `--pages-file` restricting the draw to listed doc ids).
- `function-words`: `<out-dir>/<lang>.txt`, the top `N` (100) alphabetic words of four or more letters from the embedded wordfreq list (`usnm_core::lang::wordlist`) per repeatable `--lang` (`--top`, `--out-dir`).
- `texts-export`: reads American Stories Parquet (`curated/american-stories/pages/<lccn>/<year>-<nnn>.parquet`) and the newest published (`curated/ocr-local`) or pending (`--out-url`) overlay per page (`--pages`/`--doc-id` or `--sample`), writing `<out-dir>/<doc_id>.json` (`texts.json` minus LoC's text, which `stage` fills from `loc.xml`) and `<out-dir>/<doc_id>.ours.xml` when our ALTO exists (`--curated-url`, `--out-url`, `--out-dir`).
- `stage`: units under `<root>/units/<doc_id>/` (or `<root>/pages/<doc_id>/` packed into `<root>/units/bundle-<idx>-<first_doc_id>/` when `--bundle-size > 1`) plus `<root>/places.txt`. Flags: repeatable `--page group:lang:doc_id:batch[+batch...]` or `--sample` (`--group` default `sample`), `--root`, `--engine-image`, `--texts-dir`, `--scans-dir`, `--places` (`.txt` or catalog `places.json`, deduplicated), `--function-words` or `--function-words-dir`, `--readers` (`loc,as`), `--surya-texts` (`surya collect` export, joins as `ours-surya` when named, #968), `--crop-seed` 522, `--k-crops` 12, `--gap-secs` (min 3, default 5), `--max-pages` 20, `--bundle-size` (1 to 500; `1` on CLI, `300` in `bridge-job.sh` for ~10–15 minute workunits), `--batch-cache`, `--scan-archive`. Scans: `<doc_id>.jpg`/`.xml` in `--scans-dir` with no network; else `crates/usnm-boinc/src/scans.rs` tries the scan archive when `--scan-archive` is set (`keep/<batch>/<lccn>/<date>/<edition>/<seq>.jpg`, checked against its `sha256`) and then `tile.loc.gov` through the page's batch (`--batch-cache` caches batch manifest and issue METS on the storage service), one request at a time `--gap-secs` apart, inside the host pacer when `USNM_LOC_GLOBAL_PACER` names the scan fetcher's state file ([LoC budgets](#ocrs-prefetch-manifests-1055)). Never `www.loc.gov`'s item API. It parses `loc.xml` for page dimensions and LoC's text, merges `<texts-dir>/<doc_id>.json` (normalizing reader `source` and region `"box"`/`"rect"`, copying `.ours.xml`) and writes `page.jpg`, `loc.xml`, `texts.json`, `quality.json`.
- `submit`: `<root>/units/` to `create_work` (`--root`, `--project`, `--max-units` 100), copying `places.txt` once to `download/usnm_quality_places_<sha256>` and each unit's inputs into the download hierarchy, with write-ahead `submitting`/`submitted` markers.
- `local-unit`: a unit from local files (`--page`, `--alto`, `--texts`, `--places`, `--function-words`, `--dir`, `--engine-image`, `--crop-seed` 522, `--k-crops` 12) for `usnm-boinc work --dir <dir>`.
- `check`: `--init_script` (`--files <result.json> <confusions.json> <diag.txt>`, optional `--keep <dir>`).
- `compare`: `--compare_script` (`--files <r1...> --files2 <r2...>`, optional `--log <pairs.jsonl>`).
- `collect`: joins canonical `usnm_quality` (`--root`, `--spool`), `usnm_qwen` (`--qwen-root`, `--qwen-spool`, `--qwen-crop-states`, `--qwen-pages`) and optional `usnm_surya` crop results (`--surya-root`, `--surya-spool`; reader `surya`, #968) for `--campaign`. With a Qwen reading it recomputes the crop-level hat (`legibility.basis` `crop-hat+qwen`). Writes `pages.parquet`, `crops.parquet`, `summary.md` to `--out-dir` or `--curated-url` under `audit/quality/<campaign>/`.
  - `--qwen-crop-states`: only vouched crops join; others keep rows without the reading, with `vlm_crop_state` `strict-v1`, `agree`, `looped`, `no_text` or `unjudged`. Records are part of the row-cache keys.
  - `--qwen-pages` (#1435, `/project/usnmocr/qwen/pages`): a bundle with no canonical result joins each page `page-ledger` calls `accepted` (`vlm_crop_state` `page-strict-v1`), using the first agreeing record by file name and the side whose result name sorts first, which must have finished and name its device (the two names must differ). The bundle's other pages keep rows without a reading. A canonical result, once present, wins. Only bundles' cache keys change.
- `rehat`: recomputes page hat versions 1 and 2 from a staging root's texts (`--root`, `--out` JSON lines, `--min-readers` 3, `--threads`) with no results or scans, to compare the versions on a still-staged campaign.
- `rescore` (#1079): reruns the text part of single-page units (`slot::text_analysis`, what the unit runs minus image and ALTO) from corrected LoC texts, no volunteer work. Flags: `--campaign`, `--root` (staging root), `--spool` (canonical results), `--loc-texts` (JSON lines `{"doc_id", "text"}` replacing `loc-hist`), `--places`, `--out-dir` (new, outside the root), `--threads`. Keeps image/layout features; writes `<out-dir>/units/<doc_id>/{quality.json, texts.json}` (which `rehat` reads) plus `pages.parquet` and `summary.md` with hat version 2. Bundles, units without results and pages without LoC text are counted and skipped.
- `calibrate`: see [Calibration](#calibration).
- `estimate`: offline over a page directory (`--dir`, `--out`, `--places`, `--function-words`, `--crop-seed` 522, `--k-crops` 12, repeatable `--reader NAME=PATTERN` for `<dir>/sample/{images,alto,loctext}`); with `readings.json` it verifies the originating `qwen.json` (or `readings/<page>.qwen.json`) and joins its crops. Writes per-page `result.json`, `confusions.json`, `crops.jsonl` and aggregate `pages.jsonl`, `crops.jsonl`.
- `summary`: reads `<root>/units/`, `--seen` (`<project>/quality/seen`) and `--pairs` (`<project>/quality/pairs.jsonl`) and prints a Markdown table.

`stage` stops with `HOLD` on an exported `loc` or `loc-hist` text equal to an ocr-local reason label (`bulk`, `user`, `priority`, `missing`, `empty`, `short`, `garbled`, `mixed`). Merged `ocr-local/pages` schema v2 calls the queue reason `ocr_reason`; `loc_text` and `reason` are deprecated aliases, never LoC's text, and readers validate the schema version and reject conflicting labels, historical parts included; node exchange and Japanese schemas are unchanged. Export LoC's text from the curated LoC page's `text`. v1-wave6's 10,000 pages were staged with `loc-hist` as `bulk` from the legacy column; #1079 has the repair.

### The page hat, version 2 (#962)

Whole-page `cer_sym` counts reading order and missing text as edits, and LoC (ALTO block order), American Stories (article order) and Tesseract (its own segmentation) differ mostly in order and coverage. Version 2 (`usnm_ocr_quality::page_hat`, `VERSION` 2) solves the same `d_ij = e_i + e_j` on `local_cer_sym` from [agreement v1](../../ocr/text-agreement.md): edits over the characters the local alignment placed, in any order. A pair falls back to whole-page `cer_sym` (basis `page`) when nothing placed or neither reader has half its characters placed in the other (`MIN_MATCHED` 0.5). Each reader's `matched` is the mean share of its characters placed in the others' texts (coverage, not error).

No app version is needed: `collect` rebuilds the unit's inputs from `texts.json` (`slot::reader_inputs`, LoC's ALTO text for an empty LoC text) and solves version 2. In `pages.parquet`, `hat_version` says which hat `est_cer`, `est_cer_raw`, `hat_determined`, `hat_residual` and `legibility_ceiling` hold; the unit's values are in the `_v1` columns; `matched` and `hat_pairs_json` (per pair `a`, `b`, `basis`, `d`, `cer_sym`, `local_cer_sym`, `matched_a`, `matched_b`) are new. No `hat_version` column means version 1. A page keeps version 1 (counted as `hat_v1_kept`; `rehat` counts `no_alto`) when `texts.json` is unreadable, or its LoC text is empty or `none` and `loc.xml` is missing, empty or unparseable.

The `usnm_quality` database ([scripts/onprem/quality/README.md](../../../scripts/onprem/quality/README.md)) keeps the same split: `unit_pages` (`hat_version`, 1 for older rows, `_v1` values, `hat_pairs`), `reader_scores` (`est_cer_v1`, `est_cer_raw_v1`, `matched`), `page_summary` and `page_quality_joined` (`hat_version`). `build-quality-assessment-db.py` reports only the newest version present; older rows keep the unit's estimate in `est_cer_v1` with hat columns cleared, so no mean blends versions. The crop-level hat stays on `cer_sym`, since a crop box is in line order for every reader.

### usnm-boinc qwen

- `stage`: cuts the same crops from each staged `usnm_quality` unit in `--quality-root` (its `page.jpg`, `loc.xml`, `crop_seed`, `k_crops`) with no LoC request, writing `<root>/units/<doc_id>/{page.jpg, qwen.json}`, or `<root>/pages/<doc_id>/` packed into `<root>/units/bundle-<idx>-<first_doc_id>/` when `--bundle-size > 1`. Flags: `--root`, `--engine-image`, `--engine-llama`, `--engine-weights`, `--gpu-timeout-secs` (14400), `--cpu-timeout-secs` (7200), `--max-tokens-per-1000px2` (4), `--max-pages` (20), `--bundle-size` (1 on CLI, `16` in `bridge-job.sh`; at most 32 pages and 256 crops), `--page-records <dir>` (with bundles: pack again each page `page-v1` settled as disagreed, once; [Bundle pages](#bundle-pages)). Bundles:
  - `membership.json`, written before `qwen.json` and never changed, lists per page its page unit, job id, `qwen.json` sha256, page sha256 and slice of the bundled `page.jpg`.
  - A run holds `<root>/.bundle-lock` from staging to its last bundle and claims each bundle directory, so a concurrent run fails.
  - Deadlines are per-page deadline times pages, at most 14,400 s; `submit` scales `rsc_fpops_est`/`rsc_fpops_bound` by pages (`1e13`/`1e16` a page).
  - A bundle closes before `qwen.json` would pass 512 KiB (the read limit) or the longest possible `readings.json` would pass 4 MiB (the upload limit in `boinc/apps/usnm_qwen_out`, at about 16 KiB a crop; 2 MiB before #1435's resize), so about 250 crops: 25 pages of 10, and `MAX_BUNDLE_CROPS` (256) close to it. No app version reads `MAX_READINGS`; the upload handler enforces each unit's own template, so a unit created before the resize keeps 2 MiB.
  - A run's last bundle, if under half of `--bundle-size` pages and of 256 crops, waits under `pages/` with a `held` marker (the time it was first held) for the next run's pages, up to `--max-hold-secs` (21,600, six hours; 0 publishes at once). `qwen_submit.py` counts a held page as processed.
  - A page too large alone gets a `too_large` marker and is not published; its job stays beside the marker as its disposition (`rejected-qwen.json` if its own `qwen.json` is too large, the page unit's `qwen.json` under `pages/` if only its one-page bundle is).
  - Different engine, deadline or token settings go in separate bundles. A bundled root refuses `--bundle-size 1`.
  - Workers check every page before loading the model, then decode one page at a time, so a bundle needs only its largest page's memory.
- `submit`: hands `<root>/units/` to `create_work` (`--root`, `--project`, `--max-units` default 20) with write-ahead `submitting`/`submitted` markers.
- `local-unit`: a unit from `--image` and `--alto` (or `--synthetic`, a built-in two-line block-capitals page) into `--dir` (`--engine-image`, `--engine-llama` default `USNM_QWEN_LLAMA_IMAGE`, `--engine-weights` default `USNM_QWEN_WEIGHTS`, `--crop-seed` 522, `--k-crops` 12, `--timeout-secs` 1800, `--max-tokens-per-1000px2` 4).
- `probe`: runs `/app/llama-server --version` and checks each model and projector in `usnm_boinc::qwen::default_models()` exists under `/opt/qwen/models`, printing sizes without loading.
- `check`: `--init_script` (`--files <readings.json> <result.json> <diag.txt>`, optional `--keep <dir>`).
- `compare`: `--compare_script` (`--files <r1...> --files2 <r2...>`, `--min-dice` 0.5, `--log <pairs.jsonl>`, `--rule strict-v1|crop-consensus-v2`, `--crop-states <dir>` required with `crop-consensus-v2`, `--pages <dir>`, `--bundle-verdict unit|page-v1`, `page-v1` needing `--pages`).
- `grounded`: measures `crop-consensus-v2`'s floor offline against human transcriptions (`--refs` JSON Lines `{id, source, lang, ref}`, repeatable `--reader NAME=PATH` and `--pair A:B`, `--legibility <reader>`, `--good-cer` 0.10, `--bad-cer` 0.50, `--floor` (default the rule's), `--seed`, `--out`, `--crops`): crops under the floor, crop states, treatment of pairs where both or one reading match, and a shuffled control. CER leaves Hebrew points out of both sides.
- `replay`: measures validator rules offline on retained results (`--seen <project>/qwen/seen`, `--model` `qwen3-vl-8b-q4km`, `--seed` 1420, `--controls` 2000, `--out <report.json>`): every rule on every replica pair, pairs that must fail, a third replica's check of what a pair vouched for, and each crop metric's separation of true from shuffled pairs.
- `page-ledger`: `--root`, `--pages`; one JSON line per bundle page with its job id and checksum from `membership.json` and a status: `accepted` (some pair agreed), `disagreed` (every pair so far differed), `pending` (no pair yet) or `membership_invalid` (`membership.json` missing, mismatched, or naming a page unit whose `qwen.json` changed; no page kept). Records not naming that job and page, or not holding exactly the page's staged crops once on each side, are ignored. `accepted` means two hosts agree, not that the reading is right.
- `summary`: reads `<root>/units/`, `--seen` and `--pairs` and prints a Markdown table.
- `candidates --job <qwen.json> --page <page.jpg> --candidate <seen/result-a> <seen/result-b> ... --out <report.json>`: offline audit of up to 16 retained results of one single-page job. After verifying input hash, job/engine/prompt identity, model membership and crop IDs, it reports per candidate the text, manifest, host metadata and hashes, with per-crop pixel features, pairwise CER/WER/Dice, LoC comparisons, damage counts, uncertainty and repetition markers and token-budget saturation. Missing and empty readings stay distinct. Pair metrics use NFC, joined line-end hyphens and collapsed whitespace; `validator_dice` uses the validator's unmodified word bags. Unsupported language models and zero denominators stay null, not zero damage; LoC is another fallible reader. Output is **noncanonical**: not an input to `quality collect`, and it changes neither validation nor admission. Keep the original `seen` files; selected results don't prove database completeness.

The GPU budget (`--gpu-timeout-secs`, four hours) is application runtime for the whole page, not a BOINC task limit; the old 30-minute default discarded nearly finished pages on slow GPUs. The contract accepts 60–14,400 s (shorter budgets stay available for bounded experiments), separate from BOINC's report deadline and resource bounds. Timeouts are part of the job identity: never edit submitted inputs; stage a new identity to retry and keep the original evidence.

GPU workers (`usnm_qwen` and `usnm_surya` container workers, `usnm-qwen-app`) set `CUDA_CACHE_PATH` and `MESA_SHADER_CACHE_DIR` to `/var/tmp/usnm-gpu-cache-<uid>` (mode 0700), falling back to the temp directory and then the task's scratch or slot directory if that path is not a private directory of the worker's user. Native hosts keep compiled kernels between workunits; containers (read-only root, own `/tmp` tmpfs) for one.

**WU21128.** All three platforms used the full 1,304-token budget on one crop (AMD and Intel looping a word, Apple emitting 643 `[?]`). `candidates` flags token-budget exhaustion, duplicate lines, uncertainty-only and empty readings regardless of agreement, and records the longest identical-token run; no unflagged output is labelled accepted, and the source itself can repeat lines. The native request (`crates/usnm-qwen-app`) uses temperature 0, seed 42, `top_p=1` and 4 tokens per 1,000 crop pixels, with no repetition penalty, and versions 1 and 2 have no stop rule. A larger budget could prolong a loop. Do not lower the Dice floor to accept repetition, shared hallucinations or agreed `[?]`; accuracy needs independent image-grounded transcriptions stratified by language and legibility.

## Running a campaign

### Local smoke test without LoC or BOINC

```sh
# A quality unit from local files, then the unit, then the validator's check.
usnm-boinc quality local-unit --page page.jpg --alto loc.xml --texts texts.json --places places.txt \
  --function-words function-words.txt --dir u
usnm-boinc work --dir u
usnm-boinc quality check --files u/result.json u/confusions.json u/diag.txt
# The same computation over a directory of pages, without staging.
usnm-boinc quality estimate --dir pages --out out --places places.txt --function-words function-words.txt
# A qwen unit from a page and its ALTO (the crops are cut here); inside the image, work then reads it.
usnm-boinc qwen local-unit --image page.jpg --alto loc.xml --dir q --engine-llama sha256:... --engine-weights sha256:...
```

### Server campaign through handoff-job.sh and bridge-job.sh

A quality campaign lives under `$SHARE_DIR/quality/<QUALITY_CAMPAIGN>` and a Qwen campaign under `$SHARE_DIR/qwen/<QWEN_CAMPAIGN>` (names 1 to 41 characters of `[a-z0-9-]`) for both `boinc/images/server/bridge-job.sh` and the hand-off steps (`boinc/images/bridge/handoff-job.sh`, on the on-prem host since prod's Azure hand-off job was retired on 6 November 2026). UNVERIFIED against a live server. Prod's bridge steps have run on the on-prem host since 21 October 2026: run each `boinc/scripts/start-boinc-bridge.sh --env prod --step <step>` below as `boinc/scripts/onprem-step.sh --env prod --target bridge --step <step>` with the same `--var` and `--wait-mins` (required) arguments ([boinc-onprem.md](../server/onprem.md)).

1. **Export page texts (`handoff-job.sh`, `HANDOFF_STEP=texts-export`).** Runs `boinc/scripts/texts-export-onprem.sh` on an on-prem host with `curated` at `/srv/curated`; only `ocr-local` overlays and their ALTO come from `CURATED_URL` (logged as `curated_bytes`). It runs `usnm-boinc quality texts-export` for `--doc-id` pages or a `--sample` under `quality-samples/` in `HANDOFF_URL` and uploads `<doc_id>.json` (and `.ours.xml`) under `texts/`:

   ```sh
   boinc/scripts/texts-export-onprem.sh --settings /etc/usnm/texts-export.env \
     --usnm-boinc /usr/local/bin/usnm-boinc --american-stories-root /srv/curated --sample v1.jsonl
   ```

   `--settings` holds `HANDOFF_URL`, `CURATED_URL`, `COSMOS_ENDPOINT`, `REFERENCE_URL`, `OUT_URL` and the host's Arc identity endpoints; that identity needs read on curated's `ocr-local/` and the writer role on the hand-off container. Build `usnm-boinc` with `cargo build --release -p usnm-boinc --locked`.

2. **Stage quality units (`BRIDGE_STEP=quality-stage`).** When `HANDOFF_URL` is set, pulls `texts/` into `$SHARE_DIR/quality/<QUALITY_CAMPAIGN>/texts` and, for a relative `QUALITY_SAMPLE`, `quality-samples/` into `<campaign>/samples/`; writes `<campaign>/function-words` for `QUALITY_LANGS` (ISO 639-3, comma-separated), then runs `quality stage`. Variables: `QUALITY_PAGES` (`group:lang:doc_id:batch[+batch...],...`) or `QUALITY_SAMPLE` (default `<campaign>/sample.jsonl`, `QUALITY_GROUP` default `sample`), `QUALITY_READERS` (`loc,as,ours-tesseract`), `QUALITY_PLACES` (`<campaign>/places.txt`), `QUALITY_ENGINE_IMAGE` (`sha256:<worker-digest>`), `QUALITY_MAX` (100), `QUALITY_BUNDLE_SIZE` (`300`, max `500`; a 300–500 page bundle runs 10–15 minutes with per-page checkpointing). Scans come from `SCAN_ARCHIVE_URL` (`--scan-archive`) first, then `tile.loc.gov` (`--batch-cache`):

   ```sh
   boinc/scripts/start-boinc-bridge.sh --env prod --step quality-stage \
     --var QUALITY_CAMPAIGN=v1-wave5 \
     --var HANDOFF_URL=https://stusnmoffapzy.blob.core.windows.net/boinc-handoff \
     --var QUALITY_SAMPLE=v1-wave5.jsonl --var QUALITY_GROUP=v1-wave5 \
     --var QUALITY_ENGINE_IMAGE=sha256:<worker-digest> \
     --var QUALITY_LANGS=eng --var QUALITY_BUNDLE_SIZE=300 \
     --var QUALITY_MAX=5000 --wait-mins 45
   ```

3. **Stage Qwen units (`BRIDGE_STEP=qwen-stage`).** Cuts the crops from `$SHARE_DIR/quality/<QUALITY_CAMPAIGN>` into `$SHARE_DIR/qwen/<QWEN_CAMPAIGN>` with no LoC fetch:

   ```sh
   boinc/scripts/start-boinc-bridge.sh --env prod --step qwen-stage \
     --var QUALITY_CAMPAIGN=v1-probe --var QWEN_CAMPAIGN=v1-probe \
     --var QWEN_ENGINE_IMAGE=sha256:<qwen-digest> \
     --var QWEN_ENGINE_LLAMA=sha256:b0baccbe2e00b5d74e63f85f9f023c93d61d0f329475269e1e7002e87eb875ea \
     --var QWEN_ENGINE_WEIGHTS=sha256:969266d1c24656977e06a6efacae04deac6d5093c77227fe55ac28975035cddd \
     --var QWEN_MAX=20 --wait-mins 15
   ```

   A crop whose source text passes 8192 UTF-8 bytes is never truncated: the page gets the `too_large` terminal marker naming each oversized crop, with the candidate kept as `rejected-qwen.json` beside its `loc.xml`, `quality.json`, `page.jpg` and `sample.json`; no `qwen.json` is published, and other pages continue within `--max-pages`. Repeats keep that evidence, and the feed remembers rejected documents without counting them as submitted. Other schema errors fail closed.

   `qwen_submit.py` stages with a sibling `usnm-boinc-stage` when installed (`--stage-binary` to choose) and by default submits version 1 with `usnm-boinc` and versions 3 and 4 with the staging binary, since a pre-#1437 server image refuses them; `--submit-binary` (`--qwen-submit-binary` on `quality-ship.sh`) sets one for all ([One submit binary and one collector](#one-submit-binary-and-one-collector-1449)). `--transaction` always uses the staging binary and refuses `--submit-binary`. `--bundle-size` (1 to 32, default 1) sets pages per native unit ([Bundled native units](#bundled-native-units-1435)). Install only approved hash-bound artifacts, at an idle boundary under the normal project lock.

4. **Submit (`BRIDGE_STEP=quality-submit`, `qwen-submit`).** Two results per workunit (`QUALITY_MAX` 500, `QWEN_MAX` 20); invalidates `$PROJ/html/cache/f2/server_status.php_job_status` so `https://boinc.usnewsmap.com/server_status.php` shows the new queue (uncached: `?counts=1`):

   ```sh
   boinc/scripts/start-boinc-bridge.sh --env prod --step quality-submit \
     --var QUALITY_CAMPAIGN=v1-probe --var QUALITY_MAX=500 --wait-mins 15
   boinc/scripts/start-boinc-bridge.sh --env prod --step qwen-submit \
     --var QWEN_CAMPAIGN=v1-probe --var QWEN_MAX=20 --wait-mins 15
   ```

5. **Validator summaries (`BRIDGE_STEP=quality-summary`, `qwen-summary`).** Read `<project>/quality/{seen,pairs.jsonl}` and `<project>/qwen/{seen,pairs.jsonl}` and write `<campaign>/summary.md`:

   ```sh
   boinc/scripts/start-boinc-bridge.sh --env prod --step quality-summary --var QUALITY_CAMPAIGN=v1-probe
   boinc/scripts/start-boinc-bridge.sh --env prod --step qwen-summary --var QWEN_CAMPAIGN=v1-probe
   ```

6. **Collect (`BRIDGE_STEP=quality-collect`).** Joins assimilated `usnm_quality` (`QUALITY_SPOOL`), optional `usnm_qwen` (`QWEN_CAMPAIGN`, `QWEN_SPOOL`) and optional `usnm_surya` crop (`SURYA_CAMPAIGN`, `SURYA_SPOOL`, #968) results into `<campaign>/out/{pages.parquet,crops.parquet,summary.md}`, or uploads to `QUALITY_CURATED_URL` under `audit/quality/<QUALITY_CAMPAIGN>/`:

   ```sh
   boinc/scripts/start-boinc-bridge.sh --env prod --step quality-collect \
     --var QUALITY_CAMPAIGN=v1-probe --var QUALITY_SPOOL=/project/usnmocr/results/0 \
     --var QWEN_CAMPAIGN=v1-probe --var QWEN_SPOOL=/project/usnmocr/results/0 --wait-mins 15
   ```

7. **Multi-shard campaigns (`boinc/scripts/boinc-quality-campaign.py`).** Splits large campaigns into shard archives for parallel Container Apps executions, reports progress and restores the bridge job. Azure server only (`"server": "azure"`); on-prem has no wave job:

   ```sh
   python3 boinc/scripts/boinc-quality-campaign.py prepare --campaign v1-wave6 --manifest sample.jsonl --out-dir /tmp/wave6 --count 10000 --shards 20
   python3 boinc/scripts/boinc-quality-campaign.py status --campaign v1-wave6 --shards 20
   python3 boinc/scripts/boinc-quality-campaign.py restore --env prod
   ```

### On-prem feeder (v1-wave10 on)

On-prem, quality units are fed from the feeder container's Proxmox host with no Azure call and no LoC request (#962; the Azure wave runner was `boinc/scripts/quality_corpus_runner.py`).

1. **Feeder** (`boinc/scripts/onprem/quality-feed/quality_feed.py`, `usnm-quality-feed.service`, in the `usnm-batch` container). `select` picks unstaged pages from the scan server's `/index/full.jsonl`, worst damage first. `run` reads each scan (checked against the index size and sha256), makes the worker's grey q75 `page.jpg`, writes an empty ALTO (the scan fetcher is the only LoC client), and takes LoC's text, American Stories and `ocr-local` from the read-only `/srv/curated` mount (home-infra `scripts/lab/batch-ct-quality.sh --mounts`). It writes 500-page chunks (`--chunk-pages`, max 500) and waits while `MAX_PENDING` (8) are unshipped; a chunk becomes one bundle, or two past the 1 GiB `MAX_BUNDLE_SCAN`. Busy rounds recheck every 15 s, idle ones sleep `--every-secs`.
2. **Shipper** (`quality-ship.sh`, `usnm-quality-ship.timer`, 3 minutes after the last run, up to `MAX_CHUNKS` 30 `READY` chunks a run). Over the host trust (home-infra `scripts/host-ssh-trust.sh`) it copies a chunk to the server's `/project/quality/<campaign>/incoming/`, runs `quality_receive.py` (files against `SHA256SUMS`, every scan present, so staging never falls back to tile.loc.gov), `usnm-boinc-step quality-stage` with `QUALITY_SCANS_DIR`, then `usnm-boinc quality submit` in the server container. Nothing ships while the server has more than 300 results unsent.
   - Staging holds its own host lock; later steps ignore a unit until its `quality.json` (written last) or `submitted` marker exists. Submit waits up to 30 minutes for the step lock, else exits 75 with the chunk still `READY`. It skips the `quality-submit` bridge step (whose NFS walk held the lock up to 8 minutes): submit sets file modes and the export fixes owners. The next chunk is received while the current one stages, and that receive ends before the current submit takes the step lock. `--after-submit` keeps scans the next chunk also holds. Packing reads only the run's own pages and bundles (`bundle-<n>-<first page>`); a page already staged in none of them makes it read every bundle once.
   - **`tank/boinc-bulk` runs with `sync=disabled`** (2 November 2026, maintainer-approved; a 200-page chunk dropped from 30–45 minutes to about 3, #529). Only this dataset changed. A storage-host crash can lose the last seconds of acknowledged writes; ZFS commits transaction groups in order, so what survives is a consistent earlier state. Nothing half-written is submitted: receive rechecks `SHA256SUMS`, staging rechecks earlier units against `quality.json` hashes and restages mismatches, `quality submit` checks every input before `create_work`, and a unit already `submitting` goes to the uncertain-submit check. Revert on the storage host with `zfs set sync=standard tank/boinc-bulk`.
3. **Results** go to `usnm_quality` ([scripts/onprem/quality/README.md](../../../scripts/onprem/quality/README.md)); see [Result collection](#result-collection).

```sh
# On the feeder's Proxmox host, once: the trust and the mounts (home-infra).
scripts/host-ssh-trust.sh --from <feeder-proxmox-host> --to <server-proxmox-host>
bash scripts/lab/batch-ct-quality.sh --mounts
# From a checkout: the code and units.
boinc/scripts/onprem/quality-feed/quality-feed-install.sh --campaign v1-wave10 \
  --engine-image sha256:<worker digest>
# In the feeder container: the campaign's page list (the staged list: every doc_id in the share's
# quality/*/units/*/quality.json), then start.
quality_feed.py select --index index-full.jsonl --staged staged-docs.txt --curated /srv/curated \
  --count 500 --out /var/lib/usnm-quality-feed/v1-wave10/pages.jsonl
boinc/scripts/onprem/quality-feed/quality-feed-install.sh --campaign v1-wave10 \
  --engine-image sha256:<worker digest> --enable
```

#### Result collection

`usnm-quality-results.timer` on the preparation host runs `quality_results_partial.py` two minutes after each pass, with up to 15 s jitter. A pass collects **one submitted unit** under the server's nonblocking bridge lock (each native call 150 s, remote phase 240 s) into immutable `out/{pages.parquet,crops.parquet,summary.md}` plus `coverage.json` (`campaign_complete=false`) under `/project/ops/quality-results/scoped/<campaign>/<unit-key>/snapshots/<snapshot>/`, verifies checksums and job identity, and loads with certificate-authenticated `load.py results`. No pass loads more than one scope, and the host pass lock serializes collection, transfer and loading. One service run makes up to five passes (`--units-per-run`), because each unit's lock, transfer and load round costs about two minutes whatever its size. Each pass takes the bridge lock again, with a 5 s pause between passes so a waiting bridge step gets the lock first. No pass starts after the run has used 10 minutes (`--run-budget-secs`). A busy lock after the first pass ends the run quietly; any other failure still fails it. Guarantees: `<campaign>/out` is never replaced, rows are never deleted, a pending unit loads zero rows, and the cursor advances only after a verified load, so a failure in between repeats the same upserts. A busy bridge retries next tick; no producer is preempted. Collection never submits work or alters quorum, and a successful BOINC result isn't loaded until the loader succeeds. Judge coverage from cursor and load receipts, not timer frequency.

**Frontier.** `--completion-frontier` (on for the ordinary service) spends three of four collections on canonical-present units whose dependency fingerprint has no verified nonempty load and the fourth on a fair visit over all submitted units. `usnm-boinc quality collect --inventory-only --cache ...` computes the fingerprints in a separate scheduling namespace and fails closed past 10,000 QUALITY entries or on any inventory error; `--inventory-unit` rechecks the selected key after collection. The full inventory reads every unit over NFS and took more than 15 minutes for about 2,800 wave10 units (#1655), so it no longer runs inside a pass. At most once an hour (`--inventory-refresh-secs`), before a pass, the driver runs it **without the step lock** under its own budget (`--inventory-budget-secs`, default and service setting 1 hour, at most 2; `INVENTORY_BUDGET_SECS` in `quality-results.env` overrides it) and writes `inventory.json` beside `frontier.json`. A failed or timed-out refresh is reported, the last inventory stays in use, and the next attempt waits for the interval. A pass chooses from that saved inventory, then re-inventories only the chosen unit under the lock. That fresh key replaces the saved entry, and the collection must match it. Units submitted after the last refresh wait for the next one. A pass with no saved inventory, or whose saved inventory names a unit that's no longer submitted, fails closed. The selected-unit inventory, collection and recheck share a 220 s budget inside the 240 s remote limit; each native call gets at most 150 s, keeping 20 s for teardown and writing progress. A collector failure or timeout records scheduling progress first, so one poison result can't pin the priority lane, and its fingerprint stays unacknowledged for a later retry. Inventory and recheck errors abort without advancing either lane. Cold digest warming can still time out; finished digest receipts survive the retry. These hints are not validation or completeness.

Operator notes:

- Install the matching collector before the driver; old binaries reject the inventory and cache flags, which blocks loading a stale snapshot. Without `--completion-frontier` the driver keeps its frozen-prefix cursor; don't use the flag for isolated prefix reconciliation. Host `after`/`through` fields stay compatible; frontier state is `scoped/<campaign>/frontier.json`. Never seed or rewrite acknowledgements (`frontier_ack`) by hand; a scheduling namespace change invalidates them, and the first opt-in re-verifies canonical-present units once. Zero-row visits stay pending or failed, not success.
- Whole-campaign collectors must not read these partial directories.
- Acceptance (not shown by offline tests): record installed driver/binary hashes, timer settings, lock admissions and busy exits, inventory and collection durations (with cold warming) and receipt/ack progress. Over at least two hours of ordinary arrivals with real competing producers, measured completion-to-load lag must stay bounded, pending or late-changed units must reach a new verified key, and unchanged jobs must leave the priority lane while fair visits continue. With a fence of N units, revisiting takes at most N+1 fair visits. About eight arriving jobs/hour needs at least 10.7 successful scopes/hour. Keep #962 open until then, and weaken no producer, budget, timer, quorum, floor or lock to pass. (A read-only inventory of 136 units took 10.093 s on 2026-10-28, inside the 120 s gate.)
- The installer records the preparation container in `/etc/usnm/quality-results.env` (`CTID`, default 215; the preparation container, not the storage host's container with the same ID) and enables collection once the loader DSN exists. The six-hour summary timer refreshes ranking views.

**Native Qwen join.** Enable `usnm-qwen-results.{service,timer}` on the preparation host: complete-snapshot `quality_results.py` with one `--native-campaign NAME[=COLLECTOR]` per root, joining batch-0 QUALITY outputs, `/project/qwen/<campaign>` and batch-8 readings in `/project/usnmocr/results/8`. Set `NATIVE_QWEN_ARGS` (and if needed `ENGINE_IMAGE`) in `/etc/usnm/quality-results-qwen.env`, never in feeder settings. Collectors older than #1437 can't read version 3 or 4 jobs, so those roots name a newer one and `qwen-nonenglish-v1` keeps its own so its cached rows stay ([One submit binary and one collector](#one-submit-binary-and-one-collector-1449)). List a root only once it exists; a failing root doesn't stop the others and the service fails at the end naming it. Readings pending quorum join on a later pass. It waits up to 60 s for the bridge lock (`--step-lock-wait-secs 60`), holding nothing while it waits, and runs only collection under it, never transfer or loading. Exhausted admission is a failed run with no pull or load; real collection errors are not retried as contention. Collection is bounded to 20 minutes a root, the service to 100.

**Collector binary and row cache.** Deploy the collector at `/project/ops/quality-results/usnm-boinc` **before** these units (`QUALITY_COLLECTOR` or `--collector` for another path); do not replace the server's executable or restart the BOINC server container. Row parts are content-addressed (`collected-parts/`, partial scopes under `ops/quality-results/scoped/`), keyed on the collector, staged inputs, canonical outputs and Qwen jobs, so a late reading recomputes the affected rows and Surya changes invalidate all parts. Dependency changes during computation abort publication. `--max-dirty-units` (default 8) bounds recomputation: a pass with deferred units exits nonzero **before** publishing or loading, leaving the previous snapshot intact, so a cold cache takes several ticks. Corrupt parts or digest receipts fail closed and are kept. Large staged inputs get SHA-256 receipts under `collected-parts/input-digests/`, reused when device, inode, size, nanosecond mtime and ctime are unchanged; this needs close-to-open coherence and nanosecond ctime (true of the production NFS mount), and other filesystems fall back to hashing. Measure pass duration after warming before claiming capacity for a larger campaign.

Before relying on digest reuse on a new filesystem, test on that mount (its version alone is not enough) nonzero nanosecond ctime, same-size rewrites with restored mtime, atomic replacement and fresh-open visibility; then verify cold progress, a full publication and load, clean-pass idempotence and late-reading invalidation **before restoring any paused results timer**.

### Sustained non-English Qwen crop feed

The **Temporal singleton is the only Qwen prepare/ship owner**; `usnm-qwen-feed.timer` and `usnm-qwen-ship.timer` stay disabled. Preparation and shipping are separate activities, so a preparation failure never holds READY work. A campaign-local ship lock and the project mutexes serialize consumers and publication. The ordinary quality feeder is unaffected.

Install the helpers from `boinc/scripts/onprem/quality-feed/` at the activity's idle boundary, never alongside a running old shipper:

- `qwen_feed.py`, `quality-ship.sh`, `quality_receive.py` in `/opt/usnm-quality-feed/qwen-throughput/` (other campaigns' helpers untouched).
- `scan_quarantine.py` in the preparation container's `/opt/usnm-quality-feed/` **before** the matching `quality_feed.py` and `ocr_manifest.py`.
- `archive_alto.py` on the archive host at `/opt/usnm-quality-feed/`.
- `qwen_submit.py` and `qwen_provenance.py` in the server container at `/project/ops/qwen-feed/`.

`archive_alto.py` never runs as root. On the archive host it runs as a dedicated unprivileged system account, `usnm-alto` (shell `/bin/sh` so the forced command can run, password locked), which can write only to the ALTO cache (`/tank/locstats/qwen-alto`) besides its own home directory, and reads the world-readable bulk archives in `/tank/locbulk/ocr`. The shipper's key is in that account's `~/.ssh/authorized_keys`, not root's, with `restrict` and the forced command `nice -n 19 ionice -c 3 python3 /opt/usnm-quality-feed/archive_alto.py --pages - --archives /tank/locbulk/ocr --out /tank/locstats/qwen-alto`. The shipper's `ARCHIVE_HOST` SSH alias, `usnm-qwen-archive`, sets `User usnm-alto`. Check it with an empty request: `printf '' | ssh usnm-qwen-archive` must return the inventory JSON.

The pinned preparation venv suffices; no new packages or LoC clients. SSH destinations need verified host keys, never `StrictHostKeyChecking=no`. `/etc/usnm/qwen-feed.env` on the shipper: `CAMPAIGN` (a new campaign), `SERVER_HOST`, `ARCHIVE_HOST`, `SCAN_SERVER`, `ENGINE_IMAGE`, `QWEN_ENGINE_PLAN` (a reviewed `/project/.../plan.json` containing the pinned `engine`), `QWEN_MODELS` (the checksum-pinned shared-model directory, not another copy), `QWEN_MAX_READY` (default 1000 **fresh original replicas**, 500 untouched pairs; not the global unsent count). After installing, verify the singleton's real ordinary run and native audit.

Each tick skips documents already in Qwen roots and completed source chunks, picks non-English **available** scans by descending page damage from `quality.page_summary` (`select --damage-dsn-file`), and prepares at most 100 pages. Original ALTO is extracted from the retained bulk archives into `/tank/locstats/qwen-alto` (read-only `/srv/locstats` in the preparation container), each XML with a SHA256 receipt naming archive, member and signature, carried in the checksummed source chunks; cached XML and provenance are re-verified on later ticks. Missing ALTO is deferred a day, never synthesised. Extraction yields after 30 minutes per tick (finishing at least one archive); unvisited pages return as `pending` and stay eligible.

#### Undecodable source scans

A scan must match its indexed bytes and SHA256 before decoding (batch JP2s are also MD5-checked against LoC's bag manifest). That proves the scan server returned what was fetched, not that LoC's file is whole, so a re-fetch can return the same broken bytes. Preparation logs the feeder's stdout even on failure; READY counts are pages written.

**Decoder quarantine** (`--quarantine-decode`, used by Qwen and the CPU `usnm-quality-follow.service`): three known Pillow decode failures (unidentified, truncated, broken stream, with the codec present) on one `(doc_id, batch, batch_version, bytes, sha256)` quarantine that fingerprint as `manual_review`; it is excluded and never re-fetched, while a changed fingerprint is eligible again and a successful preparation resets earlier failures. Network, codec, format and index errors stay retryable. A buffer scan answering 404 (an OCR worker read and deleted it) is skipped as retryable; a missing keep scan is a failure.

**Truncated JP2s.** A header-only check finds a box or tile-part claiming bytes past end of file, or no EOC marker. Such a scan is never decoded or retried and is not a decoder failure: with `--quarantine-decode` it is recorded at once as `source_truncated` (a changed fingerprint is eligible again); without it (`usnm-quality-feed.service`) it goes to `skipped.txt` as `source_truncated: <reason>`. Logs: `source-truncated scans found` per chunk, `source-truncated scans in campaign` with counts per `<batch>_verNN`. A JP2 that reaches EOC but is otherwise malformed still goes to the decoder and the three-failure rule. Existing `manual_review` records are left unchanged. The same check (`usnm_ocr_contract::jp2`, vectors in [`fixtures/jp2-truncation-vectors.json`](../../../fixtures/README.md), #1457) keeps such scans out of all OCR, and the page keeps LoC's text:

- `usnm-ocr-worker prefetch` lists it in `<out>/source-truncated.jsonl` instead of writing it and never asks LoC for that batch version again.
- The on-prem node worker dead-letters it as `permanent` (`source_truncated: LoC's JP2 is cut short: <reason>`).
- The BOINC worker (`usnm_ocr`) fails that page before its engine runs; a bundle's other pages still run.
- The queue-fed controller with `--skip-truncated-scans` dead-letters it before pushing ([A page whose JP2 LoC holds cut short](../server/batch-scheduler.md#a-page-whose-jp2-loc-holds-cut-short-1457)).

State is in `/var/lib/usnm-quality-feed/<campaign>/decode-quarantine.sqlite3`. Inspect read-only:

```sh
pct exec 215 -- /opt/usnm-quality-feed/venv/bin/python -c \
  'import json,sys; from pathlib import Path; sys.path.insert(0,"/opt/usnm-quality-feed"); from scan_quarantine import quarantined; print(json.dumps([{k:v for k,v in r.items() if k != "manifest_body"} for r in quarantined(Path("/var/lib/usnm-quality-feed/qwen-nonenglish-v1"))], indent=2))'
```

The manifest builder reads `QWEN_QUARANTINE_ROOT` (default `/var/lib/usnm-quality-feed`, an error if missing) and excludes quarantined documents from new non-English acquisition, serialising with quarantine writers through `.decode-quarantine.lock`; keep that file in place while either service runs. A failed PUT releases the lock and is an error. Already queued manifests and independent OCR acquisition are not rewritten or blocked; recovery needs a separately reviewed plan.

**Install and rollback.** At an idle boundary install the quarantine helper and manifest builder first, then `quality_feed.py`, then the CPU follower with matching `qwen_source.py` and `qwen_feed.py`, restarting the follower at a safe boundary and keeping its outbox and state. On rollback keep quarantine state, READY chunks and queued work, and restore the old follower unit before an older feeder (which can't parse `--quarantine-decode` and would retry bad scans again). Keep the new `scan_quarantine.py`: an older one rejects `source_truncated` records and stops the feeders with an error. Restoring the old manifest builder needs that producer paused at an idle boundary and a reviewed recovery plan. An old Qwen controller ignores quarantine.

**Language budget.** Pages are reserved before preparation; a reserved failed fingerprint retries without spending another page. A reserved page later found `source_truncated` is refunded at the next tick (`<name>_budget_refund` in `<name>-budget.json`) if its current scan has that exact quarantine record and it isn't in `done.txt` or `skipped.txt`. A page whose scan LoC replaced stays spent. A fingerprint change after quarantine, or a failure before a retry record persists or between decoding and READY, can still need budget review.

#### Native admission and shipping

The shipper stages native 8B work (12 crops a page, shared models, quorum two, distinct hosts) **before** freeing source inputs. It refuses uncertain submissions, documents duplicated across campaigns, changed hashes, wrong routing or quorum, going past the fresh-replica target (including a per-chunk reservation), or more than 2000 unresolved native workunits (retries counted separately). A fresh pair is batch 8, quorum and target two, exactly two unsent, unassigned, unvalidated results; retries and other apps never count as fresh. The ordinary non-Qwen shipper's global unsent ceiling is unchanged.

Before transfer it runs the read-only admission helper with `--preflight` and reserves the whole chunk. Under capacity pressure it logs JSON `status=deferred` (phase, reason, counts, limits, audit SHA256) and exits 75, a normal deferral that transfers nothing; admission is checked again under the project mutex before publication. A deferral rewrites no audit. Uncertain `submitting` markers are hard errors (never delete `submitting` to replay work), and changed audited job bytes need explicit reconciliation, never automatic replay. Storage floors: 50 GiB on project, bulk and thin pool, plus 64 MiB per candidate page on the least-free of the project root, `quality/` and `usnmocr/download/`, so a separate bulk mount or NFS quota can't hide behind local free space. Preparation uses the same reservation to bound its selection. Up to three unshipped chunks are buffered. Native audits stay in `audit.json`, bound to job SHA256.

#### Transactional subset shipment (#1466, opt-in)

`quality-ship.sh --qwen-subsets` (from the Temporal activity, **only after the matching helpers and binary are installed**) ships part of a parent chunk instead of reserving it whole: the oldest parent's remaining sources in sample order, up to the headroom. Limits are unchanged (1,000 fresh replicas, 2,000 unresolved units, three READY parents, 50 GiB floors, 64 MiB a source).

What it guarantees:

- An immutable manifest, written before transfer, fixes the members (`doc_id,batch,batch_version,bytes,sha256`), input hashes and staging recipe, including the engine/model plan, staging-binary checksum and places bytes (`--recipe-inputs`). Retries reuse it, and the server rechecks the pins before staging, including after a storage deferral; any change to parent bytes, membership, recipe or provenance fails closed.
- `qwen stage`, `qwen submit` and `quality submit` take `--members FILE` (ordered JSON array of doc IDs) and never drain unrelated units. Each transfer uses a unique upload directory that the server verifies and adopts atomically under the host bridge mutex, never overwriting inputs another invocation may use. Every retained native job in the root, selected or not, must use the requested version. The archive receipt must bind both original and prepared ALTO; do not rewrite the receipt. Staged jobs without these bindings are not adopted and, like a quality-stage crash before bindings persist, need explicit reconciliation. LoC provenance is frozen at `quality/<campaign>/texts/<doc_id>.loc-provenance.json` and source bindings at `quality/<campaign>/shipment-quality/<doc_id>.json`.
- Capacity and the `pve/data` thin pool (bounded `lvs`, 64 MiB a page above 50 GiB) are checked under the container mutex, held through publication; `phase=locked_thin_pool` defers.
- An interrupted publication is reconciled against BOINC by workunit identity; missing, duplicate or conflicting history keeps the journal and parent and never clears `submitting`.
- Parent and copies stay until a complete disjoint set of members is acknowledged by native and quality publication; retirement then moves the parent to `retired-parent` and deletes it (`cleaned.json`). A new directory reusing a retired name is kept and rejected; legacy retirement records without incarnation evidence need reconciliation. A parent with a transaction journal is refused by the legacy whole-chunk path, so rollback must drain or reconcile it, not bypass the journal.

Receipts: journals at `<outbox>/.qwen-transactions/<campaign>/<chunk>/`, server evidence at `/project/qwen-shipments/<native-campaign>/<transaction-id>/`. `shipment_invocation` reports `publication_attempted`, `native_acknowledged` and `quality_acknowledged`. **Exit 75 is not a zero-publication receipt**: native success then quality failure is `status=partial`, and `publication_attempted=null` means unknown. Monitor receipts, not exit status. Anything the tool can't reconcile automatically (legacy chunks without provenance, older recipes, crossing bundles) needs an explicit operator reconciliation.

Before enabling (offline tests don't cover these):

- Install reviewed `qwen_shipment.py` beside `qwen_feed.py` and `qwen_submit.py`, the matching `quality-ship.sh`, and a hash-verified `usnm-boinc-stage` with the `--members` options; every transaction Rust command uses it, not an older server-image `quality submit`.
- At an idle boundary confirm writable transaction storage, canonical `places.txt` (or its `v1-wave9` source), guarded app versions, shared pinned models, no conflicting legacy partials, unbound receipts or retirement ownership records, and that each archive receipt satisfies the downstream cohort binder. Keep both feed/ship timers disabled.
- Verify host Python, `pct exec` pipe forwarding, `flock` and `pve/data` under the real mutex (the Linux CI rehearsal is not a production Proxmox proof).
- Run one bounded transaction and check identities, quorum, one-result-per-host, receipts after an interrupted response and all floors; review mutex duration and retirement before expanding.

Ramp from 200 fresh replicas to 1000 only after measured runs show new app-6/batch-8 original jobs, zero uncertain submissions, immutable inputs, phase/rate receipts and post-cleanup floors; 2000 replicas is still 1000 pairs and must fit the unresolved ceiling with existing jobs. The indexed scan ceiling (262,920 pages) is not a count of eligible pages. Report a zero-job hour as zero.

#### Manifest timer

`usnm-qwen-manifest.timer` (preparation container, from `boinc/scripts/onprem/ocr-feed/`) runs `build-quality --non-english` ten minutes after each run, using `SCAN_SERVER` from `/etc/usnm/quality-build.env`, the certificate-authenticated DSN and state in `/var/lib/usnm-ocr-manifest/qwen-nonenglish`; its service lock prevents concurrent runs. When upgrading, let the old feed's inline manifest finish first so two versions never write the checkpoint at once, verify one pass, then enable the timer independently of `usnm-qwen-feed.timer`. It requests missing non-English scans by descending damage with their real language, excluding retained or requested pages; consumed fetch-history entries are re-requested as keep pages, buffer-only `.done` markers are untouched, and confirmed keep scans are cached a day. Only the scan fetcher downloads, under its budget; user OCR keeps priority.

`waiting_for_crop_sources` is **not corpus completion**: deferred archives, inaccessible scans, failed validation and storage backpressure remain. Outputs still need the `quality-collect` join (`QWEN_SPOOL` = native batch-8 results) and archival before any input cleanup.

### Request versions on the feed

The outbox, `done.txt` and source requests stay under `CAMPAIGN`. The shipper routes each chunk from `QWEN_SHIP_ARGS` in `/etc/usnm/qwen-feed.env`:

```
QWEN_SHIP_ARGS="--prompt-version 3 --qwen-root NAME --qwen-yiddish-root NAME --yiddish-prompt-version 4"
QWEN_FEED_ARGS="--yiddish-budget 40"
```

- Chunks with no Yiddish go to `--qwen-root` at `--prompt-version`.
- Only-Yiddish chunks go to `--qwen-yiddish-root` at version 4.
- At version 3 or 4, a chunk mixing Yiddish with another language stays READY, as does Yiddish with no root; `qwen_submit.py` also refuses both.

`--yiddish-budget N` caps the Yiddish pages the feed will ever prepare (`yiddish-budget.json` in its state directory) and removes Yiddish from the normal order, so the budget can't become a reread: pages read at version 1 stay excluded, and raising the budget is an env-file decision. Without it Yiddish keeps its place in the order. `--german-budget N` does the same for German (`deu`) during calibration (`german-budget.json`): German comes as its own chunk after any Yiddish budget and goes to `--qwen-root` at `--prompt-version`. `--german-pages FILE` (doc_ids, one per line) limits it to a frozen sample, worst damage first; the November calibration used 50 of Study 1's pilot pages from #1428, which already have `frak2021` and `deu_latf` readings. `quality_feed.py select` filters with `--language` and `--exclude-language` (repeatable). `--serbian-budget N` and `--serbian-pages FILE` do the same for Serbian (`srp`), whose BOINC OCR is held for the comparison with LoC's text ([note](../../notes/2026-11-07-serbian-trust-hold.md)): Serbian chunks come after any Yiddish and German budgets, are recorded in `serbian-budget.json` and go to `--qwen-root` at `--prompt-version`; the refill already admits Serbian. The live feed has `--serbian-budget 500 --serbian-pages /etc/usnm/serbian-compare-pages.txt` (500 of the first Tesseract batch's pages, 71 or 72 per decade).

A new root must be added to `CAMPAIGN_ARGS` in the refill's env and to `NATIVE_QWEN_ARGS` once its quality root exists. The version 1 root stays out of the refill, so its backlog goes only to the classes open to it.

### Bundled native units (#1435)

`--qwen-bundle-size N` (2 to 32) in `QWEN_SHIP_ARGS` makes `qwen_submit.py --bundle-size N` stage up to N pages a native unit in `--qwen-root`, so `llama-server` loads the model once per bundle. The `qwen stage` limits close a bundle before 32 pages: the longest possible `readings.json` must stay within its 4 MiB upload limit (about 250 crops), so `--qwen-bundle-size 32` lets that decide, at about 25 pages of the feed's mean 9.8 crops. An under-filled last bundle waits up to six hours for the next chunk. The scheduler gives a host at most 6 `usnm_qwen` tasks in progress per GPU (`max_jobs_in_progress` in `boinc/project/config.xml.template`), so a pair isn't held for hours behind one host's queue. `diag.txt` stays at 64 KiB (the app's own limit), which a bundle passes from about 75 crops: its later per-crop sections are cut, which no verdict reads. Without the flag units stay one page; `--transaction` refuses any size but 1. Gains (#1435): about a third of task time on an RX 9070 XT, a fifth on NVIDIA, little on Apple and Intel. The loop guard saves more: repeated loop text is 57% of generated tokens on every vendor.

To turn it on:

1. **A new root.** `qwen_submit.py` won't bundle a root holding single-page units, and a bundled root refuses `--bundle-size 1`. Only-Yiddish chunks stay single-page in `--qwen-yiddish-root`.
2. **The collector first.** An inconclusive bundle's agreed pages reach the rows only through `quality collect --qwen-pages` (`QWEN_PAGES_ARGS`, #1572); without it they are lost. Before the first bundle ships, deploy #1572, give every native-root collector a build from it on, set `QWEN_PAGES_ARGS` in `usnm-qwen-results.service`'s env, and add the root to `NATIVE_QWEN_ARGS`.
3. **The refill.** Add the root to `CAMPAIGN_ARGS` once `qwen-auto-cohort.py` binds bundles (#1561); before that it stops at a bundled root's first unit.
4. **The apps.** `usnm-qwen-app` reads bundles since #887, so no new app version or signing is expected; check one bundle completes on each plan class first.

5. **Page-level verdicts.** Install a build from #1435's page-verdict change with its wrapper (an image from it on, or between rolls as [Turning the crop rule on and off](#the-crop-consensus-rule) describes), use the same build as the bundled root's collector (`NATIVE_QWEN_ARGS` `NAME=COLLECTOR`) and stage binary, add `--qwen-page-records /project/usnmocr/qwen/pages` to `QWEN_SHIP_ARGS`, then write `page-v1` to `<project>/qwen/bundle-verdict`. Every collector of a bundled root must run with `--qwen-pages` before the file is written: one without it joins a settled bundle's canonical reading of every page.

The watermarks still reserve one pair per page while a bundle is one pair for up to 32 pages, and `--qwen-max-ready` counts replicas, so the same 1,000 holds more pages. A disagreed page waits for the next shipped chunk's staging to be packed again.

### One submit binary and one collector (#1449)

The version 3 and 4 roots use the static build `/project/ops/qwen-feed/request-guard-44465088/usnm-boinc` (#1437) to submit (as `usnm-boinc-stage`, since a pre-#1437 image refuses them) and to collect (the collector at `/project/ops/quality-results/usnm-boinc` can't read them; `qwen-nonenglish-v1` keeps it). Both are settings, undone by removing what was set.

**Submission**, once the server image is built from #1437 or later: add `--qwen-submit-binary usnm-boinc` to `QWEN_SHIP_ARGS` in `/etc/usnm/qwen-feed.env` (needs `quality-ship.sh` and `qwen_submit.py` from #1449). If the image still refuses a version, the tick fails and the chunk stays READY; remove the flag to revert.

```
QWEN_SHIP_ARGS="--prompt-version 3 --qwen-root NAME --qwen-yiddish-root NAME --yiddish-prompt-version 4 --qwen-submit-binary usnm-boinc"
```

**The collector.** The row-cache key (`Cache::new` in `crates/usnm-boinc/src/quality/collect_cache.rs`) includes the collector's path and bytes, the `--qwen-crop-states` directory and, for bundles, `--qwen-pages`. Changing a root's collector makes every unit dirty, so `qwen-nonenglish-v1` (about 90 units at 8 per pass) would fail about 12 ticks with no new rows. Old parts stay, so switching back is free while they exist. No single collector keeps every key, since the older one can't read versions 3 and 4, so move it once, warmed first:

1. Never name the image's `usnm-boinc` as a collector, or every rebuild rewarms every root.
2. Use the static build versions 3 and 4 already use, so only `qwen-nonenglish-v1`'s keys change, independent of the image rebuild. Keep `request-guard-44465088/` while it is named.
3. Warm it as root on the BOINC host into a scratch output (the real cache; the published snapshot isn't touched); repeat until it exits 0 instead of `collection cache warmed 8 dirty units; at least N deferred`:

   ```sh
   pct exec 230 -- flock -w 60 /run/usnm-boinc-step.lock podman exec usnm-boinc-server \
     timeout --signal=TERM --kill-after=30s 20m /project/ops/qwen-feed/request-guard-44465088/usnm-boinc \
     quality collect --campaign qwen-nonenglish-v1 --root /project/quality/qwen-nonenglish-v1 \
     --spool /project/usnmocr/results/0 --qwen-root /project/qwen/qwen-nonenglish-v1 \
     --qwen-spool /project/usnmocr/results/8 --out-dir /project/ops/quality-results/prewarm/qwen-nonenglish-v1 \
     --cache /project/quality/qwen-nonenglish-v1/collected-parts --max-dirty-units 8
   ```

4. In `/etc/usnm/quality-results-qwen.env` only (`usnm-quality-results` reads `quality-results.env` and the `/project/ops/quality-results/usnm-boinc` link, and keeps its collector), name the build once and drop per-root `=COLLECTOR`. The next tick should print `loaded_at` for all three roots and exit 0; then delete `/project/ops/quality-results/prewarm/`.

   ```
   QUALITY_COLLECTOR=/project/ops/qwen-feed/request-guard-44465088/usnm-boinc
   NATIVE_QWEN_ARGS="--native-campaign qwen-nonenglish-v1 --native-campaign qwen-guard-v3-20261102 --native-campaign qwen-guard-v4-20261102"
   ```

The crop rule changes every root's key anyway (`CROP_STATES_ARGS`) and needs a collector from #1434 (#1490) on, which `request-guard-44465088` predates. If it is close, make one move: install such a build at a new path, warm all three roots with it and `--qwen-crop-states /project/usnmocr/qwen/crop-states`, then set `QUALITY_COLLECTOR` and `CROP_STATES_ARGS` together.

### OCR's prefetch manifests (#1055)

The scan fetcher (home-infra `usnm-loc-fetch`) serves OCR's manifests before the quality pass's. `boinc/scripts/onprem/ocr-feed/ocr_manifest.py` builds OCR's hourly (`usnm-ocr-manifest.timer`): pages queued in curated `ocr-local/queued/` (read live with the container's Arc identity) with no outcome in `ocr-local/outcomes/` and no scan on the scan server, users' requests first, at most 1,000 per manifest, PUT through the scan server as `/manifests/queue-ocr-<stamp>/manifest.parquet`. The next one waits until the last is 90% fetched or a day old.

LoC budgets (code: `crates/usnm-ocr-worker/src/loc/global.rs`, `crates/usnm-boinc/src/scans.rs`, `crates/usnm-ocr-worker/src/loc/bucket.rs`):

| Component                                                                        | Rate                                                                                                                                                |
| -------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| Host-wide LoC pacer (all LoC requests on the host, via the fetcher's state file) | 20 a minute (3 s gap); 12 a minute (5 s) for 24 hours after a 429, 403 or challenge page; reset with `usnm-ocr-worker loc-pacer --file <f> --reset` |
| `quality stage` / other staging (`--gap-secs`)                                   | default 5 s (12 a minute), minimum 3 s; also inside the host pacer when `USNM_LOC_GLOBAL_PACER` is set                                              |
| LoC's `ndnp-batches` bucket (`--loc-bucket-gap-ms`)                              | default 2 s, minimum 500 ms; separate from the host pacer                                                                                           |

### The full image pass and the piggyback (#962)

`ocr_manifest.py build-quality` (`usnm-quality-build.timer`, every 2 hours) takes the next issues from `usnm_quality`'s stored summary: pages with no image features yet, not staged, grouped by issue, issues ranked round-robin over (title, decade), worst first by their worst page (mean of the damage score and LoC's doubt, `1 - wc_mean`), the first `--strata` issues of each, every page of an issue. Whole issues let the fetcher request an issue's METS and batch manifest once for all its pages, about one request a page (the first stratum-1 manifest on 25 October, one page per (title, decade), cost about three bucket requests a page). `--batches` (`--pass-batches` to the installer) limits the pass to the batches a file names, such as LoC's bucket's public ones. Pages are queued through the scan server as `queue-quality-<stamp>`, run behind OCR's manifests in one-hour slices inside the global LoC budget. Rows are marked `eng`, so scans go to the fetcher's buffer set.

#### Worst-first re-OCR lists (#1415)

`quality_campaign.py` ([#1415](https://github.com/tgoodyear/usnewsmap.com/issues/1415)) builds a worst-first re-OCR list from the stored summary: every page the [damage ranking report](https://github.com/tgoodyear/usnewsmap.com/pull/1401) calls worth re-OCRing, plus an OCR-then-compare sample of the moderate band as `<campaign>-moderate`. It reads the database read-only and writes only to `--out-dir` (it stages and ships nothing): `plan.jsonl` (every page in order), `pages.jsonl` (pages `--index` has a scan for, for `quality_feed.py run`), `manifests/m00001.parquet` for the rest (marked `eng`), and `summary.json` (filters and counts by band, language and decade). The file header has the ordering and sampling rules.

Bands default to the report's: `--worst-above 0.25`, `--moderate-above 0.1`, `--min-occurrences 50` (`head + near`). Rates are rounded to `real` before the cut, matching the report. `--dsn-file` keeps the export as `input.csv` (cut in `input.json`); `--input` replays it and can only narrow the cut. `--cell-pages` (400) caps each language and decade cell of the sample, enough to detect a 0.02 drop in damage rate at 80% power ([#1416](https://github.com/tgoodyear/usnewsmap.com/pull/1416)).

Build in the `usnm-batch` container, keeping copies of OCR state and the scan index for a rebuild:

```sh
set -a; . /etc/usnm/quality-feed.env; set +a
out=/var/tmp/usnm-worst-first-$(date -u +%Y%m%d)
mkdir -p "$out.in"
cp -a /var/lib/usnm-ocr-manifest "$out.in/ocr-state"
curl -fsS "$SCAN_SERVER/index/full.jsonl" -o "$out.in/index.jsonl"
/opt/usnm-quality-feed/venv/bin/python /opt/usnm-quality-feed/quality_campaign.py \
  --campaign worst-first-1415 --seed worst-first-1415 --out-dir "$out" \
  --dsn-file /etc/usnm/quality-db/dsn --ocr-state "$out.in/ocr-state" --index "$out.in/index.jsonl"
```

A rebuild takes `--input "$out/input.csv"` instead of `--dsn-file`, and a new `--out-dir`.

Excluded: pages in `quality.ocr_outputs` or outstanding/imported in `--ocr-state`. Pages already in a quality campaign stay in, because a `usnm_quality` unit compares existing texts and doesn't read the page, unless `--campaign-exclusions` (off by default; enables `--exclude-list`). **The moderate sample shows nothing until its pages are OCRed**: a gain needs a `quality.ocr_outputs` row to compare with LoC's damage score.

`follow` takes every fetched page under its own campaign, so pages from these manifests land there unless `follow`'s `--staged` names them. With the fetcher's `--no-loc-prefixes queue-quality-` (`USNM_PREFETCH_NO_LOC_PREFIXES`), `queue-quality-*` manifests take JP2s only from LoC's bucket or a mirror, so pages of batches the bucket doesn't serve wait; without it they are fetched from LoC within the global budget.

#### The follower

`quality_feed.py follow` (`usnm-quality-follow.service`) gives a unit to every fetched page no campaign has staged, in fetch order, in full chunks of `--chunk-pages`. A part chunk waits until no chunk has been written for `--max-wait-secs` (6 hours), so bundles aren't tiny (median 16 pages before 5 November 2026). Pages with no curated LoC text yet wait; unpreparable pages are replaced from a reserve of a tenth of `--chunk-pages`, and a short chunk ("chunk came up short") is written only once overdue. Pages never worth retrying go to `skipped.txt`.

**Keeping damaged scans.** After a chunk ships, the pass's buffer scans are deleted except the most damaged (LoC's `damage_rate`, unscored last), up to `--keep-bytes` (1 TB from `quality-feed-install.sh`, about 210,000 pages, half the fetcher's 2 TB buffer cap). Kept scans stay on the scan server for re-OCR, listed in `<state>/kept.tsv` for all campaigns; a more damaged newcomer replaces the least damaged, and if that delete fails the newcomer goes instead, so the list never exceeds the budget. OCR's scans and the keep set are never deleted. `quality-feed-install.sh --follow` enables it with the shipper.

- A kept page leaves the list only when its sidecar is gone and a DELETE confirms the bytes are too.
- Pages OCR still wants are left alone. `ocr_manifest.py build` records them in `ocr-docs.txt` (up to 500 sidecar lookups a run, misses in `claim-misses.txt`) and rewrites `ocr-active.txt` with those not yet ended; the follower reads it (`--ocr-active`; until it exists, all of `ocr-docs.txt`). Both files are written and read under `kept.tsv.lock` (`--claims-lock`). A newly shipped page in either is OCR's, neither kept nor deleted.
- `--keep-verify` (500) longest-unchecked pages are looked up per round; a failed lookup goes to the back, and an unresponsive scan server ends that round's lookups, deletes and feeding, which the next round resumes.
- Every round, even before its campaign has shipped, the follower handles the list for every campaign it has followed (`<state>/followed.txt`), so chunks shipped after a campaign switch don't leave scans behind; pages leaving it go to `<state>/retired.txt` so no campaign takes back a page another let go.

**What bounds the supply.** Each round `follow` takes every indexed full-resolution scan with no unit, not in `--staged` and not done, up to the outbox's room, so while the shipper keeps up the feed rate equals the fetch rate: the pass's manifests (`--pass-pages` a run, 2 hours apart) plus OCR's and others. Raising it needs more bucket or LoC requests or refeeding staged pages, each a maintainer choice ([5 November 2026 measurements](../../notes/2026-11-05-quality-supply.md)).

## Calibration

`usnm-boinc quality calibrate` runs the three-cornered hat per gold crop over `--crops <crops.jsonl>` (gold crops with human transcriptions), two or more `--hyp ENGINE=FILE.jsonl` hypothesis files and optional `--readings <readings.jsonl>`, writing `calibration.csv`, `calibration.md` and `by-crop.csv` to `--out`: per reader, true CER, estimated CER, bias, MAE, Pearson and Spearman, without (`three-ocr`) and with (`three-ocr+vlm`) the vision-language reader. UNVERIFIED on the full multi-engine gold crop campaign.

## Measured

### Split-image disposable Docker acceptance

[Run 38787485639](https://github.com/tgoodyear/usnewsmap.com/actions/runs/38787485639) ([evidence](../../../boinc/apps/qwen/measurements/ci-38787485639.json); Linux/amd64, Docker 28.0.4, overlay2) measured both payloads; the jobs later failed at a default-network probe because the second daemon removed the host bridge, and the harness now uses a separate network namespace and an offline probe.

| Observed bytes                   | 2B CPU         | 8B GPU payload |
| -------------------------------- | -------------- | -------------- |
| Local gzip save/load stream      | 4,861,064,470  | 8,286,566,549  |
| Logical unpacked image           | 6,670,088,145  | 10,170,682,321 |
| Cold filesystem increase         | 6,725,922,816  | 10,226,704,384 |
| Sampled peak filesystem increase | 20,098,748,416 | 30,602,686,464 |

Sampled peaks plus 2 GiB workspace and 2 GiB reserve fit the 32 and 48 GiB bounds. Peaks are sampled at 100 ms and include other runner activity, so they are not exact upper bounds, and retained image size is not the cold extraction requirement. Limits of 6 GiB memory, no swap and 1024 PIDs (CPU quotas 4 and 2) passed for both; the synthetic 2B CPU reading peaked at 2,948,436 KiB child RSS. No NVIDIA inference ran. Pulls of published images, Podman extraction, native staging and live scheduler rejection still need the [authorized acceptance](#activation-and-acceptance-operator-authorization-required).

## Unconfirmed

- How far the hat's estimates are biased low on real pages, where readers fail on the same smudges (the calibration's question).
- How far the version 2 hat's `local_cer_sym` underestimates a badly garbled reader whose worst chunks don't place (they show as low `matched`, not error). No page-level transcription of a three-reader page exists yet.
- The crop-level estimate's agreement with the page-level one.
- Time and memory per unit on volunteers' hosts, and so the `create_work` bounds and the Qwen containers' limits.
- The Dice threshold for `usnm_qwen`, to be measured from the logged pairs.
