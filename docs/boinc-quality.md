# OCR quality on BOINC

The [transcription post-training pilot](notes/2026-10-16-boinc-transcription.md) reuses `usnm_qwen`
for held-out public-gold inference. Its references stay on the server; central
training is separate from volunteer work.

Issues: [#522](https://github.com/tgoodyear/usnewsmap.com/issues/522), [#533](https://github.com/tgoodyear/usnewsmap.com/issues/533), [#559](https://github.com/tgoodyear/usnewsmap.com/issues/559). Two apps estimate, for every page and without anyone transcribing it, how accurate and how complete each text we hold of it is, how much a careful reader could recover, and what the scan looks like. `usnm_quality` (CPU) compares the texts; `usnm_qwen` (Apple Silicon Metal and NVIDIA CUDA GPUs, with a multi-thread CPU fallback) adds a vision-language reading of a sample of crops, as one more reader. Both apps support **dual native and container distribution**: native platform executables across Linux (`x86_64-pc-linux-gnu`, `aarch64-unknown-linux-gnu`), macOS (`arm64-apple-darwin`, `x86_64-apple-darwin`) and Windows (`windows_x86_64`) that require no container runtime on volunteer hosts, alongside the container app versions (`docker`, `docker_mt_2b`, `docker_nvidia_cuda_8b`). The other apps are described in [boinc/README.md](boinc/README.md), [boinc-assessment.md](notes/2026-10-16-boinc-assessment.md) and [boinc-surya.md](boinc-surya.md).

Status: implemented in code and tested locally; new split-payload releases remain UNVERIFIED on the live BOINC server and volunteer hosts. Disk metadata observations below are distinguished from conservative limits and live acceptance.

## Repeatable German comparison report (#926)

`scripts/boinc/german-quality-report.py` replaces the external experiment's
`perpage.py`, `order.py` and reporting part of `crops.py`. It uses the collector's
existing comparison metrics, not another implementation of normalization or edit
distance. It needs only Python's standard library and runs offline:

```bash
python3 scripts/boinc/german-quality-report.py \
  --sample german-sample.jsonl \
  --pages pages.jsonl --crops crops.jsonl \
  --reader loc --reader ours-tesseract --reader tess-deulatf \
  --max-crops-per-page 12 --out german-report.json
```

Keep the frozen `quality sample` JSONL, with `doc_id` and `lang=deu`, separately
from the results. For an explicitly labelled retrospective reproduction, a list
of those fields from the original **input** cohort also works; deriving it from
successful result rows would hide missing pages. Export the collector's page
and crop tables to JSONL without changing their field names or values. The
historical experiment already has these two JSONL exports. `--max-crops-per-page`
must match the staging plan's `k_crops` upper bound, and every expected OCR reader
must be named, including `loc` or `loc-hist` for reference character/word counts.

The create-only JSON report binds the sample, both exports and reporter source
by SHA-256. It emits source-qualified page IDs, per-title/year and per-page
comparisons, whole-page word-bag Dice alongside order-sensitive CER/WER, and
crop CER/Dice against the reported VLM model. These are **model agreement, not
human CER or calibrated accuracy**. Crop scores retain the collector's
normalization; they are not the old `crops.py` script's separately normalized
scores. Distinct comparator model identifiers with comparable nonempty references
cannot be pooled; attempted models without usable references remain metadata.
Different reported sources for one reader have separate `by_source` summaries,
with null pooled metrics, including different Tesseract profiles hidden behind
`ours-tesseract`. Full per-page Surya sources contain page-specific job IDs;
these are retained, not rejected or stripped to invent a shared model
configuration. Cross-job pooling needs independently verified stable configuration
identities that these exports do not provide. Source identities
and known job IDs remain in each reader summary; conflicting page job IDs are
rejected before joining LoC reference rows. Both job IDs may be absent in older
exports, but one known and one unknown are not treated as the same job. Identifiers
such as `qwen3-vl-8b-q4km` are metadata, **not immutable model-file digests**.

Absent reader rows have **unknown outcomes**, not inferred failures or timeouts:
the collector emits no reader rows for failed, timed-out or given-up units.
Keep an independent unit-outcome inventory to distinguish these from pending work.
Reported reader-row outcomes, missing metrics, absent VLM references and empty
readings remain explicit. The collector preserves completed whitespace-only
readings and exports `vlm_reading_present`; they are not used in scoring or hats.
Legacy null text without this flag is **ambiguous**, not a known missing reading.
`k_crops` is only an upper bound: unused crop capacity
is **not missing work**, and actual unobserved staged crops remain unknown without
the staging manifests. Crop joins require identical exported rectangles, strata
and VLM reference metadata. An absent crop does not identify a timeout.
Native crop rows also export the quality `job_id` and reader `source`.
Known crop jobs and sources must match the corresponding page metadata.
Crop-only readers retain their own source and bind to the page's known quality
job. Missing crop provenance, unknown page jobs, or absent page rows are
**unverified** and excluded from crop score pools, not silently upgraded using
another row's metadata. Different crop sources have separate score groups too.
Means and medians use only successful pages with nonempty, known LoC reference
counts, or crops with known nonempty normalized VLM references. Page CER/WER
require normalized reference characters; page Dice requires reference words.
`loc-hist` self-scores are excluded as ambiguous when a distinct `loc` reader
is present, rather than mislabelled as comparisons against it.
Crop CER requires `vlm_reference_chars` and crop Dice `vlm_reference_words`,
exported by the native collector using its canonical normalization and Dice
word bags. Older exports without these counts have **unknown denominators**;
raw non-whitespace text is not proof of a nonempty normalized reference.
The nullable count columns retain an Int64 Parquet schema even without readings.
The derived row-cache format is advanced without deleting previous parts.
Empty/unknown reference exclusions and
observed/missing metric denominators are separate. Summaries are unweighted. Reader
`total_ms` is kept as reported, not summed into a fictitious campaign duration.
Layout and hat-derived accuracy metrics are deliberately omitted.
Scorer languages retain page/reader attribution and explicitly labelled
**reader-row** histograms, separately from the sample's declared German cohort:
a historical `und` score must not be relabelled as a German-configured run.
Use `--crop-reader surya` for an additional crop-only reader, not `--reader surya`.
Unselected native Surya crop rows are counted as ignored, not expected page work.

This extracts reporting, **not `build_units.py` or a completed 100-page
benchmark**. Continue to use the native `quality sample`, `texts-export`,
`stage` and `collect` helpers. In particular, do not copy our ALTO to
`loc.xml` and label it LoC's layout, as the external staging script did.
Curated LoC text and verified local master staging need an explicitly
provenanced native path when original LoC ALTO is unavailable.

To close #926, freeze approximately 100 pages across titles, years and layouts,
including #925's reading-order cases; pin native app, model and llama
artifacts independently of Docker identities; retain source/input hashes and
measured wall/CPU/memory costs; then rerun the report and review all missing
or failed coverage. A 12-page, one-title historical reproduction proves only
the reporting path. Human-labelled crops remain necessary for accuracy claims.

## What a unit is

### usnm_quality

One page per workunit. Inputs, in template order: `page.jpg` (the full-size IIIF JPEG), `loc.xml` (LoC's ALTO), `texts.json`, `places.txt` and `quality.json`. `places.txt` is the campaign's, the same physical file for every unit (its name is its checksum: `usnm_quality_places_<sha256>`), marked sticky in the input template so a host downloads it once.

`quality.json` is `usnm_ocr_contract::quality::Job`: `v` (1), `id` (the sha256 of the job's fields), `doc_id`, `language`, `group`, `inputs` (`page`, `alto`, `texts`, `places`, each `{bytes, sha256, source}`), `engine {image}`, `readers` (`[{name, source}]`, in report order), `function_words`, `crop_seed` and `k_crops`. The host checks every input against its checksum before anything runs.

`texts.json` is `quality::Texts`: `{"page": {"width", "height"}, "readers": [{"name", "source", "text", "regions": [{"box": [x0, y0, x1, y1], "kind", "legibility"}], "alto": false}]}`. Every reader the job names must be in it once with the same source. LoC's text is carried here too, for one code path; LoC's words with boxes come from `loc.xml`.

Outputs, in template order: `result.json` (`quality::ResultManifest`, at most 256 KiB), `confusions.json` (a map from reader name to its top 50 confusion pairs, at most 256 KiB) and `diag.txt` (at most 64 KiB). The unit always writes all three. `result.json` holds `v`, `job_id`, `engine`, `outcome` (`ok`, `timeout`, `failed`, `unit_error`), `unit_error`, `page {image, layout}`, `readers`, `hat`, `legibility`, `timings {decode_ms, features_ms, text_ms, total_ms, cpu_ms, peak_rss_kib}`, `host`, the checksums and sizes of the other two files, and `completed_at`.

Per reader: `name`, `source`, `chars`, `words`, `est_cer` (the hat's estimate, clipped at 0), `est_cer_raw`, `est_cer_low_bound_note`, `completeness`, `precision`, `findable_content`, `findable_places` (with the counts `content_found`, `content_total`, `places_found`, `places_total`), and `pairs`: against every other reader by name, `cer_a` (this reader as the hypothesis), `cer_b`, `cer_sym` (character edits over the mean length), `char_edits`, `chars_a`, `chars_b`, `wer_a`, `wer_b`, `word_edits`, `words_a`, `words_b` and `dice`. Rates that have no denominator (an empty consensus, an empty reader) are null.

Every float in the record is rounded to six decimals; the computation is single-threaded and uses ordered maps throughout, so two hosts' records are the same bytes.

### usnm_qwen

One page per workunit. Inputs: `page.jpg` and `qwen.json` (`usnm_ocr_contract::qwen::Job`): `v`, `id`, `doc_id`, `language`, `group`, `input` (the page), `crops` (`[{id, box, stratum, loc_text}]`, at most 64), `engine {image, llama, models: [{name, file, mmproj, min_gpu_mb}], weights}`, `prompt_version` (1 or 2; below), `gpu_timeout_secs`, `cpu_timeout_secs` and `max_tokens_per_1000px2`. The crops are the same ones the quality unit of the page names (the same `crop_seed` and `k_crops`), with LoC's words in each box in visual line order.

Outputs: `readings.json` (`qwen::Readings`: `v`, `job_id`, `outcome`, `device`, `model`, `prompt_version`, `readings: [{id, text, tokens, ms}]`, `diag`), `result.json` (`qwen::ResultManifest`: the outcome, device, model, times, crops read, host, and the checksums of the other two) and `diag.txt`. Outcomes: `ok` (every crop read), `timeout` (the crops read so far are in the readings), `failed`, `no_gpu` (the GPU version ran without a visible GPU) and `unit_error`.

`prompt_version` fixes the whole request a reading is made with, so two readings of a job always used the same one. Version 1 sends the prompt with llama.cpp's default image encoding: a crop at its native resolution, one token per 32x32 pixels, about 200-400 tokens for a newspaper crop. Version 2 sends the same prompt and starts `llama-server` with `--image-min-tokens 1024`, the minimum llama.cpp gives for Qwen-VL; the crop is upscaled to it. On the #529 benchmark (39 crops, one GPU, equal token budgets) version 2 took the words of the six image-checked Yiddish crops from 23% to 59%, cut the loops on crops whose 1.14 reading looped from 14 of 14 to 5 of 14, and left the English, Serbian and repeated-print crops as they were. Version 3 is version 1's encoding and version 4 is version 2's, each with the loop guard: Study 5's `stop` rule. The request streams, generation stops when `tail_repetition` fires, checked every eight tokens from the twelfth, and the reading keeps the text before the repeated run plus one period. Closing the connection is what stops llama-server. A reading that repeats at its natural end is cut the same way, so a printed repeat loses its copies too; its generation record keeps the raw text's digest and length and says `repetition_loop_guard_cut`. A stopped reply never reaches its usage event, so its `tokens` is the count of text events, recorded as `loop_guard.stream_deltas` with `completion_tokens_reported` empty. `qwen stage --prompt-version` and `qwen_submit.py --prompt-version` choose the version, default 1. Versions 3 and 4 need app version 1.17, and older versions refuse them. `qwen_submit.py` defers a chunk (exit 75, READY kept) while any dispatchable app version can't read the version asked for, and refuses to add a second version to a root. The feed stages version 3, and version 4 for Yiddish; see [Request versions on the feed](#request-versions-on-the-feed). The refill reads `qwen-nonenglish-v1` unless `CAMPAIGN_ARGS` in its env names other roots: `--campaign NAME:VERSION`, repeated, each root staged at one request version. A named root needs an `audit.json`, which can start as `{}`. A root staged with bundles (`qwen stage --bundle-size` above 1) is bound the same way, one bundle at a time: the bundle unit the producer submitted, its `membership.json`, and each page's staged job, slice of the bundled `page.jpg`, quality record, provenance and sample. A bundle is admitted only if every page's language is, so one page outside the cohort passes the whole bundle over. `--max-admissions` and `--max-inflight-per-host` count units, and a bundle is one unit however many pages it holds. The refill admits Yiddish and Serbian. `--cohort-language LANG` (repeatable) admits another language too. It admits only what the feed staged, so the feed's budget for that language caps it. A unit passed over for its language is bound again once that language is admitted.

Under versions 1 and 2, readings are kept as read. `diag.txt` has each crop's generation record (`generation_v1/<crop>`, schema 2, or 3 with a `loop_guard` section under the guard), with `tail_periodic_repetition` when the raw output ends in a run repeated at least three times over at least 30 characters, and `repetition_loop_at_token_budget` when that run also used up the token budget. Printed text can repeat (a shelf of book spines); a model that loops runs out of tokens. The readings' `diag` says `loop_guard` (`stop` or `none`), names the looped crops, cut ones included, in `repetition_loop_crops` (a count if the names do not fit) and, always complete, `repetition_loop_crop_bits` (hex; bit `i % 8` of byte `i / 8` is the job's crop `i`), and the request's `image_min_tokens`.

## The estimator

Everything is in `crates/usnm-ocr-quality`, a library with no BOINC in it.

- `align`: the edit script between two sequences with a backtrace, in linear space (Hirschberg over the last-row computation, a full table under 2^18 cells), with ties broken in a fixed order. The pairwise metrics normalise both texts as the gold set does (`usnm_ocr_survey::text::normalize`), count character edits with the bit-parallel distance, word edits over whitespace-split words, and word-bag Dice over lower-case alphanumeric tokens.
- `agreement`: two readers with no third (#962): the pair the hat takes, word-bag cover each way and the length ratio, versioned. `usnm-ingest ocr-agreement` runs it for LoC against American Stories on every page that has both; see [Text agreement](ocr-text-agreement.md).
- `hat`: `d_ij = e_i + e_j` solved by least squares over the pairs (normal equations, Gaussian elimination with partial pivoting). Three readers with all three pairs give the exact solution `e_A = (d_AB + d_AC - d_BC) / 2` and a zero residual. Estimates are clipped at 0 with the raw value kept; the residual is the root mean square of `d_ij - e_i - e_j`; fewer than three readers, or too few pairs, leaves the system undetermined and every estimate null. The unit solves on whole-page `cer_sym` (hat version 1, in `result.json`); `collect` re-solves the page hat on the passage-matched disagreement (version 2, below) and writes that into `pages.parquet`.
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
   - Serving sticky models from the Azure server's Container App and Azure Files share incurred Azure internet egress after the first 100 GB/month free tier (~$0.087/GB in North America: ~$0.50 per new 8B volunteer and ~$0.20 per new 2B volunteer). The on-prem server (prod since 21 October 2026) serves them from the host's project tree, outside Azure.
2. **Container image (`ocr/boinc/qwen/Containerfile`, `ocr/boinc/qwen/weights.lock`, `ocr/boinc/qwen/NOTICE`)**:
   - Built on `ghcr.io/ggml-org/llama.cpp:server-cuda` at digest `sha256:b0baccbe2e00b5d74e63f85f9f023c93d61d0f329475269e1e7002e87eb875ea` (llama.cpp build 11610, `0.6.0-dev`), with no Python stack. **Separate payloads**: `QWEN_MODEL=2b` (the build default) includes only the 2B Q8_0 model and projector; `QWEN_MODEL=8b` includes only the 8B Q4_K_M model and projector. Both retain the pinned CUDA base (including CPU backends), immutable Hugging Face revisions, SHA-256 checks, full `weights.lock` identity, Apache-2.0 `LICENSE` and `NOTICE`. A full lock identifies the allowed artifacts, not a claim that every model in it is installed. `USNM_QWEN_MODEL` and the image's model label identify the actual payload. CPU execution cannot pick 8B; the 8B image refuses an insufficient or missing GPU instead of silently falling back. Smaller NVIDIA GPUs can use the native 2B class.
   - `.github/workflows/boinc-qwen.yml` builds and checks both payloads separately; a pull request that changes only the crates or the files they embed outside `ocr/boinc/qwen/` builds 2b alone, since 8b shares its `usnm-boinc` build (#1585). Only a manually authorized run on `main` publishes `ghcr.io/tgoodyear/usnm-boinc-worker:qwen-2b-<commit>` and `:qwen-8b-<commit>`. Each gets a **different digest** in the step summary. Releases continue to use anonymous digest-pinned pulls and signed wrapper, Dockerfile and job files; the release preflight now checks anonymous config metadata against the plan class's model, pinned base and weights identity and compressed-size envelope.

## App versions and plan classes

Plan classes are defined in `contrib-compute/boinc/plan_class_spec.xml`. Not all of them have a
live version: since 2 November 2026 the live `usnm_qwen` versions are GPU only (`nvidia_vulkan_8b`,
`nvidia_vulkan_6gb_8b`, `amd_vulkan_8b`, `intel_vulkan_8b` and `apple_gpu_8b`), with no CPU version.

| App            | Kind        | Plan class              | Platforms                                                                                                         | Runtime / resource requirements                                                                                                                                                                                                                                         |
| -------------- | ----------- | ----------------------- | ----------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `usnm_quality` | `native`    | _(default CPU)_         | `x86_64-pc-linux-gnu`, `aarch64-unknown-linux-gnu`, `arm64-apple-darwin`, `x86_64-apple-darwin`, `windows_x86_64` | Native binary (`crates/usnm-quality-app`), 1 CPU, writes `boinc_finish_called`                                                                                                                                                                                          |
| `usnm_quality` | `container` | `docker`                | `x86_64-pc-linux-gnu`, `aarch64-unknown-linux-gnu`, `arm64-apple-darwin`, `x86_64-apple-darwin`, `windows_x86_64` | `ocr/boinc/job.toml`: 1 CPU, 1.5 GiB, no swap                                                                                                                                                                                                                           |
| `usnm_qwen`    | `native`    | `apple_gpu_8b`          | `arm64-apple-darwin`                                                                                              | `<gpu_type>apple</gpu_type>`, Metal support (`min_metal_support` 1), >= 7,000 MB available unified RAM (~8 GiB Mac), 8B `Q4_K_M`                                                                                                                                        |
| `usnm_qwen`    | `native`    | `nvidia_cuda_8b`        | `x86_64-pc-linux-gnu`, `windows_x86_64`                                                                           | `<gpu_type>nvidia</gpu_type><cuda/>`, CUDA 12+, compute capability >= 5.0, >= 7,000 MB GPU RAM, 2 host CPUs, 8B `Q4_K_M`                                                                                                                                                |
| `usnm_qwen`    | `native`    | `intel_vulkan_8b`       | `x86_64-pc-linux-gnu`, `windows_x86_64`                                                                           | `<gpu_type>intel</gpu_type><opencl/>`, llama.cpp Vulkan, >= 7,000 MB GPU RAM and 7,000 MiB free Vulkan memory, 8B `Q4_K_M`; integrated GPUs use about 8.6 GB of host RAM ([Intel GPUs](boinc-intel-gpu.md))                                                             |
| `usnm_qwen`    | `native`    | `nvidia_vulkan_8b`      | `x86_64-pc-linux-gnu`                                                                                             | NVIDIA detection/assignment through BOINC CUDA; execution through the same pinned Vulkan runtime as Intel, >= 7,000 MB GPU RAM and 7,000 MiB free Vulkan memory, 2 host CPUs, 8B `Q4_K_M`                                                                               |
| `usnm_qwen`    | `native`    | `nvidia_vulkan_6gb_8b`  | `x86_64-pc-linux-gnu`                                                                                             | Closed qualification profile: >= 5,500 MB GPU RAM and 5,120 MiB free Vulkan memory, 24 GPU layers, CPU vision projector, 2 host CPUs; same locked 8B `Q4_K_M`, no CPU-only or smaller-model fallback                                                                    |
| `usnm_qwen`    | `native`    | `amd_vulkan_8b`         | `x86_64-pc-linux-gnu`, `windows_x86_64`                                                                           | AMD/ATI OpenCL detection/assignment, pinned native Vulkan execution, >= 7,000 MB GPU RAM and 7,000 MiB free, 2 host CPUs, 8B `Q4_K_M`; [automatic eligible-host admission](boinc-amd-gpu.md) after signed publication/promotion, initial hardware use unverified (#885) |
| `usnm_qwen`    | `native`    | `nvidia_vulkan_2b`      | `x86_64-pc-linux-gnu`                                                                                             | Closed profile for NVIDIA cards too small for 8B (#1234): >= 4,500 MB GPU RAM and 4,096 MiB free Vulkan memory (estimates), BOINC >= 8.2.9, same pinned Vulkan runtime, 2B `Q8_0` and projector fully on the GPU, 2 host CPUs                                           |
| `usnm_qwen`    | `native`    | `nvidia_cuda_2b`        | `x86_64-pc-linux-gnu`, `windows_x86_64`                                                                           | `<gpu_type>nvidia</gpu_type><cuda/>`, CUDA 12+, compute capability >= 5.0, >= 3,000 MB GPU RAM, 2 host CPUs, 2B `Q8_0`                                                                                                                                                  |
| `usnm_qwen`    | `native`    | `mt`                    | `x86_64-pc-linux-gnu`, `aarch64-unknown-linux-gnu`, `arm64-apple-darwin`, `x86_64-apple-darwin`, `windows_x86_64` | Multi-thread CPU (`min_ncpus` 4, `max_threads` 8), 2B `Q8_0` fallback                                                                                                                                                                                                   |
| `usnm_qwen`    | `container` | `docker_mt_2b`          | `x86_64-pc-linux-gnu`                                                                                             | `ocr/boinc/qwen/job-cpu.toml`: 2B only, `--cpus` from the thread count (4–8), 6 GiB, no swap                                                                                                                                                                            |
| `usnm_qwen`    | `container` | `docker_nvidia_cuda_8b` | `x86_64-pc-linux-gnu`                                                                                             | `ocr/boinc/qwen/job-gpu.toml`: 8B only, `--gpus all`, 2 CPUs, >= 7,000 MB GPU RAM, compute capability >= 6.0, 6 GiB, no swap                                                                                                                                            |

For an existing NVIDIA-equipped Debian/Ubuntu LXC, Docker is **not** a native-app
prerequisite. Run `python3 scripts/boinc/setup_nvidia_lxc.py` inside the container
to inspect the userspace-only package plan, then run
`sudo python3 scripts/boinc/setup_nvidia_lxc.py --apply` to install it.
The script reads the loaded **host kernel driver** from `/proc/driver/nvidia/version`
and requires matching userspace packages down to the patch version. It supports
Ubuntu's branch-suffixed packages, NVIDIA's unversioned Ubuntu 590+ packages,
and Debian's separate Vulkan/EGL ICD packages. It installs the Vulkan loader,
diagnostic tools and runtime libraries, pins the NVIDIA packages to the matching
version, and verifies `nvidia-smi`, `libcuda.so.1` initialization and a hardware
NVIDIA Vulkan device **as `boinc`**. The CUDA version printed by `nvidia-smi`
describes driver compatibility, not an installed toolkit; `nvcc` is not required.
Use `--boinc-user NAME` only if the service uses a different non-root account.
For diagnostics without APT changes (including existing NVIDIA `.run`
installations), use `sudo python3 scripts/boinc/setup_nvidia_lxc.py --check-only`.
The installer refuses to mix runfile and package-managed NVIDIA installations.

Run this in the LXC, **not on Proxmox**. The host operator must already have passed
the NVIDIA character devices through; the script never installs DKMS/kernel
drivers, creates devices, changes their permissions, adds a package repository,
or changes BOINC preferences. If an exact driver package set is unavailable, it
stops rather than mixing versions or downloading an unverified driver installer.
Use the container distribution's packages or the
[official NVIDIA repository](https://docs.nvidia.com/datacenter/tesla/driver-installation-guide/ubuntu.html#network-repository-enablement-amd64),
not a repository for a different distribution. After a host driver update, rerun
the script to update `/etc/apt/preferences.d/usnm-nvidia-lxc`. After current work
has checkpointed, restart `boinc-client` to load the repaired libraries and group
membership. Passing these checks establishes userspace readiness only: signed
app publication, client-version admission, model memory limits and successful
GPU results are separate requirements.

The native Intel and NVIDIA Vulkan classes share batch-8 inputs, not the container batch-18
queue. Publish signed Linux versions with the same locked Vulkan bundle first. To open a
shared dispatch window, use `scripts/boinc/native-qwen-cohort.php --project PATH
--plan-class shared_8b --workunit FIRST --workunit-end LAST` (preview, then `--apply`).
The helper requires at most ten contiguous Qwen native-8B workunits, each unaccepted with
two unsent replicas and quorum two; it closes the other classes in its shared scope and pauses the
separate NVIDIA container batch-18 queue, without changing its input format or aborting
already assigned work. This keeps NVIDIA fetching from the same bounded native pool as
Intel, rather than selecting the older container app version. `--stop --apply`
closes that shared scope, leaving independent AMD and 6 GB windows alone. This changes
eligibility only, not distinct-host validation.
When DB credentials are rendered into a separate tmpfs project, pass `--db-project PATH`
with its `config.xml` and private `html/inc` copy. The policy and journal stay under
`--project`; credentials must not be copied to the shared project filesystem.
Run one-off queue operators with the committed single-replica bridge configuration,
not replica settings left by a parallel assessment submission.

The **6 GB profile is not generally admitted or hardware-qualified** merely by installing
this policy. It starts closed and permits BOINC >= 8.2.9 **only for this profile**.
A [real 8.2.9 native run](https://github.com/tgoodyear/usnewsmap.com/issues/1234#issuecomment-6537940240)
produced all 12 crop readings with the locked 8B model, 24 GPU layers and a CPU projector:
3,725 MiB observed peak GPU memory and about 5.32 GiB aggregate process RSS. That offline
run used a managed 8 GB RTX 2060 SUPER; it does not prove server reporting, independent
validation, or the remote 6 GB card's compatibility. Other native class floors remain
8.2.11. Before admission, publish a signed native version with the pinned Vulkan bundle,
verify the volunteer's CUDA/Vulkan access and measure its successful output. The
5,120 MiB minimum is a free-memory guard, not a claim that every card or driver works.

For a one-workunit qualification, preview `native-qwen-cohort.php --project PATH
--plan-class nvidia_vulkan_6gb_8b --workunit ID --fresh-pair --admit-until UNIX`,
then apply against the preview's `--expected-policy-sha256`. This requires a pristine,
ordinary quorum-two pair, enforces distinct-host validation and rechecks it under the
policy lock. Arrange its independent peer in a reviewed shared window; opening only
the low-memory class does not guarantee that peer. The deadline prevents late admission,
not automatic closure: the operator must schedule `--plan-class nvidia_vulkan_6gb_8b
--stop --apply` at the deadline. Shared refills and server restarts preserve this
independent window, and its stop leaves the other classes untouched. Do not enable
general admission or change quorum/result state to force a successful qualification.

The **2B Linux NVIDIA profile** (`nvidia_vulkan_2b`, #1234) is the native route for such
cards that remains after Surya's retirement. It also starts closed, permits BOINC >= 8.2.9,
and has its own one-workunit window (`--plan-class nvidia_vulkan_2b --workunit ID`, then
`--stop`) for batch-2 work from `usnm-boinc qwen submit --target native-2b`. The app puts the
2B model and projector fully on the GPU and refuses less than 4,096 MiB of free Vulkan
memory; neither that floor nor the scheduler's 4,500 MB is measured yet. It has no signed
version until a maintainer releases one
([app release](../contrib-compute/docs/app-release.md#linux-nvidia-2b-nvidia_vulkan_2b)).

The server patch disables app-ID-only version memoization when plan classes have
workunit restrictions. Otherwise an earlier incompatible/paused batch can cache
either its payload or a null selection and prevent later eligible native work
from dispatching. A no-work reply mentioning 15,360 MB can refer to the separate
training app; it does not change the native 8B classes' 7,000 MB requirement.
Server startup preserves existing bounded Qwen admission fields, including a
paused container pool, while refreshing image-owned batch, resource and client
requirements. Use the cohort operator, not a server restart, to change those windows.

`scripts/boinc/install.py` supports both `--kind container` (the default) and `--kind native --binary <FILE>`. `create_work` bounds: `usnm_quality` 1e12 flops estimated, 1e14 bound, 1.5 GiB; `usnm_qwen` 1e13 estimated, 1e16 bound, 6 GiB. UNVERIFIED until the first units return.

Register the apps and platforms on the server (`scripts/boinc/onprem-step.sh --env prod --target bridge --step register-apps --wait-mins 15` on the on-prem host), then release the container or native versions with `scripts/boinc/boinc-app-release.sh` (see [contrib-compute/docs/app-release.md](../contrib-compute/docs/app-release.md)). Prod's server is on-prem, so add `--onprem-host HOST --onprem-ct ID` to each release command below:

```sh
# Container versions:
scripts/boinc/boinc-app-release.sh --env prod --app usnm_quality --version 1.0 --signing-host <host>
scripts/boinc/boinc-app-release.sh --env prod --app usnm_qwen --version 1.0 --plan-class docker_mt_2b \
  --worker-image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:<qwen-2b-digest> --signing-host <host>
scripts/boinc/boinc-app-release.sh --env prod --app usnm_qwen --version 1.1 --plan-class docker_nvidia_cuda_8b \
  --worker-image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:<qwen-8b-digest> --signing-host <host>

# Native usnm_quality binary (from .github/workflows/boinc-quality-native.yml artifacts):
scripts/boinc/boinc-app-release.sh --env prod --app usnm_quality --version 1.1 --kind native \
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
- `usnm_qwen_check` (`usnm-boinc qwen check`): shape, sizes, the checksums of `readings.json` and `diag.txt`, the readings against the result (count, device, model, outcome). Every checked result, not only the eventual canonical, is retained in `<project>/qwen/seen/<uploaded-result-manifest-name>/` before validation succeeds. New captures commit a complete fsynced directory; `capture-v1.json` binds original upload names, byte counts and SHA-256 hashes, including exact `result.raw.json` serialization and versioned generation sections in `diag.txt`. `readings-metrics-v1.json` records per-reading uncertainty/repetition/token metrics, not accuracy; source-image/damage measurements remain explicitly unavailable until the offline `candidates` join below. Legacy normalized `result.json` files are preserved and completed with the raw manifest and receipt. Replay checks existing evidence rather than overwriting mismatches. Capture/storage conflicts return stock `script_validator` transient exit **3**, deferring validation and cleanup instead of marking a volunteer invalid; malformed science retains its existing non-transient failure behavior. This does not change quorum, agreement thresholds, canonical selection, or deletion policy, and cannot recover original JSON formatting already normalized by an older validator.
- `usnm_qwen_compare` (`usnm-boinc qwen compare`): the same job and engine, both read the page or neither, and per crop both read or neither and a word-bag Dice of at least the threshold. Until the probe has measured it, the threshold is 0.5 (`PROBE_MIN_DICE`), which only rejects unrelated text. Every pair is appended to `<project>/qwen/pairs.jsonl` with the Dice per crop and both hosts' devices and models. The line's `wordless` lists every crop both hosts read where neither reading has a letter or digit: both word bags are empty, so its Dice is 1.0 whatever the two readings say. Each entry has the crop's `id`, whether the readings are the same once whitespace is collapsed (`identical`), and whether each side looped (`looped`, the loop-guard bit or a repeated run at the end, as `crop-consensus-v2` marks it). It is for measurement only; no verdict reads it ([#1463 note](notes/2026-11-04-qwen-wordless-crops.md)). For a bundle unit, the same rule is also applied to each page on its own (`<doc_id>:` crop prefix), but only when both results ran to the end (`ok`): the pair line gains `pages` (verdict per page), and with `--pages` (`<project>/qwen/pages` in the bridge wrapper) each page's verdict and both hosts' readings of it are written once to `<pages>/<job_id>/<doc_id>/<pair>.json` (`page_agreed` or `page_disagreed`, `accuracy_established: false`). A page whose every crop is blank in both is not agreed. The unit's verdict still needs every page, so a bundle with one disagreeing page stays inconclusive in BOINC while the pages two hosts agreed on are kept; the BOINC database is not changed.

`usnm-boinc qwen compare --rule crop-consensus-v2` applies the crop-by-crop rule. `--rule` defaults to `strict-v1`; the wrapper passes `crop-consensus-v2` only while `<project>/qwen/validator-rule` holds that name (see "Turning the crop rule on and off" below). Per crop, a side that loops makes the crop `looped`, two sides with no letter or digit make it `no_text`, and a crop with text on one side only is `disagree`, and otherwise the crop is `agree` when character similarity (`1 - edits / longer length`, whitespace collapsed and Hebrew points left out for every vendor, the readings unchanged) is at least 0.7, else `disagree`. A pair agrees when no crop disagrees and `agree` crops are more than half of the crops with text and hold at least 100 characters. A page where no crop has text agrees, as it does under the strict rule. Results that did not both finish keep the strict verdict. The pair log gains `rule`, `basis`, `strict_reason` and `crop_states`. The rule needs `--crop-states <dir>`, which keeps each pair's states at `<dir>/<job_id>/<pair>.json` (record version 2: `basis` is `crop-consensus-v2`, or `strict-v1` for a pair that kept the strict verdict, and `readings_sha256` names both results). A record that can't be written leaves the strict verdict in force for that pair, so a result accepted without a record was accepted crop for crop. A pair vouches for its `agree` crops only, and `quality collect --qwen-crop-states <dir>` joins only those (#1434). The measurements are in the [validation note](notes/2026-11-01-qwen-cross-vendor-validation.md) and the [floor check against human transcriptions](notes/2026-11-04-crop-consensus-floor-check.md); `usnm-boinc qwen grounded` reproduces the second.

**Turning the crop rule on and off.** The wrapper reads `<project>/qwen/validator-rule`: `crop-consensus-v2` turns the rule on for the next pair compared, and removing the file (or writing `strict-v1`) turns it off. No daemon restarts. A `usnm-boinc` next to the wrapper's target is used in place of the image's, so a build newer than the server image can validate: install the wrapper and binary together in one directory and point `bin/usnm_qwen_compare` at that wrapper. A server restart relinks `bin/` to the image's wrapper. Order matters: first every Qwen collector runs with `--qwen-crop-states <project>/qwen/crop-states` (`CROP_STATES_ARGS` on `usnm-qwen-results.service`, `QWEN_CROP_STATES` on the bridge's `quality-collect`), then the rule file goes in. A collector without the flag joins every crop of a canonical result, as under the strict rule. Turning the rule off keeps the records: results accepted under it still join only their vouched crops.

**Stopping tie-breakers by language and request version (#1477).** When a Qwen first pair disagrees, the validator raises `target_nresults` and the transitioner sends a tie-breaker, up to `max_total_results` 4. For Yiddish request version 1 a tie-breaker rarely settles the unit (see the [replica cost note](notes/2026-11-04-qwen-replica-cost.md)). `scripts/boinc/qwen-tiebreak-policy.py` stops them for the groups a policy names. The policy is `<project>/qwen/tiebreak-policy.json`, `{"schema": 1, "stop_tiebreakers": [{"language": "yid", "request_version": 1}]}`, or `--stop-tiebreakers yid:1` (repeatable) in its place. With neither, the policy is empty and nothing changes. The language is the unit's language set as `qwen-replica-cost.py` labels it, matched exactly, so `yid` doesn't match a `heb+yid` bundle. A unit in a named group is settled once its first pair has disagreed: it has no canonical result and no error, at least two returns the validator found inconclusive, no replica running, nothing waiting for the validator, and it isn't due for the transitioner. Settling one unit is one transaction under row locks (`SELECT … FOR UPDATE` on the workunit and its results, then a recheck). Its unsent replicas become OVER/DIDNT_NEED, as BOINC's `cancel_job` does. The unit gets `max_total_results` equal to its result count, `error_mask |= 8` (too many total results) and `transition_time` now. The transitioner then marks the inconclusive returns NO_CHECK and hands the unit to the assimilator with no canonical result, as for a unit that reaches its cap. The page keeps its Tesseract readings, and the router reads Yiddish with `yid` as before. A unit with a replica running, or one that can't be locked within 5 s, is left for a later pass. It doesn't apply at `create_work`, so it needs no image change. No new Yiddish v1 units are made, so the backlog is finite, and runs repeated until `plan` reports nothing to settle cover it. Settled units count as error workunits in `qwen_submit.py`'s source status, as capped units do. They also leave the feed's `unresolved_native` count, so the feed can admit that many more pages under `--max-unresolved`. A settled unit can't be reopened: re-reading its page needs a new unit. CI's full-project fixture (`scripts/boinc/db-tls/full.py`) runs it against the server image's BOINC daemons and a MariaDB database. It runs `plan`, then `settle --apply` while another session holds one unit's row lock, then `settle --apply` again. It checks that the locked unit is skipped unchanged and then settled by the second run, that units outside the policy or with a replica running are untouched, and that the real transitioner then marks the settled units' returns NO_CHECK and makes them ready to assimilate. CI's MySQL 8.4 fixture (`scripts/boinc/db-tls/mysql84.py`) loads BOINC's schema into the MySQL release production runs. It runs `settle --apply` while one unit's row is locked, and while a session has run the scheduler's `mark_as_sent` on another unit's unsent tie-breaker and not yet committed. It commits that dispatch while the tool waits for the row. It checks that the tool skips both units, settles the locked one on the next run, and leaves the dispatched one alone. It also checks that `mark_as_sent` then matches no row for a cancelled replica, so a scheduler that still has one in its feeder cache can't send it. That fixture runs no BOINC daemons, so the first production run is still bounded to one unit. The steps are below. Run them as root on the BOINC host, after the server-image roll for #1449 is finished.

1. Count what it would settle. This step is read-only: one `READ ONLY` transaction of `LIMIT`-bounded `SELECT`s over the units created in the window.

   ```sh
   pct exec 230 -- podman exec -i usnm-boinc-server python3 - plan \
     --since 2026-10-21T00:00:00Z --until 2026-11-06T00:00:00Z --stop-tiebreakers yid:1 \
     < scripts/boinc/qwen-tiebreak-policy.py
   ```

2. Run `settle` with the same arguments to preview each unit and the replicas it would cancel. `settle` with no `--apply` changes nothing.
3. Apply it under the project mutex, with a bound on units and an append-only journal. The journal records each unit's state before the change, and every unit settled or skipped. Start with `--max-units 1`. After the transitioner's next pass, check that unit: its inconclusive returns are NO_CHECK (`validate_state` 3) and its `assimilate_state` is 1 or later. Then run it again with a larger bound, for example `--max-units 500`.

   ```sh
   pct exec 230 -- flock -x -w 120 /run/usnm-boinc-step.lock podman exec -i usnm-boinc-server python3 - settle \
     --since 2026-10-21T00:00:00Z --until 2026-11-06T00:00:00Z --stop-tiebreakers yid:1 \
     --apply --max-units 1 --journal /project/usnmocr/qwen/tiebreak-journal.jsonl \
     < scripts/boinc/qwen-tiebreak-policy.py
   ```

The measurements behind it are in the [tie-breaker policy note](notes/2026-11-05-qwen-yiddish-tiebreak-policy.md).

The daemons are in `ocr/boinc/daemons.xml` and `contrib-compute/images/server/lib.sh`; the wrappers in `contrib-compute/images/bridge/`.

## CLI subcommands

### usnm-boinc quality

- `sample`: draws the campaign's stratified sample (`sample.jsonl`) from the prefetch manifest (`--manifest`, `--out`, `--pages` default 500, `--min-per-stratum` default 2, `--seed`, optional `--title-languages` JSON mapping LCCN to MARC language codes, optional `--with-as` listing doc ids with American Stories text, optional `--pages-file` restricting the draw to specific doc ids).
- `function-words`: writes `<out-dir>/<lang>.txt` with the top `N` (default 100) alphabetic words of at least four letters from the embedded wordfreq word list (`usnm_core::lang::wordlist`) for each `--lang` (`--lang` repeatable, `--top`, `--out-dir`).
- `texts-export`: reads the American Stories page Parquet shards (`curated/american-stories/pages/<lccn>/<year>-<nnn>.parquet`) and the newest published (`curated/ocr-local`) or pending (`--out-url`) overlay for each selected page (`--pages`/`--doc-id` or `--sample`), and writes `<out-dir>/<doc_id>.json` (`texts.json` without LoC's text, which `stage` fills from `loc.xml`) and `<out-dir>/<doc_id>.ours.xml` when our ALTO overlay exists (`--curated-url`, optional `--out-url`, `--out-dir`).
- `stage`: stages `usnm_quality` units under `<root>/units/<doc_id>/` (or `<root>/pages/<doc_id>/` packed into `<root>/units/bundle-<idx>-<first_doc_id>/` when `--bundle-size > 1`) and `<root>/places.txt`. Takes repeatable `--page group:lang:doc_id:batch[+batch...]` or `--sample` (a `sample.jsonl` from `quality sample`, with `--group` defaulting to `sample`), `--root`, `--engine-image`, `--texts-dir`, optional `--scans-dir`, `--places` (either a line-per-entry `.txt` or the catalog `places.json`, deduplicated into `<root>/places.txt`), `--function-words` or `--function-words-dir`, `--readers` (comma-separated, default `loc,as`), optional `--surya-texts` (`surya collect`'s export: the page's Surya text joins the unit as `ours-surya` when `--readers` names it, #968), `--crop-seed` (default 522), `--k-crops` (default 12), `--gap-secs` (at least 3, default 5), `--max-pages` (default 20), `--bundle-size` (1 to 500 pages per workunit; default `1` on CLI, `300` in `bridge-job.sh` to target ~10–15 minute workunits), optional `--batch-cache`, and optional `--scan-archive`. When `--scans-dir` has `<doc_id>.jpg` and `<doc_id>.xml` from the hand-off container, `stage` reads those local files without touching the network. Otherwise it uses `crates/usnm-boinc/src/scans.rs`: it checks the scan archive first when `--scan-archive` is set (`keep/<batch>/<lccn>/<date>/<edition>/<seq>.jpg`, verified against its stored `sha256`), and falls back to `tile.loc.gov` through the page's batch (`--batch-cache` caches the batch manifest and issue METS on the storage service), paced at one request at a time with at least `--gap-secs` between requests, and inside the host's shared LoC budget when `USNM_LOC_GLOBAL_PACER` names the scan fetcher's state file (20 requests a minute in all, 12 for 24 hours after a push-back). It never calls `www.loc.gov`'s item API. It parses `loc.xml` for the page dimensions and LoC's text, merges `<texts-dir>/<doc_id>.json` (normalizing reader `source` strings and region `"box"`/`"rect"` coordinates, and copying `<doc_id>.ours.xml` when present), and writes `page.jpg`, `loc.xml`, `texts.json`, and `quality.json`.
- `submit`: hands staged units in `<root>/units/` to `create_work` (`--root`, `--project`, `--max-units` default 100), copying `places.txt` once to `download/usnm_quality_places_<sha256>` and each unit's inputs into the project's download hierarchy with write-ahead `submitting` and `submitted` markers.
- `local-unit`: writes a unit directory from local files (`--page`, `--alto`, `--texts`, `--places`, `--function-words`, `--dir`, `--engine-image`, `--crop-seed` default 522, `--k-crops` default 12) so `usnm-boinc work --dir <dir>` can run without LoC or BOINC.
- `check`: `script_validator`'s `--init_script` (`--files <result.json> <confusions.json> <diag.txt>`, optional `--keep <dir>`).
- `compare`: `script_validator`'s `--compare_script` (`--files <r1...> --files2 <r2...>`, optional `--log <pairs.jsonl>`).
- `collect`: joins the assimilated canonical results of `usnm_quality` (`--root`, `--spool`), `usnm_qwen` (`--qwen-root`, `--qwen-spool`, and `--qwen-crop-states`, where only the crops an agreeing `crop-consensus-v2` pair vouched for join; the others keep their rows without the reading and `crops.parquet`'s `vlm_crop_state` says why: `strict-v1`, `agree`, `looped`, `no_text` or `unjudged`. The records are part of the row-cache keys) and, optionally, `usnm_surya`'s crop units (`--surya-root`, `--surya-spool`; their texts are the reader `surya` in `crops.parquet`, #968) for `--campaign`. Recomputes the crop-level three-cornered hat per page when a `usnm_qwen` reading is present (`legibility.basis` becomes `crop-hat+qwen`), and writes `pages.parquet`, `crops.parquet`, and `summary.md` either to `--out-dir` or to `--curated-url` under `audit/quality/<campaign>/`.
- `rehat`: recomputes the page hat, version 1 and version 2, from a staging root's texts (`--root`, `--out` JSON lines, `--min-readers` 3, `--threads`), with no results or scans, to compare the two versions on a campaign whose units are still staged.
- `rescore`: computes the text part of single-page units again from corrected LoC texts, with no new volunteer work (#1079). Takes `--campaign`, `--root` (the staging root), `--spool` (the canonical results), `--loc-texts` (JSON lines `{"doc_id", "text"}`, LoC's curated text, which replaces the `loc-hist` reader's), `--places`, `--out-dir` (new, outside the root) and `--threads`. The text part is everything in a unit's result except the scan's features: it is `slot::text_analysis`, the function the unit itself runs with no image and no ALTO, so a rescored page gets the numbers a unit would give it. The result's image and layout features are kept. Writes the re-staged units (`<out-dir>/units/<doc_id>/{quality.json, texts.json}`, which `rehat` reads) and `pages.parquet` and `summary.md` as `collect` writes them, with hat version 2. Bundles, units with no result, and pages with no LoC text are counted and skipped.

`stage` refuses an exported `loc` or `loc-hist` text that is one of ocr-local's reason labels (`bulk`, `user`, `priority`, `missing`, `empty`, `short`, `garbled`, `mixed`): it stops at the page and writes `HOLD`. Merged `ocr-local/pages` schema v2 names the queue reason `ocr_reason`; `loc_text` and `reason` are deprecated, equal compatibility aliases, never LoC's text. Readers validate the schema version and reject conflicting labels, including on historical parts. Node exchange and Japanese schemas are unchanged. Export LoC's text from the original curated LoC page's `text`, never from a reason column. v1-wave6's hand-off texts were built outside the repo from that legacy column, so all 10,000 of its pages were staged with `loc-hist` as `bulk`. #1079 has the repair.

### The page hat, version 2 (#962)

The whole-page `cer_sym` between two readers counts reading order and text one reader lacks as edits. LoC's text is in ALTO block order, American Stories' in article order and Tesseract's in its own segmentation order, so on real pages most of the whole-page distance is order and coverage, and the version 1 hat charges it to the readers. Version 2 (`usnm_ocr_quality::page_hat`, `VERSION` 2) solves the same `d_ij = e_i + e_j` on `local_cer_sym` from [agreement v1](ocr-text-agreement.md) for every pair of readers: edits over the characters the local alignment placed, whatever the order. A pair falls back to the whole-page `cer_sym` (basis `page`) when nothing placed or neither reader has half its characters placed in the other (`MIN_MATCHED` 0.5): garbage on one side or different text. What placed is coverage, not error: each reader's `matched` is the mean share of its characters placed in the other readers' texts.

The unit and its record don't change, so no app version is needed: `collect` reads each unit's `texts.json` (with LoC's ALTO text for an empty LoC text, as the unit does, through `slot::reader_inputs`) and solves version 2. In `pages.parquet`, `hat_version` says which hat `est_cer`, `est_cer_raw`, `hat_determined`, `hat_residual` and `legibility_ceiling` are; the unit's own values stay in `est_cer_v1`, `est_cer_raw_v1`, `hat_determined_v1`, `hat_residual_v1` and `legibility_ceiling_v1`; `matched` and `hat_pairs_json` (per pair: `a`, `b`, `basis`, `d`, `cer_sym`, `local_cer_sym`, `matched_a`, `matched_b`) are new. A table without `hat_version` is version 1. A page keeps version 1 (`hat_version` 1, counted in `collect`'s `hat_v1_kept`) when its `texts.json` can't be read, or when its LoC text is empty or `none` and the staged `loc.xml` is missing, empty or doesn't parse, since the texts the unit read can't be rebuilt then; `rehat` skips and counts those pages (`no_alto`). The `usnm_quality` database keeps the same split ([scripts/onprem/quality/README.md](../scripts/onprem/quality/README.md)): `unit_pages` carries `hat_version` (1 for rows loaded before version 2), the page's `_v1` values and `hat_pairs`, `reader_scores` each reader's `est_cer_v1`, `est_cer_raw_v1` and `matched`, and `page_summary` adds `hat_version`. `build-quality-assessment-db.py` reports one version: the newest present. Rows of an older version keep their unit's estimate in `est_cer_v1` and have their hat columns cleared, so no mean blends the two; `page_quality_joined` carries `hat_version`. The crop-level hat stays on `cer_sym`: in a crop box every reader with boxes is in line order.

- `calibrate`: runs the estimator on gold crops with human transcriptions (`--crops <crops.jsonl>`, repeatable `--hyp ENGINE=FILE.jsonl`, optional `--readings <readings.jsonl>`, `--out <dir>`) and writes `calibration.csv`, `calibration.md`, and `by-crop.csv`.
- `estimate`: runs the quality computation offline over a directory of pages (`--dir`, `--out`, `--places`, `--function-words`, `--crop-seed` default 522, `--k-crops` default 12, repeatable `--reader NAME=PATTERN` for the OCR survey layout `<dir>/sample/{images,alto,loctext}`), verifying the originating `qwen.json` (or `readings/<page>.qwen.json`) and joining on its recorded crops when `readings.json` is present, and writing per-page `result.json`, `confusions.json`, and `crops.jsonl` plus aggregate `pages.jsonl` and `crops.jsonl`.
- `summary`: reads `<root>/units/`, `<project>/quality/seen` (`--seen`), and `<project>/quality/pairs.jsonl` (`--pairs`) and prints a Markdown summary table of kept results and compared pairs.

### usnm-boinc qwen

- `stage`: reads staged `usnm_quality` units from `--quality-root` (`--root`, `--engine-image`, `--engine-llama`, `--engine-weights`, `--gpu-timeout-secs` default 14400, `--cpu-timeout-secs` default 7200, `--max-tokens-per-1000px2` default 4, `--max-pages` default 20, `--bundle-size` default 1 on CLI and `16` in `bridge-job.sh`, at most 32 pages and 256 crops per bundle), cuts the same deterministic crops from each unit's `page.jpg` and `loc.xml` (`crop_seed` and `k_crops` from `quality.json`) without contacting LoC again, and writes `<root>/units/<doc_id>/{page.jpg, qwen.json}` (or `<root>/pages/<doc_id>/` packed into `<root>/units/bundle-<idx>-<first_doc_id>/` when `--bundle-size > 1`). A bundle unit also gets `membership.json`, written before its `qwen.json` and never changed: for each page, in order, the page unit it came from, that unit's job id and `qwen.json` sha256, the page sha256 and its slice of the bundled `page.jpg`. A bundling run holds `<root>/.bundle-lock` from staging pages to publishing its last bundle and claims each bundle directory, so a concurrent run fails rather than rewriting a page unit or bundling the same pages twice. A bundle's deadlines are each page's deadline times its pages, at most 14,400 s, and `submit` scales `rsc_fpops_est`/`rsc_fpops_bound` by its pages (`1e13`/`1e16` a page). `stage` closes a bundle before its `qwen.json` would pass 512 KiB, the limit submit and the workers read it with, or before its longest possible `readings.json` would pass 2 MiB, the output upload limit (every crop read at 8 KiB of text that all escapes to two bytes, about 16 KiB a crop). That holds a bundle to about 125 crops: 10 pages at 12 crops a page. A page whose `qwen.json`, alone or as a one-page bundle, would pass 512 KiB gets a `too_large` marker and is not published. Its job is kept beside the marker as evidence, which the feed reads as the page's disposition: as `rejected-qwen.json` when the page's own `qwen.json` is too large, and as the page unit's `qwen.json` under `pages/` when only its one-page bundle is. Pages staged with different engine, deadline or token settings go in separate bundles. A root staged with `--bundle-size` above 1 refuses `--bundle-size 1`, which would stage its pages again as single-page units. The container worker and `usnm-qwen-app` check every bundle page before the model loads, then decode one page at a time, so a bundle needs the memory of its largest page, not of all its pages.

The GPU budget is four hours of application runtime for the entire page,
not a BOINC task-duration limit. It accommodates slower volunteer GPUs without
discarding a nearly complete page at the former 30-minute default. Explicit
shorter budgets remain available for bounded experiments; the job contract accepts
60–14,400 seconds. This is separate from BOINC's report deadline and resource bounds.
Timeouts are part of the immutable job identity: never edit staged inputs already
submitted to volunteers. To retry an affected page with a longer budget, stage a
new job identity and preserve the original attempt and validation evidence.

The GPU workers (the `usnm_qwen` and `usnm_surya` container workers in
`usnm-boinc`, and `usnm-qwen-app`) set `CUDA_CACHE_PATH` and
`MESA_SHADER_CACHE_DIR` to `/var/tmp/usnm-gpu-cache-<uid>`, which they create
with mode 0700. If a directory already at that path is not a private directory
owned by the user the worker runs as, the workers try the same name in the temp
directory, then a directory under the task's scratch or slot directory. On
native hosts the compiled kernels are kept from one workunit to the next. The
containers have a read-only root and a `/tmp` tmpfs of their own, so there the
cache lasts one workunit.

- `submit`: hands staged `usnm_qwen` units in `<root>/units/` to `create_work` (`--root`, `--project`, `--max-units` default 20) with write-ahead `submitting` and `submitted` markers.
- `local-unit`: writes a unit directory from `--image` and `--alto` (or `--synthetic`, which generates a built-in two-line block-capitals page and ALTO) into `--dir` (`--engine-image`, `--engine-llama` defaulting to `USNM_QWEN_LLAMA_IMAGE`, `--engine-weights` defaulting to `USNM_QWEN_WEIGHTS`, `--crop-seed` default 522, `--k-crops` default 12, `--timeout-secs` default 1800, `--max-tokens-per-1000px2` default 4).
- `probe`: runs `/app/llama-server --version` and checks that every model and multimodal projector in `usnm_boinc::qwen::default_models()` exists under `/opt/qwen/models`, printing their sizes without loading weights into memory.
- `check`: `script_validator`'s `--init_script` (`--files <readings.json> <result.json> <diag.txt>`, optional `--keep <dir>`).
- `compare`: `script_validator`'s `--compare_script` (`--files <r1...> --files2 <r2...>`, `--min-dice` default `0.5`, optional `--log <pairs.jsonl>`, `--rule` `strict-v1` or `crop-consensus-v2`, `--crop-states <dir>`, required with `crop-consensus-v2`).
- `grounded`: measures `crop-consensus-v2`'s floor on crops with human transcriptions, offline (`--refs` JSON Lines of `{id, source, lang, ref}`, `--reader NAME=PATH` and `--pair A:B` repeatable, optional `--legibility <reader>`, `--good-cer` default 0.10, `--bad-cer` default 0.50, `--floor` default the rule's, `--seed`, `--out`, optional `--crops`): per pair and group, crops under the floor, crop states, how the rule treats pairs whose readings both match the transcription or one doesn't, and a shuffled control. A reading's error is the quality tables' CER with Hebrew points left out of both sides, as the rule compares readings, so a Yiddish reading that differs from its transcription only in points counts as good.
- `replay`: measures validator rules on retained results, offline (`--seen <project>/qwen/seen`, `--model` default `qwen3-vl-8b-q4km`, `--seed` default 1420, `--controls` default 2000, `--out <report.json>`): every rule on every pair of replicas of a job, pairs that must fail, a third replica's check on what a pair vouched for, and how well each crop metric separates true pairs from shuffled ones.
- `compare --pages <dir>`: also keeps each bundle page's verdict, as above.
- `page-ledger`: `--root` and `--pages`; prints one JSON line per bundle page under `<root>/units/` with its page unit's job id and checksum from `membership.json` and a status: `accepted` (some pair agreed on the page), `disagreed` (every pair so far differed), `pending` (no pair yet) or `membership_invalid` (the bundle's `membership.json` is missing, does not match its job, or names a page unit whose `qwen.json` no longer has the recorded checksum, id and page, so no page of it is kept). A record not naming that job and page, or whose crop verdicts and two sides' readings do not each hold exactly the page's staged crops once, is ignored. `accepted` means two hosts agree, not that the reading is right.
- `summary`: reads `<root>/units/`, `<project>/qwen/seen` (`--seen`), and `<project>/qwen/pairs.jsonl` (`--pairs`) and prints a Markdown summary table of kept results and compared pairs.
- `candidates --job <qwen.json> --page <page.jpg> --candidate <seen/result-a> <seen/result-b> ... --out <report.json>`:
  a bounded offline audit of up to 16 explicitly selected retained results from
  one single-page job. It verifies the input hash, job/engine/prompt identity,
  model membership and crop IDs before publishing. The report retains every
  candidate's text, manifest, client-reported host metadata, diagnostics and
  hashes, with per-crop pixel features, pairwise CER/WER/Dice, comparisons to
  LoC, forced-language damage counts, uncertainty markers, repetition indicators
  and token-budget saturation. Missing readings remain distinct from empty
  readings. Quality pair metrics use NFC, joined line-end hyphens and collapsed
  whitespace; the separate `validator_dice` uses the validator's unmodified
  word-bag computation. These are **noncanonical observations**, not replacements for the
  canonical spool or inputs to `quality collect`. Keep the original `seen`
  files; supplying selected results is not proof of database completeness.

  Damage estimates with few head/near words are weak evidence; unsupported
  language models and zero denominators remain null, not zero damage. A
  mixed-language crop may not fit the job language. LoC is another fallible
  reader; pixel blur/contrast are not annotated physical damage. Exact text
  equality is not required by the existing validator, which already uses
  per-crop Dice. Do not lower that floor to accept token-limit repetition,
  shared hallucinations or agreed `[?]` as accurate transcription. Calibrate
  any new acceptance margin on independent, image-grounded transcriptions
  stratified by language and legibility; this command changes neither BOINC
  validation nor class admission.

  **Generation failure investigation (WU21128).** All three platforms consumed
  the full 1,304-token budget on c07. AMD and Intel repeated a word over many
  lines; Apple emitted 643 `[?]` markers. Their raw readings remain retained.
  The report labels token-budget exhaustion, consecutive duplicate lines,
  uncertainty-only and empty readings independently of pair agreement, and
  records the longest identical-token run. No unflagged output is labeled
  scientifically accepted. These flags describe observations, not calibrated
  failure probabilities; repeated lines can also exist in the source.

  The native request uses prompt v1, temperature 0, seed 42, `top_p=1` and
  an area-derived completion budget (4 tokens per 1,000 crop pixels). There
  is no repetition penalty, and request versions 1 and 2 have no stop rule.
  A successful request is not proof of an accurate crop. Increasing the budget
  could prolong a loop. Treat length-limited or degenerating generations as
  uncertain observations, preserve their raw text, and require independent legible transcription for
  accuracy—not a lower Dice floor or another agreeing repetitive replica.

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

Prod's bridge steps run on the on-prem host since 21 October 2026. Run each `scripts/boinc/start-boinc-bridge.sh --env prod --step <step>` below there as `scripts/boinc/onprem-step.sh --env prod --target bridge --step <step>`, with the same `--var` and `--wait-mins` arguments (`--wait-mins` is required). Step 1 runs on-prem too ([boinc-onprem.md](boinc-onprem.md)).

1. **Export page texts (`handoff-job.sh`, `HANDOFF_STEP=texts-export`).** It runs on an on-prem host that holds a local copy of `curated` (the ocr-sync host, `/srv/curated`, mounted from pve-big's `tank/usnm-release`), with `scripts/boinc/texts-export-onprem.sh`. The American Stories page shards, which don't change, are read from that copy. Only the `ocr-local` overlays and their ALTO are read from `CURATED_URL`, and the step logs those bytes as `curated_bytes`. It runs `usnm-boinc quality texts-export` for `--doc-id` pages or a `--sample` file under `quality-samples/` in `HANDOFF_URL`, and uploads `<doc_id>.json` (and `<doc_id>.ours.xml` when present) to `HANDOFF_URL` under `texts/`:

   ```sh
   scripts/boinc/texts-export-onprem.sh --settings /etc/usnm/texts-export.env \
     --usnm-boinc /usr/local/bin/usnm-boinc --american-stories-root /srv/curated --sample v1.jsonl
   ```

   `--settings` holds `HANDOFF_URL`, `CURATED_URL`, `COSMOS_ENDPOINT`, `REFERENCE_URL` and `OUT_URL` (the hand-off job's values) and the host's Arc identity endpoints. The host's identity needs read on curated's `ocr-local/` and the writer role on the hand-off container. `usnm-boinc` is a release build of the crate (`cargo build --release -p usnm-boinc --locked`).

2. **Stage quality units (`bridge-job.sh`, `BRIDGE_STEP=quality-stage`).** When `HANDOFF_URL` is set, pulls `texts/` into `$SHARE_DIR/quality/<QUALITY_CAMPAIGN>/texts` and, if `QUALITY_SAMPLE` is a relative sample filename, pulls `quality-samples/` into `<campaign>/samples/`; when `QUALITY_LANGS` is set (comma-separated ISO 639-3 codes), writes `<campaign>/function-words` with `usnm-boinc quality function-words`. Runs `usnm-boinc quality stage` for `QUALITY_PAGES` (`group:lang:doc_id:batch[+batch...],...`) or `QUALITY_SAMPLE` (default `<campaign>/sample.jsonl`, with `QUALITY_GROUP` default `sample`), `QUALITY_READERS` (default `loc,as,ours-tesseract`), `QUALITY_PLACES` (default `<campaign>/places.txt`), `QUALITY_ENGINE_IMAGE` (`sha256:<worker-digest>`), `QUALITY_MAX` (default 100), and `QUALITY_BUNDLE_SIZE` (default `300`, up to `500` pages per bundle; at ~1.2–2.0s/page a 300–500 page bundle runs for **10–15 minutes (`0.17–0.25 hours`)** with per-page checkpointing and packs `5,000` pages into `10–17` bundled workunits), reading scans from `SCAN_ARCHIVE_URL` (`--scan-archive`) first and then `tile.loc.gov` through the batch (`--batch-cache`):

   ```sh
   scripts/boinc/start-boinc-bridge.sh --env prod --step quality-stage \
     --var QUALITY_CAMPAIGN=v1-wave5 \
     --var HANDOFF_URL=https://stusnmoffapzy.blob.core.windows.net/boinc-handoff \
     --var QUALITY_SAMPLE=v1-wave5.jsonl --var QUALITY_GROUP=v1-wave5 \
     --var QUALITY_ENGINE_IMAGE=sha256:<worker-digest> \
     --var QUALITY_LANGS=eng --var QUALITY_BUNDLE_SIZE=300 \
     --var QUALITY_MAX=5000 --wait-mins 45
   ```

3. **Stage Qwen units (`bridge-job.sh`, `BRIDGE_STEP=qwen-stage`).** Cuts the deterministic crops from the staged `usnm_quality` units in `$SHARE_DIR/quality/<QUALITY_CAMPAIGN>` into `$SHARE_DIR/qwen/<QWEN_CAMPAIGN>` without fetching from LoC again:

   ```sh
   scripts/boinc/start-boinc-bridge.sh --env prod --step qwen-stage \
     --var QUALITY_CAMPAIGN=v1-probe --var QWEN_CAMPAIGN=v1-probe \
     --var QWEN_ENGINE_IMAGE=sha256:<qwen-digest> \
     --var QWEN_ENGINE_LLAMA=sha256:b0baccbe2e00b5d74e63f85f9f023c93d61d0f329475269e1e7002e87eb875ea \
     --var QWEN_ENGINE_WEIGHTS=sha256:969266d1c24656977e06a6efacae04deac6d5093c77227fe55ac28975035cddd \
     --var QWEN_MAX=20 --wait-mins 15
   ```

   Crop source text has a separate, unchanged 8192-byte UTF-8 limit. If any selected crop exceeds it, staging retains the complete candidate as `rejected-qwen.json`, the original `loc.xml`, `quality.json`, `page.jpg` and available `sample.json`, then writes the existing `too_large` terminal marker. Its reason names every oversized crop, box, byte count, text hash and source hashes. No `qwen.json` is published, no text is truncated, and other pages continue within `--max-pages`. Repeats preserve that evidence; feed inventory remembers rejected documents without counting them as submitted work. Other schema errors still fail closed.

   For an isolated reviewed staging-binary rollout, `qwen_submit.py` uses a sibling `usnm-boinc-stage` executable when installed, otherwise the existing `usnm-boinc`; `--stage-binary` can select an explicit executable. By default, requests at version 1 submit with the existing `usnm-boinc`, and requests at versions 3 and 4 with the staging binary, because a server image built before #1437 has a `qwen submit` that refuses them. `--submit-binary` names one submit executable for every version (`--qwen-submit-binary` on `quality-ship.sh`, set in `QWEN_SHIP_ARGS`). After the image is rebuilt, the operator switches by configuration alone (see [One submit binary and one collector](#one-submit-binary-and-one-collector-1449)). Transactions (`--transaction`) always submit with the staging binary and refuse `--submit-binary`. `--bundle-size` (1 to 32, default 1) sets the pages a native unit holds (see [Bundled native units](#bundled-native-units-1435)). Install only an approved hash-bound artifact at an idle boundary under the normal project lock. Shared shipper helpers, service units and timers need no changes.

4. **Submit both apps' workunits (`bridge-job.sh`, `BRIDGE_STEP=quality-submit` and `BRIDGE_STEP=qwen-submit`).** Hands the staged units to `create_work` with two results per workunit (`QUALITY_MAX` default 500, `QWEN_MAX` default 20) and invalidates `$PROJ/html/cache/f2/server_status.php_job_status` so `https://boinc.usnewsmap.com/server_status.php` reflects the new queue immediately (live uncached counts are also always available at `https://boinc.usnewsmap.com/server_status.php?counts=1`):

   ```sh
   scripts/boinc/start-boinc-bridge.sh --env prod --step quality-submit \
     --var QUALITY_CAMPAIGN=v1-probe --var QUALITY_MAX=500 --wait-mins 15
   scripts/boinc/start-boinc-bridge.sh --env prod --step qwen-submit \
     --var QWEN_CAMPAIGN=v1-probe --var QWEN_MAX=20 --wait-mins 15
   ```

5. **Inspect validator summaries (`bridge-job.sh`, `BRIDGE_STEP=quality-summary` and `BRIDGE_STEP=qwen-summary`).** Reads `<project>/quality/{seen,pairs.jsonl}` and `<project>/qwen/{seen,pairs.jsonl}` and writes `<campaign>/summary.md`:

   ```sh
   scripts/boinc/start-boinc-bridge.sh --env prod --step quality-summary --var QUALITY_CAMPAIGN=v1-probe
   scripts/boinc/start-boinc-bridge.sh --env prod --step qwen-summary --var QWEN_CAMPAIGN=v1-probe
   ```

6. **Collect canonical results (`bridge-job.sh`, `BRIDGE_STEP=quality-collect`).** Joins the assimilated `usnm_quality` (`QUALITY_SPOOL`), optional `usnm_qwen` (`QWEN_CAMPAIGN` and `QWEN_SPOOL`) and optional `usnm_surya` crop (`SURYA_CAMPAIGN` and `SURYA_SPOOL`, #968) results into `<campaign>/out/{pages.parquet,crops.parquet,summary.md}`, or uploads them to `QUALITY_CURATED_URL` under `audit/quality/<QUALITY_CAMPAIGN>/`:

   ```sh
   scripts/boinc/start-boinc-bridge.sh --env prod --step quality-collect \
     --var QUALITY_CAMPAIGN=v1-probe --var QUALITY_SPOOL=/project/usnmocr/results/0 \
     --var QWEN_CAMPAIGN=v1-probe --var QWEN_SPOOL=/project/usnmocr/results/0 --wait-mins 15
   ```

7. **Monitor or manage multi-shard campaigns (`scripts/boinc/boinc-quality-campaign.py`).** For 5,000–10,000+ workunit campaigns sharded across parallel Container Apps executions (`v1-wave5`, `v1-wave6`), use the campaign CLI to split non-overlapping manifests into shard archives, inspect live shard + server queue progress, and restore the bridge job specification after completion. The CLI drives the Azure bridge job and the quality wave job, which `"server": "onprem"` removes, and the quality wave does not run on-prem yet ([boinc-onprem.md](boinc-onprem.md)), so this applies to a server with `"server": "azure"`:

   ```sh
   python3 scripts/boinc/boinc-quality-campaign.py prepare --campaign v1-wave6 --manifest sample.jsonl --out-dir /tmp/wave6 --count 10000 --shards 20
   python3 scripts/boinc/boinc-quality-campaign.py status --campaign v1-wave6 --shards 20
   python3 scripts/boinc/boinc-quality-campaign.py restore --env prod
   ```

### On-prem feeder (v1-wave10 on)

The wave runner (`scripts/boinc/quality_corpus_runner.py`) read the scan archive and curated
in Azure. On-prem, quality units are fed from pve-laptop instead (#962), with no Azure call and
no LoC request:

1. **Feeder, CT215 `usnm-batch` on pve-laptop** (`scripts/boinc/onprem/quality-feed/quality_feed.py`,
   `usnm-quality-feed.service`). `select` picks pages from the scan server's index
   (`/index/full.jsonl` on pve-big's CT211) that no campaign has staged, worst damage first.
   `run` reads each scan from the scan server (checked against its index line's size and
   sha256), makes the worker's grey q75 `page.jpg` as the runner does, writes an empty ALTO
   (LoC's would be a LoC request; CT211's fetcher is the only LoC client), and takes LoC's text,
   American Stories and `ocr-local` from the curated copy, mounted read-only at `/srv/curated`
   (home-infra `scripts/lab/batch-ct-quality.sh --mounts`). It writes chunks of 500 pages
   (`--chunk-pages`, at most 500: a bundle's page limit) to its outbox and waits while
   `MAX_PENDING` (8) are unshipped; the shipper stages each chunk as one 500-page bundle
   (`--bundle-size 500`), or two when its scans pass the bundle's 1 GiB `MAX_BUNDLE_SCAN`
   (about 13% of chunks), a few minutes of work for a host at 0.06–0.4 s a page. After a round that made chunks or
   found the outbox full it checks every 15 seconds for room, so the outbox refills as the
   shipper takes chunks; only a round with nothing to feed sleeps the full `--every-secs`.
2. **Shipper, pve-laptop itself** (`quality-ship.sh`, `usnm-quality-ship.timer`, 3 minutes
   after the last run ends; a run ships every `READY` chunk up to `MAX_CHUNKS`, 30). Over the host trust pve-laptop → pve (home-infra `scripts/host-ssh-trust.sh`) it
   copies a chunk into CT230's `/project/quality/<campaign>/incoming/`, runs
   `quality_receive.py` there (every file against `SHA256SUMS`, and every page's scan present,
   so `quality stage` never falls back to tile.loc.gov from the server), then runs
   `usnm-boinc-step` `quality-stage` with `QUALITY_SCANS_DIR`, then `usnm-boinc quality submit`
   in the server container. It ships
   nothing while the server has more than 300 results unsent. `quality-stage` holds its own
   lock on the host, so one stage runs at a time while the controller's steps go on: it uses no
   database or project files, and later steps ignore a unit until its `quality.json`, written
   last, or its `submitted` marker exists. Submitting waits its turn on the step lock for up to
   30 minutes; if the lock stays held, the run exits 75 and the chunk stays `READY`. It skips
   the `quality-submit` bridge step, whose walk of the NFS download tree held the lock for up
   to 8 minutes: submit sets each file's mode and the export fixes its owner.

   The next chunk is copied and received while the current one stages, outside both locks,
   and that receive ends before the current chunk's submit takes the step lock, so the lock is
   held no longer than before. `--after-submit` keeps the scans of any page the chunk received
   ahead also holds. On 2 November 2026 (#529) a 200-page chunk took 30 to 45 minutes end to
   end: receive 11 to 16, stage 14 to 24, submit 1 to 4. Nearly all of it was synchronous
   writes on the NFS share, `tank/boinc-bulk` on pve-big's HDD mirror: an fsynced 64 KB write
   took 925 ms, a plain one 618 ms and a chown 278 ms. With `sync=disabled` on that dataset
   they take 7, 4 and 1 ms, and a chunk takes about 3 minutes: receive about 1 (hidden behind
   the stage), stage about 2 (0.4 s a page, then packing), submit and freeing under a minute.

   **`tank/boinc-bulk` runs with `sync=disabled`** (set 2 November 2026, approved by the
   maintainer; it was `sync=standard`, the default). A power cut or crash of pve-big can lose
   the last few seconds of writes that the share had already acknowledged. ZFS still commits
   whole transaction groups in order, so what survives is a consistent earlier state. Nothing
   half-written is submitted: receiving copies and checks the whole chunk against
   `SHA256SUMS` on every try; staging checks each page unit it staged before, and each bundle
   not yet `submitted` or `submitting`, against the hashes in its `quality.json`, and stages or
   packs again whatever doesn't match or has no `quality.json`; `quality submit` checks every
   input against those hashes before `create_work`. A unit already `submitting` stays for the
   existing uncertain-submit check. Revert with `zfs set sync=standard tank/boinc-bulk` on
   pve-big. Only this dataset changed.

   Packing reads only the run's own pages and the bundles named after them (a bundle is
   `bundle-<n>-<first page>`), not the whole `pages/` tree: at 39,000 pages that listing took
   four minutes cold over NFS. A page found staged already that none of those bundles holds
   makes it read every bundle once, as before.

3. **Results** go to `usnm_quality` ([scripts/onprem/quality/README.md](../scripts/onprem/quality/README.md)).
   On the preparation host, `usnm-quality-results.timer` runs two minutes after
   the previous pass finishes, with up to 15 seconds of jitter:
   `quality_results_partial.py` collects **one submitted unit** under CT230's normal
   nonblocking bridge lock. Collection is bounded to 120 seconds, with a 180-second
   remote phase bound including metadata enumeration. It writes immutable per-pass
   `out/{pages.parquet,crops.parquet,summary.md}` and `coverage.json` under
   `/project/ops/quality-results/scoped/<campaign>/<unit-key>/snapshots/<snapshot>/`.
   It never replaces `<campaign>/out` or claims a whole campaign is current.
   Coverage explicitly says `campaign_complete=false`, identifies the selected job
   and documents, and counts submitted and unvisited units for **this pass only**.
   Table schemas and the existing complete-snapshot consumer contract are unchanged.
   All three files are checksum-verified in preparation CT215; Parquet documents
   and job identity must match coverage before the existing certificate-authenticated
   `load.py results` is allowed to upsert. Missing or unvisited rows are never deleted.
   A pending unit yields zero rows, not a loaded result. The host pass lock also
   serializes collection, transfer and loading. A busy bridge retries next tick;
   no active producer is preempted. The shorter interval avoids a structural
   backlog: one unit per 15 minutes cannot match the shipper's maximum two units
   per 15-minute interval even without contention. Actual throughput still depends
   on collection time and free lock windows; verify cursor and load receipts,
   rather than treating the timer frequency as proof of campaign coverage.
   Source and preparation snapshots, coverage and host load receipts are retained.
   The host cursor advances only after successful verification and loading. Failure
   after a database commit but before cursor update safely repeats the same upserts.
   The ordinary service explicitly enables `--completion-frontier`: three of every
   four successful collections prioritize canonical-present units whose dependency
   fingerprint has not been acknowledged by a verified, nonempty certificate load.
   The fourth visit fairly revisits all submitted units, including pending, failed
   and unchanged units. With no priority work, every visit is fair. Each lane fixes
   an upper-name fence for its finite cycle; continuous arrivals cannot postpone
   wrapping to earlier units. Thus a pending unit becoming canonical is immediately
   priority-eligible rather than waiting for its fair visit. Priority selection is
   cyclic, not newest-only. None of these scheduling hints constitutes scientific
   validation, independent truth, quorum, or campaign completeness.
   `usnm-boinc quality collect --inventory-only --cache ...` produces these hints
   using a **separate scheduling namespace**, covering the binary, staged inputs,
   canonical QUALITY outputs and late/changed Qwen dependencies. Fine-resolution
   Unix staged PAGE/ALTO/TEXTS identities use device/inode/size/mtime/ctime hints,
   rechecking both the descriptor and reopened path before publication. Coarse
   timestamps and non-Unix inputs retain content hashing. Canonical outputs,
   sidecars and reading dependencies remain content-checked; selected collection
   still uses full content-validated row-cache keys. Scheduling hints cannot be
   used as row keys or scientific acceptance.
   It computes no rows and publishes no Parquet. It examines only the configured
   campaign and its reading dependencies, with at most 10,000 QUALITY directory
   entries; larger campaigns fail explicitly. The selected scheduling key is
   rechecked after collection (`--inventory-unit`); inventory errors, missing
   configured roots, corrupt metadata, or changed selected inputs fail closed,
   without silently falling back to an old cursor. All inventory and collection
   remain inside the unchanged nonblocking bridge lock and 180-second remote bound.
   On filesystems requiring content fallback, cold digest warming can still time
   out; completed digest receipts survive the retry.
   No pass verifies/loads more than one scope.

   **Migration:** install the matching collector binary before this driver/service;
   old binaries reject inventory flags. Without `--completion-frontier`, the driver
   retains its original frozen-prefix cursor behavior, including programmatic
   `host_pass(Namespace(...))` reconciliation callers. Do not enable the flag for
   explicit isolated prefix reconciliation. Existing host `after`/`through` fields
   remain compatible; frontier progress is separate schema-1
   `scoped/<campaign>/frontier.json`, bounded by the campaign limit. A host receipt
   carries the selected fingerprint; only after successful nonempty load does the
   host cursor carry `frontier_ack`, applied idempotently next admission. A crash
   before acknowledgement repeats safe upserts. Remote lane progress alone never
   marks a scope loaded; zero-row visits remain pending/failed, not success.
   Collector validation failures and collector timeouts persist scheduling-only
   progress before failing the pass, so a poison canonical result cannot pin the
   priority lane or suppress fair revisits. Its fingerprint remains unacknowledged
   and is retried in later cycles. Inventory and selected-dependency recheck errors
   still abort without advancing either lane.
   Frontier inventory, collection and selected-key recheck share a 160-second
   monotonic work budget inside the existing 180-second remote limit. Each native
   invocation gets at most 120 seconds and at most the remaining budget, reserving
   20 seconds for subprocess teardown and durable scheduling progress. A slow
   inventory therefore reduces the collector's timeout rather than allowing outer
   termination to interrupt its failure-progress write. This reserve is not a
   guarantee against unavailable storage, process kill, or host failure.
   No prior cursor, DB count, or output timestamp proves an exact dependency key
   was loaded: the first opt-in conservatively re-verifies canonical-present units
   once. Do not seed acknowledgements from guessed historical coverage.
   Each visit invokes the collector's current dependency and content-addressed cache
   checks; old parts are never published just because a receipt exists.
   This is independent per-page science, not a globally consistent campaign snapshot
   or a database-wide freshness guarantee. Old science stays queryable; consult
   coverage/load receipts for what was freshly observed. Whole-campaign collectors
   still refuse incomplete publication and must not consume these partial directories
   as complete snapshots.

   **Read-only native inventory measurement (2026-10-28):** candidate source
   `2a77f08223b21acd1623154456cf54c5ef0e18fc`, binary SHA256
   `fefad24a826370e57f0c1e9f8fa4bc2976f4b0adf973cb2317605334d5936108`,
   inventoried all 136 submitted native units in **10.093 seconds**, with 130
   canonical-present and six pending. This used an initially empty private cache,
   the real project mounted read-only, no network, one CPU and 2 GiB; its disposable
   container was removed. It neither took the production bridge lock nor activated
   the candidate; the installed collector and ordinary timer remained unchanged.
   This passes the isolated 120-second inventory timing gate, **not** the real
   collection/load or sustained priority/fair/late-reader acceptance below.
   A scheduling namespace change conservatively invalidates old acknowledgements;
   never rewrite them to claim historical coverage.

   **Native acceptance (not demonstrated by offline tests):** record installed
   driver/binary hashes, timer settings, lock admissions/busy exits, inventory and
   collection durations (including cold digest warming), and receipt/ack progression.
   After conservative bootstrap, observe at least two hours spanning ordinary
   two-chunk/15-minute arrivals and real competing producers; compare exact submitted
   job/page IDs with certificate DB coverage and measure completion-to-verified-load
   lag, not just cursor position. Observe a previously pending completion and a
   genuine late independent-reading change reaching a new verified key, and show
   unchanged jobs no longer consume priority slots while fair visits continue.
   With a frozen fair fence of N units, revisitation takes at most N+1 fair visits
   (at most four times that many successful collections), not a wall-clock guarantee.
   Sustainability requires usable lock windows for arrivals plus the reserved fair
   share: roughly eight arriving jobs/hour require at least 10.7 successful scopes/hour
   under continuous priority demand. This selector cannot repair insufficient lock
   availability; do not close #962 until measured lag stays bounded. No producer,
   source budget, timer, quorum, floor, or shared lock should be weakened for acceptance.
   The installer records its selected preparation container in
   `/etc/usnm/quality-results.env` (`CTID`, default 215), never pve-big's CT215.
   Collection does not submit work or alter quorum. Repeated loads upsert the same
   campaign/page/reader keys. The existing six-hour summary refresh timer updates
   ranking views; a successful BOINC result is not a loaded result until the loader
   succeeds. The installer enables collection once the loader DSN exists.

   Native Qwen requires a separate scheduled join, not just the image-pass campaign:
   install and enable `usnm-qwen-results.{service,timer}` on the preparation host.
   It continues using the complete-snapshot `quality_results.py` driver with one
   `--native-campaign NAME[=COLLECTOR]` per root, joining QUALITY canonical outputs from
   batch 0 with `/project/qwen/<campaign>` and native batch-8 canonical readings from
   `/project/usnmocr/results/8`. Set `NATIVE_QWEN_ARGS` (and, if needed, the
   QUALITY scorer's `ENGINE_IMAGE`) in `/etc/usnm/quality-results-qwen.env`, never by
   modifying feeder settings. A collector older than #1437 can't read version 3 or 4
   jobs, so those roots name a newer one, and `qwen-nonenglish-v1` keeps its own so its
   cached rows stay as they are. [One submit binary and one collector](#one-submit-binary-and-one-collector-1449)
   moves all three roots to one collector without a cold pass. A root must exist before it is listed. A root that
   fails doesn't stop the next; the service fails at the end and names it. Each pass archives fresh `pages.parquet`, `crops.parquet`
   and summary on the server, then certificate-loads page/reader scores. Readings
   pending quorum remain pending and join on a later pass; they are not reported as
   loaded Qwen science. The two collectors share the host pass lock and server bridge
   lock. Neither changes work identities, submission, source selection or quorum.
   The native service waits at most 60 seconds for the ordinary bridge lock
   (`--step-lock-wait-secs 60`); the waiter holds no bridge lock until admitted.
   Acquisition is explicitly logged, and only collection runs under that lock,
   never transfer or loading. Exhausted admission remains a failed invocation
   without pulling or loading; genuine collection errors are not retried as
   contention. Collection stays bounded to 20 minutes and the service to 40 minutes.
   The SSH phase allows only the configured admission budget in addition to its
   existing collection bound; the maximum sequential phase budgets remain below
   the service limit. Other callers retain nonblocking admission by default.

   Both services use native content-addressed row parts; the partial driver passes
   a single-unit root and its persistent isolated cache. Deploy the matching
   `usnm-boinc` collector binary at the
   dedicated persistent `/project/ops/quality-results/usnm-boinc` path **before**
   installing these service units. Only collectors use this binary; do not
   replace the server's shared executable or restart the BOINC server container.
   `QUALITY_COLLECTOR` in the results environment selects a different path if
   needed (`--collector` on the driver). A missing or old binary rejects the cache flags and
   safely prevents pulling or loading a stale snapshot. Each campaign retains
   immutable JSON row parts (complete campaigns under `collected-parts/`, partial
   unit scopes under `ops/quality-results/scoped/`). Keys include the collector
   executable and format version, staged scientific inputs and optional OCR
   sidecars, canonical QUALITY outputs, and the relevant Qwen jobs/canonical
   outputs (including missing-to-present transitions). Surya dependencies
   conservatively invalidate all parts. A late reading therefore recomputes
   affected crop joins/hat rows instead of treating a previously loaded page as
   permanently complete. Dependency changes during computation abort publication.

   `--max-dirty-units` (default 8, positive) bounds expensive recomputation per pass.
   Cold caches or upgrades may need multiple ticks to warm: completed parts are
   fsynced, but any deferred unit makes collection exit nonzero **before** replacing
   campaign outputs or starting pull/load. The prior scientific snapshot and all
   database rows remain intact. A subsequent tick resumes from those parts; a
   clean pass assembles the complete current campaign and uses the existing
   idempotent certificate loader. Corrupt parts fail closed and remain available
   for operator diagnosis. No scientific inputs, historical parts or database rows
   are deleted. Large staged `page.jpg`, `loc.xml`, and `texts.json` blobs
   additionally have immutable checksum-protected SHA-256 receipts under
   `collected-parts/input-digests/`. On Unix, unchanged opened-file device,
   inode, size, nanosecond mtime and ctime allow reuse of the digest; identity
   is checked again after hashing or lookup and against a freshly opened path.
   Rewrites (even same-size with restored mtime) and replacements invalidate it.
   This requires normal filesystem change-time and close-to-open coherence
   (including the production NFS mount); coarse whole-second ctime or non-Unix
   filesystems fall back to content hashing. Job descriptors, canonical outputs,
   optional sidecars, and collector versions remain content-hashed each pass.
   Corrupt digest receipts fail closed and are retained, never silently rebuilt.
   Once the dirty-unit limit is reached, a warming pass stops before reading or
   hashing any further unit. It does not hash another cold bundle merely to
   discover that it must defer it. Even cached or pending trailing units require
   a subsequent pass; an all-clean pass can publish the complete campaign.
   Cold digest warming can still require multiple bounded passes; successful
   publication still requires a complete current campaign and final dependency
   revalidation. File enumeration, metadata checks, table assembly, transfer and
   loading remain linear; measure end-to-end pass duration after warming before
   claiming capacity for a larger campaign.
   Before deploying digest reuse on another filesystem, retain an isolated
   fixture receipt from the actual mount/client: verify nonzero nanosecond ctime,
   same-size rewrites with restored mtime, atomic replacement, and fresh-open
   visibility. Mount version alone is not acceptance. Then verify cold progress,
   a complete ordinary publication/load, clean-pass idempotence and late canonical
   reading invalidation before restoring any paused results timer.

```sh
# On pve-laptop, once: the trust and the mounts (home-infra).
scripts/host-ssh-trust.sh --from pve-laptop.tjg.fyi --to pve.tjg.fyi
bash scripts/lab/batch-ct-quality.sh --mounts
# From a checkout: the code and units.
scripts/boinc/onprem/quality-feed/quality-feed-install.sh --campaign v1-wave10 \
  --engine-image sha256:<worker digest>
# In CT215: the campaign's page list (the staged list: every doc_id in the share's
# quality/*/units/*/quality.json), then start.
quality_feed.py select --index index-full.jsonl --staged staged-docs.txt --curated /srv/curated \
  --count 500 --out /var/lib/usnm-quality-feed/v1-wave10/pages.jsonl
scripts/boinc/onprem/quality-feed/quality-feed-install.sh --campaign v1-wave10 \
  --engine-image sha256:<worker digest> --enable
```

### Sustained non-English Qwen crop feed

The existing **Temporal singleton is the sole Qwen prepare/ship owner**. The retained
`usnm-qwen-feed.timer` and `usnm-qwen-ship.timer` units are intentionally disabled;
do not enable them alongside Temporal. Preparation and shipping remain separate
activities, so archive extraction or preparation failure does not hold already-READY
work behind preparation. A campaign-local ship lock prevents concurrent consumers
from copying/cleaning one parent chunk; normal project mutexes serialize publication.
This does not replace the ordinary quality feeder.
Install `qwen_feed.py`, `quality-ship.sh` and `quality_receive.py` into a Qwen-owned
`/opt/usnm-quality-feed/qwen-throughput/` directory, leaving other campaigns'
installed shared helpers unchanged. Install `scan_quarantine.py` in the preparation CT's
`/opt/usnm-quality-feed/` **before** the matching `quality_feed.py` and `ocr_manifest.py`.
Install `archive_alto.py` on the archive
host at `/opt/usnm-quality-feed/`, and both `qwen_submit.py` and `qwen_provenance.py` inside the server container at
`/project/ops/qwen-feed/`. The existing pinned preparation venv suffices; no new
packages or LoC clients are needed. Both SSH destinations must have verified host keys
and the existing host trust, never `StrictHostKeyChecking=no`.

Configure `/etc/usnm/qwen-feed.env` on the shipper host with `CAMPAIGN` (a new campaign),
`SERVER_HOST`, `ARCHIVE_HOST`, `SCAN_SERVER`, the existing quality `ENGINE_IMAGE`,
`QWEN_ENGINE_PLAN` (a reviewed `/project/.../plan.json` containing the pinned `engine`),
and `QWEN_MODELS` (the existing checksum-pinned shared-model directory, not another
model copy). `QWEN_MAX_READY` defaults to 1000 **fresh original replicas** (500
untouched native pairs); it is not the global unsent-replica headline. Install the
helpers from `scripts/boinc/onprem/quality-feed/` at the Temporal activity's idle
boundary. Do not start a competing consumer while an old shipper is active, and keep
the systemd feed/ship timers disabled. Verify the singleton's genuine ordinary run
and authoritative native audit. Source/archive budgets are unchanged.
Phase JSON distinguishes inventory, selection, original ALTO and
READY preparation; native shipping logs normal mutex acquisition.

Each tick excludes all retained Qwen single-page/bundle documents and the campaign's
completed source chunks, selects explicitly non-English **available** scans by descending
individual-page damage, and prepares at most 100 pages. The live indexed
`quality.page_summary` supplies language/damage (`select --damage-dsn-file`); the
standalone selector retains its Parquet mode. Original ALTO is read from the
retained bulk archives into `/tank/locstats/qwen-alto`, exposed through the preparation
CT's existing read-only `/srv/locstats` mount. Each XML has a SHA256 receipt naming its
archive, member and archive signature; receipts travel in the checksummed source chunks.
Absent original ALTO is explicitly deferred for a day, not replaced with synthetic boxes.
Archive extraction yields between archives after a 30-minute budget (finishing at
least one archive), returning unvisited pages as `pending`, not `missing`. Cached
XML and provenance are verified on subsequent ticks, so large first-time selections
make incremental progress rather than repeatedly exceeding the SSH timeout. Pending
pages remain eligible immediately; only confirmed missing sources get the daily deferral.

#### Undecodable source scans

Preparation forwards the feeder's captured stdout to the journal even when the child
exits unsuccessfully or times out; its stderr remains inherited. READY counts report
pages actually written, not merely selected. A scan must match its indexed byte count
and SHA256 before decoding. The fetcher computes both from the bytes it downloaded (a
batch JP2 is first MD5-checked against LoC's bag manifest), so they prove the scan
server returned what was fetched, not that the image is complete: LoC's own copy of a
JP2 can lack its codestream end and still match. With Qwen's `--quarantine-decode`,
only a known Pillow unidentified/truncated/broken-stream error, with the expected codec
available, counts toward quarantine. Missing codecs, format disagreements, network
errors, scan/index mismatches and other errors remain visible, retryable failures and
do not increment the decoder-failure count. A buffer scan the scan server answers 404
for (an OCR worker read and deleted it after selection; the original-ALTO phase can
take an hour) is logged and left out without failing the run. It stays out of
`done.txt` and, with `--quarantine-decode`, is recorded as retryable, so selection
takes it again once its scan is back. A missing keep scan remains a failure. A
successfully prepared page resets earlier, pre-quarantine failures. Three decoder
failures of the same
`(doc_id, batch, batch_version, bytes, sha256)` record quarantine it; the ordinary
selector excludes that fingerprint, not every future version of the document. Other
successfully prepared pages keep their READY chunks.
The updated CPU `usnm-quality-follow.service` opts `quality_feed.py follow` into the
same verification and three-failure threshold. It filters quarantined fingerprints
before filling its bounded outbox, so an unchanged bad scan neither retries nor crowds
out other pages. A changed fingerprint remains eligible; network, integrity and unknown
decoder errors continue to retry. CPU and Qwen quarantines are recorded as
`manual_review` and never queue a re-fetch. These changes do not alter BOINC work requests.

Before decoding, every verified `.jp2` gets a cheap completeness check that reads only
its box, marker-segment and tile-part headers (no pixels). A box or tile-part that
declares bytes past the end of the file, or a codestream that stops without its EOC
marker, means the image was cut short. The bag MD5 shows that LoC's copy holds the same
bytes, so neither a retry nor a re-fetch can fix it. Such a scan is never decoded or
retried and does not count as a decoder failure. With `--quarantine-decode` (the CPU
follower and Qwen), it is recorded at once as `source_truncated` for that exact
fingerprint in the same database. It is then excluded like a quarantine, including by
the manifest builder's non-English acquisition. A changed fingerprint is eligible
again. Without that option (`usnm-quality-feed.service`), the page goes to
`skipped.txt` with a `source_truncated: <reason>` line. Each chunk logs
`source-truncated scans found`. `run` at start and `follow` whenever the total changes log
`source-truncated scans in campaign`, giving page counts per `<batch>_verNN`.
A JP2 that reaches EOC but is malformed in some other way still goes to the decoder
and the three-failure rule. An existing `manual_review` record is left unchanged.
This is a safe default that needs no decision: it only stops quality preparation from
retrying the page. A stricter option, not implemented here, would also reject such
pages from OCR. That is a separate maintainer decision.

The preparation CT stores this evidence, decoder versions, failure counts/timestamps
and review state in the campaign's
`/var/lib/usnm-quality-feed/<campaign>/decode-quarantine.sqlite3`, separate from
`skipped.txt`. For example, inspect quarantined records without changing them:

```sh
pct exec 215 -- /opt/usnm-quality-feed/venv/bin/python -c \
  'import json,sys; from pathlib import Path; sys.path.insert(0,"/opt/usnm-quality-feed"); from scan_quarantine import quarantined; print(json.dumps([{k:v for k,v in r.items() if k != "manifest_body"} for r in quarantined(Path("/var/lib/usnm-quality-feed/qwen-nonenglish-v1"))], indent=2))'
```

This change adds no repair/re-fetch producer or new queue format. The existing fetcher and
dashboard continue to consume ordinary `usnm-prefetch-manifest/1` manifests. The Qwen
manifest builder reads `QWEN_QUARANTINE_ROOT` (default `/var/lib/usnm-quality-feed`)
and excludes quarantined documents from new ordinary non-English acquisition. A missing
or unreadable root or helper is an error. Already queued manifests and independent OCR
acquisition are not rewritten or blocked; recovery needs a separately reviewed plan.
Quarantine writers and the manifest builder must use the same root: they serialize
quarantine transitions with the builder's final exclusion check and ordinary manifest PUT.
Keep its `.decode-quarantine.lock` file in place while either service runs; replacing it
would break that coordination. A failed PUT releases the lock and remains an error.

At an idle boundary, install the quarantine helper and matching manifest builder/service
first, then the shared `quality_feed.py` before the updated CPU follower service and
matching `qwen_source.py` and `qwen_feed.py`. Restart the follower only at a safe
boundary, preserving its outbox and state. To roll back, preserve quarantine state,
READY chunks and queued work. Restore the previous CPU follower unit before an older
shared feeder, which cannot parse `--quarantine-decode`; it will resume retrying bad
scans. Keep the new helper and manifest exclusion while quarantines need protection.
A `scan_quarantine.py` older than the `source_truncated` state rejects such a record
as invalid. The feeder, Qwen and the manifest builder then stop with an error rather
than retry, so keep the new helper when rolling back `quality_feed.py`.
If the old manifest builder must be restored, pause that producer at an idle boundary
and obtain a reviewed recovery plan. An old Qwen controller ignores quarantine.

Language-budget reservations are still made before preparation; an already-reserved
failed fingerprint can be retried without spending another page. A reserved page
found `source_truncated` is refunded (#1457): at the start of each tick the feed takes it
out of `<name>-budget.json` (logged as `<name>_budget_refund`), and the tick can select
another page in its place. It is refunded only if its current scan, the last full one in
the retained index as `select` reads it, has that quarantine record (same doc, batch,
version, size and SHA256), and it is not in `done.txt` or `skipped.txt`. A page whose scan
LoC has replaced stays spent, because it can still be prepared. A changed
fingerprint after quarantine, or a failure before a retry record is persisted or
after decoding but before writing READY, can still require budget review. The refund reads
the quarantine with the existing `quarantined()` helper, so it needs no new
`scan_quarantine.py`.

Matching size, SHA256 and (for batch JP2s) the advertised bag MD5 proves the retained
bytes match those recorded checksums, **not** that the bytes are decodable or whether
damage originated upstream. A re-fetch may return identical source bytes; resolving
source versus decoder problems requires separate evidence rather than a retry loop.

The shipper stages native 8B work **before** freeing source inputs, reusing normal Rust
staging/submission, 12 crops/page, shared models, quorum two and distinct-host validation.
It refuses submission uncertainty, cross-campaign duplicate documents, changed hashes,
incorrect database routing/quorum, the configured fresh-original-replica target
(including a per-chunk reservation), or more than 2000 unresolved native workunits
(including retries, counted separately). A fresh pair is batch 8, quorum/target two,
canonical/error zero, exactly two total results, both unsent/host zero/outcome zero/
unvalidated, and no assignment row. Retry replicas and unrelated applications never
masquerade as useful fresh supply or block it via a global ready headline. The
ordinary non-Qwen shipper's global unsent ceiling is unchanged.

Before transferring or staging a native READY chunk, the shipper calls the same
read-only admission helper with `--preflight` and reserves its **whole** page count.
Normal capacity pressure emits JSON `status=deferred` with the phase, reason,
fresh-pair count, reservation, limits and existing audit SHA256, then exits 75
(already a normal deferred exit for `usnm-qwen-ship.service`). Preflight deferral
transfers zero pages; it neither acknowledges READY nor reports a publication.
Unchanged full-watermark ticks only recheck admission, not recopy the sources.
Admission is always checked again under the normal project mutex before native
publication: a capacity race also defers with JSON/exit 75 and retains READY and
staged partials. Uncertain `submitting` markers or changes to previously audited
job bytes remain hard errors, not normal backpressure. No audit is rewritten by a
deferred decision; the logged SHA256 binds the existing immutable receipt.

The 50 GiB project/bulk/thin-pool floors remain; native admission additionally
reserves at least 64 MiB **per candidate page** before staging on the least-free
backing filesystem. Preparation uses the same conservative per-page reservation to
bound its selection. Up to three unshipped chunks are buffered. Failures retain READY
chunks and submission journals; never delete `submitting` to replay work.
The filesystem floor uses the minimum available space across the project root,
`quality/`, and `usnmocr/download/`, so a separate bulk-data mount or NFS quota
cannot be hidden by free space on the local root filesystem.
Successful native workunit audits are retained in `audit.json`, bound to the exact job
SHA256. Historical jobs need not still exist in BOINC's periodically purged database;
new jobs always require a fresh authoritative audit; changed previously audited
job bytes require explicit reconciliation, never automatic replay.

#### Transactional subset shipment (#1466, opt-in)

The legacy invocation above still reserves a whole chunk. The existing Temporal
ship activity may explicitly select `quality-ship.sh --qwen-subsets` **only after
the matching helpers and binary are installed**. No timer, scheduler, language
budget, routing policy, or queue ceiling is changed by this switch. The limits
remain 1,000 fresh-original replicas (500 pairs), 2,000 unresolved native units,
three READY parents, 50 GiB storage floors, and 64 MiB reserved per selected source.

The transaction takes the oldest parent's remaining sources in their original
sample order. Thus a 96-source parent with 54 slots of headroom ships exactly 54,
not the entire parent with a smaller `--max-pages`. The immutable manifest binds
the parent's checksum inventory, ordered original-source fingerprints
(`doc_id,batch,batch_version,bytes,sha256`), selected sample rows, every selected
input's size/SHA256, and the staging recipe. It is persisted before transfer;
retries reuse its content-addressed identity and never silently shrink or replace
its membership. Changed parent bytes, membership, recipe, or retained provenance
fail closed. Legacy source chunks without image provenance require explicit
reconciliation; a marker alone cannot acknowledge a member.
Before transfer, the existing server helper's read-only `--recipe-inputs` also
binds the full native engine/model plan, staging-binary checksum, and exact
campaign places bytes into that parent recipe. Every subset uses those same
pins; the server rechecks them before staging, including after a storage deferral.
Incomplete older recipes require reconciliation, not automatic pin adoption.
The pinned binary determines the generated function words and staging defaults;
retained jobs must match the full engine, native runtime parameters, places, and
bound source-job crop/function-word parameters, not just reader and image hashes.
Bundled pages inherit the bundle's function-word list when their serialized list
is absent or empty, exactly as the Rust contract and quality worker specify.
Explicit nonempty overrides still must match the retained source job; crop seed
and crop-count checks remain strict. A Rust staging regression feeds actual
default-size bundles through both initial and retained-acknowledgement checks.

Only the selected scans/texts and generated selected sample travel over the
existing LAN transport. Each transfer uses a unique upload directory; the server
verifies it and atomically adopts it under the existing host bridge mutex, never
overwriting inputs an earlier invocation may still be using. Native
`qwen stage --members FILE`, `qwen submit --members FILE`, and
`quality submit --members FILE` use an ordered JSON array of document IDs.
Unrelated pending units are never drained. Native subset staging requires
single-page units. Quality staging uses the selected sample and the existing
bundle recipe. An unsubmitted pre-existing bundle crossing the selection boundary
is refused before staging/publication; reconcile that legacy partial explicitly
rather than silently publishing its extra members.
Every retained native job in the root, including unselected and submitted jobs,
must use the requested version. Subset selection never bypasses this root-wide
cohort invariant.

New quality source jobs also acquire a retained
`quality/<campaign>/shipment-quality/<doc_id>.json` binding to the exact frozen
exported-text bytes, requested reader recipe, and resulting source-job checksum.
The binding also retains the complete frozen staging recipe.
Bundle reader lists and prepared-text checksums must match that bound source job.
Previously staged jobs without this evidence are not adopted, even when their
JPEG/ALTO match. They require explicit reconciliation, as does a quality-stage
crash that leaves jobs before their bindings are persisted; sources stay retained
and no publication is attempted.

The exact supplied `texts/<doc_id>.loc-provenance.json` bytes are frozen at
`quality/<campaign>/texts/<doc_id>.loc-provenance.json` before staging/publication
and remain after retirement. Replay and retirement recheck their frozen hashes.
Missing or conflicting archive provenance requires explicit reconciliation.
The archive receipt must bind both original and prepared ALTO: if namespace
normalization changes its hash, publication fails closed because the existing
cohort binder requires that same hash. Do not rewrite the supplied receipt or
silently change the cohort controller's contract.

Storage is rechecked under the bridge mutex before staging, and authoritative
native capacity is rechecked again immediately before publication. Reservation
counts the **entire selected subset**, including eventual `no_crops`/`too_large`
dispositions; new native capacity counts only still-unpublished native jobs.
Requests, pinned engines, scientific identities, batch 8, quorum two, and
distinct-host scheduling are unchanged. The native and quality submits both run
under that same existing mutex. The scheduler's short database publication
read-lock interval may briefly delay their INSERTs.
The host-side gate first acquires that same CT mutex, then probes `pve/data`
with a bounded `lvs` call and reserves the full selected count at 64 MiB/page
above the 50 GiB floor. Only a passing probe releases the waiting command into
the server container; the mutex remains held through publication. A failed
probe or EOF cannot start staging. `phase=locked_thin_pool` deferrals preserve
earlier positive/uncertain publication evidence.

Per-application publication intents bind job checksums before submission. An
interrupted publication is reconciled against BOINC by exact workunit identity,
application, input name, routing, and quorum. Positive evidence completes the
marker; missing/duplicate/conflicting history retains the journal and parent,
never clears `submitting` to replay. Native audit rows additionally bind the exact
source/JPEG. A fully acknowledged durable transaction can replay without requiring
already-purged BOINC history, but its retained jobs, intents, markers, and source
audits must still agree.
Both Rust submitters fsync the unit directory after the submission-marker rename.
Positive reconciliation also syncs already-present submitted markers before
acknowledgement; matching interrupted duplicate markers are completed, never
replayed. Acknowledgement validation uses one campaign-audit snapshot per
transaction, rather than reopening the entire audit for every member.
Campaign places are installed with the frozen-byte atomic-copy helper; a partial
`.pending` copy cannot become an accepted campaign vocabulary on retry.

Producer journals live outside the removable parent:
`<outbox>/.qwen-transactions/<campaign>/<chunk>/`, with `parent.json`, `recipe.json`,
`owner.json`, and `<transaction-id>/{manifest,transfer,receipt,invocation}.json`.
Server evidence lives at `/project/qwen-shipments/<native-campaign>/<transaction-id>/`.
The emitted `shipment_invocation` includes transaction/parent IDs, reservation and
transferred counts, phase, exit code, `publication_attempted`,
`native_acknowledged`, and `quality_acknowledged`. **Exit 75 is not itself a
zero-publication receipt**: native success followed by quality failure is
`status=partial`; a lost response is uncertain until replay/reconciliation.
Storage and step-window deferrals combine positive/uncertain evidence from both
local journals, never prefer an older zero-publication receipt or erase an
unreconciled attempt. With the ship mutex unavailable, publication state is
unknown (`publication_attempted=null`), not a zero-publication claim.
Use these receipts, not only process status, for monitoring.

The original parent and all subset input copies remain until a disjoint, complete
cover of the parent's members has both native disposition and quality publication
acknowledgements. Server retirement verifies retained evidence again and persists
its retirement record before freeing transferred input copies. Only then may the
producer remove the parent. Manifests, intents, audits, source bindings, and
receipts stay retained. Before reading a READY parent's files, cleanup retries
validate its durable `retired.json`, frozen parent/recipe, and a complete,
disjoint set of exact transaction acknowledgements. The original directory also
has a unique `.qwen-shipment-owner.json` bound to the retained `owner.json`.
Cleanup persists a `retirement-move.json` intent, checks that incarnation, then
atomically moves the directory to the journal's `retired-parent` and fsyncs both
directories before recursive deletion. Retries discover unfinished tombstones
even without a READY directory, including after scans, READY, or the ownership
marker were removed; `cleaned.json` records completion. A new directory reusing a
retired name is retained and rejected, never deleted using the old receipts.
Legacy retirement records without incarnation evidence require explicit
reconciliation. Once a parent has a transaction journal, the legacy whole-chunk
path refuses it; rollback must drain or reconcile it, not bypass the journal.

Deployment/integration gates (offline tests do not satisfy these):

- Install reviewed `qwen_shipment.py` beside `qwen_feed.py` on the shipper and beside
  `qwen_submit.py` in the server container. Install the matching `quality-ship.sh`.
  The existing preparation/archive helpers need no new dependency or network access.
- Install a hash-verified matching `usnm-boinc-stage` with all three `--members`
  options and quality staging/function-word commands. All transaction Rust commands
  use this binary, not an older server-image `quality submit`.
- At an idle singleton boundary, confirm writable durable transaction storage,
  canonical quality `places.txt` (or its existing `v1-wave9` source), compatible
  guarded app versions, shared pinned models, and no conflicting legacy partial
  bundles/unbound source receipts or retirement ownership records. Confirm each
  selected archive receipt satisfies the actual downstream cohort binder.
  Keep both feed/ship timers disabled.
- Verify host Python, `pct exec` pipe forwarding, `flock`, and the `pve/data`
  inventory under the real bridge mutex. The Linux CI rehearsal uses real `flock`
  with disposable `pct`/LVM stand-ins; it is not a production Proxmox proof.
- Enable the activity's explicit flag, then verify one real bounded transaction,
  exact native/quality database identities, audit/quorum and one-result-per-host
  configuration, receipts after an interrupted response, and all backing-store
  floors. Review observed mutex duration and retirement before expanding use.
  No production invocation, database mutation, deployment, or SSH rehearsal is
  implied by the mocked offline process tests.

Ramp the configured target only after measured automatic runs, not to manufacture
10,000 via a number change. For example, start at 200 fresh replicas (100 pairs),
verify new app-6/batch-8 original jobs, zero uncertain submissions, immutable inputs,
phase/rate receipts and post-cleanup floors, then use 1000 replicas (500 pairs).
A later 2000-replica stage is still only 1000 fresh pairs and must fit the unresolved
ceiling plus the existing jobs. At the 28 October measurement, 189 unique READY
sources occupied 523,298,131 bytes, worst page 4,708,052 bytes; 19 native download
page/job inputs occupied 42,208,871 bytes and the two shared models 5,780,074,528 bytes
once. This supports a conservative 64-MiB/page incremental budget, not an assertion
that all source candidates are input-valid or scientifically accepted.
The indexed scan ceiling was 262,920 unique pages, **not** a count of non-English,
original-ALTO-available, crop-valid pages. Language/source/crop checks still decide
eligibility. A measured 19-job arrival is not 10,000 useful jobs, and a trailing
zero-job hour must be reported as zero even when old replicas remain ready.

The separate `usnm-qwen-manifest.timer` in the preparation CT runs
`build-quality --non-english` every ten minutes after its previous execution finishes.
Install its service/timer from `scripts/boinc/onprem/ocr-feed/`; it reuses
`SCAN_SERVER` from `/etc/usnm/quality-build.env`, the existing certificate-authenticated
database DSN, and `/var/lib/usnm-ocr-manifest/qwen-nonenglish` state. When upgrading,
let the old Qwen feed's inline manifest finish before starting this timer, so the two
versions never write that checkpoint concurrently. Verify one manifest pass, then
enable its timer independently of `usnm-qwen-feed.timer`.
Manifest or source-preparation failure/slowness cannot prevent the separate ship
timer from shipping already-ready native work. Its separate service lock prevents concurrent
manifest runs.

This checkpoint requests missing
non-English corpus scans in descending page-damage order, retaining the real language
(and therefore the keep set). Already-retained or already-requested pages are excluded
through a temporary SQL table, not a command-line-sized `IN` list. Consumed fetch-history
entries are not evidence of retained scans: they are re-requested as true-language keep
pages through the normal fetcher. Its buffer-only `.done` markers remain untouched.
Confirmed keep scans are cached for a day; manifest completion checks actual keep-side
availability. Only the normal scan
fetcher performs these downloads, under its existing global budget, holds and failure
limits. User OCR keeps its existing priority.

`waiting_for_crop_sources` is **not corpus completion**. Deferred archives, inaccessible
scans, failed validation and storage backpressure remain outstanding. Canonical outputs
still need the normal `quality-collect` join (`QWEN_SPOOL` is the native batch-8 result
directory) and archival before any input-retention cleanup. This automation does not
delete scientific inputs/results, qualify GPUs, reopen broken plan classes, or replace
whole-page searchable OCR with sampled crop readings.

### Request versions on the feed

The outbox, `done.txt` and source requests stay under `CAMPAIGN`. The shipper chooses where each chunk goes, from `QWEN_SHIP_ARGS` in `/etc/usnm/qwen-feed.env`:

```
QWEN_SHIP_ARGS="--prompt-version 3 --qwen-root NAME --qwen-yiddish-root NAME --yiddish-prompt-version 4"
QWEN_FEED_ARGS="--yiddish-budget 40"
```

- Chunks with no Yiddish go to `--qwen-root` at `--prompt-version`.
- Chunks of only Yiddish go to `--qwen-yiddish-root` at version 4.
- At version 3 or 4, a chunk that mixes Yiddish with another language stays READY, as does Yiddish with no root to go to. `qwen_submit.py` refuses both a second time.

`--yiddish-budget N` caps the Yiddish pages the feed will ever prepare, recorded in `yiddish-budget.json` in its state directory. The normal order then leaves Yiddish out, so the budget can't turn into a reread: pages already read at version 1 stay excluded as they always were, and raising the budget is a decision made in the env file. Without the flag Yiddish keeps its place in the normal order. Language filters on `quality_feed.py select` are `--language` and `--exclude-language`.

`--german-budget N` does the same for German (`deu`) while it is being calibrated, recorded in `german-budget.json`. German pages come as a chunk of their own after any Yiddish budget, and the normal order leaves German out. A German chunk goes to `--qwen-root` at `--prompt-version`. `--german-pages FILE` (doc_ids, one a line) limits the German budget to a frozen sample, worst damage first. The November calibration used 50 of Study 1's pilot pages from #1428, which already have `frak2021` and `deu_latf` readings. `--exclude-language` on `quality_feed.py select` may repeat.

A new root needs the refill and the collector told about it: add the root to `CAMPAIGN_ARGS` in the refill's env, and to `NATIVE_QWEN_ARGS` once its quality root exists. The version 1 root stays out of the refill, so its backlog goes only to the classes open to it.

### Bundled native units (#1435)

By default each native unit is one page, so `llama-server` starts and loads the model once a page. `--qwen-bundle-size N` (2 to 32) in `QWEN_SHIP_ARGS` has `qwen_submit.py --bundle-size N` stage up to N pages a unit in `--qwen-root` (`qwen stage --bundle-size`). The crop and size limits under `qwen stage` above still close a bundle early: at 12 crops a page that's about 10 pages, so `--qwen-bundle-size 10` is the useful setting. Without the flag nothing changes: neither native call names a bundle size, and `qwen_submit.py` stages one page a unit, as before. `--transaction` refuses any size but 1.

Who gains, from the per-task numbers in #1435. A bundle pays the model load, and the gap before the next task, once rather than once a page:

- **RX 9070 XT**: a task takes 32 s, of which 3.5 s is the load and 25 s is reading, and the GPU sits about 10 s idle between tasks. A bundle saves up to 13.5 s of every 42 s a page, about a third.
- **NVIDIA**: 22 s loading against 80 s reading. A bundle saves up to 22 s of every 102 s, about a fifth.
- **Apple and Intel**: loading is under 1% of a unit, so they gain little. They read bundles all the same.

The loop guard saves more: repeated loop text is 57% of generated tokens on every vendor. Bundling comes after it.

Turning it on:

1. **A new root.** `qwen_submit.py` refuses to bundle a root that already holds single-page units, and refuses `--bundle-size 1` for a root staged with bundles (`qwen stage` refuses that too). Name a new `--qwen-root` with the flag. Chunks of only Yiddish keep single pages in `--qwen-yiddish-root`, which already holds single-page units and where only 12% of strict pairs agree.
2. **The collector, first.** A bundle is canonical only when every page agrees, so an inconclusive bundle's agreed pages reach the rows only through `quality collect --qwen-pages`. That flag, `quality_results.py --qwen-pages` and the service's `QWEN_PAGES_ARGS` come with #1572, not with this change: until #1572 is merged and deployed, setting `QWEN_PAGES_ARGS` does nothing and an inconclusive bundle loses all its pages. Deploy it, give every collector named for a native root a build from #1572 on, set `QWEN_PAGES_ARGS` in the collector's env (`usnm-qwen-results.service`), and add the new root to `NATIVE_QWEN_ARGS`, all before the first bundle ships.
3. **The refill.** Add the root to `CAMPAIGN_ARGS` in the refill's env once `qwen-auto-cohort.py` binds bundles (#1561) and is deployed. Before that, the refill stops at a bundled root's first unit.
4. **The apps.** `usnm-qwen-app` has read bundles since #887, so no new app version or signing is expected. Before the whole feed moves, check that one bundle completes on each plan class.

Admission stays conservative: the watermarks reserve one fresh pair per page, while a bundle is one pair for up to 10 pages. `--qwen-max-ready` still counts replicas, so the same 1,000 holds more pages.

What is still open: the quorum. A bundle still needs every page to agree to be canonical, so an inconclusive bundle goes out to a third and fourth replica for its disagreeing pages. Whether crop-consensus or a page-level verdict decides the unit, with its credit and retry, is a maintainer decision.

### One submit binary and one collector (#1449)

The roots at versions 3 and 4 depend on the static build `/project/ops/qwen-feed/request-guard-44465088/usnm-boinc` (#1437) twice:

- `qwen_submit.py` submits them with it, as `usnm-boinc-stage`, because a server image built before #1437 has a `qwen submit` that refuses them.
- `usnm-qwen-results` collects them with it, because the collector at `/project/ops/quality-results/usnm-boinc` can't read them. `qwen-nonenglish-v1` keeps that older collector.

Both are settings, so neither move needs a code change. Each is undone by removing what it set.

**Submission, after the server image is rebuilt.** Once the server runs an image built from #1437 or later, add `--qwen-submit-binary usnm-boinc` to `QWEN_SHIP_ARGS` in `/etc/usnm/qwen-feed.env` on the shipping host. The flag needs the `quality-ship.sh` and `qwen_submit.py` from #1449 or later.

```
QWEN_SHIP_ARGS="--prompt-version 3 --qwen-root NAME --qwen-yiddish-root NAME --yiddish-prompt-version 4 --qwen-submit-binary usnm-boinc"
```

Every version then submits with the image's `usnm-boinc`. Staging keeps `usnm-boinc-stage`, and so does transactional submission. Watch the next tick that ships a chunk. If the image still refuses a version, the submit fails, the tick fails, and the chunk stays READY. Removing the flag goes back to the static build.

**The collector.** The row cache's key (`Cache::new` in `crates/usnm-boinc/src/quality/collect_cache.rs`) includes the collector's resolved path and its bytes, and the `--qwen-crop-states` directory. A root whose collector changes has every unit dirty. A pass recomputes at most `--max-dirty-units` (8) of them and exits nonzero before publishing, so `qwen-nonenglish-v1` (about 90 units) would fail about 12 ticks, three hours, with no new rows. The old parts stay, so switching back costs nothing while they're there. No single collector keeps every key, because the older one can't read versions 3 and 4. `qwen-nonenglish-v1` moves once, warmed first:

1. Never name the image's `usnm-boinc`, or anything a rebuild replaces, as a collector. Every image rebuild would then rewarm every root. The rebuild for #1449 doesn't change a collector.
2. Use the static build that versions 3 and 4 already collect with. The versions 3 and 4 keys stay the same (same resolved path, same bytes), and only `qwen-nonenglish-v1` changes. This doesn't depend on the image rebuild. Keep `request-guard-44465088/` in place while it is named.
3. Warm `qwen-nonenglish-v1` with that build before the switch, so no tick fails. This is the service's own collection with the static build, written to a scratch output directory: the cache is the real one, and the published snapshot isn't touched. Run it on the BOINC host, as root. A pass that still has cold units writes 8 of them, then fails with `collection cache warmed 8 dirty units; at least N deferred`. Repeat it until it exits 0.

   ```sh
   pct exec 230 -- flock -w 60 /run/usnm-boinc-step.lock podman exec usnm-boinc-server \
     timeout --signal=TERM --kill-after=30s 20m /project/ops/qwen-feed/request-guard-44465088/usnm-boinc \
     quality collect --campaign qwen-nonenglish-v1 --root /project/quality/qwen-nonenglish-v1 \
     --spool /project/usnmocr/results/0 --qwen-root /project/qwen/qwen-nonenglish-v1 \
     --qwen-spool /project/usnmocr/results/8 --out-dir /project/ops/quality-results/prewarm/qwen-nonenglish-v1 \
     --cache /project/quality/qwen-nonenglish-v1/collected-parts --max-dirty-units 8
   ```

4. In `/etc/usnm/quality-results-qwen.env` on the preparation host, name the build once and drop the per-root `=COLLECTOR`:

   ```
   QUALITY_COLLECTOR=/project/ops/qwen-feed/request-guard-44465088/usnm-boinc
   NATIVE_QWEN_ARGS="--native-campaign qwen-nonenglish-v1 --native-campaign qwen-guard-v3-20261102 --native-campaign qwen-guard-v4-20261102"
   ```

   Set it in this file only. `usnm-quality-results` reads `quality-results.env` and the `/project/ops/quality-results/usnm-boinc` link, and keeps its collector. The next tick should print a `loaded_at` line for each of the three roots and exit 0. Then delete `/project/ops/quality-results/prewarm/`.

Turning on the crop rule changes every root's key anyway, through `CROP_STATES_ARGS`, and needs a collector from #1434 (#1490) on, which `request-guard-44465088` predates. If the crop rule is coming soon, make one move instead of two. Install one such build at a new path of its own, warm all three roots with it and `--qwen-crop-states /project/usnmocr/qwen/crop-states`, as in step 3, and then set `QUALITY_COLLECTOR` and `CROP_STATES_ARGS` together.

### OCR's prefetch manifests (#1055)

The scan fetcher (home-infra `usnm-loc-fetch`) takes OCR's manifests before the quality
pass's, all inside the host's 45-a-minute LoC budget. `scripts/boinc/onprem/ocr-feed/ocr_manifest.py`
makes OCR's in CT215, hourly (`usnm-ocr-manifest.timer`): the pages queued for OCR
(curated `ocr-local/queued/`, read live with the container's Arc identity) that have no
outcome since (`ocr-local/outcomes/`) and no scan on pve-big's scan server, users' requests
first, at most 1,000 a manifest, PUT through the scan server as
`/manifests/queue-ocr-<stamp>/manifest.parquet`. It makes the next one only once the last
is mostly fetched (90% on the scan server) or a day old.

### The full image pass and the piggyback (#962)

`ocr_manifest.py build-quality` (`usnm-quality-build.timer`, every 2 hours in CT215) takes
the next issues from `usnm_quality`'s stored summary: pages with no image features yet, not
staged, grouped by issue, the issues ranked round-robin over (title, decade) with the worst
first by their worst page (the mean of the damage score and LoC's own doubt, `1 - wc_mean`),
the first `--strata` issues of each, every page of an issue. Whole issues let the fetcher
ask for an issue's METS, and its batch's manifest, once for all its pages: about one request
a page. The first stratum-1 manifest (25 October), one page per (title, decade), cost about
three bucket requests a page. `--batches` (`--pass-batches` to the installer) limits the pass
to the batches a file names, such as LoC's bucket's public ones. It queues the pages through
the scan server as `queue-quality-<stamp>`, which the fetcher runs behind OCR's manifests in
one-hour slices, inside the host's global LoC budget. Rows are marked `eng`, so the scans go
to the fetcher's buffer set.

`quality_campaign.py` builds a worst-first re-OCR list from the stored summary ([#1415](https://github.com/tgoodyear/usnewsmap.com/issues/1415)): every page the [damage ranking report](https://github.com/tgoodyear/usnewsmap.com/pull/1401) calls worth re-OCRing, and an OCR-then-compare sample of the moderate band as its own campaign, `<campaign>-moderate`. It reads the database in a read-only transaction and writes only to its `--out-dir`, so it stages, queues and ships nothing. Each campaign gets `plan.jsonl` (every page in order), `pages.jsonl` (the ones `--index` has a scan for, which `quality_feed.py run` reads) and prefetch manifests for the rest (`manifests/m00001.parquet`, marked `eng` like the image pass's). The file's header has the ordering and sampling rules.

The bands are named arguments whose defaults are the report's: `--worst-above 0.25` (the re-OCR list), `--moderate-above 0.1` (the sample's band, up to `--worst-above`) and `--min-occurrences 50` (word occurrences, `head + near`, for both). The export cuts at the last two in SQL; each row is tested again as it is read, with the rate rounded to a `real` first, so a page Postgres stores as 0.1 is over 0.1 as it is in the report. `--dsn-file` keeps the export as `input.csv` with its cut in `input.json`; `--input` replays it, and can narrow the cut but not widen it. `--cell-pages` (default 400) caps each language and decade cell of the sample: a paired test in one cell then detects a mean drop of 0.02 in damage rate at 80% power and two-sided 5%, from the SD of 0.138 measured on 353 paired English pages ([#1416](https://github.com/tgoodyear/usnewsmap.com/pull/1416)). `summary.json`, also printed, has the filters and the counts by band, language and decade.

To build the lists in CT 215 (`usnm-batch`) on pve-laptop, with copies of the OCR state and the scan index kept beside them for a rebuild:

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

A rebuild from the same export takes `--input "$out/input.csv"` in place of `--dsn-file`, and a new `--out-dir`.

Only pages our OCR has done or is about to do are left out: rows in `quality.ocr_outputs`, and pages `--ocr-state` shows outstanding or imported. Pages in a quality campaign stay in, because a `usnm_quality` unit compares the texts a page already has and doesn't read the page. `--campaign-exclusions` (off by default) leaves those out too, and enables `--exclude-list`.

**The moderate sample shows nothing until its pages are OCRed.** A quality unit on a page our OCR hasn't read gives image features and LoC's agreement with American Stories. A gain needs a `quality.ocr_outputs` row for the page, compared with LoC's damage score.

Two things limit how a list reaches the fetcher and the feed. `follow` takes every fetched page under its own campaign, so a page fetched from one of these manifests lands there unless `follow`'s `--staged` list names it; `plan.jsonl` still tells the bands apart. And when the scan fetcher is installed with `--no-loc-prefixes queue-quality-` (`USNM_PREFETCH_NO_LOC_PREFIXES`), a manifest named `queue-quality-*` takes JP2s only from LoC's bucket or a mirror, so pages of batches the bucket doesn't serve wait in that queue. Without it, those pages are fetched from LoC within the global LoC budget.

`quality_feed.py follow` (`usnm-quality-follow.service`) gives every page the fetcher writes
that no campaign has staged (OCR's and the pass's) a unit, in fetch order, in full chunks of
`--chunk-pages`. It holds back a part chunk until no chunk has been written for
`--max-wait-secs` (6 hours): it used to chunk whatever had been fetched since its last
round, a minute or so, and in the 24 hours to 5 November 2026 the median bundle held 16
pages and ran for seconds. Pages of a batch the curated copy has no LoC text for yet don't
count towards a full chunk; they wait, logged, until it does. A page that can't be prepared
(its scan can't be fetched or decoded) is replaced by the next one, from a reserve of a
tenth of `--chunk-pages`; a chunk still short after that ("chunk came up short") is
written only once it's overdue, so a scan server that stops answering mid-chunk doesn't
ship a small unit.

Once a chunk has shipped, the pass's buffer scans are deleted, except the most damaged
(LoC's `damage_rate`; unscored pages last), up to `--keep-bytes` in all (1 TB from
`quality-feed-install.sh`, about 210,000 pages at 4.7 MB a scan, half the fetcher's 2 TB
buffer cap so OCR's own scans keep flowing). They stay on the scan server for re-OCR and are
listed in `<state>/kept.tsv`, one list for every campaign, so a new campaign's budget counts
what an earlier one kept. A newly shipped page more damaged than the least damaged kept
one takes its place, and that one is deleted; if that delete fails, the kept page holds its
place and the newcomer is deleted instead, so the list never passes the budget and no scan
sits on the server outside it. A kept page leaves the list only once its sidecar is found
gone and a DELETE makes sure its bytes are too (OCR's worker deletes a scan it read, best
effort, and one of ours that marked it done but couldn't unlink it leaves just the sidecar
gone). One OCR still wants stays listed and counted, and is neither evicted nor looked up
while it does: `ocr_manifest.py build` records every page OCR asked for in `ocr-docs.txt`
(every queued page whose scan is already on the scan server, not only its manifests' pages)
and rewrites `ocr-active.txt` each run with those no outcome has ended yet, which the
follower reads (`--ocr-active`; until that file exists, every page in `ocr-docs.txt`), and
a newly shipped page in either is OCR's, neither kept nor deleted. Its check time is reset, so it is among the first looked
up once OCR is done with it, and it stays a kept page like any other if OCR's delete failed.
`--keep-verify` (500) of the longest-unchecked are looked up a round; a lookup that fails goes to the back, and one the scan server doesn't answer ends that round's
lookups and deletes, which the next round picks up where it left off. The follower looks
after the list every round, before its campaign has shipped anything too, and deletes (or
keeps) the shipped scans of every campaign it has followed (`<state>/followed.txt`), so the
chunks a shipper takes after a campaign switch don't leave their scans behind. Its own
campaign goes first, so room its lookups free is there for every campaign's newly shipped
pages, and pages leaving the list are recorded in `<state>/retired.txt`, which every
campaign reads, so one campaign never takes back a page another let go.
Before 5 November 2026 every one was deleted, 150,000 pages, 54,000 of them with a damage
rate of 0.1 or more. OCR's scans and the keep set are never deleted.
`quality-feed-install.sh --follow` turns both on with the shipper.

**What bounds the supply.** Each round, `follow` takes every full-resolution scan in the index
that has no unit or staging record, isn't in `--staged` and isn't done, up to the outbox's
room. While the shipper keeps up, it holds no backlog, so the pages it feeds in an hour are the
pages the fetcher wrote in that hour. Those come from the pass's manifests (`--pass-pages` a
run, with the timer's 2 hours between the end of one run and the start of the next) and from
whatever OCR's and the other manifests fetch. The pass's size and the bucket bound the first.
The rest depends on which batches the other manifests reach, how many of their pages are
already held, and LoC's budget. A held scan that a campaign staged before isn't fed again.
Raising the supply therefore needs more bucket or LoC requests, or feeding staged pages again,
and each of those is the maintainer's choice. The [measurements of 5 November
2026](notes/2026-11-05-quality-supply.md) split the supply by source.

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
- How far the version 2 hat's `local_cer_sym` underestimates a badly garbled reader, whose worst chunks don't place (they show as low `matched`, not as error). No page-level transcription of a three-reader page exists yet to measure it.
- The crop-level estimate's agreement with the page-level one.
- Time and memory per unit on volunteers' hosts, and so the `create_work` bounds and the Qwen containers' limits.
- The Dice threshold for `usnm_qwen`, to be measured from the logged pairs.
