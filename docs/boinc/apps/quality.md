# OCR quality on BOINC

Issues: [#522](https://github.com/tgoodyear/usnewsmap.com/issues/522), [#533](https://github.com/tgoodyear/usnewsmap.com/issues/533), [#559](https://github.com/tgoodyear/usnewsmap.com/issues/559). Two BOINC apps estimate, for every page and with no transcription, how accurate and complete each of our texts is and what the scan looks like:

- `usnm_quality` (CPU) measures the scan and compares the texts we hold of the page (LoC's, American Stories', ours).
- `usnm_qwen` (GPU) reads sampled word crops of the page with Qwen3-VL on `llama-server`, as one more reader.

Both run as native executables with no container runtime. Since 2 November 2026 every live `usnm_qwen` version is a GPU version; the CPU and container Qwen classes are retired ([App versions and plan classes](#app-versions-and-plan-classes)). On 10 November 2026 the public [server status page](https://boinc.usnewsmap.com/server_status.php) listed both apps with tasks in progress and their validators and assimilators running.

Other apps: [boinc/README.md](../README.md). The [transcription pilot](../../notes/2026-10-16-boinc-transcription.md) used `usnm_qwen` for held-out public-gold inference.

## What a unit is

### usnm_quality

A unit is one page, or a bundle of up to 500 pages (`quality stage --bundle-size`; the on-prem feeder packs 500-page chunks and `bridge-job.sh` uses 300). The host checks every input's checksum before running, and the unit always writes all three outputs. Floats are rounded to six decimals and the computation is single-threaded over ordered maps, so two hosts' records are identical bytes.

| File                    | Contents                                                                                                                                                                                                                                                                                                                                                     |
| ----------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `page.jpg` (in)         | full-size IIIF JPEG (a bundle's pages concatenated, at most 1 GiB)                                                                                                                                                                                                                                                                                           |
| `loc.xml` (in)          | LoC's ALTO; used for features and crops                                                                                                                                                                                                                                                                                                                      |
| `texts.json` (in)       | `quality::Texts`: `{"page": {"width", "height"}, "readers": [{"name", "source", "text", "regions": [{"box": [x0, y0, x1, y1], "kind", "legibility"}], "alto": false}]}`; every reader the job names, once, same source; every reader's text (LoC's too) comes from here                                                                                      |
| `places.txt` (in)       | the campaign's place names, one sticky file for all units, `usnm_quality_places_<sha256>`                                                                                                                                                                                                                                                                    |
| `quality.json` (in)     | `usnm_ocr_contract::quality::Job`: `v` (1), `id` (sha256 of the fields), `doc_id`, `language`, `group`, `inputs` (`page`, `alto`, `texts`, `places`, each `{bytes, sha256, source}`), `engine {image}`, `readers` (`[{name, source}]`, report order), `function_words`, `crop_seed`, `k_crops`, and `pages` for a bundle                                     |
| `result.json` (out)     | `quality::ResultManifest`, at most 8 MiB: `v`, `job_id`, `engine`, `outcome` (`ok`, `timeout`, `failed`, `unit_error`), `unit_error`, `page {image, layout}`, `pages` (bundles), `readers`, `hat`, `legibility`, `timings {decode_ms, features_ms, text_ms, total_ms, cpu_ms, peak_rss_kib}`, `host`, the other outputs' checksums and sizes, `completed_at` |
| `confusions.json` (out) | reader name to its top 50 confusion pairs, at most 8 MiB                                                                                                                                                                                                                                                                                                     |
| `diag.txt` (out)        | at most 64 KiB                                                                                                                                                                                                                                                                                                                                               |

Per reader in `result.json`: `name`, `source`, `chars`, `words`, `est_cer` (clipped at 0), `est_cer_raw`, `est_cer_low_bound_note`, `completeness`, `precision`, `findable_content`, `findable_places` (`content_found`, `content_total`, `places_found`, `places_total`), and `pairs` against every other reader: `cer_a` (this reader as hypothesis), `cer_b`, `cer_sym` (edits over mean length), `char_edits`, `chars_a`, `chars_b`, `wer_a`, `wer_b`, `word_edits`, `words_a`, `words_b`, `dice`. Rates without a denominator are null.

### usnm_qwen

A unit is one page, or a bundle of up to 32 pages and 256 crops ([Bundled native units](#bundled-native-units-1435)).

- `qwen.json` (`usnm_ocr_contract::qwen::Job`): `v`, `id`, `doc_id`, `language`, `group`, `input`, `crops` (`[{id, box, stratum, loc_text}]`, at most 64 a page; the quality unit's crops, LoC's words in visual line order), `engine {image, llama, models: [{name, file, mmproj, min_gpu_mb}], weights}`, `prompt_version`, `gpu_timeout_secs`, `cpu_timeout_secs`, `max_tokens_per_1000px2`. Plus `page.jpg`.
- `readings.json` (`qwen::Readings`, at most 2 MiB): `v`, `job_id`, `outcome`, `device`, `model`, `prompt_version`, `readings: [{id, text, tokens, ms}]`, `diag`.
- `result.json` (`qwen::ResultManifest`): outcome, device, model, times, crops read, host, checksums of the other two. Plus `diag.txt`.
- Outcomes: `ok`, `timeout` (crops read so far kept), `failed`, `no_gpu` (GPU version, no usable GPU), `unit_error`.

The GPU budget (`gpu_timeout_secs`, default 14,400 s) is application runtime for the whole page, separate from BOINC's report deadline and resource bounds; the contract accepts 60 to 14,400 s. Timeouts are part of the job identity: never edit submitted inputs; stage a new identity to retry and keep the original evidence.

#### Request versions

`prompt_version` fixes the whole request:

| Version | Encoding                                                                                   | Loop guard |
| ------- | ------------------------------------------------------------------------------------------ | ---------- |
| 1       | llama.cpp default: native resolution, one token per 32x32 px (about 200-400 tokens a crop) | none       |
| 2       | `llama-server --image-min-tokens 1024` (Qwen-VL's minimum); crop upscaled to it            | none       |
| 3       | as 1                                                                                       | `stop`     |
| 4       | as 2                                                                                       | `stop`     |

On #529 version 2 raised image-checked Yiddish words from 23% to 59% and left English, Serbian and repeated-print crops as they were. The loop guard (Study 5's `stop` rule) streams the request, checks `tail_repetition` every eight tokens from the twelfth, and on a hit closes the connection (which stops llama-server), keeping the text before the repeated run plus one period. A cut reading, including printed repeats at a natural end, records the raw digest and length and `repetition_loop_guard_cut`; `tokens` is then the count of text events (`loop_guard.stream_deltas`, `completion_tokens_reported` empty). Versions 3 and 4 need app version 1.17; older versions refuse them.

`qwen stage --prompt-version` and `qwen_submit.py --prompt-version` choose it (default 1). `qwen_submit.py` defers a chunk (exit 75, READY kept) while any dispatchable app version can't read the version, and refuses a second version in one root. The feed stages 3, and 4 for Yiddish ([Request versions on the feed](#request-versions-on-the-feed)).

Under versions 1 and 2 readings are kept as read.

#### Generation records

The native worker keeps a generation record per crop in its atomic checkpoint (`qwen_checkpoint.json`) and writes it to `diag.txt` as a `== generation_v1/<crop>` section (`generation_record` in `crates/usnm-qwen-app/src/server.rs`; schema 2, or 3 with `loop_guard`). A record holds the server's finish reason (missing means unknown; a non-string or oversized one is flagged `invalid_or_oversized_finish_reason`, not taken as a stop), the reported completion tokens, the requested budget, SHA-256 and byte counts of the response and of the raw content, whether cleaning changed the content, the longest run of identical tokens, and flags: `completion_token_budget_exhausted`, `tail_periodic_repetition` (output ends in a run repeated three or more times over at least 30 characters), `repetition_loop_at_token_budget` (that run also hit the budget; printed text can repeat, a looping model runs out of tokens), `repetition_loop_guard_cut`, `consecutive_duplicate_lines`, `empty_reading` and `uncertainty_only`. Repetition is measured on the full raw content, before cleaning cuts a reading to 8 KiB. The hashes identify the response; they are not copies of it. Sections are cut at 2 KiB and `diag.txt` at 64 KiB with a cut notice, so a section missing from a large bundle's diagnostics is unknown, not a success. A checkpoint from before the records loads without invented finish reasons, and the checkpoint rejects unknown fields, so an older worker can't resume a newer checkpoint. The flags label uncertainty: they don't establish accuracy (`scientific_acceptance` says so), and token exhaustion or repetition is no reason to lower a validation threshold.

The readings' `diag` gives `loop_guard` (`stop`/`none`), `repetition_loop_crops` (names, or a count if they don't fit), `repetition_loop_crop_bits` (hex, always complete; bit `i % 8` of byte `i / 8` is crop `i`) and `image_min_tokens`.

The request uses temperature 0, seed 42, `top_p=1` and 4 tokens per 1,000 crop pixels, with no repetition penalty. On WU21128 three platforms spent the whole 1,304-token budget on one crop (AMD and Intel looping a word, Apple emitting 643 `[?]`). A larger budget could prolong a loop. Do not lower the Dice floor to accept repetition, shared hallucinations or agreed `[?]`; accuracy needs independent image-grounded transcriptions stratified by language and legibility.

## Redundancy and validation

Both apps: `min_quorum` 2, `target_nresults` 2, at most 3 errors, 3 successes and 4 results in all (`create_work_args` in `crates/usnm-boinc/src/quality/submit.rs` and `qwen/submit.rs`). Daemons are in `boinc/apps/daemons.xml` (rendered by `render_config` in `boinc/images/server/lib.sh`), the validator wrappers in `boinc/images/bridge/`. `script_validator` runs:

- `usnm_quality_check` (`usnm-boinc quality check`): shape, sizes, UTF-8, checksums of `confusions.json` and `diag.txt`, record consistent with outcome. Every result is copied to `<project>/quality/seen/<result>/`.
- `usnm_quality_compare` (`usnm-boinc quality compare`): same job, engine and outcome, and equal canonical records (keys sorted, floats to six decimals, without host, timings, time and diagnostics). Every pair goes to `<project>/quality/pairs.jsonl` with the verdict and, if different, which part differs.
- `usnm_qwen_check` (`usnm-boinc qwen check`): shape, sizes, checksums of `readings.json` and `diag.txt`, readings against the result (count, device, model, outcome). Every checked result, not only the canonical one, is kept in `<project>/qwen/seen/<uploaded-result-manifest-name>/` before validation succeeds, as a complete fsynced directory: `capture-v1.json` binds upload names, byte counts and SHA-256s (including exact `result.raw.json` and versioned generation sections in `diag.txt`), and `readings-metrics-v1.json` holds per-reading uncertainty, repetition and token metrics (not accuracy). Legacy normalized `result.json` files are completed with the raw manifest and receipt; replay never overwrites mismatches. Capture or storage conflicts return transient exit 3 (deferred, volunteer not marked invalid); malformed science fails non-transiently.
- `usnm_qwen_compare` (`usnm-boinc qwen compare`): same job and engine, both read the page or neither, and per crop both read or neither with word-bag Dice at least 0.5 (`PROBE_MIN_DICE`), which rejects only unrelated text. Every pair goes to `<project>/qwen/pairs.jsonl` with per-crop Dice and both hosts' devices and models. The line's `wordless` lists crops neither reading has a letter or digit in (Dice 1.0 regardless), with `id`, `identical` (equal once whitespace is collapsed) and `looped` (loop-guard bit or a trailing repeated run), for measurement only ([#1463 note](../../notes/2026-11-04-qwen-wordless-crops.md)).

### Bundle pages

For a bundle unit the Qwen rule also runs per page (`<doc_id>:` crop prefix) when both results finished (`ok`): the pair line gains `pages`, and with `--pages` (`<project>/qwen/pages` in the wrapper) each page's verdict and readings go to `<pages>/<job_id>/<doc_id>/<pair>.json` (`page_agreed` or `page_disagreed`, `accuracy_established: false`). An all-blank page is not agreed. The unit still needs every page, so a bundle with one disagreeing page stays inconclusive in BOINC while its agreed pages are kept for `quality collect --qwen-pages` (#1435).

### The crop-consensus rule

`usnm-boinc qwen compare --rule crop-consensus-v2` judges crop by crop (default `strict-v1`). A crop is `looped` if either side loops, `no_text` if neither side has a letter or digit, `disagree` if only one side has text, else `agree` when character similarity (`1 - edits / longer length`, whitespace collapsed, Hebrew points ignored, readings unchanged) is at least 0.7. A pair agrees when no crop disagrees and `agree` crops are over half of the crops with text and hold at least 100 characters; a page with no text agrees, as under the strict rule. Results that did not both finish keep the strict verdict. The pair log gains `rule`, `basis`, `strict_reason`, `crop_states`. `--crop-states <dir>` (required with this rule) keeps each pair's states at `<dir>/<job_id>/<pair>.json` (record version 2, `basis` `crop-consensus-v2` or `strict-v1`, `readings_sha256` naming both results); if the record can't be written the strict verdict stands, so a result accepted without a record was accepted crop for crop. A pair vouches only for its `agree` crops, and `quality collect --qwen-crop-states <dir>` joins only those (#1434). Evidence: [validation note](../../notes/2026-11-01-qwen-cross-vendor-validation.md), [floor check against human transcriptions](../../notes/2026-11-04-crop-consensus-floor-check.md) (reproduced by `usnm-boinc qwen grounded`).

The wrapper reads `<project>/qwen/validator-rule`: `crop-consensus-v2` turns the rule on for the next pair; removing the file or writing `strict-v1` turns it off, with no restart. A `usnm-boinc` beside the wrapper's target overrides the image's, so a build newer than the server image can validate: install wrapper and binary in one directory and point `bin/usnm_qwen_compare` at it; a server restart relinks `bin/` to the image's wrapper. Order: run every Qwen collector with `--qwen-crop-states <project>/qwen/crop-states` first (`CROP_STATES_ARGS` on `usnm-qwen-results.service`, `QWEN_CROP_STATES` on the bridge's `quality-collect`), then add the rule file; a collector without it joins every crop of a canonical result. Turning the rule off keeps the records, and results accepted under it still join only their vouched crops.

### Stopping tie-breakers by language and request version (#1477)

When a Qwen first pair disagrees, the validator raises `target_nresults` and the transitioner sends a tie-breaker, up to `max_total_results` 4. For Yiddish request version 1 a tie-breaker rarely settles the unit ([replica cost note](../../notes/2026-11-04-qwen-replica-cost.md), [tie-breaker policy note](../../notes/2026-11-05-qwen-yiddish-tiebreak-policy.md)), so `boinc/scripts/qwen-tiebreak-policy.py` stops them for named groups: `<project>/qwen/tiebreak-policy.json` (`{"schema": 1, "stop_tiebreakers": [{"language": "yid", "request_version": 1}]}`) or `--stop-tiebreakers yid:1` (repeatable). With neither, nothing changes; turning it on for `yid:1` is a maintainer decision (#1477). The language is the unit's language set as `qwen-replica-cost.py` labels it, matched exactly (`yid` does not match `heb+yid`).

A unit is settled once its first pair has disagreed: no canonical result or error, at least two inconclusive returns, no replica running, nothing waiting for the validator, not due for the transitioner. Settling is one transaction under row locks (`SELECT … FOR UPDATE`, then a recheck): unsent replicas become OVER/DIDNT_NEED (as `cancel_job` does), `max_total_results` = result count, `error_mask |= 8` (too many total results), `transition_time` now. The transitioner then marks the returns NO_CHECK and assimilates with no canonical result, as for a capped unit; the page keeps its Tesseract readings and the router still reads Yiddish with `yid`. Units with a running replica, or not lockable within 5 s, wait for a later pass. No new Yiddish v1 units are made, so repeat until `plan` reports nothing. Settled units count as error workunits in `qwen_submit.py`'s source status and leave the feed's `unresolved_native` count (freeing room under `--max-unresolved`). A settled unit can't be reopened; re-reading its page needs a new unit.

CI fixtures `boinc/scripts/db-tls/full.py` (server daemons, MariaDB) and `boinc/scripts/db-tls/mysql84.py` (production's MySQL, no daemons) cover row-locked units, running replicas and a concurrent `mark_as_sent`, and check that `mark_as_sent` then matches no row for a cancelled replica, so a stale feeder cache can't send it. With no daemons in the second, bound the first production run to one unit.

Run as root on the BOINC host:

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

## The estimator

Everything is in `crates/usnm-ocr-quality`, a library with no BOINC in it.

- `align`: edit script with backtrace in linear space (Hirschberg; a full table under 2^18 cells), fixed tie order. Texts are normalised as the gold set does (`usnm_ocr_survey::text::normalize`); character edits use the bit-parallel distance, word edits whitespace-split words, word-bag Dice lower-case alphanumeric tokens.
- `agreement`: two readers with no third (#962): the hat's pair, word-bag cover each way and length ratio, versioned. `usnm-ingest ocr-agreement` runs it for LoC against American Stories ([Text agreement](../../ocr/text-agreement.md)).
- `hat`: `d_ij = e_i + e_j` by least squares (normal equations, Gaussian elimination with partial pivoting); with three readers `e_A = (d_AB + d_AC - d_BC) / 2`. Estimates clipped at 0 (raw kept), residual is the RMS of `d_ij - e_i - e_j`, fewer than three readers or too few pairs gives nulls. The unit solves on whole-page `cer_sym` (hat version 1); `collect` re-solves as [version 2](#the-page-hat-version-2-962) and writes that into `pages.parquet`.
- `consensus`: tokens normalised, whitespace-split, edge punctuation stripped, case-folded; word-level alignment of every reader pair links occurrences, and components spanning at least two readers are consensus words. Completeness: share of consensus words a reader has. Precision: share of its tokens that are consensus occurrences. Substitutions against a missing consensus word feed the confusion counts.
- `findability`: content words have four or more letters and are not function words; places are `places.txt` tokenised the same way, matched as consecutive sequences. Rate: share of the consensus's distinct content words (and places) the reader contains.
- `confusion`: per disagreeing word pair, consecutive non-matching alignment steps merged (`rn` for `m` is one), top 50 per reader.
- `features`: on a grey copy downscaled by an integer factor to at most 1,024 px on the long side: skew (-3 to 3 degrees in 0.1 steps, maximising projection-profile variance; positive descends right), blur (3x3 Laplacian variance), contrast (95th less 5th percentile), noise (mean absolute 3x3 median residual), bleed-through (share in the middle third of grey between ink and background modes), ink density (share below Otsu). From ALTO: blocks, lines, words, median line height, columns (x-centre clustering with a gap of half the median block width), mean word confidence.
- `crops`: the gold set's recipe (`scripts/ocr-survey/crops.py`): a line of at least 4 words spanning at least 600 px; box from its left and top less 15 px to its right (at most 1,600 px from the left) plus 15 px, down to 6% of page height below the line's top. Six strata (block word-confidence terciles by top/bottom half); `k` crops (12 by default) round-robin over strata, each shuffled by SplitMix64 from the seed.
- `report`: the record, its JSON Schema (`report::schema()`), the rounding.

Unit legibility is `1 - min est_cer` (`basis: page-hat`). The crop-level estimate and the join with `usnm_qwen` readings happen in `quality collect`, since the apps run on different hosts; with a Qwen reading it recomputes the crop-level hat (`legibility.basis` `crop-hat+qwen`).

### The page hat, version 2 (#962)

Whole-page `cer_sym` counts reading order and missing text as edits, and LoC (ALTO block order), American Stories (article order) and Tesseract (its own segmentation) differ mostly in order and coverage. Version 2 (`usnm_ocr_quality::page_hat`, `VERSION` 2) solves the same `d_ij = e_i + e_j` on `local_cer_sym` from [agreement v1](../../ocr/text-agreement.md): edits over the characters the local alignment placed, in any order. A pair falls back to whole-page `cer_sym` (basis `page`) when nothing placed or neither reader has half its characters placed in the other (`MIN_MATCHED` 0.5). Each reader's `matched` is the mean share of its characters placed in the others' texts (coverage, not error).

No app version is needed: `collect` rebuilds the unit's inputs from `texts.json` (`slot::reader_inputs`, LoC's ALTO text for an empty LoC text) and solves version 2. In `pages.parquet`, `hat_version` says which hat `est_cer`, `est_cer_raw`, `hat_determined`, `hat_residual` and `legibility_ceiling` hold; the unit's values are in the `_v1` columns; `matched` and `hat_pairs_json` (per pair `a`, `b`, `basis`, `d`, `cer_sym`, `local_cer_sym`, `matched_a`, `matched_b`) are new. No `hat_version` column means version 1. A page keeps version 1 (counted as `hat_v1_kept`; `rehat` counts `no_alto`) when `texts.json` is unreadable, or its LoC text is empty or `none` and `loc.xml` is missing, empty or unparseable.

The `usnm_quality` database ([scripts/onprem/quality/README.md](../../../scripts/onprem/quality/README.md)) keeps the same split: `unit_pages` (`hat_version`, 1 for older rows, `_v1` values, `hat_pairs`), `reader_scores` (`est_cer_v1`, `est_cer_raw_v1`, `matched`), `page_summary` and `page_quality_joined` (`hat_version`). `build-quality-assessment-db.py` reports only the newest version present; older rows keep the unit's estimate in `est_cer_v1` with hat columns cleared, so no mean blends versions. The crop-level hat stays on `cer_sym`, since a crop box is in line order for every reader.

## Calibration

`usnm-boinc quality calibrate` runs the three-cornered hat per gold crop over `--crops <crops.jsonl>` (gold crops with human transcriptions), two or more `--hyp ENGINE=FILE.jsonl` hypothesis files and optional `--readings <readings.jsonl>`, writing `calibration.csv`, `calibration.md` and `by-crop.csv` to `--out`: per reader, true CER, estimated CER, bias, MAE, Pearson and Spearman, without (`three-ocr`) and with (`three-ocr+vlm`) the vision-language reader. UNVERIFIED on the full multi-engine gold crop campaign: no calibration report is in the repository.

### Unconfirmed

- How far the hat's estimates are biased low on real pages, where readers fail on the same smudges (the calibration's question).
- How far the version 2 hat's `local_cer_sym` underestimates a badly garbled reader whose worst chunks don't place (they show as low `matched`, not error). No page-level transcription of a three-reader page exists yet.
- How far the crop-level estimate agrees with the page-level one.
- Time and memory per unit on volunteers' hosts, and so whether the `create_work` bounds fit ([Registering and releasing](#registering-and-releasing)).
- The Dice threshold for `usnm_qwen`, to be measured from the logged pairs.

## App versions and plan classes

Plan classes are in `boinc/project/plan_class_spec.xml`; the platforms each can be released for are `NATIVE_PLAN_CLASSES` in `boinc/scripts/install.py`. Since 2 November 2026 the live `usnm_qwen` versions are GPU only (`nvidia_vulkan_8b`, `nvidia_vulkan_6gb_8b`, `amd_vulkan_8b`, `intel_vulkan_8b`, `apple_gpu_8b`), and on Windows only `amd_vulkan_8b` has a live version. "All five" platforms: `x86_64-pc-linux-gnu`, `aarch64-unknown-linux-gnu`, `arm64-apple-darwin`, `x86_64-apple-darwin`, `windows_x86_64`. Every 8B class runs `Q4_K_M`, every 2B class `Q8_0`. Qwen GPU classes take 2 host CPUs. Every Qwen GPU class except `apple_gpu_8b` ships closed (`min_wu_id` 2, `max_wu_id` 1) until an operator opens it ([Native 8B dispatch windows](#native-8b-dispatch-windows)).

| App            | Kind      | Plan class              | Platforms                       | Requirements                                                                                                                                                              |
| -------------- | --------- | ----------------------- | ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `usnm_quality` | native    | _(none)_                | all five                        | `crates/usnm-quality-app`, 1 CPU                                                                                                                                          |
| `usnm_quality` | container | `docker`                | all five                        | `boinc/apps/job.toml`: 1 CPU, 1.5 GiB, no swap; BOINC 8.2.11                                                                                                              |
| `usnm_qwen`    | native    | `apple_gpu_8b`          | `arm64-apple-darwin`            | `<gpu_type>apple</gpu_type>`, at least 8,192 MB GPU (unified) RAM                                                                                                         |
| `usnm_qwen`    | native    | `nvidia_vulkan_8b`      | Linux x86-64                    | BOINC's CUDA detection, the pinned Vulkan runtime, at least 7,000 MB GPU RAM and 7,000 MiB free                                                                           |
| `usnm_qwen`    | native    | `nvidia_vulkan_6gb_8b`  | Linux x86-64                    | at least 5,500 MB GPU RAM and 5,120 MiB free, 24 GPU layers, projector on the CPU, BOINC 8.2.9                                                                            |
| `usnm_qwen`    | native    | `intel_vulkan_8b`       | Linux x86-64, Windows           | `<gpu_type>intel</gpu_type><opencl/>`, Vulkan, at least 7,000 MB GPU RAM and 7,000 MiB free; an integrated GPU uses about 8.6 GB of host RAM ([Intel GPUs](intel-gpu.md)) |
| `usnm_qwen`    | native    | `amd_vulkan_8b`         | Linux x86-64, Windows           | AMD/ATI OpenCL detection, Vulkan, at least 7,000 MB and 7,000 MiB free ([AMD GPUs](amd-gpu.md))                                                                           |
| `usnm_qwen`    | native    | `nvidia_vulkan_2b`      | Linux x86-64                    | at least 4,500 MB GPU RAM and 4,096 MiB free (estimates), BOINC 8.2.9, 2B fully on the GPU; no signed version yet                                                         |
| `usnm_qwen`    | native    | `nvidia_cuda_8b`        | Linux x86-64 and ARM64, Windows | no live version since 2 November 2026; CUDA 12.4, compute capability 6.0, at least 7,000 MB GPU RAM                                                                       |
| `usnm_qwen`    | native    | `nvidia_cuda_2b`        | Linux x86-64 and ARM64, Windows | no live version since 2 November 2026; CUDA 12.4, compute capability 5.0, at least 3,000 MB GPU RAM                                                                       |
| `usnm_qwen`    | native    | `mt`                    | all five                        | retired on 2 November 2026 (CPU, `min_ncpus` 4, `max_threads` 8, 2B)                                                                                                      |
| `usnm_qwen`    | container | `docker_mt_2b`          | Linux x86-64                    | retired on 2 November 2026 (2B CPU image, `boinc/apps/qwen/job-cpu.toml`)                                                                                                 |
| `usnm_qwen`    | container | `docker_nvidia_cuda_8b` | Linux x86-64                    | retired on 2 November 2026 (8B CUDA image, `boinc/apps/qwen/job-gpu.toml`)                                                                                                |

Native 8B classes take batch-8 workunits (`qwen submit --target native-8b`), native 2B classes batch 2 (`native-2b`); the retired container classes took batches 12 and 18 (`Target::batch` in `crates/usnm-boinc/src/qwen/submit.rs`).

### Native distribution

Stock BOINC restarts a task that exits `0` without `boinc_finish_called` in its slot and aborts it after 100 such exits with `ERR_TOO_MANY_EXITS` (`client/app_control.cpp`). The Rust binaries (`crates/usnm-quality-app`, `crates/usnm-qwen-app`) meet that contract without the C++ API or `samples/wrapper`:

- Page and control files carry `<copy_file/>`, so the slot holds real files. Native Qwen's sticky GGUFs are the exception: slot `<soft_link>` aliases to the shared project cache.
- The binary writes `fraction_done` (`0o644`) while running.
- After flushing all outputs and setting `fraction_done` to `1.000000\n`, it atomically writes `boinc_finish_called` (`0\n`) and exits `0`.

`usnm_quality` depends only on `usnm-ocr-quality` and the contract types in `usnm-ocr-contract` (no `tokio`, `reqwest`, `arrow`/`parquet`, `rustix`, `usnm-store`). `.github/workflows/boinc-quality-native.yml` builds it and the `usnm_qwen` driver `--locked` for `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `aarch64-apple-darwin`, `x86_64-apple-darwin` and `x86_64-pc-windows-msvc`, runs the determinism and loopback tests on each, and uploads `boinc-native-<platform>` artifacts holding `usnm_quality_<platform>[.exe]`, `usnm_qwen_<platform>[.exe]` and `SHA256SUMS`.

The `usnm_qwen` driver starts `llama-server` on `127.0.0.1` at an ephemeral port (`--ctx-size 4096 --temp 0 --seed 42 --parallel 1 --no-webui`; `-ngl 99` on a GPU, 24 for the 6 GB profile, 0 on a CPU), polls `/health` and posts each crop as a PNG data URL to `/v1/chat/completions`. `boinc/apps/qwen/llama.lock` pins the llama.cpp tag (`b8799`) and per-platform archive URLs and SHA-256s (`macos-arm64`, `macos-x64`, `win-cuda-12.4-x64` and its runtime, `win-vulkan-x64`, `ubuntu-x64`, `ubuntu-vulkan-x64`, `ubuntu-arm64`), verified before staging and signed with the version.

Weights are not in app versions. Model and projector (8B: `Qwen3VL-8B-Instruct-Q4_K_M.gguf` 5,027,784,800 bytes + `mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf` 752,289,728; 2B: `Qwen3-VL-2B-Instruct-Q8_0.gguf` 1,834,427,296 + `mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf` 445,053,056) are staged in `download/` as `usnm_qwen_model_<sha256>` with Apache-2.0 `LICENSE` and `NOTICE`. The templates `boinc/apps/usnm_qwen_8b_in` and `usnm_qwen_2b_in` mark them `<sticky/>` (a host downloads them once) and `<no_delete/>` (`file_deleter` never unlinks them). The on-prem server redirects the four model files to R2 at `models.usnewsmap.com` ([Qwen model files on R2](../server/onprem.md#qwen-model-files-on-r2)).

GPU workers set `CUDA_CACHE_PATH` and `MESA_SHADER_CACHE_DIR` to `/var/tmp/usnm-gpu-cache-<uid>` (mode 0700), falling back to the temp directory and then the task's scratch or slot directory if that path is not a private directory of the worker's user, so native hosts keep compiled kernels between workunits.

### Container distribution (retired)

The container Qwen classes `docker_mt_2b` and `docker_nvidia_cuda_8b` were retired on 2 November 2026; this is what their results were made with. `boinc/apps/qwen/Containerfile` builds on `ghcr.io/ggml-org/llama.cpp:server-cuda@sha256:b0baccbe2e00b5d74e63f85f9f023c93d61d0f329475269e1e7002e87eb875ea` (llama.cpp build 11610), with separate payloads: `QWEN_MODEL=2b` holds only the 2B Q8_0 pair, `QWEN_MODEL=8b` only the 8B Q4_K_M pair, and `USNM_QWEN_MODEL` and the image label name it. `.github/workflows/boinc-qwen.yml` builds and checks both; a manual run on `main` publishes `ghcr.io/tgoodyear/usnm-boinc-worker:qwen-2b-<commit>` and `:qwen-8b-<commit>`, each a different digest. Container `usnm_quality` (`docker`) runs `usnm-boinc` in the worker image (`boinc/apps/Containerfile`) under `boinc/apps/job.toml`.

`usnm-boinc qwen submit` and the bridge's `qwen-submit` step still default to `--target container` (batch 12), which no live version takes; pass `--target native-8b` (`QWEN_TARGET=native-8b`).

### NVIDIA userspace in an LXC

Native apps need no Docker. On a Debian or Ubuntu LXC whose host already passes the NVIDIA character devices through, run inside the LXC (not on the Proxmox host):

```sh
python3 boinc/scripts/setup_nvidia_lxc.py                     # show the userspace package plan
sudo python3 boinc/scripts/setup_nvidia_lxc.py --apply        # install it
sudo python3 boinc/scripts/setup_nvidia_lxc.py --check-only   # diagnose without APT changes (also .run installs)
```

It matches userspace packages to the host kernel driver in `/proc/driver/nvidia/version` down to the patch version (Ubuntu's branch-suffixed packages, NVIDIA's unversioned Ubuntu 590+ packages, Debian's separate Vulkan/EGL ICD packages), installs the Vulkan loader, diagnostics and runtime libraries, pins them, and verifies `nvidia-smi`, `libcuda.so.1` and a hardware Vulkan device as `boinc` (`--boinc-user NAME` for another non-root account). `nvidia-smi`'s CUDA version is driver compatibility, not an installed toolkit; `nvcc` is not needed.

It never installs kernel drivers, creates devices or changes their permissions, adds repositories or changes BOINC preferences; it refuses to mix runfile and package installs and stops if no exact package set exists. Use the distribution's packages or the [official NVIDIA repository](https://docs.nvidia.com/datacenter/tesla/driver-installation-guide/ubuntu.html#network-repository-enablement-amd64). After a host driver update rerun it (it updates `/etc/apt/preferences.d/usnm-nvidia-lxc`), then restart `boinc-client` at a checkpoint. Passing proves userspace readiness only; signed app publication, client-version admission, model memory limits and successful GPU results are separate.

### Native 8B dispatch windows

`boinc/scripts/native-qwen-cohort.php` opens and closes the classes: bounded windows, the shared `shared_8b` window, general admission, tie-breakers, fresh pairs and stops. [Intel and NVIDIA GPUs](intel-gpu.md#admission) has the commands and each class's current admission, [AMD GPUs](amd-gpu.md) the AMD route, and [Linux NVIDIA 2B](app-release.md#linux-nvidia-2b-nvidia_vulkan_2b) the 2B class. Change windows with the helper, not a server restart: startup keeps existing Qwen admission while refreshing image-owned batch, resource and client requirements. A no-work reply mentioning 15,360 MB comes from the retired training class (`docker_nvidia_train`); the native 8B classes need 7,000 MB.

The 6 GB profile (`nvidia_vulkan_6gb_8b`, #1234) is the only 8B class that admits BOINC 8.2.9. One [8.2.9 run](https://github.com/tgoodyear/usnewsmap.com/issues/1234#issuecomment-6537940240) read all 12 crops at 3,725 MiB peak GPU memory; a real 6 GB card is unproven. Before admitting a host, publish a signed native version with the pinned Vulkan bundle, verify the volunteer's CUDA and Vulkan access and measure a successful output. Then qualify one workunit with `--plan-class nvidia_vulkan_6gb_8b --workunit ID --fresh-pair --admit-until UNIX` and its `--expected-policy-sha256`, and arrange the peer in a reviewed shared window, since opening only the low-memory class does not guarantee one. Schedule the class's `--stop --apply` for the deadline. Do not enable general admission or alter quorum or result state to force a pass.

### Registering and releasing

Registration, staging, signing and publication are in [Releasing a BOINC app version](app-release.md) (`boinc/scripts/install.py --kind native --binary`, `boinc/scripts/boinc-app-release.sh`) and [Register the apps](../server/bridge-steps.md#register-the-apps).

`create_work` bounds, per page (a bundle multiplies the FLOP figures by its pages):

| App            | `rsc_fpops_est` | `rsc_fpops_bound` | `rsc_memory_bound` | `rsc_disk_bound`                                                                 |
| -------------- | --------------- | ----------------- | ------------------ | -------------------------------------------------------------------------------- |
| `usnm_quality` | 1e12            | 1e14              | 1.5 GiB            | 2 GiB                                                                            |
| `usnm_qwen`    | 1e13            | 1e16              | 6 GiB              | by target ([disk admission](#qwen-disk-admission-and-cold-cache-acceptance-559)) |

Sources: `FPOPS_EST`, `FPOPS_BOUND`, `MEMORY_BOUND` in `crates/usnm-boinc/src/quality/submit.rs` and `qwen/submit.rs`; both note the estimates are unmeasured on volunteers' hosts.

## Qwen disk admission and cold-cache acceptance (#559)

[`boinc/apps/qwen/disk-policy.json`](../../../boinc/apps/qwen/disk-policy.json) is the versioned policy for submission, release checks, host preflight and tests. GiB = 1,073,741,824 bytes. The scheduler always advertises the cold bound and rejects `rsc_disk_bound > disk_available` (`sched/sched_check.cpp::check_disk`), subtracting the bound per dispatched unit, so BOINC disk preferences and quota must allow it.

| Target (`qwen submit --target`) | Payload                         | Cold `rsc_disk_bound`, bytes (GiB) | Warm incremental disk envelope                |
| ------------------------------- | ------------------------------- | ---------------------------------- | --------------------------------------------- |
| `native-8b`                     | sticky 8B model + native bundle | 19,327,352,832 (18)                | up to 4 GiB: app bundle slot copy + workspace |
| `native-2b`                     | sticky 2B model + native bundle | 12,884,901,888 (12)                | up to 4 GiB: app bundle slot copy + workspace |
| `container` (retired class)     | 2B CPU image                    | 34,359,738,368 (32)                | 2 GiB slot/workspace; no new image layers     |
| `container-8b` (retired class)  | 8B GPU image                    | 51,539,607,552 (48)                | 2 GiB slot/workspace; no new image layers     |

Warm figures assume the exact payload is resident, are incremental and exclude a 2 GiB free-space reserve; warm hosts still need the cold bound free. Nothing prunes volunteers' caches, so extra retained revisions use more disk. Native cold admission covers one model set, two copies of a native app bundle bounded at 2 GiB (cache plus slot; `install.py` rejects larger), 2 GiB workspace and 2 GiB reserve. Native templates soft-link GGUFs from the shared project cache instead of copying them into slots; revisit the bounds if that changes. These are conservative envelopes, not measurements.

Container admission covered the pinned CUDA base's compressed layers (2,592,304,104 bytes, manifest `sha256:cb7f872937c65952258e5670d8c1da7bae7394aef50c1c90a8bb251bde46c32e`) plus the GGUF bytes and 512 MiB overhead, two unpacked copies, 2 GiB workspace and 2 GiB reserve. `boinc/scripts/qwen_cold_cache.py` measured it on a disposable runner in [run 38787485639](https://github.com/tgoodyear/usnewsmap.com/actions/runs/38787485639) ([evidence](../../../boinc/apps/qwen/measurements/ci-38787485639.json); Docker 28.0.4, overlay2):

| Observed bytes                   | 2B CPU         | 8B GPU payload |
| -------------------------------- | -------------- | -------------- |
| Local gzip save/load stream      | 4,861,064,470  | 8,286,566,549  |
| Logical unpacked image           | 6,670,088,145  | 10,170,682,321 |
| Cold filesystem increase         | 6,725,922,816  | 10,226,704,384 |
| Sampled peak filesystem increase | 20,098,748,416 | 30,602,686,464 |

Sampled peaks plus 2 GiB workspace and 2 GiB reserve fit the 32 and 48 GiB bounds. Peaks are sampled at 100 ms and include other runner activity. Cold transfer and extraction are startup overhead, so the ~513-second container acceptance must not calibrate per-crop inference or FLOPS. The #559 runtime acceptance of 18 October 2026 on volunteer hosts passed for the CPU image (cold and warm, and a rootless-Podman host rejected for low disk) and for the CUDA image in a private loopback fixture, not under production quorum.

## Diagnostics

Both runners write `diag.txt`: from `init_data.xml` only client version, app version, plan class, GPU type and device number, CPU count and memory bound (never the account key or host name); cgroup limits and memory. `usnm_qwen` adds GPU devices, driver details, the chosen model and llama-server's lines; `usnm_quality` adds the job's readers and timings. A unit that cannot run (checksum mismatch, missing or different engine build) returns `unit_error` with the reason and no record.

## CLI subcommands

### usnm-boinc quality

- `sample`: stratified `sample.jsonl` from the prefetch manifest (`--manifest`, `--out`, `--pages` 500, `--min-per-stratum` 2, `--seed`, optional `--title-languages` JSON of LCCN to MARC codes, `--with-as` doc ids with American Stories text, `--pages-file` restricting the draw to listed doc ids).
- `function-words`: `<out-dir>/<lang>.txt`, the top `--top` (100) alphabetic words of four or more letters from the embedded wordfreq list (`usnm_core::lang::wordlist`) per repeatable `--lang` (`--out-dir`).
- `texts-export`: reads American Stories Parquet (`curated/american-stories/pages/<lccn>/<year>-<nnn>.parquet`, from `--american-stories-url` when given, else `--curated-url`) and the newest `ocr-local` overlay per page from `--curated-url`, for `--doc-id` pages (repeatable) or a `--sample`, writing `<out-dir>/<doc_id>.json` (`texts.json` minus LoC's text, which `stage` fills from `loc.xml`) and `<out-dir>/<doc_id>.ours.xml` when our ALTO exists.
- `stage`: units under `<root>/units/<doc_id>/` (or `<root>/pages/<doc_id>/` packed into `<root>/units/bundle-<idx>-<first_doc_id>/` when `--bundle-size` is above 1) plus `<root>/places.txt`. Flags: repeatable `--page group:lang:doc_id:batch[+batch...]` or `--sample` (`--group` default `sample`), `--root`, `--engine-image`, `--texts-dir`, `--scans-dir`, `--places` (`.txt` or catalog `places.json`, deduplicated), `--function-words-dir` (alias `--function-words`), `--readers` (`loc,as`), `--surya-texts` (`surya collect` export, joins as `ours-surya` when named, #968), `--crop-seed` 522, `--k-crops` 12, `--gap-secs` (default 5, at least 3), `--max-pages` 20, `--bundle-size` (1 to 500, default 1), `--batch-cache`, `--scan-archive`. Scans come from `<doc_id>.jpg`/`.xml` in `--scans-dir` with no network; else `crates/usnm-boinc/src/scans.rs` tries the scan archive when `--scan-archive` is set (`keep/<batch>/<lccn>/<date>/<edition>/<seq>.jpg`, checked against its `sha256`) and then `tile.loc.gov` through the page's batch (`--batch-cache` caches batch manifest and issue METS), one request at a time `--gap-secs` apart, inside the host pacer when `USNM_LOC_GLOBAL_PACER` names the scan fetcher's state file ([LoC budgets](#ocrs-prefetch-manifests-1055)). It never uses `www.loc.gov`'s item API. It parses `loc.xml` for page dimensions and LoC's text, merges `<texts-dir>/<doc_id>.json` (normalizing reader `source` and region `"box"`/`"rect"`, copying `.ours.xml`) and writes `page.jpg`, `loc.xml`, `texts.json`, `quality.json`.
- `submit`: `<root>/units/` to `create_work` (`--root`, `--project`, `--max-units` 100, `--members` an ordered JSON array of doc ids), copying `places.txt` once to `download/usnm_quality_places_<sha256>` and each unit's inputs into the download hierarchy, with write-ahead `submitting`/`submitted` markers.
- `local-unit`: a unit from local files (`--page`, `--alto`, `--texts`, `--places`, `--function-words`, `--dir`, `--engine-image`, `--crop-seed` 522, `--k-crops` 12) for `usnm-boinc work --dir <dir>`.
- `check`: `--init_script` (`--files <result.json> <confusions.json> <diag.txt>`, optional `--keep <dir>`).
- `compare`: `--compare_script` (`--files <r1...> --files2 <r2...>`, optional `--log <pairs.jsonl>`).
- `collect`: joins canonical `usnm_quality` (`--root`, `--spool`), `usnm_qwen` (`--qwen-root`, `--qwen-spool`, both required; `--qwen-crop-states`, `--qwen-pages`) and optional `usnm_surya` crop results (`--surya-root`, `--surya-spool`; reader `surya`, #968) for `--campaign`. Writes `pages.parquet`, `crops.parquet`, `summary.md` to `--out-dir`, or under `audit/quality/<campaign>/` at `--curated-url`. `--cache` reuses content-addressed per-unit rows, `--max-dirty-units` (8) bounds recomputation per pass, and `--inventory-only` (with `--inventory-unit`) prints dependency fingerprints without computing rows.
  - `--qwen-crop-states`: only vouched crops join; others keep rows without the reading, with `vlm_crop_state` `strict-v1`, `agree`, `looped`, `no_text` or `unjudged`. Records are part of the row-cache keys.
  - `--qwen-pages` (#1435, `/project/usnmocr/qwen/pages`): a bundle with no canonical result joins each page `page-ledger` calls `accepted` (`vlm_crop_state` `page-strict-v1`), using the first agreeing record by file name and the side whose result name sorts first, which must have finished and name its device (the two names must differ). The bundle's other pages keep rows without a reading. A canonical result, once present, wins. Only bundles' cache keys change.
- `rehat`: recomputes page hat versions 1 and 2 from a staging root's texts (`--root`, `--out` JSON lines, `--min-readers` 3, `--threads` 1) with no results or scans, to compare the versions on a still-staged campaign.
- `rescore` (#1079): reruns the text part of single-page units (`slot::text_analysis`, what the unit runs minus image and ALTO) from corrected LoC texts, with no volunteer work. Flags: `--campaign`, `--root` (staging root), `--spool` (canonical results), `--loc-texts` (JSON lines `{"doc_id", "text"}` replacing `loc-hist`), `--places`, `--out-dir` (new, outside the root), `--threads` 1. Keeps image and layout features; writes `<out-dir>/units/<doc_id>/{quality.json, texts.json}` (which `rehat` reads) plus `pages.parquet` and `summary.md` with hat version 2. Bundles, units without results and pages without LoC text are counted and skipped.
- `calibrate`: see [Calibration](#calibration).
- `estimate`: offline over a page directory (`--dir`, `--out`, `--places`, `--function-words`, `--crop-seed` 522, `--k-crops` 12, repeatable `--reader NAME=PATTERN` for `<dir>/sample/{images,alto,loctext}`); with `readings.json` it verifies the originating `qwen.json` (or `readings/<page>.qwen.json`) and joins its crops. Writes per-page `result.json`, `confusions.json`, `crops.jsonl` and aggregate `pages.jsonl`, `crops.jsonl`.
- `summary`: reads `--root`'s `units/`, `--seen` (`<project>/quality/seen`) and `--pairs` (`<project>/quality/pairs.jsonl`) and prints a Markdown table.

`stage` stops with `HOLD` on an exported `loc` or `loc-hist` text equal to an ocr-local reason label (`bulk`, `user`, `priority`, `missing`, `empty`, `short`, `garbled`, `mixed`). Merged `ocr-local/pages` schema v2 calls the queue reason `ocr_reason`; `loc_text` and `reason` are deprecated aliases, never LoC's text, and readers validate the schema version and reject conflicting labels, historical parts included. Export LoC's text from the curated LoC page's `text`. v1-wave6's 10,000 pages were staged with `loc-hist` as `bulk` from the legacy column; `rescore` (#1079) is the repair.

### usnm-boinc qwen

- `stage`: cuts the same crops from each staged `usnm_quality` unit in `--quality-root` (its `page.jpg`, `loc.xml`, `crop_seed`, `k_crops`) with no LoC request, writing `<root>/units/<doc_id>/{page.jpg, qwen.json}`, or `<root>/pages/<doc_id>/` packed into `<root>/units/bundle-<idx>-<first_doc_id>/` when `--bundle-size` is above 1. Flags: `--root`, `--members`, `--engine-image`, `--engine-llama`, `--engine-weights`, `--gpu-timeout-secs` (14400), `--cpu-timeout-secs` (7200), `--max-tokens-per-1000px2` (4), `--prompt-version` (1), `--max-pages` (20), `--bundle-size` (1 to 32, default 1; `16` in `bridge-job.sh`). Bundles:
  - `membership.json`, written before `qwen.json` and never changed, lists per page its page unit, job id, `qwen.json` sha256, page sha256 and slice of the bundled `page.jpg`.
  - A run holds `<root>/.bundle-lock` from staging to its last bundle and claims each bundle directory, so a concurrent run fails.
  - Deadlines are the per-page deadline times pages, at most 14,400 s; `submit` scales `rsc_fpops_est` and `rsc_fpops_bound` by pages.
  - A bundle closes before `qwen.json` would pass 512 KiB (the read limit) or the longest possible `readings.json` would pass 2 MiB (the upload limit, at about 16 KiB a crop), so about 125 crops: 10 pages of 12.
  - A page too large alone gets a `too_large` marker and is not published; its job stays beside the marker as its disposition (`rejected-qwen.json` if its own `qwen.json` is too large, the page unit's `qwen.json` under `pages/` if only its one-page bundle is).
  - Different engine, deadline or token settings go in separate bundles. A bundled root refuses `--bundle-size 1`.
  - Workers check every page before loading the model, then decode one page at a time, so a bundle needs only its largest page's memory.
- `submit`: hands `<root>/units/` to `create_work` (`--root`, `--project`, `--max-units` 20, `--members`, `--target` `native-8b`, `native-2b`, or the retired `container` (default) and `container-8b`, `--model-dir`) with write-ahead `submitting`/`submitted` markers.
- `local-unit`: a unit from `--image` and `--alto` (or `--synthetic`, a built-in two-line block-capitals page) into `--dir` (`--engine-image`, `--engine-llama` default `USNM_QWEN_LLAMA_IMAGE`, `--engine-weights` default `USNM_QWEN_WEIGHTS`, `--crop-seed` 522, `--k-crops` 12, `--timeout-secs` 1800, `--max-tokens-per-1000px2` 4).
- `probe`: runs `/app/llama-server --version` and checks each model and projector in `usnm_boinc::qwen::default_models()` exists under `/opt/qwen/models`, printing sizes without loading (container image).
- `check`: `--init_script` (`--files <readings.json> <result.json> <diag.txt>`, optional `--keep <dir>`).
- `compare`: `--compare_script` (`--files <r1...> --files2 <r2...>`, `--min-dice` 0.5, `--log <pairs.jsonl>`, `--rule strict-v1|crop-consensus-v2`, `--crop-states <dir>` required with `crop-consensus-v2`, `--pages <dir>`).
- `grounded`: measures `crop-consensus-v2`'s floor offline against human transcriptions (`--refs` JSON Lines `{id, source, lang, ref}`, repeatable `--reader NAME=PATH` and `--pair A:B`, `--legibility FILE`, `--good-cer` 0.10, `--bad-cer` 0.50, `--floor` (default the rule's 0.7), `--seed` 1434, `--out`, `--crops`): crops under the floor, crop states, treatment of pairs where both or one reading match, and a shuffled control. CER leaves Hebrew points out of both sides.
- `replay`: measures validator rules offline on retained results (`--seen <project>/qwen/seen`, `--model` `qwen3-vl-8b-q4km`, `--seed` 1420, `--controls` 2000, `--out <report.json>`): every rule on every replica pair, pairs that must fail, a third replica's check of what a pair vouched for, and each crop metric's separation of true from shuffled pairs.
- `page-ledger`: `--root`, `--pages`; one JSON line per bundle page with its job id and checksum from `membership.json` and a status: `accepted` (some pair agreed), `disagreed` (every pair so far differed), `pending` (no pair yet) or `membership_invalid` (`membership.json` missing, mismatched, or naming a page unit whose `qwen.json` changed; no page kept). Records not naming that job and page, or not holding exactly the page's staged crops once on each side, are ignored. `accepted` means two hosts agree, not that the reading is right.
- `summary`: reads `--root`'s `units/`, `--seen` and `--pairs` and prints a Markdown table.
- `candidates --job <qwen.json> --page <page.jpg> --candidate <seen/result-a> <seen/result-b> ... --out <report.json>`: offline audit of up to 16 retained results of one single-page job. After verifying input hash, job, engine and prompt identity, model membership and crop IDs, it reports per candidate the text, manifest, host metadata and hashes, with per-crop pixel features, pairwise CER/WER/Dice, LoC comparisons, damage counts, uncertainty and repetition markers and token-budget saturation. Missing and empty readings stay distinct. Pair metrics use NFC, joined line-end hyphens and collapsed whitespace; `validator_dice` uses the validator's unmodified word bags. Unsupported language models and zero denominators stay null. Output is noncanonical: not an input to `quality collect`, and it changes neither validation nor admission.
- `benchmark-stage` and `benchmark-collect`: public gold crops for the [transcription pilot](../../notes/2026-10-16-boinc-transcription.md).

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
# A qwen unit from a page and its ALTO (the crops are cut here).
usnm-boinc qwen local-unit --image page.jpg --alto loc.xml --dir q --engine-llama sha256:... --engine-weights sha256:...
```

### Bridge steps

A quality campaign lives under `$SHARE_DIR/quality/<QUALITY_CAMPAIGN>` and a Qwen campaign under `$SHARE_DIR/qwen/<QWEN_CAMPAIGN>` (names 1 to 41 characters of `[a-z0-9-]`), for `boinc/images/server/bridge-job.sh` and the hand-off steps (`boinc/images/bridge/handoff-job.sh`). Prod runs them on the on-prem host: bridge steps since 21 October 2026, hand-off steps since the Azure hand-off job was retired on 6 November 2026 ([BOINC server on-prem](../server/onprem.md)). Each step is `boinc/scripts/onprem-step.sh --env prod --target bridge --step <step> --var NAME=VALUE ... --wait-mins N`. The on-prem feeder below drives staging and submission for the live waves; these steps are for one-off campaigns.

1. Export page texts (`handoff-job.sh`, `HANDOFF_STEP=texts-export`). `boinc/scripts/texts-export-onprem.sh` runs it on a host with `curated` at `/srv/curated`; only `ocr-local` overlays and their ALTO come from `CURATED_URL` (logged as `curated_bytes`). It runs `usnm-boinc quality texts-export` for `--doc-id` pages or a `--sample` under `quality-samples/` in `HANDOFF_URL` and uploads `<doc_id>.json` (and `.ours.xml`) under `texts/`:

   ```sh
   boinc/scripts/texts-export-onprem.sh --settings /etc/usnm/texts-export.env \
     --usnm-boinc /usr/local/bin/usnm-boinc --american-stories-root /srv/curated --sample v1.jsonl
   ```

   `--settings` holds `HANDOFF_URL`, `CURATED_URL`, `COSMOS_ENDPOINT`, `REFERENCE_URL`, `OUT_URL` and the host's Arc identity endpoints; that identity needs read on curated's `ocr-local/` and the writer role on the hand-off container. Build `usnm-boinc` with `cargo build --release -p usnm-boinc --locked`.

2. `quality-stage`. When `HANDOFF_URL` is set, pulls `texts/` into the campaign's `texts/` and, for a relative `QUALITY_SAMPLE`, `quality-samples/` into `samples/`; writes `function-words/` for `QUALITY_LANGS` (ISO 639-3, comma-separated), then runs `quality stage`. Variables: `QUALITY_PAGES` (`group:lang:doc_id:batch[+batch...],...`) or `QUALITY_SAMPLE` (default `<campaign>/sample.jsonl`; `QUALITY_GROUP` default `sample`), `QUALITY_READERS` (`loc,as,ours-tesseract`), `QUALITY_PLACES` (`<campaign>/places.txt`), `QUALITY_ENGINE_IMAGE` (`sha256:<worker-digest>`), `QUALITY_MAX` (100), `QUALITY_BUNDLE_SIZE` (300), `QUALITY_SCANS_DIR`. Scans come from `SCAN_ARCHIVE_URL` (`--scan-archive`) first, then `tile.loc.gov` (`--batch-cache`).

   ```sh
   boinc/scripts/onprem-step.sh --env prod --target bridge --step quality-stage \
     --var QUALITY_CAMPAIGN=v1-wave5 --var HANDOFF_URL=<hand-off container URL> \
     --var QUALITY_SAMPLE=v1-wave5.jsonl --var QUALITY_GROUP=v1-wave5 \
     --var QUALITY_ENGINE_IMAGE=sha256:<worker-digest> \
     --var QUALITY_LANGS=eng --var QUALITY_MAX=5000 --wait-mins 45
   ```

3. `qwen-stage`. Cuts the crops from `$SHARE_DIR/quality/<QUALITY_CAMPAIGN>` into `$SHARE_DIR/qwen/<QWEN_CAMPAIGN>` with no LoC fetch (`QWEN_ENGINE_IMAGE`, `QWEN_ENGINE_LLAMA`, `QWEN_ENGINE_WEIGHTS`, `QWEN_MAX` 20, `QWEN_BUNDLE_SIZE` 16). UNVERIFIED for native targets: the live native feed stages through `qwen_submit.py` instead ([Sustained non-English Qwen crop feed](#sustained-non-english-qwen-crop-feed)).

   A crop whose source text passes 8192 UTF-8 bytes is never truncated: the page gets the `too_large` terminal marker naming each oversized crop, with the candidate kept as `rejected-qwen.json` beside its `loc.xml`, `quality.json`, `page.jpg` and `sample.json`; no `qwen.json` is published, and other pages continue within `--max-pages`. Other schema errors fail closed.

4. `quality-submit` (`QUALITY_MAX` 500) and `qwen-submit` (`QWEN_MAX` 20, `QWEN_TARGET`, which defaults to the retired `container`). Two results per workunit. Both invalidate `$PROJ/html/cache/f2/server_status.php_job_status` so the [server status page](https://boinc.usnewsmap.com/server_status.php) shows the new queue.

5. `quality-summary` and `qwen-summary` read `<project>/quality/{seen,pairs.jsonl}` and `<project>/qwen/{seen,pairs.jsonl}` and write `<campaign>/summary.md`.

6. `quality-collect` joins assimilated `usnm_quality` (`QUALITY_SPOOL`), optional `usnm_qwen` (`QWEN_CAMPAIGN`, `QWEN_SPOOL`, `QWEN_CROP_STATES`) and optional `usnm_surya` crop (`SURYA_CAMPAIGN`, `SURYA_SPOOL`, #968) results into `<campaign>/out/{pages.parquet,crops.parquet,summary.md}`, or uploads to `QUALITY_CURATED_URL` under `audit/quality/<QUALITY_CAMPAIGN>/`:

   ```sh
   boinc/scripts/onprem-step.sh --env prod --target bridge --step quality-collect \
     --var QUALITY_CAMPAIGN=v1-probe --var QUALITY_SPOOL=/project/usnmocr/results/0 \
     --var QWEN_CAMPAIGN=v1-probe --var QWEN_SPOOL=/project/usnmocr/results/8 --wait-mins 15
   ```

`boinc/scripts/boinc-quality-campaign.py` split large campaigns into shard archives for Azure Container Apps executions; it needs the Azure server and has no use since prod moved on-prem.

### On-prem feeder (v1-wave10 on)

On-prem, quality units are fed from the feeder container's Proxmox host with no Azure call and no LoC request (#962; the Azure wave runner was `boinc/scripts/quality_corpus_runner.py`).

1. Feeder (`boinc/scripts/onprem/quality-feed/quality_feed.py`, `usnm-quality-feed.service`, in the `usnm-batch` container). `select` picks unstaged pages from the scan server's `/index/full.jsonl`, worst damage first. `run` reads each scan (checked against the index size and sha256), makes the worker's grey q75 `page.jpg`, writes an empty ALTO (the scan fetcher is the only LoC client), and takes LoC's text, American Stories and `ocr-local` from the read-only `/srv/curated` mount. It writes 500-page chunks (`--chunk-pages`, at most 500) and waits while `--max-pending` (8 from the installer) are unshipped; a chunk becomes one bundle, or two past the 1 GiB `MAX_BUNDLE_SCAN`. Busy rounds recheck every 15 s, idle ones sleep `--every-secs`.
2. Shipper (`quality-ship.sh`, `usnm-quality-ship.timer`, 3 minutes after the last run, up to `MAX_CHUNKS` 30 `READY` chunks a run). Over SSH host trust it copies a chunk to the server's `/project/quality/<campaign>/incoming/`, runs `quality_receive.py` (files against `SHA256SUMS`, every scan present, so staging never falls back to tile.loc.gov), `usnm-boinc-step quality-stage` with `QUALITY_SCANS_DIR`, then `usnm-boinc quality submit` in the server container. Nothing ships while the server has more than 300 results unsent.
   - Staging holds its own host lock; later steps ignore a unit until its `quality.json` (written last) or `submitted` marker exists. Submit waits up to 30 minutes for the step lock, else exits 75 with the chunk still `READY`. It skips the `quality-submit` bridge step (whose NFS walk held the lock up to 8 minutes): submit sets file modes and the export fixes owners. The next chunk is received while the current one stages, and that receive ends before the current submit takes the step lock. `--after-submit` keeps scans the next chunk also holds. Packing reads only the run's own pages and bundles (`bundle-<n>-<first page>`); a page already staged in none of them makes it read every bundle once.
   - `tank/boinc-bulk` runs with `sync=disabled` since 2 November 2026 (maintainer-approved; a 200-page chunk dropped from 30–45 minutes to about 3, #529). A storage-host crash can lose the last seconds of acknowledged writes; ZFS commits transaction groups in order, so what survives is a consistent earlier state. Nothing half-written is submitted: receive rechecks `SHA256SUMS`, staging rechecks earlier units against `quality.json` hashes and restages mismatches, `quality submit` checks every input before `create_work`, and a unit already `submitting` goes to the uncertain-submit check. Revert on the storage host with `zfs set sync=standard tank/boinc-bulk`.
3. Results go to `usnm_quality` ([scripts/onprem/quality/README.md](../../../scripts/onprem/quality/README.md)); see [Result collection](#result-collection).

```sh
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

`usnm-quality-results.timer` on the preparation host runs `quality_results_partial.py` two minutes after each pass, with up to 15 s jitter. A pass collects one submitted unit under the server's nonblocking bridge lock (each native call 150 s, remote phase 240 s) into immutable `out/{pages.parquet,crops.parquet,summary.md}` plus `coverage.json` (`campaign_complete=false`) under `/project/ops/quality-results/scoped/<campaign>/<unit-key>/snapshots/<snapshot>/`, verifies checksums and job identity, and loads with certificate-authenticated `load.py results`. The host pass lock serializes collection, transfer and loading. One service run makes up to five passes (`--units-per-run`), each taking the bridge lock again with a 5 s pause between passes so a waiting bridge step gets it first; no pass starts after 10 minutes (`--run-budget-secs`). A busy lock after the first pass ends the run quietly; any other failure fails it. `<campaign>/out` is never replaced, rows are never deleted, a pending unit loads zero rows, and the cursor advances only after a verified load, so a failure in between repeats the same upserts. Collection never submits work or alters quorum. Judge coverage from cursor and load receipts, not timer frequency.

Frontier: `--completion-frontier` (on for the ordinary service) spends three of four collections on canonical-present units whose dependency fingerprint has no verified nonempty load and the fourth on a fair visit over all submitted units. `usnm-boinc quality collect --inventory-only --cache ...` computes the fingerprints in a separate scheduling namespace and fails closed past 10,000 entries or on any inventory error; `--inventory-unit` rechecks the selected key after collection. The full inventory took more than 15 minutes for about 2,800 wave10 units (#1655), so it runs outside the step lock at most once an hour (`--inventory-refresh-secs`) under its own budget (`--inventory-budget-secs`, 3,600 s by default and in the service, at most 7,200; `INVENTORY_BUDGET_SECS` in `quality-results.env` overrides it) and writes `inventory.json` beside `frontier.json`. A failed refresh is reported and the last inventory stays in use. A pass chooses from that saved inventory and re-inventories only the chosen unit under the lock; units submitted after the last refresh wait for the next one. A pass with no saved inventory, or whose saved inventory names a unit no longer submitted, fails closed. The selected-unit inventory, collection and recheck share a 220 s budget inside the 240 s remote limit. A collector failure or timeout records scheduling progress first, so one poison result can't pin the priority lane, and its fingerprint stays unacknowledged for a later retry.

- Install the matching collector before the driver; old binaries reject the inventory and cache flags. Without `--completion-frontier` the driver keeps its frozen-prefix cursor. Frontier state is `scoped/<campaign>/frontier.json`; never seed or rewrite acknowledgements (`frontier_ack`) by hand. Zero-row visits stay pending or failed, not success.
- Whole-campaign collectors must not read these partial directories.
- The installer records the preparation container in `/etc/usnm/quality-results.env` (`CTID`, default 215) and enables collection once the loader DSN exists. The six-hour summary timer refreshes ranking views.
- Acceptance, not shown by offline tests: over at least two hours of ordinary arrivals with real competing producers, completion-to-load lag must stay bounded, pending or late-changed units must reach a new verified key, and unchanged jobs must leave the priority lane while fair visits continue. About eight arriving jobs an hour needs at least 10.7 successful scopes an hour. Weaken no producer, budget, timer, quorum, floor or lock to pass.

Native Qwen join: `usnm-qwen-results.{service,timer}` on the preparation host runs complete-snapshot `quality_results.py` with one `--native-campaign NAME[=COLLECTOR]` per root, joining batch-0 QUALITY outputs, `/project/qwen/<campaign>` and batch-8 readings in `/project/usnmocr/results/8`. Set `NATIVE_QWEN_ARGS`, `QUALITY_COLLECTOR` (and if needed `ENGINE_IMAGE`) in `/etc/usnm/quality-results-qwen.env`, never in feeder settings ([One submit binary and one collector](#one-submit-binary-and-one-collector-1449)). List a root only once it exists; a failing root doesn't stop the others and the service fails at the end naming it. Readings pending quorum join on a later pass. It waits up to 60 s for the bridge lock (`--step-lock-wait-secs 60`) and runs only collection under it. Collection is bounded to 20 minutes a root, the service to 100.

Collector binary and row cache: deploy the collector at `/project/ops/quality-results/usnm-boinc` before these units (`QUALITY_COLLECTOR` or `--collector` for another path); do not replace the server's executable or restart the BOINC server container. Row parts are content-addressed (`collected-parts/`, partial scopes under `ops/quality-results/scoped/`), keyed on the collector, staged inputs, canonical outputs and Qwen jobs, so a late reading recomputes the affected rows and Surya changes invalidate all parts. `--max-dirty-units` (default 8) bounds recomputation: a pass with deferred units exits nonzero before publishing or loading, leaving the previous snapshot intact, so a cold cache takes several ticks. Corrupt parts or digest receipts fail closed and are kept. Large staged inputs get SHA-256 receipts under `collected-parts/input-digests/`, reused when device, inode, size, nanosecond mtime and ctime are unchanged; this needs close-to-open coherence and nanosecond ctime (true of the production NFS mount), and other filesystems fall back to hashing. Before relying on digest reuse on a new mount, test nonzero nanosecond ctime, same-size rewrites with restored mtime, atomic replacement and fresh-open visibility on it.

### Sustained non-English Qwen crop feed

A Temporal singleton is the only Qwen prepare and ship driver; `usnm-qwen-feed.timer` and `usnm-qwen-ship.timer` stay disabled. Preparation and shipping are separate activities, so a preparation failure never holds READY work. A campaign-local ship lock and the project mutexes serialize consumers and publication.

Install the helpers from `boinc/scripts/onprem/quality-feed/` at the activity's idle boundary, never alongside a running old shipper:

- `qwen_feed.py`, `quality-ship.sh`, `quality_receive.py` in `/opt/usnm-quality-feed/qwen-throughput/`.
- `scan_quarantine.py` in the preparation container's `/opt/usnm-quality-feed/` before the matching `quality_feed.py` and `ocr_manifest.py`.
- `archive_alto.py` on the archive host at `/opt/usnm-quality-feed/`.
- `qwen_submit.py` and `qwen_provenance.py` in the server container at `/project/ops/qwen-feed/`.

`archive_alto.py` never runs as root. On the archive host it runs as a dedicated unprivileged account, `usnm-alto` (shell `/bin/sh` so the forced command can run, password locked), which can write only to the ALTO cache (`/tank/locstats/qwen-alto`) and its home directory, and reads the world-readable bulk archives in `/tank/locbulk/ocr`. The shipper's key is in that account's `~/.ssh/authorized_keys` with `restrict` and the forced command `nice -n 19 ionice -c 3 python3 /opt/usnm-quality-feed/archive_alto.py --pages - --archives /tank/locbulk/ocr --out /tank/locstats/qwen-alto`. The shipper's `ARCHIVE_HOST` SSH alias, `usnm-qwen-archive`, sets `User usnm-alto`. Check it with an empty request: `printf '' | ssh usnm-qwen-archive` must return the inventory JSON. SSH destinations need verified host keys.

`/etc/usnm/qwen-feed.env` on the shipper: `CAMPAIGN` (a new campaign), `SERVER_HOST`, `ARCHIVE_HOST`, `SCAN_SERVER`, `ENGINE_IMAGE`, `QWEN_ENGINE_PLAN` (a reviewed `/project/.../plan.json` containing the pinned `engine`), `QWEN_MODELS` (the checksum-pinned shared-model directory), `QWEN_MAX_READY` (default 1000 fresh original replicas, 500 untouched pairs; not the global unsent count).

Each tick skips documents already in Qwen roots and completed source chunks, picks non-English available scans by descending page damage from `quality.page_summary` (`select --damage-dsn-file`), and prepares at most 100 pages. Original ALTO is extracted from the retained bulk archives into `/tank/locstats/qwen-alto` (read-only `/srv/locstats` in the preparation container), each XML with a SHA256 receipt naming archive, member and signature, carried in the checksummed source chunks; cached XML and provenance are re-verified on later ticks. Missing ALTO is deferred a day, never synthesised. Extraction yields after 30 minutes per tick (finishing at least one archive); unvisited pages return as `pending` and stay eligible.

The cohort refill (`boinc/scripts/qwen-auto-cohort.py`) reads `qwen-nonenglish-v1` unless `CAMPAIGN_ARGS` in its env names roots as `--campaign NAME:VERSION` (repeatable; each needs an `audit.json`, which can start as `{}`). Bundled roots are bound a bundle at a time (unit, `membership.json`, each page's job, image slice, quality record, provenance, sample) and only if every page's language is admitted. `--max-admissions` (500) and `--max-inflight-per-host` (10) count units. Admitted languages: Yiddish and Serbian, plus repeatable `--cohort-language LANG`, capped by the feed's budget.

#### Undecodable source scans

A scan must match its indexed bytes and SHA256 before decoding (batch JP2s are also MD5-checked against LoC's bag manifest). That proves the scan server returned what was fetched, not that LoC's file is whole, so a re-fetch can return the same broken bytes. Preparation logs the feeder's stdout even on failure; READY counts are pages written.

Decoder quarantine (`--quarantine-decode`, used by Qwen and the CPU `usnm-quality-follow.service`): three known Pillow decode failures (unidentified, truncated, broken stream, with the codec present) on one `(doc_id, batch, batch_version, bytes, sha256)` quarantine that fingerprint as `manual_review`; it is excluded and never re-fetched, while a changed fingerprint is eligible again and a successful preparation resets earlier failures. Network, codec, format and index errors stay retryable. A buffer scan answering 404 (an OCR worker read and deleted it) is skipped as retryable; a missing keep scan is a failure.

Truncated JP2s: a header-only check finds a box or tile-part claiming bytes past end of file, or no EOC marker. Such a scan is never decoded or retried and is not a decoder failure: with `--quarantine-decode` it is recorded at once as `source_truncated` (a changed fingerprint is eligible again); without it (`usnm-quality-feed.service`) it goes to `skipped.txt` as `source_truncated: <reason>`. Logs: `source-truncated scans found` per chunk, `source-truncated scans in campaign` with counts per `<batch>_verNN`. A JP2 that reaches EOC but is otherwise malformed still goes to the decoder and the three-failure rule. The same check (`usnm_ocr_contract::jp2`, vectors in [`fixtures/jp2-truncation-vectors.json`](../../../fixtures/README.md), #1457) keeps such scans out of all OCR, and the page keeps LoC's text:

- `usnm-ocr-worker prefetch` lists it in `<out>/source-truncated.jsonl` instead of writing it and never asks LoC for that batch version again.
- The on-prem node worker dead-letters it as `permanent` (`source_truncated: LoC's JP2 is cut short: <reason>`).
- The BOINC worker (`usnm_ocr`) fails that page before its engine runs; a bundle's other pages still run.
- The queue-fed controller with `--skip-truncated-scans` dead-letters it before pushing ([A page whose JP2 LoC holds cut short](../server/batch-scheduler.md#a-page-whose-jp2-loc-holds-cut-short-1457)).

State is in `/var/lib/usnm-quality-feed/<campaign>/decode-quarantine.sqlite3`. Inspect read-only:

```sh
pct exec 215 -- /opt/usnm-quality-feed/venv/bin/python -c \
  'import json,sys; from pathlib import Path; sys.path.insert(0,"/opt/usnm-quality-feed"); from scan_quarantine import quarantined; print(json.dumps([{k:v for k,v in r.items() if k != "manifest_body"} for r in quarantined(Path("/var/lib/usnm-quality-feed/qwen-nonenglish-v1"))], indent=2))'
```

The manifest builder reads `QWEN_QUARANTINE_ROOT` (default `/var/lib/usnm-quality-feed`, an error if missing) and excludes quarantined documents from new non-English acquisition, serialising with quarantine writers through `.decode-quarantine.lock`; keep that file in place while either service runs. Already queued manifests and independent OCR acquisition are not rewritten or blocked.

Install order: at an idle boundary install the quarantine helper and manifest builder first, then `quality_feed.py`, then the CPU follower with matching `qwen_source.py` and `qwen_feed.py`, restarting the follower at a safe boundary and keeping its outbox and state. On rollback keep quarantine state, READY chunks and queued work, and restore the old follower unit before an older feeder (which can't parse `--quarantine-decode` and would retry bad scans). Keep the new `scan_quarantine.py`: an older one rejects `source_truncated` records and stops the feeders.

Language budget: pages are reserved before preparation; a reserved failed fingerprint retries without spending another page. A reserved page later found `source_truncated` is refunded at the next tick (`<name>_budget_refund` in `<name>-budget.json`) if its current scan has that exact quarantine record and it isn't in `done.txt` or `skipped.txt`. A page whose scan LoC replaced stays spent.

#### Native admission and shipping

The shipper stages native 8B work (12 crops a page, shared models, quorum two, distinct hosts) before freeing source inputs. It refuses uncertain submissions, documents duplicated across campaigns, changed hashes, wrong routing or quorum, going past the fresh-replica target (including a per-chunk reservation), or more than 2000 unresolved native workunits (retries counted separately). A fresh pair is batch 8, quorum and target two, exactly two unsent, unassigned, unvalidated results.

Before transfer it runs the read-only admission helper with `--preflight` and reserves the whole chunk. Under capacity pressure it logs JSON `status=deferred` (phase, reason, counts, limits, audit SHA256) and exits 75, a normal deferral that transfers nothing; admission is checked again under the project mutex before publication. Uncertain `submitting` markers are hard errors (never delete `submitting` to replay work), and changed audited job bytes need explicit reconciliation. Storage floors: 50 GiB on project, bulk and thin pool, plus 64 MiB per candidate page on the least-free of the project root, `quality/` and `usnmocr/download/`. Up to three unshipped chunks are buffered. Native audits stay in `audit.json`, bound to job SHA256.

`qwen_submit.py` stages with a sibling `usnm-boinc-stage` when installed (`--stage-binary` to choose) and submits with the binary `--submit-binary` names (`--qwen-submit-binary` on `quality-ship.sh`); `--transaction` always uses the staging binary and refuses `--submit-binary`. `--bundle-size` (1 to 32, default 1) sets pages per native unit.

#### Transactional subset shipment (#1466, opt-in)

`quality-ship.sh --qwen-subsets` (from the Temporal activity, only after the matching helpers and binary are installed) ships part of a parent chunk instead of reserving it whole: the oldest parent's remaining sources in sample order, up to the headroom. Limits are unchanged (1,000 fresh replicas, 2,000 unresolved units, three READY parents, 50 GiB floors, 64 MiB a source).

- An immutable manifest, written before transfer, fixes the members (`doc_id,batch,batch_version,bytes,sha256`), input hashes and staging recipe, including the engine and model plan, staging-binary checksum and places bytes (`qwen_submit.py --recipe-inputs`). Retries reuse it, and the server rechecks the pins before staging; any change to parent bytes, membership, recipe or provenance fails closed.
- `qwen stage`, `qwen submit` and `quality submit` take `--members FILE` (ordered JSON array of doc IDs) and never drain unrelated units. Each transfer uses a unique upload directory that the server verifies and adopts atomically under the host bridge mutex. Every retained native job in the root must use the requested version. The archive receipt must bind both original and prepared ALTO. LoC provenance is frozen at `quality/<campaign>/texts/<doc_id>.loc-provenance.json` and source bindings at `quality/<campaign>/shipment-quality/<doc_id>.json`.
- Capacity and the `pve/data` thin pool (bounded `lvs`, 64 MiB a page above 50 GiB) are checked under the container mutex, held through publication; `phase=locked_thin_pool` defers.
- An interrupted publication is reconciled against BOINC by workunit identity; missing, duplicate or conflicting history keeps the journal and parent and never clears `submitting`.
- Parent and copies stay until a complete disjoint set of members is acknowledged by native and quality publication; retirement then moves the parent to `retired-parent` and deletes it (`cleaned.json`). A parent with a transaction journal is refused by the legacy whole-chunk path, so rollback must drain or reconcile it.

Receipts: journals at `<outbox>/.qwen-transactions/<campaign>/<chunk>/`, server evidence at `/project/qwen-shipments/<native-campaign>/<transaction-id>/`. `shipment_invocation` reports `publication_attempted`, `native_acknowledged` and `quality_acknowledged`. Exit 75 is not a zero-publication receipt: native success then quality failure is `status=partial`, and `publication_attempted=null` means unknown. Monitor receipts, not exit status.

Before enabling (offline tests don't cover these): install reviewed `qwen_shipment.py` beside `qwen_feed.py` and `qwen_submit.py`, the matching `quality-ship.sh`, and a hash-verified `usnm-boinc-stage` with the `--members` options; at an idle boundary confirm writable transaction storage, canonical `places.txt`, guarded app versions, shared pinned models and no conflicting legacy partials; verify host Python, `pct exec` pipe forwarding, `flock` and `pve/data` under the real mutex; run one bounded transaction and check identities, quorum, one result per host, receipts after an interrupted response and all floors. Ramp from 200 fresh replicas to 1000 only after measured runs show new batch-8 original jobs, zero uncertain submissions, immutable inputs and post-cleanup floors. The indexed scan ceiling (262,920 pages) is not a count of eligible pages.

#### Manifest timer

`usnm-qwen-manifest.timer` (preparation container, from `boinc/scripts/onprem/ocr-feed/`) runs `ocr_manifest.py build-quality --non-english` ten minutes after each run, using `SCAN_SERVER` from `/etc/usnm/quality-build.env`, the certificate-authenticated DSN and state in `/var/lib/usnm-ocr-manifest/qwen-nonenglish`; its service lock prevents concurrent runs. It requests missing non-English scans by descending damage with their real language, excluding retained or requested pages; consumed fetch-history entries are re-requested as keep pages, and confirmed keep scans are cached a day. Only the scan fetcher downloads, under its budget; user OCR keeps priority.

`waiting_for_crop_sources` does not mean the corpus is complete: deferred archives, inaccessible scans, failed validation and storage backpressure remain. Outputs still need the collector join and archival before any input cleanup.

### Request versions on the feed

The outbox, `done.txt` and source requests stay under `CAMPAIGN`. The shipper routes each chunk from `QWEN_SHIP_ARGS` in `/etc/usnm/qwen-feed.env`:

```
QWEN_SHIP_ARGS="--prompt-version 3 --qwen-root NAME --qwen-yiddish-root NAME --yiddish-prompt-version 4 --qwen-submit-binary usnm-boinc"
QWEN_FEED_ARGS="--yiddish-budget 40"
```

- Chunks with no Yiddish go to `--qwen-root` at `--prompt-version`.
- Only-Yiddish chunks go to `--qwen-yiddish-root` at version 4.
- At version 3 or 4, a chunk mixing Yiddish with another language stays READY, as does Yiddish with no root; `qwen_submit.py` also refuses both.

`--yiddish-budget N` caps the Yiddish pages the feed will ever prepare (`yiddish-budget.json` in its state directory) and removes Yiddish from the normal order, so the budget can't become a reread: pages read at version 1 stay excluded, and raising the budget is an env-file decision. Without it Yiddish keeps its place in the order. `--german-budget N` does the same for German (`deu`) during calibration (`german-budget.json`): German comes as its own chunk after any Yiddish budget and goes to `--qwen-root` at `--prompt-version`. `--german-pages FILE` (doc_ids, one per line) limits it to a frozen sample, worst damage first; the November calibration used 50 of Study 1's pilot pages from #1428, which already have `frak2021` and `deu_latf` readings. `--serbian-budget N` and `--serbian-pages FILE` do the same for Serbian (`srp`), whose BOINC OCR is held for the comparison with LoC's text ([note](../../notes/2026-11-07-serbian-trust-hold.md)): Serbian chunks come after any Yiddish and German budgets, are recorded in `serbian-budget.json` and go to `--qwen-root` at `--prompt-version`. The live feed has `--serbian-budget 500 --serbian-pages /etc/usnm/serbian-compare-pages.txt` (500 of the first Tesseract batch's pages, 71 or 72 per decade). `quality_feed.py select` filters with `--language` and `--exclude-language` (repeatable).

A new root must be added to `CAMPAIGN_ARGS` in the refill's env and to `NATIVE_QWEN_ARGS` once its quality root exists. The version 1 root stays out of the refill, so its backlog goes only to the classes open to it.

### Bundled native units (#1435)

`--qwen-bundle-size N` (2 to 32) in `QWEN_SHIP_ARGS` makes `qwen_submit.py --bundle-size N` stage up to N pages a native unit in `--qwen-root`, so `llama-server` loads the model once per bundle. The `qwen stage` limits close a bundle at about 10 pages of 12 crops, so use `--qwen-bundle-size 10`. Without the flag units stay one page; `--transaction` refuses any size but 1. Gains measured on #1435: about a third of task time on an RX 9070 XT, a fifth on NVIDIA, little on Apple and Intel. The loop guard saves more: repeated loop text is 57% of generated tokens on every vendor.

The feed (#1555), collector (#1572) and refill (#1561) support is merged. To turn it on:

1. Use a new root. `qwen_submit.py` won't bundle a root holding single-page units, and a bundled root refuses `--bundle-size 1`. Only-Yiddish chunks stay single-page in `--qwen-yiddish-root`.
2. Deploy the collector first. An inconclusive bundle's agreed pages reach the rows only through `quality collect --qwen-pages`; without it they are lost. Give every native-root collector a build from #1572 on, set `QWEN_PAGES_ARGS` in `usnm-qwen-results.service`'s env, and add the root to `NATIVE_QWEN_ARGS`.
3. Add the root to the refill's `CAMPAIGN_ARGS`.
4. `usnm-qwen-app` reads bundles since #887, so no new app version is expected; check that one bundle completes on each plan class first.

The watermarks still reserve one pair per page while a bundle is one pair for up to 10 pages, and `--qwen-max-ready` counts replicas, so the same 1,000 holds more pages. A bundle needs every page to agree, so disagreeing pages send the whole unit to third and fourth replicas. Whether crop consensus or a page-level verdict decides the unit, with its credit and retry, is a maintainer decision.

### One submit binary and one collector (#1449)

Since 5 November 2026 (#1449) the server image is built from a commit after #1437, so it reads request versions 3 and 4:

- Submission: `QWEN_SHIP_ARGS` carries `--qwen-submit-binary usnm-boinc`, so `qwen_submit.py` submits every version with the image's `usnm-boinc`. Without the flag versions 3 and 4 fall back to the static staging build. If the image refuses a version, the tick fails and the chunk stays READY.
- Collection: `/etc/usnm/quality-results-qwen.env` names one collector for all three roots, the static build `/project/ops/qwen-feed/request-guard-44465088/usnm-boinc`, and drops per-root `=COLLECTOR`. `usnm-quality-results` keeps its own collector (`quality-results.env` and the `/project/ops/quality-results/usnm-boinc` link). Keep `request-guard-44465088/` while it is named.

  ```
  QUALITY_COLLECTOR=/project/ops/qwen-feed/request-guard-44465088/usnm-boinc
  NATIVE_QWEN_ARGS="--native-campaign qwen-nonenglish-v1 --native-campaign qwen-guard-v3-20261102 --native-campaign qwen-guard-v4-20261102"
  ```

The row-cache key (`Cache::new` in `crates/usnm-boinc/src/quality/collect_cache.rs`) includes the collector's path and bytes, the `--qwen-crop-states` directory and, for bundles, `--qwen-pages`. Changing a root's collector makes every unit dirty, so with `--max-dirty-units 8` a large root fails several ticks with no new rows unless it is warmed first. Never name the image's `usnm-boinc` as a collector, or every rebuild rewarms every root. To move collectors, warm each moved root as root on the BOINC host into a scratch output (the real cache; the published snapshot isn't touched), repeating until it exits 0 instead of `collection cache warmed 8 dirty units; at least N deferred`, then change the env file and delete the scratch output:

```sh
pct exec 230 -- flock -w 60 /run/usnm-boinc-step.lock podman exec usnm-boinc-server \
  timeout --signal=TERM --kill-after=30s 20m <collector> \
  quality collect --campaign qwen-nonenglish-v1 --root /project/quality/qwen-nonenglish-v1 \
  --spool /project/usnmocr/results/0 --qwen-root /project/qwen/qwen-nonenglish-v1 \
  --qwen-spool /project/usnmocr/results/8 --out-dir /project/ops/quality-results/prewarm/qwen-nonenglish-v1 \
  --cache /project/quality/qwen-nonenglish-v1/collected-parts --max-dirty-units 8
```

The crop rule is the next such move: it changes every root's key (`CROP_STATES_ARGS`) and needs a collector from #1490 on, which `request-guard-44465088` predates. Install such a build at a new path, warm all three roots with it and `--qwen-crop-states /project/usnmocr/qwen/crop-states`, then set `QUALITY_COLLECTOR` and `CROP_STATES_ARGS` together.

### OCR's prefetch manifests (#1055)

The scan fetcher (`usnm-loc-fetch`) serves OCR's manifests before the quality pass's. `boinc/scripts/onprem/ocr-feed/ocr_manifest.py` builds OCR's hourly (`usnm-ocr-manifest.timer`): pages queued in curated `ocr-local/queued/` (read live with the container's Arc identity) with no outcome in `ocr-local/outcomes/` and no scan on the scan server, users' requests first, at most 1,000 per manifest, PUT through the scan server as `/manifests/queue-ocr-<stamp>/manifest.parquet`. The next one waits until the last is 90% fetched or a day old.

LoC budgets (`crates/usnm-ocr-worker/src/loc/global.rs`, `crates/usnm-boinc/src/scans.rs`, `crates/usnm-ocr-worker/src/loc/bucket.rs`):

| Component                                                                        | Rate                                                                                                                                                |
| -------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| Host-wide LoC pacer (all LoC requests on the host, via the fetcher's state file) | 20 a minute (3 s gap); 12 a minute (5 s) for 24 hours after a 429, 403 or challenge page; reset with `usnm-ocr-worker loc-pacer --file <f> --reset` |
| `quality stage` and other staging (`--gap-secs`)                                 | default 5 s (12 a minute), minimum 3 s; also inside the host pacer when `USNM_LOC_GLOBAL_PACER` is set                                              |
| LoC's `ndnp-batches` bucket (`--loc-bucket-gap-ms`)                              | default 2 s, minimum 1 s; separate from the host pacer                                                                                              |

### The full image pass and the piggyback (#962)

`ocr_manifest.py build-quality` (`usnm-quality-build.timer`, every 2 hours) takes the next issues from `usnm_quality`'s stored summary: pages with no image features yet, not staged, grouped by issue, issues ranked round-robin over (title, decade), worst first by their worst page (mean of the damage score and LoC's doubt, `1 - wc_mean`), the first `--strata` issues of each, every page of an issue. Whole issues let the fetcher request an issue's METS and batch manifest once for all its pages, about one request a page (the first stratum-1 manifest on 25 October 2026, one page per (title, decade), cost about three bucket requests a page). `--batches` (`--pass-batches` to the installer) limits the pass to the batches a file names, such as those LoC's public bucket serves; `--pass-pages` to the installer sets the pages a run. Pages are queued through the scan server as `queue-quality-<stamp>`, run behind OCR's manifests in one-hour slices inside the global LoC budget. Rows are marked `eng`, so scans go to the fetcher's buffer set.

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

Excluded: pages in `quality.ocr_outputs` or outstanding or imported in `--ocr-state`. Pages already in a quality campaign stay in, because a `usnm_quality` unit compares existing texts and doesn't read the page, unless `--campaign-exclusions` (off by default; enables `--exclude-list`). The moderate sample shows nothing until its pages are OCRed: a gain needs a `quality.ocr_outputs` row to compare with LoC's damage score.

`follow` takes every fetched page under its own campaign, so pages from these manifests land there unless `follow`'s `--staged` names them. With the fetcher's `--no-loc-prefixes queue-quality-` (`USNM_PREFETCH_NO_LOC_PREFIXES`), `queue-quality-*` manifests take JP2s only from LoC's bucket or a mirror, so pages of batches the bucket doesn't serve wait; without it they are fetched from LoC within the global budget.

#### The follower

`quality_feed.py follow` (`usnm-quality-follow.service`) gives a unit to every fetched page no campaign has staged, in fetch order, in full chunks of `--chunk-pages`. A part chunk waits until no chunk has been written for `--max-wait-secs` (6 hours), so bundles aren't tiny (median 16 pages before 5 November 2026). Pages with no curated LoC text yet wait; unpreparable pages are replaced from a reserve of a tenth of `--chunk-pages`, and a short chunk ("chunk came up short") is written only once overdue. Pages never worth retrying go to `skipped.txt`.

Keeping damaged scans: after a chunk ships, the pass's buffer scans are deleted except the most damaged (LoC's `damage_rate`, unscored last), up to `--keep-bytes` (1 TB from `quality-feed-install.sh`, about 210,000 pages, half the fetcher's 2 TB buffer cap). Kept scans stay on the scan server for re-OCR, listed in `<state>/kept.tsv` for all campaigns; a more damaged newcomer replaces the least damaged, and if that delete fails the newcomer goes instead, so the list never exceeds the budget. OCR's scans and the keep set are never deleted. `quality-feed-install.sh --follow` enables it with the shipper.

- A kept page leaves the list only when its sidecar is gone and a DELETE confirms the bytes are too.
- Pages OCR still wants are left alone. `ocr_manifest.py build` records them in `ocr-docs.txt` (up to 500 sidecar lookups a run, misses in `claim-misses.txt`) and rewrites `ocr-active.txt` with those not yet ended; the follower reads it (`--ocr-active`; until it exists, all of `ocr-docs.txt`). Both files are written and read under `kept.tsv.lock` (`--claims-lock`). A newly shipped page in either is OCR's, neither kept nor deleted.
- `--keep-verify` (500) longest-unchecked pages are looked up per round; a failed lookup goes to the back, and an unresponsive scan server ends that round's lookups, deletes and feeding, which the next round resumes.
- Every round, even before its campaign has shipped, the follower handles the list for every campaign it has followed (`<state>/followed.txt`), so chunks shipped after a campaign switch don't leave scans behind; pages leaving it go to `<state>/retired.txt` so no campaign takes back a page another let go.

Supply: each round `follow` takes every indexed full-resolution scan with no unit, not in `--staged` and not done, up to the outbox's room, so while the shipper keeps up the feed rate equals the fetch rate: the pass's manifests plus OCR's and others. Raising it needs more bucket or LoC requests or refeeding staged pages, each a maintainer choice ([5 November 2026 measurements](../../notes/2026-11-05-quality-supply.md)).

## German comparison report (#926)

`boinc/scripts/german-quality-report.py` replaces the external experiment's `perpage.py`, `order.py` and the reporting part of `crops.py`, reusing the collector's metrics. Standard-library Python, offline:

```bash
python3 boinc/scripts/german-quality-report.py \
  --sample german-sample.jsonl \
  --pages pages.jsonl --crops crops.jsonl \
  --reader loc --reader ours-tesseract --reader tess-deulatf \
  --max-crops-per-page 12 --out german-report.json
```

`--sample` is the frozen `quality sample` JSONL (`doc_id`, `lang=deu`), kept apart from results (deriving it from result rows hides missing pages). `--pages` and `--crops` are the collector's tables exported to JSONL unchanged. `--max-crops-per-page` must match the plan's `k_crops`, which is only an upper bound: unused crop capacity is not missing work, and unobserved staged crops stay unknown without the staging manifests. Name every expected reader, including `loc` or `loc-hist` for reference counts; use `--crop-reader surya` for a crop-only reader.

The report is create-only, binds sample, exports and reporter source by SHA-256, and gives per-title/year and per-page word-bag Dice, CER/WER and crop CER/Dice against the VLM. These measure model agreement, not human CER or calibrated accuracy, and crop scores keep the collector's normalization.

- Distinct comparator models and distinct sources of one reader (`by_source`) are never pooled; model IDs like `qwen3-vl-8b-q4km` are metadata, not file digests.
- Conflicting page job IDs are rejected; one known and one unknown are not the same job. Crop rows must match the page's job, source, rectangles and strata, or they are excluded as unverified.
- Absent reader rows are unknown outcomes (failed and timed-out units emit none), so keep a separate unit-outcome inventory. The collector keeps completed whitespace-only readings and exports `vlm_reading_present`; legacy null text without it is ambiguous.
- Means use only rows with known nonempty reference counts (`vlm_reference_chars`/`vlm_reference_words` for crops); older exports have unknown denominators. Summaries are unweighted; reader `total_ms` is kept as reported; `loc-hist` self-scores are dropped when `loc` is present. Layout and hat-derived accuracy metrics are omitted.
- Languages are reported per reader row; a historical `und` score is not relabelled German.

It is reporting only: stage with the native `quality sample`, `texts-export`, `stage` and `collect`, and never copy our ALTO to `loc.xml` as LoC's layout. To close #926: freeze about 100 pages across titles, years and layouts (with #925's reading-order cases), pin native app, model and llama artifacts, keep source and input hashes and measured wall, CPU and memory, rerun and review missing coverage. The 12-page, one-title reproduction proves only the reporting path; accuracy claims need human-labelled crops.
