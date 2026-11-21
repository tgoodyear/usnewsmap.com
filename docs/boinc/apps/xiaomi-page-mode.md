# Xiaomi-OCR-0 page mode and the English pilot (design)

Issues: [#1922](https://github.com/tgoodyear/usnewsmap.com/issues/1922) (full-page VLM path) and [#1867](https://github.com/tgoodyear/usnewsmap.com/issues/1867) (10,000-page English pilot). Related: #1896 (blind pick task), #1915 (completeness gate), #1963 and its design PR #1988 (per-crop settlement), #1621 (Cherokee line re-read, the closest existing merge path), #1965 (empty-return guard).

Status: design only. Nothing here is built, staged or deployed. The Science Coordinator decides the open questions at the end. Nothing from this path reaches search until the gate in "Gating" is met.

## Evidence this rests on

Scored on the x08 benchmark:

| Measure                                | Value                                     |
| -------------------------------------- | ----------------------------------------- |
| English OverProof CER, Xiaomi-OCR-0 q8 | 2.11%                                     |
| English OverProof CER, Tesseract PSM 6 | 4.06%                                     |
| English OverProof CER, LoC             | 9.54%                                     |
| q8 against the full model              | within 0.02 percentage points             |
| Throughput, one request at a time      | about 22 characters per second per GPU    |
| Cost of a page at that rate            | about 30 GPU-minutes (about 40,000 chars) |

The throughput figure was measured with one request in flight. Both clients start llama-server with `--parallel 1` (`qwen::work::serve` in the container worker and the native app's `server.rs`) and send one crop at a time. Parallel slots are new in this design, and the measurement plan below decides how far they move that figure.

## 1. Page mode

### Regions

A page-mode unit reads every text region of a page, not the 64-crop sample `qwen stage` cuts today.

- Regions come from LoC's ALTO for the page, the same file the quality and line paths already read. Every English Chronicling America page in the pilot has one. Pages without ALTO are out of scope here; #1922's line-finder route stays for later.
- A region is one LoC `TextBlock`. A block whose LoC text is over a character budget (proposed 1,500 characters, about half of Xiaomi's 2,048 output tokens) is split into runs of whole consecutive `TextLine`s under the budget. Each run becomes its own region and, in the assembled ALTO, its own `TextBlock` with the id `<LoC block id>.<n>`, in LoC's order.
- A region's image is the union of its lines' boxes with the padding the line path uses. A region taller or wider than a pixel cap (to be set from the first units' `gpu_mem_peak_mib`, see units) is split at a line boundary like an over-budget block.
- Non-text blocks (illustrations, rules) are not read and stay as LoC has them.
- Blocks with fewer than a minimum of LoC characters (proposed 3) are not read and keep LoC's text, recorded as `loc:small`.

The worker keeps guarded 1.17+'s behaviour per region: the loop guard (`tail_repetition`, `loop_cut`, `repetition_loop_crop_bits`), one retry of a failed request, and the checkpoint (`qwen_checkpoint.json`) so a restarted task does not reread finished regions. Request version stays 5 (`PROMPT_VERSION_XIAOMI`, "Extract the text in the image.").

### Assembly

Assembly runs at the server, after the pair is judged (section 3). It starts from LoC's ALTO, as `lines::merge` does for Cherokee lines, and writes ALTO v4 in scan pixels:

- Each region keeps LoC's geometry: the block box, its line boxes, LoC's reading order.
- A region that takes Xiaomi's text gets it line by line. When the reading has as many lines as LoC's region, line `i` goes to LoC line `i`. Otherwise the words are spread over LoC's lines in proportion to each LoC line's character count. The block records which (`fit=lines` or `fit=reflow`).
- Word boxes are split in proportion to word length inside the line box and get no `WC`, as #1922 proposes. Whether that is enough for the page API (#1907) is still an open question there.
- Granularity is raised to `line` when any region took Xiaomi's text, as the Cherokee merge does.

LoC's text is kept for a region when:

| Reason     | When                                                                   |
| ---------- | ---------------------------------------------------------------------- |
| `empty`    | the canonical reading has no letter or digit and LoC's region does     |
| `looped`   | either side of the pair looped on it (crop state `looped`)             |
| `disagree` | the pair's readings fall under the crop-consensus-v2 floor (section 3) |
| `small`    | not read (under the minimum size)                                      |
| `failed`   | not read (the request failed after its retry, or the deadline passed)  |

### Per-block provenance

Each assembled `TextBlock` names who produced its text. ALTO v4 allows `<Processing ID="...">` steps in the `Description` and a `PROCESSINGREFS` attribute on blocks, so the writer adds two steps, one for Xiaomi-OCR-0 (with the model, request version, image and weights digests) and one for LoC's OCR (the LoC software line from `quality.loc_software`), and each block refers to one of them. `usnm-alto` writes neither today, so the writer and its bounded reader both need the attribute; the check in `ocr-local` (`check_alto`) must accept it.

The same facts go in a `blocks.json` sidecar beside the published ALTO, one entry per region: id, LoC block id, source (`xiaomi` or `loc`), the LoC-kept reason, the crop-consensus-v2 score, `fit`, LoC and Xiaomi character counts. The sidecar is what the analysis reads, so nobody has to parse ALTO for it.

Page-level provenance follows the BOINC import's existing `fileIdentifier` notes (`retry::splice_notes`): `usnm_engine_image` (the native build's digest), `usnm_job_id`, `usnm_boinc_host` and `usnm_platform_family` for the canonical result, plus two new ones, `usnm_llama_image` (llama.cpp's digest) and `usnm_pair_host` (the other host of the agreeing pair). A `Description` post-processing step carries the counts: regions read, regions Xiaomi, regions LoC by reason, and the page's agreeing share.

### Engine entries

| Where                            | Change                                                                                                                                                                                                  |
| -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `ocr_source`                     | `usnm-xiaomi`, added to `checks::ENGINE_SOURCES` in `usnm-ocr-local`                                                                                                                                    |
| `ocr_engine` tag (13 §13.8.1)    | `xiaomi-ocr-0/bbd8cf8d21f2 profile=q8_0.rv5 image=sha256:<12> models=sha256:<12> route=boinc-page@r1 contract=0`                                                                                        |
| `ocr/engines/engines.toml`       | an `[[engine]]` entry `xiaomi-ocr-0`, family `xiaomi-ocr-0`, Apache-2.0, xiaomi-research/Xiaomi-OCR-0 at revision `bbd8cf8d21f2…`, with `production = false` until the gate is met                      |
| Per-language list (#1922 item 4) | a release-build config list of the languages this engine may replace LoC in; empty until the Science Coordinator adds `eng`, and the release build fails if a `usnm-xiaomi` row's language is not on it |

In the tag, the version is the upstream revision, short. The profile is the quantisation and the request version. `image` is the first 12 hex digits of the native build's manifest digest (the `Engine.image` the unit was staged for). `models` is the first 12 hex digits of the sha256 of the sorted `<name> <sha256>` lines of the two files read, `xiaomi-ocr-0-q8_0.gguf` and `mmproj-xiaomi-ocr-0-f16.gguf` from `boinc/apps/qwen/weights.lock`. That is the Xiaomi model alone, not the whole lock (#1867's weights digest `sha256:f614f48b…` stays in the ALTO notes and the unit). `OcrEngineTag::new` already accepts every field.

`EngineKind` (`usnm-ocr-contract::routing`) is the Tesseract and NDLOCR-Lite job router and does not gain a variant: page-mode results do not come from `boinc::Job`. The page-mode import builds its `OverlayRow` from the qwen result and the assembly, then goes through the same `published_alto`, `put_same` and ledger checks as the existing BOINC import, so `ocr-sync` sees an ordinary part.

### The completeness gate

The assembled page enters the release as our OCR, with LoC's text kept as `text_loc`. It must pass rule v1 and then the #1915 completeness gate (`rule::Completeness`: at least 0.85 of LoC's characters, word cover at least 0.30, LoC texts under 20 words not weighed). Unlike Cherokee line jobs, page-mode pages are not exempt: a page-mode page can replace a whole page, so it is judged as one. The `ocr_local.json` counts `short` and `cover` per engine, so the pilot's gate failures show apart from Tesseract's.

## 2. Units and GPU use

### Device classes

Page mode gets its own plan classes, at a new batch number (proposed 109) so no 1.20 client receives a page unit:

| Class (proposed)     | Platforms                    |
| -------------------- | ---------------------------- |
| `nvidia_vulkan_x08p` | Linux x86_64, Windows x86_64 |
| `amd_vulkan_x08p`    | Linux x86_64, Windows x86_64 |
| `apple_gpu_x08p`     | macOS arm64                  |

Intel stays excluded, as decided. `mt_x08` (CPU) is not used for page mode: at the measured 20 seconds per crop on three threads, a page of a few hundred regions would take hours. Windows builds ship with the Linux ones, since Windows is most of the volunteer GPU supply. The classes mirror the x08 classes' gates (2,000 MB minimum GPU memory, the AMD peak-FLOPS floor that keeps integrated Radeons out) and start closed (`min_wu_id 2`, `max_wu_id 1`), as every new class does.

### One llama-server per GPU, parallel slots

A task holds one GPU and runs one llama-server on it with several slots (`--parallel N`) and continuous batching (`--cont-batching`, on by default in current llama.cpp builds; the client passes it anyway). The client reads up to `N` regions at once over HTTP, so the server batches their decoding. BOINC's several-tasks-per-GPU setting is not used: each task would load its own copy of the weights.

Slot count. The client picks `N` when the server starts:

```
kv_per_slot = 2 × n_layer × n_ctx_slot × n_head_kv × head_dim × 2 bytes   (f16 cache)
fixed       = model file + projector file + compute reserve
N           = clamp(floor((free_vram × 0.85 − fixed) / kv_per_slot), 1, 8)
```

- `n_layer`, `n_head_kv` and `head_dim` are read from the GGUF metadata at start, not hard-coded.
- `n_ctx_slot` is what one region needs: image tokens at the region pixel cap, the prompt, and the 2,048 output tokens. Today's `XIAOMI_CTX_SIZE` of 16,384 is for one crop with room to spare; page mode measures the largest region's need on the first units and sets `n_ctx_slot` from it (expected 8,192 or less).
- llama-server divides `--ctx-size` among its slots, so the client passes `--ctx-size N × n_ctx_slot`.
- The compute reserve covers llama.cpp's compute buffers and the projector's image encoding. It is measured per class on the first units (from `gpu_mem_peak_mib` with `N = 1`) and kept in the app's config, not guessed.
- `free_vram` is what the GPU reports free at start, less the 15% margin. The floor is 1 slot, so a card that fits the model still runs. The ceiling is 8 until measurements above it exist.
- If the server fails to start or reports out-of-memory, the client halves `N` and tries again, down to 1, and records the step in `diag`.
- `N` is recorded in the result diagnostics (`slots`), with the KV and fixed sizes used.

The plan classes reserve 2,000 MB (`gpu_ram_used_mb`). With more than one slot the task may use more than that. Whether to raise the reservation per class, or cap `N` so the task stays within it, is an open question (the client knows the free memory at start; the BOINC scheduler does not).

Batching and agreement. Batched decoding can change the arithmetic order, so two hosts at different `N` may not produce byte-identical readings even at `--temp 0 --seed 42`. Crop-consensus-v2's floor of 0.7 similarity is far above that noise, but the pilot measures it: agreement by the pair's slot counts is one of the reported cuts.

### Packing

- A unit is one page by default. At about 30 GPU-minutes a page with one slot, a page is far over the five-minute floor on every class, and it has hundreds of regions, enough to keep 8 slots busy.
- Regions are queued longest first (by LoC characters), so the last regions to finish are short ones and slots do not sit idle at the end.
- A page whose estimated time on the fastest class (from LoC characters and that class's measured characters per second at its usual `N`) is under five minutes is packed with other small pages of the same campaign, up to 4 pages, into one queue of regions. The existing bundle format (`pages`, `BundlePage`) carries them, with page-mode regions in place of crops.
- The deadline is set from LoC characters at the slowest admitted class's measured rate, under `MAX_DEADLINE_SECS` (4 hours). A page whose estimate is over that is not staged in the pilot and is counted in the campaign notes.
- Page-mode limits replace the sample limits for these jobs: regions per page (proposed 1,024; `MAX_CROPS` of 64 is for samples), readings size (`MAX_READINGS` of 4 MiB holds a 40,000-character page with room).

### Measurement plan for slots

Before the 10,000 pages are staged, a calibration set of 50 pilot pages (spread over decades and damage bands) is read on each class at `N` = 1, 2, 4 and 8, forced by a job field the calibration units carry and production units do not. Per class and `N`:

- output tokens per second and characters per second for the whole unit (wall time from first request to last answer),
- per-region latency (the existing `Reading.ms`),
- `gpu_mem_peak_mib`, out-of-memory restarts,
- agreement of each region with the same host's `N = 1` reading.

The default `N` per class is the smallest that reaches 90% of the best characters per second, so memory is not spent for a few percent. The table goes in `docs/boinc/apps/` with the hosts and GPUs named by model only.

## 3. Quorum and settlement

### Pair

Each unit is sent to two hosts (quorum 2), under the existing pairing rules: different hosts, the reader-mismatch check (`Job.model` pins `xiaomi-ocr-0-q8`), and the #1965 empty-return guard first (a result with more than 10% unexplained empty readings is invalid on its own).

Each region is then judged by crop-consensus-v2's per-crop state, unchanged (`consensus::crop_state`: similarity with Hebrew points left out, floor 0.7, `looped`, `no_text`, one-sided text counts as `disagree`).

### Two separate thresholds

The design splits what BOINC needs from what search needs:

- **Validity** (BOINC, credit, a canonical result): the pair validates when the empty guard passes, both finished, `agree` regions are more than half of the regions with text, and they hold at least 100 characters. This is v2 without "any disagreeing crop fails the pair", the same change #1988 proposes for samples. A page of 300 regions would almost never pass v2 as it stands.
- **Acceptance** (the page may go to assembly and import): the share of the page's text regions that agree, by characters, is at least `A`. `A` is not fixed by this design. The pilot measures the distribution first (below), and the Science Coordinator sets `A` from it. Until then nothing is accepted.

Proposed rule name for the validator: `page-blocks-v1`, gated by a project file like `page-v1`'s `<project>/qwen/bundle-verdict`, for batch 109 only.

### Reconciling with #1988

#1988's per-crop settlement treats a crop as settled when an agreeing pair judged it `agree`. Page mode uses the same states with region for crop:

| #1988 state | Page mode                                                                |
| ----------- | ------------------------------------------------------------------------ |
| `settled`   | region takes the canonical result's Xiaomi text                          |
| `unsettled` | not used in the pilot: a disagreeing region keeps LoC's text, `disagree` |
| `looped`    | keeps LoC's text, `looped`                                               |
| `blank`     | keeps LoC's text (empty on LoC's side too, so nothing changes)           |
| `failed`    | keeps LoC's text, `failed`                                               |

The pilot does not re-issue disagreeing regions. Their LoC text stays, and they are counted. If #1988's pooled re-issue lands, disagreeing regions could join its pools by language and app version; that is an open question, since a re-read region would change a page already assembled. The crop-state record (`consensus::Record`) is written per region either way, at whatever `RECORD_VERSION` #1988 settles on, so the collector reads both paths the same way.

Which text: an agreeing region takes the canonical result's reading. The other side's reading is kept in the ledger by `readings_sha256`, not merged.

### What the pilot measures before `A` is set

- Per page: the agreeing share by regions and by characters, and its spread by decade and damage band.
- Per region, on the calibration pages that have human truth (en-op and en-as pages read in page mode as extra units, outside the 10,000): CER of agreeing regions and of disagreeing regions against truth, Xiaomi's and LoC's. This shows whether agreement tracks accuracy, which is what makes `A` meaningful.
- Pair outcomes: first-pair validity, third replicas sent, and agreement by device-class pair and slot count.

## 4. Pilot selection

10,000 English pages, from the worst tenth of English pages by qe-v1 (`qe_lm_oov`, the study's decision measure; higher is worse), leaving out pages Tesseract's worst-first wave has already fixed, stratified by decade and damage band in proportion to the pool.

Definitions:

- English: `qe_loc_scores.language = 'eng'` with `language_source = 'page'`, valid rows, `qe_version = 'qe-v1'`, at least 50 words.
- Damage band, from `quality.page_summary.damage_rate`, using #1401's cuts: `worst` over 0.25, `moderate` over 0.10, `low` otherwise. A page in the worst qe tenth with a low damage rate is kept: where the two measures disagree is worth seeing.
- Fixed by Tesseract: the newest `ours-tesseract` output in `quality.ocr_outputs` has a damage rate at least 0.02 lower than LoC's, `quality_gain.py`'s per-page measure and its `--min-gain`. Pages Tesseract has read without that gain stay in (a three-way comparison), and so do pages it has not read yet. Both are flagged.
- Each stratum gets its proportional share, with a floor of 30 pages (`quality_gain.py`'s smallest interval) or the whole stratum if smaller. The floor can push the total past 10,000; the staging script trims the largest strata back to it and records the trim.
- Order inside a stratum is a seeded hash, so the draw can be repeated.

Run on the replica as `quality_reader`:

```sql
-- psql -v n=10000 -v seed=x08-eng-pilot-1 -f pilot.sql
WITH eng AS (
  SELECT doc_id, lm_oov
  FROM quality.qe_loc_scores
  WHERE qe_version = 'qe-v1' AND NOT invalid
    AND language = 'eng' AND language_source = 'page'
    AND NOT coalesce(no_words, false) AND words >= 50
    AND lm_oov IS NOT NULL
),
ranked AS (
  SELECT doc_id, lm_oov, ntile(10) OVER (ORDER BY lm_oov DESC, doc_id) AS tenth
  FROM eng
),
tess AS (
  SELECT DISTINCT ON (doc_id) doc_id, damage_rate
  FROM quality.ocr_outputs
  WHERE reader = 'ours-tesseract'
  ORDER BY doc_id, ocred_at DESC
),
pool AS (
  SELECT r.doc_id, s.decade, r.lm_oov, s.damage_rate,
         CASE WHEN s.damage_rate > 0.25 THEN 'worst'
              WHEN s.damage_rate > 0.10 THEN 'moderate'
              ELSE 'low' END AS band,
         t.doc_id IS NOT NULL AS tess_read
  FROM ranked r
  JOIN quality.page_summary s USING (doc_id)
  LEFT JOIN tess t USING (doc_id)
  WHERE r.tenth = 1
    AND s.damage_rate IS NOT NULL
    AND (t.doc_id IS NULL OR s.damage_rate - t.damage_rate < 0.02)
),
alloc AS (
  SELECT decade, band, count(*) AS pages,
         greatest(least(count(*), 30),
                  round(:n * count(*)::numeric / sum(count(*)) OVER ()))::int AS take
  FROM pool
  GROUP BY decade, band
),
drawn AS (
  SELECT p.*, row_number() OVER (PARTITION BY decade, band
                                 ORDER BY md5(:'seed' || doc_id)) AS k
  FROM pool p
)
SELECT d.doc_id, d.decade, d.band, d.lm_oov, d.damage_rate, d.tess_read, a.pages AS stratum_pages
FROM drawn d
JOIN alloc a USING (decade, band)
WHERE d.k <= a.take
ORDER BY d.decade, d.band, d.k;
```

As #1867 says, the pages on disk come from earlier campaigns and are not representative, so each stratum is filled from disk where it can be and the rest fetched through the paced locfetch path, which the Compute Coordinator queues. The campaign notes record stratum counts, the on-disk and fetched split, the trim, and the seed.

This replaces #1867's draw from `wf1415-eng` with a draw from the qe-v1 worst tenth. Membership of `wf1415-eng` is recorded per page so the two can be compared.

## 5. Gating

Assembled pilot pages are held. They are written under the campaign's held root (the `usnm-qwen-results-x08` collector already holds `x08-eng-pilot`, never loads it) and loaded into `quality.ocr_outputs` as a new reader, `ours-xiaomi`, so `quality_gain.py` can score them. Nothing is written to `ocr-out`, so `ocr-sync` and the release never see them. Two more locks stay shut until the decision: `engines.toml` has `production = false`, and the per-language list is empty.

The Science Coordinator decides from two measurements, both pre-registered here:

**quality_gain.** `quality_gain.py --readers ours-xiaomi,ours-tesseract --plan <pilot plan>`, cells of (band, `eng`, decade), weighted as the plan gives:

- the mean drop in damage rate against LoC, with its 95% interval, and the cell decision at `--min-gain 0.02`;
- the drop in estimated CER where a `usnm_quality` unit gives both;
- the drop in qe-v1 `lm_oov`, as a third, reference-free measure;
- the #1915 gate's pass rate (`short`, `cover`);
- for flagged pages Tesseract also read, Xiaomi against Tesseract.

**Blind picks (#1896).** `en_pick.py` on a random sample of accepted pilot pages, capped per decade and band, with calibration items from en-op and en-as. `en_pick_analysis.py` gives Xiaomi's win rate with a page-clustered bootstrap interval by decade and band, and how often settled picks match truth on calibration items.

The two agree when, pooled and in each band with enough pages:

- quality_gain's decision is `go`, and
- the blind-pick win rate's interval lies above 0.5, and
- calibration picks match truth often enough that the pickers can be trusted (the rate is the Coordinator's to set; the dry run had Xiaomi right on 96.2% of certain words with truth).

Cells where they disagree, or either is still measuring, stay on LoC. On a go, the Coordinator adds `eng` to the list, `production` is set to true, and the held pages are published through `ocr-out` with the engine tag above, so the existing holds (`quality.boinc_page_holds` by engine tag, `ocr-local/revoked`) can take them back out.

## 6. Cost accounting

With several slots, a region's request time overlaps others, so summing `Reading.ms` overstates cost. The GPU is held for the unit's wall time, so cost is wall time shared out:

- **Unit GPU-seconds** = `load_ms + engine_ms`, from the result manifest, for every returned result, including the second replica, third replicas and invalid ones.
- **Page GPU-seconds**, in a multi-page unit, is the unit's GPU-seconds split by each page's share of the unit's completion tokens (`Reading.tokens`).
- **Cost per accepted page** for a class is the sum over all results for the campaign's pages, divided by pages accepted, so failed and extra replicas count.

New table `quality.vlm_page_cost`, one row per result per page, loaded by the collector (append-only, like `qe_loc_scores`):

| Column                                | From                                                    |
| ------------------------------------- | ------------------------------------------------------- |
| `doc_id`, `campaign`, `job_id`        | the unit                                                |
| `result_id`, `host_id`, `app_version` | the BOINC result (as `boinc_page_provenance` gets them) |
| `plan_class`, `platform`              | the app version                                         |
| `gpu`, `gpu_mem_total_mib`            | the result's `host` block, else host.misc's GPU list    |
| `slots`, `ctx_size`                   | the result's diagnostics (new)                          |
| `load_ms`, `engine_ms`, `page_share`  | the manifest; the page's token share                    |
| `gpu_seconds`                         | `(load_ms + engine_ms) × page_share / 1000`             |
| `regions`, `tokens`, `chars_out`      | the readings                                            |
| `gpu_mem_peak_mib`                    | the manifest                                            |
| `outcome`, `validated`, `canonical`   | the result and the validator                            |

From it, the report gives GPU-seconds per page and per 1,000 output characters by class and slot count, and the estimate for a larger rollout: pages × (cost per accepted page by class) × the class mix volunteers have supplied.

## 7. Release plan

In order, each its own PR:

1. **Contract** (`usnm-ocr-contract::qwen`): a page-mode job field (region list with LoC block and line ids, the forced slot count for calibration units only), page-mode limits, `slots` and `ctx_size` in diagnostics. Old clients refuse the new jobs (`deny_unknown_fields`), which is wanted, and batch 109 keeps them from being sent anyway.
2. **Client** (native app first, then the container worker): parallel slots and the slot rule, concurrent region requests with the longest-first queue, checkpoint per region, out-of-memory halving. A new `usnm_qwen` version for the three page classes on each platform. No change for the sample classes, which keep `--parallel 1` until their own measurement.
3. **Plan classes**: the three `*_x08p` classes in `plan_class_spec.xml`, closed; `refresh-plan-policy.py`'s Xiaomi class list, `publish-appver.sh` and `check_appver.py` allow-lists, release records under `boinc/infra/boinc/releases/`.
4. **Stager**: `qwen stage --page-mode` cutting regions from LoC's ALTO, packing small pages, deadline per page.
5. **Collector**, rolled before the server: reads per-region records into `crops.parquet` (`vlm_crop_state`), assembles held pages and `blocks.json`, loads `ours-xiaomi` outputs and `vlm_page_cost`.
6. **Server roll**: the `page-blocks-v1` validator behind its project file, for batch 109 only.
7. **Import, held off**: `usnm-xiaomi` source, engine tag, `engines.toml` entry at `production = false`, the per-language list, `PROCESSINGREFS` in `usnm-alto` and `check_alto`.
8. **Calibration** (slots), then the **pilot**, when the Compute Coordinator says. Open one class at a time over the named roots, as `native-qwen-cohort.php --plan-class` does today.
9. **Decision** by the Science Coordinator; publication only after it.

### Tests

- Region cutting: block split at the budget and the pixel cap at line boundaries, non-text blocks skipped, small blocks kept as LoC, ids and order stable.
- Assembly: line-for-line and reflowed fits; each LoC-kept reason; `PROCESSINGREFS` and `blocks.json` agree; the result parses with `usnm-alto` and passes `check_alto`; LoC's ALTO changing since staging is refused (as `lines::merge` does).
- Slot rule: the formula at small and large free memory, the floor and ceiling, halving after an out-of-memory start, `--ctx-size` equal to slots × per-slot context.
- Validator: `page-blocks-v1` at the boundaries, the empty guard first, a page with many agreeing and a few disagreeing regions validates, a page with a looping side.
- Acceptance: pages at, below and above `A`; with `A` unset nothing is accepted.
- Engine tag: builds and parses; `ocr-local` accepts `usnm-xiaomi`; the release build fails for a language not on the list.
- Selection query: a fixture database with known tenths, a Tesseract-fixed page left out, the 30-page floor, the trim, repeatability with the seed.
- Cost: token-share split in a multi-page unit, invalid and third replicas counted.

## Open questions for the Science Coordinator

1. **Acceptance share `A`**, by characters or by regions, and per band or one value. Proposed: set it from the pilot's agreeing-share distribution and the truth-page CER of agreeing against disagreeing regions.
2. **Disagreeing regions**: LoC only (proposed for the pilot), or into #1988's pooled re-issue once that is decided.
3. **Validity floor**: v2's floors without "any disagreement fails" (proposed), or #1988's 80% if that is adopted for samples too.
4. **"Fixed by Tesseract"**: a 0.02 damage drop on the newest output (proposed), or the release's replaced list, which also applies the completeness gate but is not in the database.
5. **Agreement rule** between quality_gain and blind picks: pooled and per band (proposed), or per decade too; and the calibration match rate the pickers must reach.
6. **Pilot pool**: the qe-v1 worst tenth (as asked), against #1867's `wf1415-eng`. The query records both.
7. **GPU memory reservation** with more than one slot: raise `gpu_ram_used_mb` per class, or cap slots at the reservation.
8. **Region budget and minimum size** (1,500 and 3 characters proposed), to be checked on the calibration pages.
9. **App and batch**: the same `usnm_qwen` app at batch 109 with new classes (proposed), or a separate app.
10. **Credit** for page units, which run 30 GPU-minutes or more at one slot.
11. **Word boxes**: whether proportional boxes without `WC` are enough, or #1907 needs alignment against LoC's word boxes first.
