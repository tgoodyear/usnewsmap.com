# OCR quality on BOINC

The [transcription post-training pilot](boinc-transcription.md) reuses `usnm_qwen`
for held-out public-gold inference. Its references stay on the server; central
training is separate from volunteer work.

Issues: [#522](https://github.com/tgoodyear/usnewsmap.com/issues/522), [#533](https://github.com/tgoodyear/usnewsmap.com/issues/533), [#559](https://github.com/tgoodyear/usnewsmap.com/issues/559). Two apps estimate, for every page and without anyone transcribing it, how accurate and how complete each text we hold of it is, how much a careful reader could recover, and what the scan looks like. `usnm_quality` (CPU) compares the texts; `usnm_qwen` (Apple Silicon Metal and NVIDIA CUDA GPUs, with a multi-thread CPU fallback) adds a vision-language reading of a sample of crops, as one more reader. Both apps support **dual native and container distribution**: native platform executables across Linux (`x86_64-pc-linux-gnu`, `aarch64-unknown-linux-gnu`), macOS (`arm64-apple-darwin`, `x86_64-apple-darwin`) and Windows (`windows_x86_64`) that require no container runtime on volunteer hosts, alongside the container app versions (`docker`, `docker_mt_2b`, `docker_nvidia_cuda_8b`). The other apps are described in [boinc.md](boinc.md), [boinc-assessment.md](boinc-assessment.md) and [boinc-surya.md](boinc-surya.md).

Status: implemented in code and tested locally; new split-payload releases remain UNVERIFIED on the live BOINC server and volunteer hosts. Disk metadata observations below are distinguished from conservative limits and live acceptance.

## What a unit is

### usnm_quality

One page per workunit. Inputs, in template order: `page.jpg` (the full-size IIIF JPEG), `loc.xml` (LoC's ALTO), `texts.json`, `places.txt` and `quality.json`. `places.txt` is the campaign's, the same physical file for every unit (its name is its checksum: `usnm_quality_places_<sha256>`), marked sticky in the input template so a host downloads it once.

`quality.json` is `usnm_ocr_contract::quality::Job`: `v` (1), `id` (the sha256 of the job's fields), `doc_id`, `language`, `group`, `inputs` (`page`, `alto`, `texts`, `places`, each `{bytes, sha256, source}`), `engine {image}`, `readers` (`[{name, source}]`, in report order), `function_words`, `crop_seed` and `k_crops`. The host checks every input against its checksum before anything runs.

`texts.json` is `quality::Texts`: `{"page": {"width", "height"}, "readers": [{"name", "source", "text", "regions": [{"box": [x0, y0, x1, y1], "kind", "legibility"}], "alto": false}]}`. Every reader the job names must be in it once with the same source. LoC's text is carried here too, for one code path; LoC's words with boxes come from `loc.xml`.

Outputs, in template order: `result.json` (`quality::ResultManifest`, at most 256 KiB), `confusions.json` (a map from reader name to its top 50 confusion pairs, at most 256 KiB) and `diag.txt` (at most 64 KiB). The unit always writes all three. `result.json` holds `v`, `job_id`, `engine`, `outcome` (`ok`, `timeout`, `failed`, `unit_error`), `unit_error`, `page {image, layout}`, `readers`, `hat`, `legibility`, `timings {decode_ms, features_ms, text_ms, total_ms, cpu_ms, peak_rss_kib}`, `host`, the checksums and sizes of the other two files, and `completed_at`.

Per reader: `name`, `source`, `chars`, `words`, `est_cer` (the hat's estimate, clipped at 0), `est_cer_raw`, `est_cer_low_bound_note`, `completeness`, `precision`, `findable_content`, `findable_places` (with the counts `content_found`, `content_total`, `places_found`, `places_total`), and `pairs`: against every other reader by name, `cer_a` (this reader as the hypothesis), `cer_b`, `cer_sym` (character edits over the mean length), `char_edits`, `chars_a`, `chars_b`, `wer_a`, `wer_b`, `word_edits`, `words_a`, `words_b` and `dice`. Rates that have no denominator (an empty consensus, an empty reader) are null.

Every float in the record is rounded to six decimals; the computation is single-threaded and uses ordered maps throughout, so two hosts' records are the same bytes.

### usnm_qwen

One page per workunit. Inputs: `page.jpg` and `qwen.json` (`usnm_ocr_contract::qwen::Job`): `v`, `id`, `doc_id`, `language`, `group`, `input` (the page), `crops` (`[{id, box, stratum, loc_text}]`, at most 64), `engine {image, llama, models: [{name, file, mmproj, min_gpu_mb}], weights}`, `prompt_version` (1), `gpu_timeout_secs`, `cpu_timeout_secs` and `max_tokens_per_1000px2`. The crops are the same ones the quality unit of the page names (the same `crop_seed` and `k_crops`), with LoC's words in each box in visual line order.

Outputs: `readings.json` (`qwen::Readings`: `v`, `job_id`, `outcome`, `device`, `model`, `prompt_version`, `readings: [{id, text, tokens, ms}]`, `diag`), `result.json` (`qwen::ResultManifest`: the outcome, device, model, times, crops read, host, and the checksums of the other two) and `diag.txt`. Outcomes: `ok` (every crop read), `timeout` (the crops read so far are in the readings), `failed`, `no_gpu` (the GPU version ran without a visible GPU) and `unit_error`.

## The estimator

Everything is in `crates/usnm-ocr-quality`, a library with no BOINC in it.

- `align`: the edit script between two sequences with a backtrace, in linear space (Hirschberg over the last-row computation, a full table under 2^18 cells), with ties broken in a fixed order. The pairwise metrics normalise both texts as the gold set does (`usnm_ocr_survey::text::normalize`), count character edits with the bit-parallel distance, word edits over whitespace-split words, and word-bag Dice over lower-case alphanumeric tokens.
- `hat`: `d_ij = e_i + e_j` solved by least squares over the pairs (normal equations, Gaussian elimination with partial pivoting). Three readers with all three pairs give the exact solution `e_A = (d_AB + d_AC - d_BC) / 2` and a zero residual. Estimates are clipped at 0 with the raw value kept; the residual is the root mean square of `d_ij - e_i - e_j`; fewer than three readers, or too few pairs, leaves the system undetermined and every estimate null. The disagreement solved on is `cer_sym`.
- `consensus`: readers' texts as tokens (normalised, split on whitespace, leading and trailing punctuation stripped, matched case-folded). Every pair of readers is aligned at word level; matches link occurrences; the connected components with members from at least two readers are the consensus words. Completeness is the share of consensus words a reader has; precision the share of its tokens that are consensus occurrences. A reader's token aligned as a substitution against a consensus word it lacks is kept for the confusion counts.
- `findability`: content words are tokens with at least four letters that are not in the job's function-word list; place names are `places.txt` tokenised the same way and matched as consecutive token sequences. Each reader's rate is the share of the consensus sequence's distinct content words, and separately its place names, that the reader's tokens contain.
- `confusion`: the character alignment of each disagreeing word pair, consecutive non-matching steps merged into one pair (`rn` for `m` is one substitution), counted per reader, the top 50 by count.
- `features`: on a grey copy downscaled by an integer factor to a long side of at most 1,024 px: skew (the angle in -3 to 3 degrees, 0.1 degree steps, at which the ink's horizontal projection profile has the highest variance; positive when lines descend to the right), blur (variance of the 3x3 Laplacian), contrast (95th less 5th percentile of grey), noise (mean absolute 3x3 median residual), bleed-through (share of pixels in the middle third of grey between the ink and background modes), ink density (share below the Otsu threshold). From the ALTO: text blocks, lines, words, median line height, columns (blocks clustered by x-centre with a gap of half the median block width), mean word confidence.
- `crops`: the gold set's recipe (`scripts/ocr-survey/crops.py`): a line with at least 4 words whose word boxes span at least 600 px; the box is the line's left and top less 15 px, to its right (at most 1,600 px from the left) plus 15 px, down to 6% of the page height below the line's top. Six strata (terciles of the block's mean word confidence by top or bottom half of the page); `k` crops (12 by default) round-robin over the strata, each stratum shuffled by SplitMix64 from the seed.
- `report`: the record above, its JSON Schema (`report::schema()`), and the rounding.

Legibility in the unit is `1 - min est_cer` over the page's readers (`basis: page-hat`). The crop-level estimate with the vision-language reader, and the join with `usnm_qwen`'s readings, is done on the server by `quality collect`, not in the unit: the two apps run on different hosts. UNVERIFIED: how far the page-level and the crop-level estimates agree.

Text from `texts.json` is used as the readers' text for every reader, LoC included; `loc.xml` is used for the features and the crops.

## Dual native and container distribution

### The `boinc_finish_called` contract for native binaries

Stock BOINC does not accept a plain executable that writes its output files and exits `0`. In `client/app_control.cpp:533-546` (and the Windows branch at `475-480`), a task process that exits `0` without the file `boinc_finish_called` in the slot directory is treated as a premature exit (`handle_premature_exit`), restarted from scratch, and after 100 such exits aborted with `ERR_TOO_MANY_EXITS` ("too many exit(0)s"). Linking `boinc_finish(0)` in `api/boinc_api.cpp:864-872` (`lib/app_ipc.h:240`) writes `boinc_finish_called` containing `0\n`.

Rather than linking the C++ BOINC API across five targets or adding `samples/wrapper`, our native Rust binaries (`crates/usnm-quality-app` and the native `usnm_qwen` driver) satisfy the slot contract directly:

- Page and control inputs and outputs carry `<copy_file/>`, so the BOINC client places real files in the slot directory (`client/file_names.cpp:47-56`). Native Qwen's sticky GGUF inputs are the deliberate exception: their slot `<soft_link>` aliases resolve to shared project-cache files, not another model copy.
- During execution, the binary writes `fraction_done` (`0o644`) in the current directory.
- After flushing all output files (`result.json`, `confusions.json` or `readings.json`, and `diag.txt`) and setting `fraction_done` to `1.000000\n`, the binary atomically writes `boinc_finish_called` containing `0\n` as its final step before exiting `0`.

### Native `usnm_quality` (`crates/usnm-quality-app`)

`crates/usnm-quality-app` is a dependency-light native binary crate that depends only on `usnm-ocr-quality` and the pure contract types in `usnm-ocr-contract` (without `tokio`, `reqwest`, `arrow`/`parquet`, `rustix`, or `usnm-store`). `.github/workflows/boinc-quality-native.yml` builds it with `--locked` across all five BOINC platforms:

- `x86_64-pc-linux-gnu` (`x86_64-unknown-linux-musl`, static binary with no glibc floor)
- `aarch64-unknown-linux-gnu` (`aarch64-unknown-linux-musl`, static binary)
- `arm64-apple-darwin` (`aarch64-apple-darwin`)
- `x86_64-apple-darwin` (`x86_64-apple-darwin`)
- `windows_x86_64` (`x86_64-pc-windows-msvc`, `.exe`)

The workflow runs the cross-platform golden determinism test on every target and uploads `usnm_quality_<ver>_<platform>[.exe]` plus `SHA256SUMS`. On container hosts (`x86_64-pc-linux-gnu__docker`), `usnm_quality` also runs inside the standard worker image (`ocr/boinc/Containerfile`) under `ocr/boinc/job.toml` (1 CPU, 1.5 GiB).

### Native and container `usnm_qwen` (`ocr/boinc/qwen/llama.lock` and `<sticky/><no_delete/>` models)

`usnm_qwen` ships in two forms:

1. **Native driver + `ocr/boinc/qwen/llama.lock` + `<sticky/><no_delete/>` GGUF models**:
   - The native driver starts `llama-server` as a child process bound to `127.0.0.1` on an ephemeral port (`--no-webui --ctx-size 4096 --seed 522 --temp 0`, with `--n-gpu-layers 99` on Apple Metal and NVIDIA CUDA and `0` on `mt` CPU), polls `/health`, posts each crop as a PNG data URL to `/v1/chat/completions`, and writes `readings.json`, `result.json`, `diag.txt`, `fraction_done`, and `boinc_finish_called`.
   - `ocr/boinc/qwen/llama.lock` pins the upstream `llama.cpp` release tag (e.g. `b11630`) and the per-platform release archive URLs and SHA-256 digests (`macos-arm64`, `macos-x64`, `win-cuda-12.4-x64`, `ubuntu-x64`, `ubuntu-arm64`), verified before staging and signed as part of the BOINC app version.
   - Instead of baking multi-gigabyte weights into every native app version update, GGUF model and multimodal projector files (`Qwen3VL-8B-Instruct-Q4_K_M.gguf` 4.68 GiB + `mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf` 0.70 GiB for 8B; `Qwen3-VL-2B-Instruct-Q8_0.gguf` 1.71 GiB + `mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf` 0.41 GiB for 2B) are staged in the project's `download/` hierarchy as `usnm_qwen_model_<sha256>` alongside Apache-2.0 `LICENSE` and `NOTICE`. Their input templates (`ocr/boinc/usnm_qwen_8b_in`, `ocr/boinc/usnm_qwen_2b_in`) mark every weight file with **both `<sticky/>` and `<no_delete/>`**:
     - `<sticky/>` (`client/client_types.cpp:387, 652-668`, `sched/sched_send.cpp:76-118`) instructs the volunteer's BOINC client to retain the downloaded model file across workunits and merge existing file metadata (`cs_scheduler.cpp:878`) so each host downloads its model set only once.
     - `<no_delete/>` (`sched/file_deleter.cpp:22, 160-171`) instructs the server's `file_deleter` daemon never to unlink the shared model files from `download/` when the first workunit referencing them is assimilated and purged.
   - Serving sticky models from the server Container App's Azure Files share incurs Azure internet egress after the first 100 GB/month free tier (~$0.087/GB in North America: ~$0.50 per new 8B volunteer and ~$0.20 per new 2B volunteer).
2. **Container image (`ocr/boinc/qwen/Containerfile`, `ocr/boinc/qwen/weights.lock`, `ocr/boinc/qwen/NOTICE`)**:
   - Built on `ghcr.io/ggml-org/llama.cpp:server-cuda` at digest `sha256:b0baccbe2e00b5d74e63f85f9f023c93d61d0f329475269e1e7002e87eb875ea` (llama.cpp build 11610, `0.6.0-dev`), with no Python stack. **Separate payloads**: `QWEN_MODEL=2b` (the build default) includes only the 2B Q8_0 model and projector; `QWEN_MODEL=8b` includes only the 8B Q4_K_M model and projector. Both retain the pinned CUDA base (including CPU backends), immutable Hugging Face revisions, SHA-256 checks, full `weights.lock` identity, Apache-2.0 `LICENSE` and `NOTICE`. A full lock identifies the allowed artifacts, not a claim that every model in it is installed. `USNM_QWEN_MODEL` and the image's model label identify the actual payload. CPU execution cannot pick 8B; the 8B image refuses an insufficient or missing GPU instead of silently falling back. Smaller NVIDIA GPUs can use the native 2B class.
   - `.github/workflows/boinc-qwen.yml` builds and checks both payloads separately. Only a manually authorized run on `main` publishes `ghcr.io/tgoodyear/usnm-boinc-worker:qwen-2b-<commit>` and `:qwen-8b-<commit>`. Each gets a **different digest** in the step summary. Releases continue to use anonymous digest-pinned pulls and signed wrapper, Dockerfile and job files; the release preflight now checks anonymous config metadata against the plan class's model, pinned base and weights identity and compressed-size envelope.

## App versions and plan classes

Plan classes are defined in `contrib-compute/boinc/plan_class_spec.xml`:

| App            | Kind        | Plan class              | Platforms                                                                                                         | Runtime / resource requirements                                                                                                                                                                             |
| -------------- | ----------- | ----------------------- | ----------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `usnm_quality` | `native`    | _(default CPU)_         | `x86_64-pc-linux-gnu`, `aarch64-unknown-linux-gnu`, `arm64-apple-darwin`, `x86_64-apple-darwin`, `windows_x86_64` | Native binary (`crates/usnm-quality-app`), 1 CPU, writes `boinc_finish_called`                                                                                                                              |
| `usnm_quality` | `container` | `docker`                | `x86_64-pc-linux-gnu`, `aarch64-unknown-linux-gnu`, `arm64-apple-darwin`, `x86_64-apple-darwin`, `windows_x86_64` | `ocr/boinc/job.toml`: 1 CPU, 1.5 GiB, no swap                                                                                                                                                               |
| `usnm_qwen`    | `native`    | `apple_gpu_8b`          | `arm64-apple-darwin`                                                                                              | `<gpu_type>apple</gpu_type>`, Metal support (`min_metal_support` 1), >= 7,000 MB available unified RAM (~8 GiB Mac), 8B `Q4_K_M`                                                                            |
| `usnm_qwen`    | `native`    | `nvidia_cuda_8b`        | `x86_64-pc-linux-gnu`, `windows_x86_64`                                                                           | `<gpu_type>nvidia</gpu_type><cuda/>`, CUDA 12+, compute capability >= 5.0, >= 7,000 MB GPU RAM, 2 host CPUs, 8B `Q4_K_M`                                                                                    |
| `usnm_qwen`    | `native`    | `intel_vulkan_8b`       | `x86_64-pc-linux-gnu`, `windows_x86_64`                                                                           | `<gpu_type>intel</gpu_type><opencl/>`, llama.cpp Vulkan, >= 7,000 MB GPU RAM and 7,000 MiB free Vulkan memory, 8B `Q4_K_M`; integrated GPUs use about 8.6 GB of host RAM ([Intel GPUs](boinc-intel-gpu.md)) |
| `usnm_qwen` | `native` | `nvidia_vulkan_8b` | `x86_64-pc-linux-gnu` | NVIDIA detection/assignment through BOINC CUDA; execution through the same pinned Vulkan runtime as Intel, >= 7,000 MB GPU RAM and 7,000 MiB free Vulkan memory, 2 host CPUs, 8B `Q4_K_M` |
| `usnm_qwen`    | `native`    | `nvidia_cuda_2b`        | `x86_64-pc-linux-gnu`, `windows_x86_64`                                                                           | `<gpu_type>nvidia</gpu_type><cuda/>`, CUDA 12+, compute capability >= 5.0, >= 3,000 MB GPU RAM, 2 host CPUs, 2B `Q8_0`                                                                                      |
| `usnm_qwen`    | `native`    | `mt`                    | `x86_64-pc-linux-gnu`, `aarch64-unknown-linux-gnu`, `arm64-apple-darwin`, `x86_64-apple-darwin`, `windows_x86_64` | Multi-thread CPU (`min_ncpus` 4, `max_threads` 8), 2B `Q8_0` fallback                                                                                                                                       |
| `usnm_qwen`    | `container` | `docker_mt_2b`          | `x86_64-pc-linux-gnu`                                                                                             | `ocr/boinc/qwen/job-cpu.toml`: 2B only, `--cpus` from the thread count (4–8), 6 GiB, no swap                                                                                                                |
| `usnm_qwen`    | `container` | `docker_nvidia_cuda_8b` | `x86_64-pc-linux-gnu`                                                                                             | `ocr/boinc/qwen/job-gpu.toml`: 8B only, `--gpus all`, 2 CPUs, >= 7,000 MB GPU RAM, compute capability >= 6.0, 6 GiB, no swap                                                                                |

`scripts/boinc/install.py` supports both `--kind container` (the default) and `--kind native --binary <FILE>`. `create_work` bounds: `usnm_quality` 1e12 flops estimated, 1e14 bound, 1.5 GiB; `usnm_qwen` 1e13 estimated, 1e16 bound, 6 GiB. UNVERIFIED until the first units return.

Register the apps and platforms on the server (`scripts/start-boinc-bridge.sh --env prod --step register-apps --wait-mins 15`), then release the container or native versions with `scripts/boinc-app-release.sh` (see [contrib-compute/docs/app-release.md](../contrib-compute/docs/app-release.md)):

```sh
# Container versions:
scripts/boinc-app-release.sh --env prod --app usnm_quality --version 1.0 --signing-host <host>
scripts/boinc-app-release.sh --env prod --app usnm_qwen --version 1.0 --plan-class docker_mt_2b \
  --worker-image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:<qwen-2b-digest> --signing-host <host>
scripts/boinc-app-release.sh --env prod --app usnm_qwen --version 1.1 --plan-class docker_nvidia_cuda_8b \
  --worker-image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:<qwen-8b-digest> --signing-host <host>

# Native usnm_quality binary (from .github/workflows/boinc-quality-native.yml artifacts):
scripts/boinc-app-release.sh --env prod --app usnm_quality --version 1.1 --kind native \
  --platform arm64-apple-darwin --binary /path/to/usnm_quality_1.1_arm64-apple-darwin \
  --binary-sha256 <sha256> --signing-host <host>
```

## Qwen disk admission and cold-cache acceptance (#559)

[`ocr/boinc/qwen/disk-policy.json`](../ocr/boinc/qwen/disk-policy.json) is the
versioned policy used by submission, release checks, host preflight and tests.
**GiB = 1,073,741,824 bytes.** The scheduler always advertises the cold bound,
even for warm caches; presence of an image or tag is not proof that every shared
layer is complete. This intentionally over-reserves per concurrent unit.

| Target (`qwen submit --target`) | Payload                         | Cold `rsc_disk_bound`, bytes (GiB) | Warm incremental disk envelope                |
| ------------------------------- | ------------------------------- | ---------------------------------- | --------------------------------------------- |
| `container` (default)           | CPU 2B only                     | 34,359,738,368 (32)                | 2 GiB slot/workspace; no new image layers     |
| `container-8b`                  | GPU 8B only                     | 51,539,607,552 (48)                | 2 GiB slot/workspace; no new image layers     |
| `native-2b`                     | sticky 2B model + native bundle | 12,884,901,888 (12)                | up to 4 GiB: app bundle slot copy + workspace |
| `native-8b`                     | sticky 8B model + native bundle | 19,327,352,832 (18)                | up to 4 GiB: app bundle slot copy + workspace |

Warm numbers assume the **exact digest-qualified payload is already resident**,
are incremental rather than total retained cache, and exclude a further 2 GiB
free-space reserve. They are not the scheduler admission threshold: warm hosts
still need the full cold bound free. Multiple retained image or model revisions
consume additional disk; nothing prunes volunteers' caches.

Read-only metadata retrieved 2026-10-15 (sources in the policy):

- The pinned CUDA index's Linux/amd64 manifest is
  `sha256:cb7f872937c65952258e5670d8c1da7bae7394aef50c1c90a8bb251bde46c32e`:
  its compressed layer descriptors sum to **2,592,304,104 bytes**.
  This is the base only, not a new worker-image measurement.
- Pinned Hugging Face LFS metadata, whose SHA-256 OIDs match `weights.lock`:
  2B model **1,834,427,296** + projector **445,053,056** =
  **2,279,480,352 bytes**; 8B model **5,027,784,800** + projector
  **752,289,728** = **5,780,074,528 bytes**. Native and container paths use the
  same model bytes, not the same filesystem overhead.
- These observations fetch manifests, configs and LFS metadata only, not layer
  or model bodies. No local Docker daemon or cache was available; disposable CI
  measurements are recorded separately below. #559's historical ~27 GiB
  shared-layer `du` and ~20 GiB filesystem observation lacks exact artifact and
  node provenance in the issue, so it is **not** treated as a measured size of
  either new split image.

Conservative, **not measured**, envelopes: compressed image <= base compressed
bytes + selected uncompressed GGUF bytes + 512 MiB app and licence overhead;
unpacked image <= 8 GiB base + selected GGUF bytes + 512 MiB overhead. Cold
container admission covers compressed storage **plus two unpacked copies**
(extraction and copy overlap), 2 GiB workspace and 2 GiB reserve, rounded up to
32 GiB and 48 GiB. CI must fail rather than publish if the actual image exceeds
these envelopes. Native cold admission covers one shared model set, **two copies
of a native app bundle bounded at 2 GiB** (project cache plus slot), 2 GiB
workspace and 2 GiB reserve. `install.py` rejects larger native bundles. The
native model templates deliberately do **not** copy GGUFs into slots: BOINC
soft-link files resolve to the shared project model cache. Native bounds must be
reconsidered if this layout changes.

Before dispatch, stock BOINC's pinned `sched/sched_check.cpp::check_disk`
rejects `rsc_disk_bound > disk_available`; `sched_send.cpp` subtracts the bound
for each dispatched unit. Thus the old 2 GiB selection criterion is gone. BOINC
disk preferences and quota must permit the new bound as well as filesystem free
space. The managed rootless-Podman preflight checks free disk **before even
hello-world or a model-image pull**, on BOINC slots, image graph storage and
extraction scratch. Qwen requires these to share BOINC's filesystem so
subsequent scheduler reports reflect runtime consumption. The managed profile
puts graph storage and image-copy scratch under the BOINC data directory; a
separate mounted store or scratch directory is rejected, not optimistically
admitted. A cached image does not bypass this check. Startup pull budget is 30
minutes within a 40-minute service startup budget, not an inference deadline.
Manual Docker installations must provide equivalent co-location and free-disk
checks before enabling work; a server cannot inspect a remote Docker store.

### Activation and acceptance (operator authorization required)

1. Build and publish **both** split images via the reviewed manual workflow on
   `main`; retain the anonymous manifest and config check and signed app release
   procedure. Release CPU with the 2B digest and `docker_mt_2b`, GPU with the 8B
   digest and **new** `docker_nvidia_cuda_8b`. Legacy dual-model images and old
   GPU plan classes are refused by the release script. Do not reuse one digest.
2. Roll out the server base's updated batch routing, plan-class XML, bridge and
   templates **before** submitting new units. Batch 12 is the CPU container,
   batch 18 the GPU container, batch 2 native 2B and batch 8 native 8B. Other
   apps and legacy work retain batch 1's `docker_mt` or `docker_nvidia_cuda`;
   those old app versions cannot receive new batch 12 or 18 units. Retire
   superseded Qwen app versions through the normal reviewed release process; do
   not reset nodes or rewrite queued workunits. Previously submitted workunits
   keep their old bounds and must drain or be handled explicitly by the
   operator.
3. Stage separate CPU and GPU campaigns using each actual image digest, the
   pinned base digest and the full lock's weights identity from the build
   summary. Submit GPU with `--target container-8b` (bridge:
   `--var QWEN_TARGET=container-8b`); default `container` submits CPU. Native
   targets retain their model-specific sticky input templates. The workunit
   engine/identity continues to carry the immutable campaign image digest.
   Benchmark NOTICE inputs preserve the selected container target's batch and
   disk bound; native benchmark targets fail explicitly before submission
   because native NOTICE and model templates are not yet installed.
4. On a **disposable acceptance runner**, `qwen_cold_cache.py` streams a locally
   built, cached image into a new isolated Docker daemon, store and network
   namespace, without pulling, pruning, or changing the existing daemon's
   bridge. Each model's CI artifact `qwen-disk-2b` or `qwen-disk-8b` records
   image ID, local gzip archive bytes (not registry compressed bytes), logical
   unpacked size, graph `du`, filesystem delta and peak, second-load warm
   increment and load wall time. It asserts bounds and exact GGUF membership. Do
   not sum shared-layer `du` as unique filesystem usage. The build action checks
   all three error outputs for both payloads and runs the synthetic 2B CPU
   reading under the released memory, swap, PID and isolation limits. Native
   fake-model and loopback tests cover shared aliases and outputs.
5. Live acceptance still needs authorized publication, signing and rollout plus
   an eligible >=7 GB NVIDIA host: anonymous cold pull, all three output files,
   upload and validation, a warm repeat and a deliberately insufficient-disk
   host rejected before selection or pull. Record the exact digest, platform,
   runtime and storage driver; the free bytes on each filesystem before, after
   and at peak; transfer and extraction time; model load time; and inference
   time, each **separately**. No live reset or deletion, cloud build or
   deployment, or volunteer mutation is part of this code change.

Cold image transfer and extraction is shared startup overhead, not model
inference. The historical ~513-second elapsed acceptance must not calibrate
per-crop inference or FLOPS. Existing `load_ms`, `engine_ms` and per-crop timing
semantics are unchanged.

## Diagnostics

Both runners write `diag.txt` as the Surya worker does: from `init_data.xml` only the client version, app version, plan class, GPU type and device number, CPU count and memory bound (never the account key or host name); the container runtime and whether it is rootless; the cgroup limits and memory. `usnm_qwen` adds the NVIDIA device nodes, variables, driver and `nvidia-smi`'s output, the model chosen and llama-server's lines. `usnm_quality` adds the job's readers and the timings. A unit that cannot run (a checksum that does not match, a missing or different engine build) returns `unit_error` with the reason and no record.

## Redundancy and validation

Both apps: `min_quorum` 2, `target_nresults` 2, at most 4 results. `script_validator` runs:

- `usnm_quality_check` (`usnm-boinc quality check`): shape, sizes, UTF-8, the checksums of `confusions.json` and `diag.txt`, the record's consistency with its outcome. Every result is copied to `<project>/quality/seen/<result>/`.
- `usnm_quality_compare` (`usnm-boinc quality compare`): the same job and engine, the same outcome, and the canonical records equal: keys sorted, floats to six decimals, without the host, the timings, the time and the diagnostics. Every pair is appended to `<project>/quality/pairs.jsonl` with the verdict and, when they differ, which part differs.
- `usnm_qwen_check` (`usnm-boinc qwen check`): shape, sizes, the checksums of `readings.json` and `diag.txt`, the readings against the result (count, device, model, outcome). Every result is copied to `<project>/qwen/seen/<result>/`.
- `usnm_qwen_compare` (`usnm-boinc qwen compare`): the same job and engine, both read the page or neither, and per crop both read or neither and a word-bag Dice of at least the threshold. Until the probe has measured it, the threshold is 0.5 (`PROBE_MIN_DICE`), which only rejects unrelated text. Every pair is appended to `<project>/qwen/pairs.jsonl` with the Dice per crop and both hosts' devices and models.

The daemons are in `ocr/boinc/daemons.xml` and `contrib-compute/images/server/lib.sh`; the wrappers in `contrib-compute/images/bridge/`.

## CLI subcommands

### usnm-boinc quality

- `sample`: draws the campaign's stratified sample (`sample.jsonl`) from the prefetch manifest (`--manifest`, `--out`, `--pages` default 500, `--min-per-stratum` default 2, `--seed`, optional `--title-languages` JSON mapping LCCN to MARC language codes, optional `--with-as` listing doc ids with American Stories text, optional `--pages-file` restricting the draw to specific doc ids).
- `function-words`: writes `<out-dir>/<lang>.txt` with the top `N` (default 100) alphabetic words of at least four letters from the embedded wordfreq word list (`usnm_core::lang::wordlist`) for each `--lang` (`--lang` repeatable, `--top`, `--out-dir`).
- `texts-export`: reads the American Stories page Parquet shards (`curated/american-stories/pages/<lccn>/<year>-<nnn>.parquet`) and the newest published (`curated/ocr-local`) or pending (`--out-url`) overlay for each selected page (`--pages`/`--doc-id` or `--sample`), and writes `<out-dir>/<doc_id>.json` (`texts.json` without LoC's text, which `stage` fills from `loc.xml`) and `<out-dir>/<doc_id>.ours.xml` when our ALTO overlay exists (`--curated-url`, optional `--out-url`, `--out-dir`).
- `stage`: stages `usnm_quality` units under `<root>/units/<doc_id>/` (or `<root>/pages/<doc_id>/` packed into `<root>/units/bundle-<idx>-<first_doc_id>/` when `--bundle-size > 1`) and `<root>/places.txt`. Takes repeatable `--page group:lang:doc_id:batch[+batch...]` or `--sample` (a `sample.jsonl` from `quality sample`, with `--group` defaulting to `sample`), `--root`, `--engine-image`, `--texts-dir`, optional `--scans-dir`, `--places` (either a line-per-entry `.txt` or the catalog `places.json`, deduplicated into `<root>/places.txt`), `--function-words` or `--function-words-dir`, `--readers` (comma-separated, default `loc,as`), `--crop-seed` (default 522), `--k-crops` (default 12), `--gap-secs` (at least 5, default 5), `--max-pages` (default 20), `--bundle-size` (1 to 500 pages per workunit; default `1` on CLI, `300` in `bridge-job.sh` to target ~10–15 minute workunits), optional `--batch-cache`, and optional `--scan-archive`. When `--scans-dir` has `<doc_id>.jpg` and `<doc_id>.xml` from the hand-off container, `stage` reads those local files without touching the network. Otherwise it uses `crates/usnm-boinc/src/scans.rs`: it checks the scan archive first when `--scan-archive` is set (`keep/<batch>/<lccn>/<date>/<edition>/<seq>.jpg`, verified against its stored `sha256`), and falls back to `tile.loc.gov` through the page's batch (`--batch-cache` caches the batch manifest and issue METS on the storage service), paced at one request at a time with at least `--gap-secs` between requests. It never calls `www.loc.gov`'s item API. It parses `loc.xml` for the page dimensions and LoC's text, merges `<texts-dir>/<doc_id>.json` (normalizing reader `source` strings and region `"box"`/`"rect"` coordinates, and copying `<doc_id>.ours.xml` when present), and writes `page.jpg`, `loc.xml`, `texts.json`, and `quality.json`.
- `submit`: hands staged units in `<root>/units/` to `create_work` (`--root`, `--project`, `--max-units` default 100), copying `places.txt` once to `download/usnm_quality_places_<sha256>` and each unit's inputs into the project's download hierarchy with write-ahead `submitting` and `submitted` markers.
- `local-unit`: writes a unit directory from local files (`--page`, `--alto`, `--texts`, `--places`, `--function-words`, `--dir`, `--engine-image`, `--crop-seed` default 522, `--k-crops` default 12) so `usnm-boinc work --dir <dir>` can run without LoC or BOINC.
- `check`: `script_validator`'s `--init_script` (`--files <result.json> <confusions.json> <diag.txt>`, optional `--keep <dir>`).
- `compare`: `script_validator`'s `--compare_script` (`--files <r1...> --files2 <r2...>`, optional `--log <pairs.jsonl>`).
- `collect`: joins the assimilated canonical results of `usnm_quality` (`--root`, `--spool`) and `usnm_qwen` (`--qwen-root`, `--qwen-spool`) for `--campaign`. Recomputes the crop-level three-cornered hat per page when a `usnm_qwen` reading is present (`legibility.basis` becomes `crop-hat+qwen`), and writes `pages.parquet`, `crops.parquet`, and `summary.md` either to `--out-dir` or to `--curated-url` under `audit/quality/<campaign>/`.
- `calibrate`: runs the estimator on gold crops with human transcriptions (`--crops <crops.jsonl>`, repeatable `--hyp ENGINE=FILE.jsonl`, optional `--readings <readings.jsonl>`, `--out <dir>`) and writes `calibration.csv`, `calibration.md`, and `by-crop.csv`.
- `estimate`: runs the quality computation offline over a directory of pages (`--dir`, `--out`, `--places`, `--function-words`, `--crop-seed` default 522, `--k-crops` default 12, repeatable `--reader NAME=PATTERN` for the OCR survey layout `<dir>/sample/{images,alto,loctext}`), verifying the originating `qwen.json` (or `readings/<page>.qwen.json`) and joining on its recorded crops when `readings.json` is present, and writing per-page `result.json`, `confusions.json`, and `crops.jsonl` plus aggregate `pages.jsonl` and `crops.jsonl`.
- `summary`: reads `<root>/units/`, `<project>/quality/seen` (`--seen`), and `<project>/quality/pairs.jsonl` (`--pairs`) and prints a Markdown summary table of kept results and compared pairs.

### usnm-boinc qwen

- `stage`: reads staged `usnm_quality` units from `--quality-root` (`--root`, `--engine-image`, `--engine-llama`, `--engine-weights`, `--gpu-timeout-secs` default 1800, `--cpu-timeout-secs` default 7200, `--max-tokens-per-1000px2` default 4, `--max-pages` default 20), cuts the same deterministic crops from each unit's `page.jpg` and `loc.xml` (`crop_seed` and `k_crops` from `quality.json`) without contacting LoC again, and writes `<root>/units/<doc_id>/{page.jpg, qwen.json}`.
- `submit`: hands staged `usnm_qwen` units in `<root>/units/` to `create_work` (`--root`, `--project`, `--max-units` default 20) with write-ahead `submitting` and `submitted` markers.
- `local-unit`: writes a unit directory from `--image` and `--alto` (or `--synthetic`, which generates a built-in two-line block-capitals page and ALTO) into `--dir` (`--engine-image`, `--engine-llama` defaulting to `USNM_QWEN_LLAMA_IMAGE`, `--engine-weights` defaulting to `USNM_QWEN_WEIGHTS`, `--crop-seed` default 522, `--k-crops` default 12, `--timeout-secs` default 1800, `--max-tokens-per-1000px2` default 4).
- `probe`: runs `/app/llama-server --version` and checks that every model and multimodal projector in `usnm_boinc::qwen::default_models()` exists under `/opt/qwen/models`, printing their sizes without loading weights into memory.
- `check`: `script_validator`'s `--init_script` (`--files <readings.json> <result.json> <diag.txt>`, optional `--keep <dir>`).
- `compare`: `script_validator`'s `--compare_script` (`--files <r1...> --files2 <r2...>`, `--min-dice` default `0.5`, optional `--log <pairs.jsonl>`).
- `summary`: reads `<root>/units/`, `<project>/qwen/seen` (`--seen`), and `<project>/qwen/pairs.jsonl` (`--pairs`) and prints a Markdown summary table of kept results and compared pairs.

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

On the server (`contrib-compute/images/server/bridge-job.sh`) and the hand-off job in the main Container Apps environment (`contrib-compute/images/bridge/handoff-job.sh`), a quality campaign lives under `$SHARE_DIR/quality/<QUALITY_CAMPAIGN>` and a Qwen campaign under `$SHARE_DIR/qwen/<QWEN_CAMPAIGN>` (each campaign name is 1 to 41 characters of `[a-z0-9-]`). UNVERIFIED against a live server.

1. **Export page texts (`handoff-job.sh`, `HANDOFF_STEP=texts-export`).** The hand-off job reaches `CURATED_URL` and `OUT_URL` privately in the main environment, runs `usnm-boinc quality texts-export` for `HANDOFF_PAGES` (comma-separated doc ids) or `HANDOFF_SAMPLE` (a sample file under `quality-samples/` in `HANDOFF_URL`), and uploads `<doc_id>.json` (and `<doc_id>.ours.xml` when present) to `HANDOFF_URL` under `texts/`:

   ```sh
   scripts/start-boinc-bridge.sh --env prod --target handoff --step texts-export \
     --var HANDOFF_SAMPLE=v1.jsonl --wait-mins 30
   ```

2. **Stage quality units (`bridge-job.sh`, `BRIDGE_STEP=quality-stage`).** When `HANDOFF_URL` is set, pulls `texts/` into `$SHARE_DIR/quality/<QUALITY_CAMPAIGN>/texts` and, if `QUALITY_SAMPLE` is a relative sample filename, pulls `quality-samples/` into `<campaign>/samples/`; when `QUALITY_LANGS` is set (comma-separated ISO 639-3 codes), writes `<campaign>/function-words` with `usnm-boinc quality function-words`. Runs `usnm-boinc quality stage` for `QUALITY_PAGES` (`group:lang:doc_id:batch[+batch...],...`) or `QUALITY_SAMPLE` (default `<campaign>/sample.jsonl`, with `QUALITY_GROUP` default `sample`), `QUALITY_READERS` (default `loc,as,ours-tesseract`), `QUALITY_PLACES` (default `<campaign>/places.txt`), `QUALITY_ENGINE_IMAGE` (`sha256:<worker-digest>`), `QUALITY_MAX` (default 100), and `QUALITY_BUNDLE_SIZE` (default `300`, up to `500` pages per bundle; at ~1.2–2.0s/page a 300–500 page bundle runs for **10–15 minutes (`0.17–0.25 hours`)** with per-page checkpointing and packs `5,000` pages into `10–17` bundled workunits), reading scans from `SCAN_ARCHIVE_URL` (`--scan-archive`) first and then `tile.loc.gov` through the batch (`--batch-cache`):

   ```sh
   scripts/start-boinc-bridge.sh --env prod --step quality-stage \
     --var QUALITY_CAMPAIGN=v1-wave5 \
     --var HANDOFF_URL=https://stusnmoffapzy.blob.core.windows.net/boinc-handoff \
     --var QUALITY_SAMPLE=v1-wave5.jsonl --var QUALITY_GROUP=v1-wave5 \
     --var QUALITY_ENGINE_IMAGE=sha256:<worker-digest> \
     --var QUALITY_LANGS=eng --var QUALITY_BUNDLE_SIZE=300 \
     --var QUALITY_MAX=5000 --wait-mins 45
   ```

3. **Stage Qwen units (`bridge-job.sh`, `BRIDGE_STEP=qwen-stage`).** Cuts the deterministic crops from the staged `usnm_quality` units in `$SHARE_DIR/quality/<QUALITY_CAMPAIGN>` into `$SHARE_DIR/qwen/<QWEN_CAMPAIGN>` without fetching from LoC again:

   ```sh
   scripts/start-boinc-bridge.sh --env prod --step qwen-stage \
     --var QUALITY_CAMPAIGN=v1-probe --var QWEN_CAMPAIGN=v1-probe \
     --var QWEN_ENGINE_IMAGE=sha256:<qwen-digest> \
     --var QWEN_ENGINE_LLAMA=sha256:b0baccbe2e00b5d74e63f85f9f023c93d61d0f329475269e1e7002e87eb875ea \
     --var QWEN_ENGINE_WEIGHTS=sha256:969266d1c24656977e06a6efacae04deac6d5093c77227fe55ac28975035cddd \
     --var QWEN_MAX=20 --wait-mins 15
   ```

4. **Submit both apps' workunits (`bridge-job.sh`, `BRIDGE_STEP=quality-submit` and `BRIDGE_STEP=qwen-submit`).** Hands the staged units to `create_work` with two results per workunit (`QUALITY_MAX` default 500, `QWEN_MAX` default 20) and invalidates `$PROJ/html/cache/f2/server_status.php_job_status` so `https://boinc.usnewsmap.com/server_status.php` reflects the new queue immediately (live uncached counts are also always available at `https://boinc.usnewsmap.com/server_status.php?counts=1`):

   ```sh
   scripts/start-boinc-bridge.sh --env prod --step quality-submit \
     --var QUALITY_CAMPAIGN=v1-probe --var QUALITY_MAX=500 --wait-mins 15
   scripts/start-boinc-bridge.sh --env prod --step qwen-submit \
     --var QWEN_CAMPAIGN=v1-probe --var QWEN_MAX=20 --wait-mins 15
   ```

5. **Inspect validator summaries (`bridge-job.sh`, `BRIDGE_STEP=quality-summary` and `BRIDGE_STEP=qwen-summary`).** Reads `<project>/quality/{seen,pairs.jsonl}` and `<project>/qwen/{seen,pairs.jsonl}` and writes `<campaign>/summary.md`:

   ```sh
   scripts/start-boinc-bridge.sh --env prod --step quality-summary --var QUALITY_CAMPAIGN=v1-probe
   scripts/start-boinc-bridge.sh --env prod --step qwen-summary --var QWEN_CAMPAIGN=v1-probe
   ```

6. **Collect canonical results (`bridge-job.sh`, `BRIDGE_STEP=quality-collect`).** Joins the assimilated `usnm_quality` (`QUALITY_SPOOL`) and optional `usnm_qwen` (`QWEN_CAMPAIGN` and `QWEN_SPOOL`) results into `<campaign>/out/{pages.parquet,crops.parquet,summary.md}`, or uploads them to `QUALITY_CURATED_URL` under `audit/quality/<QUALITY_CAMPAIGN>/`:

   ```sh
   scripts/start-boinc-bridge.sh --env prod --step quality-collect \
     --var QUALITY_CAMPAIGN=v1-probe --var QUALITY_SPOOL=/project/usnmocr/results/0 \
     --var QWEN_CAMPAIGN=v1-probe --var QWEN_SPOOL=/project/usnmocr/results/0 --wait-mins 15
   ```

7. **Monitor or manage multi-shard campaigns (`scripts/boinc-quality-campaign.py`).** For 5,000–10,000+ workunit campaigns sharded across parallel Container Apps executions (`v1-wave5`, `v1-wave6`), use the campaign CLI to split non-overlapping manifests into shard archives, inspect live shard + server queue progress, and restore the bridge job specification after completion:

   ```sh
   python3 scripts/boinc-quality-campaign.py prepare --campaign v1-wave6 --manifest sample.jsonl --out-dir /tmp/wave6 --count 10000 --shards 20
   python3 scripts/boinc-quality-campaign.py status --campaign v1-wave6 --shards 20
   python3 scripts/boinc-quality-campaign.py restore --env prod
   ```

## Calibration

`usnm-boinc quality calibrate` runs the three-cornered hat per gold crop over `--crops <crops.jsonl>` (gold crops with human transcriptions), two or more `--hyp ENGINE=FILE.jsonl` OCR hypothesis files, and optional `--readings <readings.jsonl>` vision-language readings, and writes `calibration.csv`, `calibration.md` and `by-crop.csv` to `--out`: per reader, true CER, estimated CER, bias, MAE, Pearson and Spearman, both without (`three-ocr`) and with (`three-ocr+vlm`) the vision-language reader. UNVERIFIED on the full multi-engine gold crop campaign.

## Measured

### Split-image disposable Docker acceptance

[Run
38787485639](https://github.com/tgoodyear/usnewsmap.com/actions/runs/38787485639)
measured both payloads on Linux/amd64, Docker 28.0.4, overlay2. [Committed
evidence](../ocr/boinc/qwen/measurements/ci-38787485639.json) records exact
image IDs, source, head and base revisions, artifact links and archive SHA-256
digests. Both measurement steps passed; the **overall jobs failed later** at a
separate default-network probe because the second daemon removed the host
bridge. The harness now uses a separate network namespace and an offline probe.

| Observed bytes                   | 2B CPU         | 8B GPU payload |
| -------------------------------- | -------------- | -------------- |
| Local gzip save/load stream      | 4,861,064,470  | 8,286,566,549  |
| Logical unpacked image           | 6,670,088,145  | 10,170,682,321 |
| Retained graph `du`              | 6,726,541,312  | 10,227,142,656 |
| Cold filesystem increase         | 6,725,922,816  | 10,226,704,384 |
| Sampled peak filesystem increase | 20,098,748,416 | 30,602,686,464 |
| Warm reimport increase           | 40,960         | 61,440         |

The sampled peaks plus 2 GiB workspace and 2 GiB reserve pass the existing 32
GiB and 48 GiB admission bounds; no threshold increase or skipped acceptance is
needed. Peaks are sampled at 100 ms and include other runner filesystem
activity, so these observations are not exact upper bounds. Retained image size
is not the cold extraction requirement.

Cold import took 248.29 seconds for 2B and 538.52 seconds for 8B; warm archive
reimport took 235.93 and 450.69 seconds. These are **local gzip Docker save and
load** times, not registry transfer, normal warm task startup, or inference
latency. Exact selected model membership, all three error outputs, and the
actual limits of 6 GiB memory, no swap and 1024 PIDs passed for both payloads
(CPU quotas 4 and 2 respectively). The synthetic 2B CPU reading passed
separately: model load 1,002 ms, engine 12,573 ms, child peak RSS 2,948,436 KiB.
No NVIDIA inference ran on these non-GPU runners.

Anonymous published-image pulls, managed Podman extraction, native
cross-platform staging, live scheduler rejection, and the full live and
calibration campaign still require the authorized acceptance described above.
These measurements do not claim those checks passed.

## Unconfirmed

- How much the hat's estimates are biased low on real pages, where readers fail on the same smudges (the calibration's question).
- Whether `cer_sym` (edits over the mean length) is the right disagreement to solve on when one reader drops much of the page; the calibration compares it with the alternatives.
- The crop-level estimate's agreement with the page-level one.
- Time and memory per unit on volunteers' hosts, and so the `create_work` bounds and the Qwen containers' limits.
- The Dice threshold for `usnm_qwen`, to be measured from the logged pairs.
