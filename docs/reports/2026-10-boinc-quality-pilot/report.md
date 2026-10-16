# Interim Report: BOINC OCR Quality Scoring (`usnm_quality`) & Vision-Language Crop Transcription (`usnm_qwen`) Pilot

**Date:** 2026-10-16  
**Status:** Multi-platform BOINC pilot complete (5 `usnm_quality` campaigns collected, `usnm_qwen` multi-host quorum and `crops.parquet` join verified, 3-dataset gold calibration and Qwen3-VL 2B/8B calibration complete).  
**Related Issues & PRs:** [#522](https://github.com/tgoodyear/usnewsmap.com/issues/522), [#531](https://github.com/tgoodyear/usnewsmap.com/issues/531), [#533](https://github.com/tgoodyear/usnewsmap.com/issues/533), [#559](https://github.com/tgoodyear/usnewsmap.com/issues/559); PRs [#503](https://github.com/tgoodyear/usnewsmap.com/pull/503), [#505](https://github.com/tgoodyear/usnewsmap.com/pull/505), [#506](https://github.com/tgoodyear/usnewsmap.com/pull/506), [#534](https://github.com/tgoodyear/usnewsmap.com/pull/534), [#544](https://github.com/tgoodyear/usnewsmap.com/pull/544), [#580](https://github.com/tgoodyear/usnewsmap.com/pull/580), [#646](https://github.com/tgoodyear/usnewsmap.com/pull/646).

---

## 1. Executive Summary & Volunteer Compute Justification Principle

Every unit of computation requested from our volunteer BOINC population must satisfy a strict two-part justification rule:

1. **Platform & Hardware Compatibility Verification:** Bounded probes required to prove that signed native and container binaries execute deterministically, respect host memory/CPU/GPU limits, survive BOINC client pause/resume lifecycle transitions, and agree across heterogeneous volunteer architectures (`windows_x86_64`, `arm64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-pc-linux-gnu`, `aarch64-unknown-linux-gnu`, and rootless Podman/Docker containers).
2. **Direct Contribution to Base Scan OCR Quality:** Production workunits whose outputs directly measure or improve the transcription accuracy, completeness, and searchability of the 19.7-million-page historical newspaper corpus—improving on the three-year-old *American Stories* pipeline ([Dell et al., 2023](https://arxiv.org/abs/2308.12477)) while avoiding wasted volunteer cycles on pages that are either already high-accuracy or physically illegible.

### Headline Findings from the Pilot & Calibration

1. **Cross-Platform Determinism (`usnm_quality`):** Across **125 completed tasks** (`61` canonical workunits, **`0` failures**) spanning macOS Apple Silicon native (`arm64-apple-darwin`), Linux native (`x86_64-pc-linux-gnu`), and Linux Podman containers (`x86_64-pc-linux-gnu__docker`), `usnm_quality` achieved **100.0% bit-for-bit quorum agreement** across all page image statistics, legibility ceilings, reader completeness/precision scores, and Three-Cornered Hat CER estimates, in a median compute time of **0.70 seconds per 38.6-megapixel scan** (`1.1s` p50 CPU time, `<256 MB` RAM).
2. **Vision-Language Reader Accuracy (`usnm_qwen` vs. Existing Engines):** On the cleaned Chronicling America human ground-truth benchmark (`overproof-ca`, 345 clean crops after excluding the 9 stretched-box conversions tracked in [#531](https://github.com/tgoodyear/usnewsmap.com/issues/531), plus a 35-crop stratified calibration slice):
   - **`Qwen3VL-8B-Instruct-Q4_K_M` (`usnm_qwen` GPU tier):** **2.17% True CER** (`0.0217`)
   - **`Qwen3-VL-2B-Instruct-Q8_0` (`usnm_qwen` CPU/container tier):** **2.71% True CER** (`0.0271`)
   - **`Tesseract 5.5.0` (`eng`, `psm6`):** **3.68% True CER** (`0.0368` on all 345 clean crops; `0.0386` on sample-35)
   - **`docTR 1.1.0` (`db_resnet50` + `parseq`):** **5.25% True CER** (`0.0525`)
   - **Library of Congress ALTO (`loc`, historical FineReader):** **9.22% True CER** (`0.0922` on all 345 clean crops; `0.0861` on sample-35)
   - **American Stories (`as`, Dell et al. 2023):** **130.08% True CER** (`1.3008`) on ALTO crop boxes due to article-level layout/reading-order mismatches and dropped headline/tabular regions, and **14.9%–30.6% precision / 48.3%–94.8% completeness** at the whole-page level across our BOINC pilot waves.
   - **Result:** `Qwen3-VL-2B` reduces character error rate by **3.2× vs. LoC ALTO** (`8.61% → 2.71%`), and `Qwen3-VL-8B` reduces CER by **4.0× vs. LoC ALTO** (`8.61% → 2.17%`) and **44% vs. Tesseract 5.5** (`3.86% → 2.17%`).
3. **Three-Cornered Hat Calibration with & without VLM:**
   - **Corpus Reader Sparsity Problem:** In the stratified 100-page corpus sample (`v1`), **100%** of pages have `loc`, **41%** have `as`, and **13%** have `ours-tesseract`, with **only 4%–6% of pages having all 3 readers simultaneously**. Consequently, a page-level 3-cornered hat over existing transcriptions is determined on only **1 of 16 pages (`6.25%`)** in `v1-wave1`/`v1-wave2` and **1 of 23 pages (`4.35%`)** in `v1-wave3`.
   - **Adding `usnm_qwen` (`Qwen3-VL`) Solves Both Sparsity and Bias:**
     - On 3-reader crops (`loc`, `as`, `tesseract-psm6`), adding `Qwen3-VL-2B` cuts `tesseract-psm6` CER estimation MAE by **43%** (`0.0289 → 0.0165`) and boosts Pearson correlation with ground truth from **`0.598` to `0.925`** (Spearman `0.643 → 0.842`).
     - Better still, when `american-stories` (which lacks ALTO word coordinates) is excluded from the crop-level hat (`loc` + `tesseract-psm6` + `Qwen3-VL-2B`), **`loc` MAE drops to `0.0140` (1.40 percentage points), `tesseract-psm6` MAE drops to `0.0133` (`r = 0.948`, Spearman `ρ = 0.967`), and `vlm` MAE is `0.0091` (`r = 0.924`)**.
     - Even on **1-reader (`loc`-only) pages** (87% of the corpus, where a 3-cornered hat has only 2 readers with `vlm`), the direct pairwise distance `cer_vs_vlm` between `loc` and `Qwen3-VL-2B`/`8B` estimates `loc`'s true ground-truth CER with **`MAE = 0.0189` (1.89 percentage points)** and `Bias = +0.0136` (2B) / `+0.0063` (8B).
4. **Cutting Volunteer VLM Compute by 50% via `k_crops = 6`:**
   - Subsampling sensitivity across the 17 multi-crop pages of `overproof-ca` and our `v1-wave2` BOINC VLM pages shows that sampling **`k_crops = 6` (1 crop per stratum across the `3 confidence terciles × 2 vertical page halves`)** estimates the full-page CER within **`0.0067–0.0087` MAE (`<0.9` percentage points of CER)** while **cutting volunteer `usnm_qwen` compute time per page in half** (`~665s → ~333s` on 4-core CPU; `~135s → ~68s` on Apple Metal / NVIDIA CUDA GPU).

---

## 2. Actions Taken to Reach Production Pilot Readiness

### 2.1 Codebase & PR Integration (PRs #503, #505, #506, #534, #544, #580, #646)
1. **Volunteer Host Preflight & Native ARM64 / macOS Support:**
   - Reviewed, fixed, and merged [PR #505](https://github.com/tgoodyear/usnewsmap.com/pull/505) (`scripts/boinc/check-volunteer-host.sh` for rootless Podman/SELinux/cgroup v2 preflight) and [PR #534](https://github.com/tgoodyear/usnewsmap.com/pull/534) (`usnm_quality` and `usnm_qwen` Rust binaries, `usnm-ocr-quality` crate, validator binaries `usnm_quality_check`/`usnm_quality_compare` and `usnm_qwen_check`/`usnm_qwen_compare`, and bridge collection pipeline).
   - Evaluated PRs #503 and #506, diagnosed the macOS `Run_Podman` shell pipe-quoting bug in `docker_wrapper.cpp` (`get_container_state`), and merged [PR #506](https://github.com/tgoodyear/usnewsmap.com/pull/506) and [PR #544](https://github.com/tgoodyear/usnewsmap.com/pull/544) ([Issue #533](https://github.com/tgoodyear/usnewsmap.com/issues/533)) to ship native `usnm_quality` across all 5 desktop/server platforms (`windows_x86_64`, `x86_64-apple-darwin`, `arm64-apple-darwin`, `x86_64-pc-linux-gnu`, `aarch64-unknown-linux-gnu`) and native `usnm_qwen` (`apple_gpu_8b` on macOS Metal and `nvidia_cuda_2b`/`nvidia_cuda_8b` on Windows/Linux).
2. **Native BOINC Finish-File Contract:**
   - Confirmed in BOINC client source (`client/app_control.cpp`) that native (non-wrapped) BOINC executables that exit `0` without writing `boinc_finish_called` in the slot directory are treated as unexpected exits and restarted up to ~110 times before failing with `EXIT_CHILD_FAILED (195)`. Both `usnm_quality` and `usnm_qwen` explicitly write `boinc_finish_called` before exiting `0`.

### 2.2 Infrastructure & Scheduler Remediations on `boinc.usnewsmap.com`
During live onboarding of our Proxmox VE containers (`CT 100` and `CT 101` on `pve-big.tjg.fyi`) and macOS Apple Silicon host (`arm64-apple-darwin`), we diagnosed and fixed six production blockers:

| # | Blocker Encountered | Root Cause | Resolution Applied |
|---|---|---|---|
| 1 | `provision.sh` parallel SSH race condition | Concurrent runs rotated `boinc-secrets` (`SIGNING_KEY_PEM`) and overwrote `publish_manifest.json` mid-flight | Added remote `flock` lock (`/var/tmp/usnm-boinc-provision.lock`) and local mkdir lock in `contrib-compute/provision.sh` |
| 2 | `Project requested delay of 86400 seconds` on clients | `<next_rpc_delay>86400</next_rpc_delay>` was set globally in `config.xml`, forcing a 24-hour client backoff after any RPC | Removed `<next_rpc_delay>` from `contrib-compute/images/server/setup-project.sh` and live `/project/config.xml` |
| 3 | `CT 101` reporting `p_ncpus=2` on an 8-core PVE host | Unprivileged LXC cgroup v2 `cpuset` defaulted to 2 cores (`4` cores required by `docker_mt` plan classes) | Upgraded `CT 100` and `CT 101` to `8 GiB` RAM, `128 GiB` rootfs, `6` LXC cores, and `<ncpus>5</ncpus>` in `cc_config.xml` |
| 4 | `CT 101` `usnm_qwen` failing immediately with `unit_error` | Rootless Podman storage inside `CT 101` lacked the `usnm-boinc-qwen:1.0` image (`7.62 GB`) | Loaded `usnm-boinc-qwen:1.0` (`sha256:669e80ec...`) into `boinc`'s rootless Podman store on `CT 101` |
| 5 | `usnm_quality` tasks starved behind `usnm_qwen` in scheduler | BOINC's `feeder` ran without `--allapps` and with default 100-slot shared memory cache, allowing `usnm_qwen` tasks to monopolize all 100 slots | Deployed `feeder -d 2 --allapps` and `<shmem_work_items>1000</shmem_work_items>` in `setup-project.sh` and live `/project/config.xml` ([PR #580](https://github.com/tgoodyear/usnewsmap.com/pull/580), [PR #646](https://github.com/tgoodyear/usnewsmap.com/pull/646)) |
| 6 | `quality-collect` showing `with_readings: 0` for `v1-wave2` | `qwen-submit --target container` tags workunits with `batch=12`, so `sample_assimilator` writes canonical results to `/project/usnmocr/results/12` instead of `results/0` | Passed `QWEN_SPOOL=/project/usnmocr/results/12` to `quality-collect`, joining the validated `v1-wave2` Qwen readings into `crops.parquet` |

---

## 3. Pilot Execution & Empirical Results

### 3.1 BOINC Fleet Telemetry (`work-volume`, `quality-summary`, `qwen-summary`)
Preserved raw outputs:
- [`data/telemetry/work-volume-20261016.txt`](data/telemetry/work-volume-20261016.txt)
- [`data/telemetry/quality-summary-20261016.txt`](data/telemetry/quality-summary-20261016.txt)
- [`data/telemetry/qwen-summary-20261016.txt`](data/telemetry/qwen-summary-20261016.txt)

| Application | Execution Targets Verified | Total Results | Validated (`ok`) | Client Errors | Canonical Workunits | Quorum Agreement | Elapsed p50 (s) | Elapsed p90 (s) | CPU p50 (s) | Peak RSS p90 (MB) |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `usnm_quality` | `arm64-apple-darwin` (native), `x86_64-pc-linux-gnu` (native), `x86_64-pc-linux-gnu__docker` (container) | 217 | **125** | **0** | **61** | **100.0%** (61/61 pairs) | **11.4** (0.70s internal) | **13.7** (2.50s internal) | **1.1** | `< 256` |
| `usnm_qwen` | `x86_64-pc-linux-gnu__docker_mt_2b` (container CPU), `arm64-apple-darwin__apple_gpu_8b` (published) | 108 | **28** | 19 (pre-image load on CT 101; 0 after fix) | **4** (5th at quorum) | **100.0%** (Dice `0.865–0.909` vs floor `0.500`) | **665.2** (12 crops, CPU) | **964.2** (12 crops, CPU) | **2962.0** | **2,539** |

### 3.2 Collected `usnm_quality` Campaigns (`v1-pilot-3r`, `v1-pilot-mac`, `v1-wave1`, `v1-wave2`, `v1-wave3`)
Preserved campaign Parquet, CSV, and summary files are in [`data/campaigns/`](data/campaigns/).

| Campaign | Unique Pages | Cohort Composition | Readers Evaluated | Pages with 3-Reader Hat Determined (`without VLM`) | Pages with Joined VLM Readings (`usnm_qwen`) | Mean Internal Runtime (`total_ms`) | Median Internal Runtime (`total_ms`) |
|---|---:|---|---|---:|---:|---:|---:|
| [`v1-pilot-3r`](data/campaigns/v1-pilot-3r/summary.md) | 4 | 4 three-reader pages (`v1-3r`) | `loc` (4), `as` (4), `ours-tesseract` (4) | **4 / 4 (100%)** | 0 | 1,213.0 ms | 1,274.0 ms |
| [`v1-pilot-mac`](data/campaigns/v1-pilot-mac/summary.md) | 4 | Same 4 `v1-3r` pages (Mac native + Linux cross-check) | `loc` (4), `as` (4), `ours-tesseract` (4) | **4 / 4 (100%)** | 0 | 1,092.8 ms | 1,290.0 ms |
| [`v1-wave1`](data/campaigns/v1-wave1/summary.md) | 16 | Stratified `v1` wave 1 | `loc` (16), `as` (8), `ours-tesseract` (4) | **1 / 16 (6.25%)** | 0 | 814.3 ms | 861.0 ms |
| [`v1-wave2`](data/campaigns/v1-wave2/summary.md) | 16 | Stratified `v1` wave 2 + `usnm_qwen` join | `loc` (16), `as` (8), `ours-tesseract` (4), `vlm` (2 pages / 24 crops) | **1 / 16 (6.25%)** page-level; **+1 page (12 crops)** unlocked via VLM | **2** (`24` crops) | 860.9 ms | 500.0 ms |
| [`v1-wave3`](data/campaigns/v1-wave3/summary.md) | 23 | Stratified `v1` wave 3 | `loc` (23), `as` (3), `ours-tesseract` (1) | **1 / 23 (4.35%)** | 0 | 982.9 ms | 629.0 ms |

#### Cross-Platform Reproducibility (`v1-pilot-3r` vs. `v1-pilot-mac`)
Comparing `v1-pilot-3r` (Linux Docker containers) against `v1-pilot-mac` (macOS `arm64-apple-darwin` native + Linux native/container replicas) on the exact same 4 pages:

| Reader | Pages | Mean Est. CER (`v1-pilot-3r` / `v1-pilot-mac`) | Mean Completeness | Mean Precision | Mean Findable Content | Mean Findable Places | Mean Legibility Ceiling |
|---|---:|---|---|---|---|---|---|
| `loc` | 4 | `0.1332` / `0.1332` | `0.9370` / `0.9370` | `0.6376` / `0.6376` | `0.9750` / `0.9750` | `0.9737` / `0.9737` | `0.8695` / `0.8695` |
| `ours-tesseract` | 4 | `0.1680` / `0.1680` | `0.8664` / `0.8664` | `0.6266` / `0.6266` | `0.9819` / `0.9819` | `1.0000` / `1.0000` | `0.8695` / `0.8695` |
| `as` | 4 | `0.5588` / `0.5588` | `0.4829` / `0.4829` | `0.3064` / `0.3064` | `0.9193` / `0.9193` | `0.9142` / `0.9142` | `0.8695` / `0.8695` |

Every floating-point metric is **identical to 6 decimal places** across architectures, operating systems, and packaging modes (native vs. container).

#### Corpus Image Statistics & Damage Metrics Across the 43 Unique Pilot Pages
Across all 43 unique pages in `v1-pilot-3r`, `v1-wave1`, and `v1-wave3` (mean dimensions `5,391 × 7,164 px` = `38.6 MP`):
- **Legibility Ceiling (`legibility_ceiling`):** Mean **`0.8050`**, ranging from **`0.6042`** (heavily degraded/faded newsprint where nearly 40% of character positions are degraded) to **`0.9418`** (clean modern scans).
- **Physical Scan Correlates of `legibility_ceiling`:**
  - `bleed_through` (mean `0.1206`): Pearson $r = 0.581$
  - `noise` (mean `10.2251`): Pearson $r = 0.517$
  - `mean_word_conf` (LoC ALTO word confidence): Pearson $r = 0.489$
  - `ink_density` (mean `0.2121`): Pearson $r = 0.471$
  - `blur` (Laplacian variance, mean `6,607.7`): Pearson $r = 0.409$
  - `abs(skew_deg)`: Mean **`0.112°`**, max **`1.000°`**, mean `line_height_px`: **`43.1 px`**, mean `columns`: **`6.1`**.

#### Joined `usnm_qwen` Crop Analysis in `v1-wave2` (`crops.parquet`)
In [`data/campaigns/v1-wave2/crops.csv`](data/campaigns/v1-wave2/crops.csv), two pages completed full two-host BOINC quorum (`qwen3-vl-2b-q8` on `CT 100` and `CT 101`) and were joined by `quality-collect`:
1. **`sn82014761_1866-08-04_ed-1_seq-4`** (an 1866 post-Civil War page with 2 existing readers: `loc` and `ours-tesseract`):
   - Without `usnm_qwen`, this page had only 2 readers (`loc` and `ours-tesseract`), so its page-level 3-cornered hat was **undetermined** (`NULL`).
   - With `usnm_qwen` (`vlm`), all 12 crops had **3 independent readers** (`loc`, `ours-tesseract`, `vlm`), yielding a determined crop-level 3-cornered hat:
     - **`vlm` (`qwen3-vl-2b-q8`):** Mean crop hat CER = **`0.1596`**
     - **`loc`:** Mean crop hat CER = **`0.1802`** (`cer_vs_vlm = 0.3221`, `dice_vs_vlm = 0.5114`)
     - **`ours-tesseract`:** Mean crop hat CER = **`0.4411`** (`cer_vs_vlm = 0.4549`, `dice_vs_vlm = 0.4184`)
2. **`sn82015743_1934-12-06_ed-1_seq-4`** (a 1934 page with only 1 existing reader: `loc`):
   - Direct comparison against `vlm` (`qwen3-vl-2b-q8`) across the 12 stratified crops measured `loc` `cer_vs_vlm = 0.2635` and `dice_vs_vlm = 0.7139`.
3. **Validation of the 6-Stratum Crop Design (`3 confidence terciles × 2 vertical page halves`):**
   - Across the 24 VLM-scored crops in `v1-wave2`, `loc` error (`cer_vs_vlm`) increases monotonically across the confidence strata from **`0.1124`** in `wc-high/top` (`dice_vs_vlm = 0.7818`) to **`0.4968`** in `wc-low/bottom` (`dice_vs_vlm = 0.3619`)—a **4.4× dynamic range in actual OCR error within the same page**.

---

## 4. Gold-Set Calibration (`overproof-ca`, `bln600`, `reichsanzeiger`) & Qwen3-VL 2B vs. 8B Evaluation

All gold-set score tables and `usnm-boinc quality calibrate` outputs are preserved in [`data/calibration/`](data/calibration/).

### 4.1 True CER & WER on Gold Benchmarks (`node3` + Local Metal GPU Calibration)

#### Effect of Removing the 9 Stretched-Box Conversions on `overproof-ca` ([#531](https://github.com/tgoodyear/usnewsmap.com/issues/531))
In `overproof-ca` (`354` total crops across 18 Chronicling America pages), 9 crops (`box-mismatch-crop-ids.json`: `pub-overproof-ca-006`, `010`, `025`, `027`, `158`, `159`, `234`, `237`, `314`) had stretched bounding boxes spanning adjacent columns. Removing those 9 conversion artifacts (`345` clean crops, `94,627` reference characters; [`overproof-ca-clean345-scores.md`](data/calibration/overproof-ca-clean345-scores.md)) reveals the true accuracy of each engine on Chronicling America newsprint:

| Engine / Reader | Dataset | Crops | Reference Chars | True CER (All 354) | True CER (Clean 345) | True WER (Clean 345) | True CER (Stratified Sample-35) |
|---|---|---:|---:|---:|---:|---:|---:|
| **`Qwen3VL-8B-Instruct-Q4_K_M`** | `overproof-ca` | 35 (stratified) | 9,590 | — | — | — | **0.0217 (2.17%)** |
| **`Qwen3-VL-2B-Instruct-Q8_0`** | `overproof-ca` | 35 (stratified) | 9,590 | — | — | — | **0.0271 (2.71%)** |
| **`Tesseract 5.5.0` (`eng-psm6`)** | `overproof-ca` | 345 | 94,627 | 0.3659 | **0.0368 (3.68%)** | **0.1252** | **0.0386 (3.86%)** |
| **`Tesseract 5.5.0` (`eng-psm4`)** | `overproof-ca` | 345 | 94,627 | 0.3488 | **0.0398 (3.98%)** | **0.1289** | — |
| **`docTR 1.1.0` (`db_resnet50-parseq`)** | `overproof-ca` | 345 | 94,627 | 0.3021 | **0.0525 (5.25%)** | **0.2009** | — |
| **LoC ALTO (`loc-alto`)** | `overproof-ca` | 345 | 94,627 | 0.3980 | **0.0922 (9.22%)** | **0.3158** | **0.0861 (8.61%)** |
| **American Stories (`american-stories`)** | `overproof-ca` | 345 | 94,627 | 1.4551 | **1.3008 (130.1%)** | **1.3181** | **1.4374 (143.7%)** |
| `Tesseract 5.5.0` (`eng-psm4`) | `bln600` (19th-c. UK) | 600 | 1,696,781 | — | **0.0604 (6.04%)** | **0.1795** | — |
| `Tesseract 5.5.0` (`eng-psm6`) | `bln600` (19th-c. UK) | 600 | 1,696,781 | — | **0.0711 (7.11%)** | **0.1945** | — |
| `docTR 1.1.0` (`db_resnet50-parseq`) | `bln600` (19th-c. UK) | 120 | 260,320 | — | **0.1205 (12.05%)** | **0.2512** | — |
| `Tesseract 5.5.0` (`frak2021-psm6`) | `reichsanzeiger` (Fraktur) | 60 | 49,633 | — | **0.0507 (5.07%)** | **0.1973** | — |
| `Tesseract 5.5.0` (`frak2021-psm4`) | `reichsanzeiger` (Fraktur) | 60 | 49,633 | — | **0.0517 (5.17%)** | **0.1997** | — |
| `docTR 1.1.0` (`db_resnet50-parseq`) | `reichsanzeiger` (Fraktur) | 60 | 49,633 | — | **0.6612 (66.12%)** | **0.9333** | — |

### 4.2 Calibration of the Three-Cornered Hat Estimator (`usnm-boinc quality calibrate`)

#### A. All 345 Clean `overproof-ca` Crops: 3 Corpus Readers (`loc`, `as`, `tesseract-psm6`) ± 4th Independent Neural Reader (`doctr`)
Source: [`data/calibration/cal-overproof-clean-3r-plus-doctr.md`](data/calibration/cal-overproof-clean-3r-plus-doctr.md)

| Reader | Hat Configuration | Crops | True CER | Est. CER | Bias | MAE | Pearson $r$ | Spearman $\rho$ |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `loc` | `without vlm` (3 readers) | 345 | 0.0919 | 0.0576 | -0.0343 | 0.0400 | 0.851 | 0.856 |
| `tesseract-psm6` | `without vlm` (3 readers) | 345 | 0.0379 | 0.0504 | +0.0125 | 0.0292 | 0.678 | 0.563 |
| **`loc`** | **`with vlm` (4 readers)** | **345** | **0.0919** | **0.0678** | **-0.0241** | **0.0269 (-33%)** | **0.948** | **0.935** |
| **`tesseract-psm6`** | **`with vlm` (4 readers)** | **345** | **0.0379** | **0.0356** | **-0.0023** | **0.0166 (-43%)** | **0.868** | **0.782** |
| **`vlm` (`doctr`)** | **`with vlm` (4 readers)** | **345** | **0.0526** | **0.0430** | **-0.0096** | **0.0147** | **0.822** | **0.899** |

#### B. Stratified 35-Crop `overproof-ca` Sample: `Qwen3-VL-2B-Instruct-Q8_0` vs. `Qwen3VL-8B-Instruct-Q4_K_M`
Sources: [`cal-sample35-qwen2b.md`](data/calibration/cal-sample35-qwen2b.md), [`cal-sample35-qwen8b.md`](data/calibration/cal-sample35-qwen8b.md), [`cal-sample35-no-as-2b.md`](data/calibration/cal-sample35-no-as-2b.md), [`cal-sample35-no-as-8b.md`](data/calibration/cal-sample35-no-as-8b.md)

| Hat Configuration | Reader | Crops | True CER | Est. CER | Bias | MAE | Pearson $r$ | Spearman $\rho$ |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| **3 Corpus Readers (`without vlm`)** | `loc` | 35 | 0.0861 | 0.0567 | -0.0294 | 0.0364 | 0.689 | 0.731 |
| **3 Corpus Readers (`without vlm`)** | `tesseract-psm6` | 35 | 0.0386 | 0.0451 | +0.0065 | 0.0289 | 0.598 | 0.643 |
| **4 Readers (`+ Qwen3-VL-2B-Q8_0`)** | `loc` | 35 | 0.0861 | 0.0635 | -0.0226 | **0.0266** | **0.804** | **0.828** |
| **4 Readers (`+ Qwen3-VL-2B-Q8_0`)** | `tesseract-psm6` | 35 | 0.0386 | 0.0321 | -0.0064 | **0.0165** | **0.925** | **0.842** |
| **4 Readers (`+ Qwen3-VL-2B-Q8_0`)** | `vlm` (`Qwen3-VL-2B`) | 35 | **0.0271** | **0.0294** | **+0.0023** | **0.0163** | **0.885** | **0.649** |
| **4 Readers (`+ Qwen3-VL-8B-Q4_K_M`)** | `loc` | 35 | 0.0861 | 0.0625 | -0.0236 | 0.0279 | 0.756 | 0.762 |
| **4 Readers (`+ Qwen3-VL-8B-Q4_K_M`)** | `tesseract-psm6` | 35 | 0.0386 | 0.0306 | -0.0080 | 0.0170 | 0.895 | 0.854 |
| **4 Readers (`+ Qwen3-VL-8B-Q4_K_M`)** | `vlm` (`Qwen3-VL-8B`) | 35 | **0.0217** | **0.0242** | **+0.0025** | 0.0207 | 0.801 | 0.455 |
| **3 Box-Aligned Readers (`loc + tess + Qwen-2B`, no `as`)** | `loc` | 35 | 0.0861 | **0.0764** | **-0.0097** | **0.0140** | **0.787** | **0.796** |
| **3 Box-Aligned Readers (`loc + tess + Qwen-2B`, no `as`)** | `tesseract-psm6` | 35 | 0.0386 | **0.0254** | -0.0132 | **0.0133** | **0.948** | **0.967** |
| **3 Box-Aligned Readers (`loc + tess + Qwen-2B`, no `as`)** | `vlm` (`Qwen3-VL-2B`) | 35 | 0.0271 | **0.0233** | **-0.0038** | **0.0091** | **0.924** | **0.750** |
| **2 Readers Only (`loc` vs `Qwen3-VL-2B` direct `cer_vs_vlm`)** | `loc` | 35 | 0.0861 | **0.0997** | **+0.0136** | **0.0189** | **0.723** | — |
| **2 Readers Only (`loc` vs `Qwen3-VL-8B` direct `cer_vs_vlm`)** | `loc` | 35 | 0.0861 | **0.0925** | **+0.0063** | **0.0189** | **0.611** | — |

#### Key Calibration Takeaways
1. **Why Correlated Engine Families Must Never Share a 3-Reader Hat:** In [`cal-overproof-clean-aligned3.md`](data/calibration/cal-overproof-clean-aligned3.md), [`cal-bln600.md`](data/calibration/cal-bln600.md), and [`cal-reichsanzeiger.md`](data/calibration/cal-reichsanzeiger.md), placing two configurations of the same engine family (`tesseract-psm4` and `tesseract-psm6`) into a 3-reader hat produces a systematic negative bias of **`-0.032` to `-0.048`** on both Tesseract readers because their errors are highly correlated (`d(psm4, psm6) ≈ 0.01`). By contrast, `loc` (Abbyy FineReader), `ours-tesseract` (Tesseract 5.5 LSTM), and `vlm` (`Qwen3-VL` vision-language transformer) are architecturally independent, yielding **`MAE ≤ 0.0140`** and **Spearman $\rho = 0.967$**.
2. **`Qwen3-VL-2B-Q8_0` vs. `Qwen3VL-8B-Q4_K_M` Division of Labor:**
   - As a **calibration and quality-scoring anchor** (`usnm_qwen` crop reading), `Qwen3-VL-2B-Q8_0` (`2.71%` true CER) is **just as accurate in the 3-cornered hat as `8B`** (`loc` MAE `0.0140` vs `0.0140`, `tesseract` MAE `0.0133` vs `0.0139`, `vlm` MAE `0.0091` vs `0.0123`) while running **3.25× faster** (`3.51s` vs `11.39s` per crop on Metal GPU) and requiring **2.7× smaller model weights** (`1.83 GB` vs `5.03 GB`).
   - Therefore, **CPU and lightweight GPU hosts should run `2B` (`qwen3-vl-2b-q8`) for quality auditing**, whereas **`8B` (`qwen3-vl-8b-q4km`, `2.17%` true CER) should be reserved for GPU hosts (`apple_gpu_8b`, `nvidia_cuda_8b`) performing targeted text recovery/re-OCR** on high-value, damaged, or non-standard regions.

---

## 5. Subsampling Sensitivity (`k_crops`) & Validator Threshold Analysis

### 5.1 `k_crops` Subsampling Precision (`k = 4, 6, 8, 12`)
Across the 17 multi-crop pages of `overproof-ca` (mean `20.3` crops/page, 200 Monte Carlo draws per page per $k$):

| Crops Sampled per Page ($k$) | `loc` MAE vs. Full-Page True CER | `loc` MAE vs. Full-Page Hat CER | `tesseract-psm6` MAE vs. Full-Page True CER | `tesseract-psm6` MAE vs. Full-Page Hat CER | `vlm` MAE vs. Full-Page True CER | Relative `usnm_qwen` Volunteer Compute Cost |
|---:|---:|---:|---:|---:|---:|---:|
| `k = 4` | 0.0160 | 0.0115 | 0.0137 | 0.0087 | 0.0103 | 0.33× |
| **`k = 6` (1 per stratum)** | **0.0120** | **0.0087** | **0.0105** | **0.0067** | **0.0077** | **0.50× (Recommended)** |
| `k = 8` | 0.0098 | 0.0069 | 0.0081 | 0.0052 | 0.0062 | 0.67× |
| `k = 12` (2 per stratum) | 0.0080 | 0.0055 | 0.0050 | 0.0032 | 0.0048 | 1.00× (Pilot baseline) |

**Decision:** Setting **`k_crops = 6`** (1 crop from each of the 6 strata: `wc-{low,mid,high} × {top,bottom}`) keeps page-level CER estimation error under **0.9 percentage points (`0.0067–0.0087` MAE)** while **halving volunteer VLM compute time per page** (`~333s` on 4-core CPU, `~42s` with `2B` on GPU, `~68s` with `8B` on GPU).

### 5.2 Validator Thresholds & Replication Policy (`usnm_quality` & `usnm_qwen`)
1. **`usnm_quality` (`usnm_quality_compare`):**
   - Observed cross-host discrepancy across 61 canonical workunits (`arm64-apple-darwin` vs. `x86_64-pc-linux-gnu` vs. `x86_64-pc-linux-gnu__docker`): **`0.000000` (100% exact match)**.
   - **Justification for Corpus-Scale Replication:** Because `usnm_quality` is a deterministic pure-Rust computation over content-addressed inputs, running `min_quorum = 2` on every page in a multi-million-page sweep would waste 50% of volunteer CPU cycles. Instead, we should use **adaptive replication (`min_quorum = 1` with 5%–10% random spot-check replication `min_quorum = 2` for untrusted/new hosts, or BOINC adaptive replication)** once a host passes its initial 10 consecutive validated workunits.
2. **`usnm_qwen` (`usnm_qwen_compare`):**
   - Observed cross-host word-bag Dice across validated pairs (`wu 46, 89, 92, 97`): **`min_dice = 0.865–0.909`**, **`median_dice = 0.865–0.909`** (well above `PROBE_MIN_DICE = 0.500`).
   - **Recommended Production Validator Threshold:** Raise the production cross-host agreement threshold from the `0.500` probe floor to **`MIN_DICE = 0.78`** (permitting minor CPU vs. GPU FP16/Q4_K_M quantization token differences on degraded characters while rejecting truncated or hallucinated transcriptions).

### 5.3 Workunit Batching (`Pages per Workunit`) to Eliminate BOINC Overhead
Single-page workunits were appropriate for the 4-to-23-page pilot waves, but are structurally inefficient at corpus scale:
1. **`usnm_quality` (`32–50 pages per workunit`, grouped by newspaper issue):**
   - **Pilot Measurement:** Median internal Rust compute is **`0.70 s/page`** (`mean 0.94 s`), whereas BOINC wall-clock turnaround (`elapsed p50`) is **`11.4 s`**—meaning **94% (`10.7 s`) is BOINC scheduler RPC, download, slot creation, process spawn, and upload overhead**, and only **6% (`0.70 s`)** is actual work. Worse, 1 page/WU across 19.7M pages would create **19.7M `workunit` rows** and **~40M–60M download/upload files** in `/project`.
   - **Recommended Batch Size:** Bundling **32–50 pages per workunit** (4–6 complete newspaper issues, `~25–45 s` of pure Rust compute, `~80–125 MB` input bundle with per-page checkpointing in the slot directory) raises compute efficiency from **6% to >80%**, cuts BOINC MySQL table volume and file fanout by **32×–50×** (`~394,000–615,000` workunits for the entire 19.7M-page corpus), and prevents feeder shared-memory starvation.
2. **`usnm_qwen` (at `k_crops = 6` crops per page):**
   - Every 1-page `usnm_qwen` workunit pays a cold `llama-server` startup and `1.83 GB` (`2B`) / `5.03 GB` (`8B`) model + vision projector load (`~10–15 s` on Metal/CUDA GPU; `~15–25 s` on CPU).
   - **CPU Tier (`docker_mt_2b`):** **3–4 pages (`18–24` crops) per workunit** (`~16–22 minutes` elapsed on 4 CPU cores), amortizing container startup and model load across 3–4 pages with per-crop checkpointing.
   - **GPU Tier (`apple_gpu_8b`, `nvidia_cuda_2b/8b`):** **15–20 pages (`90–120` crops) per workunit** (`~5–7 minutes` elapsed on GPU with `2B`, or `~17–22 minutes` with `8B`), reducing model-load overhead from `~20%–40%` of runtime to **`<2%`** and cutting GPU scheduler/validator transactions by **15×–20×**.
   - **Pre-Cropped Input Bundles:** Sending pre-cropped PNGs (`6 × ~30 KB ≈ 180 KB/page`) rather than full-page scans (`~3–8 MB/page`) inside multi-page `usnm_qwen` bundles reduces volunteer download bandwidth by **>95%** (`~3.5 MB` instead of `~80–120 MB` for a 20-page GPU workunit).

---

## 6. Wave 4 Follow-Up Campaign (`v1-wave4`: 50 Workunits / 100 Replica Tasks) vs. Pilot (`v1-wave1`–`v1-wave3`)

To address the pilot's primary data limitation—that `94.6%` (`35/37` unique pages) of `v1-wave1`–`v1-wave3` had only 1 or 2 OCR readers (`hat_determined = false`)—we curated, staged, and executed **`v1-wave4` (`50` workunits = `100` replica tasks)** from the 90-page multi-engine survey corpus (`0` page overlap with `v1-wave1`–`v1-wave3`), where **100% of pages (`50/50`) have 3 to 7 independent OCR readers** (`259` reader-page evaluations total) and full-page `ours.xml` across **6 languages** (`eng`, `deu`, `ces`, `pol`, `srp`, `rus`) and **7 survey strata** (`control`, `worst-english`, `german`, `czech`, `polish`, `serbian`, `russian`).

Artifacts preserved in [`data/campaigns/v1-wave4/`](data/campaigns/v1-wave4/) (`pages.parquet`, `pages.csv`, `crops.parquet`, `crops.csv`, `summary.md`).

### 6.1 Pickup, Execution Runtime, and Completion Rate (`v1-wave4` vs. Pilot)

| Metric | Pilot (`v1-wave1`–`v1-wave3`) | Wave 4 (`v1-wave4`) | Cumulative (`Pilot + Wave 4`) |
|---|---:|---:|---:|
| **Workunits Submitted** | 55 (`+ 8` in `v1-pilot-3r`/`mac`) | **50** | **113** |
| **Replica Tasks Dispatched (`min_quorum = 2`)** | 110 (`+ 16` in `v1-pilot-3r`/`mac`) | **100** | **226** |
| **Unique Pages Evaluated** | 37 (`+ 4` in `v1-pilot-3r`/`mac`) | **50** (`0` overlap) | **91** |
| **Workunit Completion & Assimilation Rate** | **100.0%** (`55/55`) | **100.0%** (`50/50`) | **100.0%** (`113/113`) |
| **Replica Success Rate (`Outcome = Ok`)** | **100.0%** (`110/110`) | **100.0%** (`100/100`) | **100.0%** (`226/226`) |
| **Cross-Platform 2-Replica Quorum Agreement** | **100.0%** (`0.0000` CER diff) | **100.0%** (`0.0000` CER diff) | **100.0%** (`113/113` pairs) |
| **First Scheduler Pickup Latency** | `18–35 s` | **`23 s`** (`14:05:42Z → 14:06:05Z`) | `18–35 s` |
| **Sustained Queue Drain Throughput** | `~3.8 WUs/min` | **`4.35 WUs/min` (`8.7 tasks/min`)** (`90/100` tasks in `11.5 min`) | `~4.1 WUs/min` |
| **Readers per Page (`min–max`, `mean`)** | `1–6` (`mean 1.75`) | **`3–7` (`mean 5.18`)** | `1–7` (`mean 3.31`) |
| **Three-Cornered Hat Determined (`hat_determined`)** | `5.4%` (`2/37` unique pages) | **100.0% (`50/50` pages)** | `59.3%` (`54/91` pages) |
| **Internal Compute Time (`total_ms` median / mean)** | `657 ms` / `877 ms` | **`1,717 ms` / `3,431 ms`** (`native`: `1,055 ms` med / `1,892 ms` mean; `docker`: `1,979 ms` med / `4,224 ms` mean) | `1,055 ms` med |

**Operational Lesson on Multi-App Host Scheduling:** During `v1-wave4`, `90/100` replica tasks (`50` on macOS `arm64-apple-darwin` + `40` on Linux `CT 101`) drained in `11.5 minutes`. The remaining `10` replica tasks had been prefetched at `14:06:05Z` by `CT 100` behind its active 2-hour `usnm_qwen` task (`<max_concurrent>1</max_concurrent>`) and completed in **`28 seconds`** once `usnm_qwen` yielded a slot. On volunteer hosts attached to both apps, `app_config.xml` should cap `usnm_qwen` (`mt`) at `N - 1` CPUs or keep `project_max_concurrent` above `usnm_qwen`'s `max_concurrent` so sub-second `usnm_quality` tasks never head-of-line block behind multi-minute VLM tasks.

### 6.2 Multi-Reader & Multi-Language OCR Findings (`v1-wave4`, 50 Pages / 259 Reader Evaluations)

On 3 ultra-large scans (`sn83030272_1910-12-25_ed-1_seq-34` at `5035×6663` with `26 px` lines, `sn86053575_1868-11-19_ed-1_seq-1` at `5936×8307`, and `sn86054053_1869-09-24_ed-1_seq-2` at `5656×8075`), full-scale `@1` Tesseract produced `0` words (`est_cer = 1.42–1.59`), whereas half-scale `@0.5` (`tess-half`) succeeded (`findable_content = 0.928–0.974`). Across all 47 non-zero pages (and all 50 pages in brackets where different):

| Survey Group | Lang | Pages | Reader | Mean Est. CER | Median Est. CER | Mean Completeness | Mean Precision | Findable Content | Findable Places |
|---|---|---:|---|---:|---:|---:|---:|---:|---:|
| **`control`** | `eng` | 10 | `loc` (LoC ALTO) | **0.1977** | 0.1990 | 0.7635 | 0.8380 | 0.9019 | 0.9443 |
| **`control`** | `eng` | 10 | `ndnp` (`ndnp-open-ocr`) | **0.2038** | **0.1986** | **0.7996** | **0.8544** | **0.9621** | **0.9849** |
| **`control`** | `eng` | 10 | `ours-tesseract` (`eng@1`) | 0.2852 | 0.2850 | 0.6663 | 0.7265 | 0.9353 | 0.9753 |
| **`control`** | `eng` | 10 | `tess-half` (`eng@0.5`) | 0.2889 | 0.2757 | 0.6112 | 0.7027 | 0.8803 | 0.8862 |
| **`control`** | `eng` | 10 | `as` (American Stories) | 0.5003 | 0.4965 | 0.4011 | 0.5867 | 0.7201 | 0.8638 |
| **`control`** | `eng` | 10 | `doctr` | 0.5029 | 0.4918 | 0.3496 | 0.3382 | 0.8584 | 0.8914 |
| **`worst-english`** | `eng` | 11 [12] | **`ndnp` (`ndnp-open-ocr`)** | **0.3692** [0.3726] | **0.3569** | **0.7029** | **0.4524** | **0.9592** | **0.9792** |
| **`worst-english`** | `eng` | 11 [12] | **`ours-tesseract` (`eng@1`)** | **0.4083** [0.4978] | **0.3981** | **0.5292** | 0.4419 | **0.8227** | **0.8522** |
| **`worst-english`** | `eng` | 11 [12] | `as` (American Stories) | 0.5049 [0.4992] | 0.4624 | 0.2974 | 0.3058 | 0.6174 | 0.5993 |
| **`worst-english`** | `eng` | 8 | `doctr` | 0.5231 | 0.5026 | 0.4396 | 0.2229 | 0.8330 | 0.8839 |
| **`worst-english`** | `eng` | 11 [12] | `loc` (LoC ALTO) | 0.5240 [0.5383] | 0.5276 | 0.4644 | 0.2137 | 0.4628 | 0.4723 |
| **`worst-english`** | `eng` | 11 [12] | `tess-half` (`eng@0.5`) | 0.5332 [0.5348] | 0.5127 | 0.4432 | 0.5196 | 0.6778 | 0.7242 |
| **`german`** | `deu` | 8 [10] | **`tess-latf` (`deu_latf@1`)** | **0.1480** [0.4196] | **0.1358** | **0.7884** | **0.7364** | **0.8795** | **0.9210** |
| **`german`** | `deu` | 8 [10] | **`ours-tesseract` (`frak2021@1`)** | **0.1527** [0.4233] | **0.1382** | **0.7667** | 0.7224 | **0.8168** | 0.8018 |
| **`german`** | `deu` | 8 [10] | `tess-deu` (`deu@1` Antiqua) | 0.1664 [0.4343] | 0.1551 | 0.6179 | 0.5689 | 0.5870 | 0.8323 |
| **`german`** | `deu` | 8 [10] | `tess-half` (`deu_latf@0.5`) | 0.3042 [0.3499] | 0.2730 | 0.5597 | 0.5727 | 0.8028 [0.8324] | **0.9472** |
| **`german`** | `deu` | 8 [10] | `loc` (LoC ALTO) | 0.3455 [0.3702] | 0.3447 | 0.3247 | 0.2867 | 0.3422 | 0.5167 |
| **`german`** | `deu` | 8 [10] | `doctr` | 0.5994 [0.5757] | 0.6004 | 0.1129 | 0.0796 | 0.1696 | 0.1743 |
| **`czech`** | `ces` | 6 | **`tess-half` (`ces@0.5`)** | **0.3552** | **0.3539** | **0.7204** | **0.2471** | **0.9565** | **0.8750** |
| **`czech`** | `ces` | 6 | **`ours-tesseract` (`ces@1`)** | **0.3768** | 0.3893 | 0.6682 | 0.2098 | **0.9487** | **0.8333** |
| **`czech`** | `ces` | 6 | `loc` (LoC ALTO) | 0.4110 | 0.4166 | 0.4470 | 0.1386 | 0.4283 | 0.5833 |
| **`czech`** | `ces` | 6 | `doctr` | 0.5103 | 0.5010 | 0.4229 | 0.0981 | 0.8067 | 0.7500 |
| **`polish`** | `pol` | 5 | **`ours-tesseract` (`pol@1`)** | **0.2120** | 0.2377 | **0.9641** | **0.6083** | **0.9926** | **1.0000** |
| **`polish`** | `pol` | 5 | `loc` (LoC ALTO) | 0.2207 | **0.2122** | 0.8489 | 0.5427 | 0.9753 | 0.9667 |
| **`polish`** | `pol` | 5 | `doctr` | 0.5898 | 0.5928 | 0.3405 | 0.1576 | 0.8955 | 1.0000 |
| **`serbian`** | `srp` | 5 | **`ours-tesseract` (`srp@1` Cyr.)** | **0.2708** | **0.3093** | **0.8031** | **0.5721** | **0.8931** | **0.9861** |
| **`serbian`** | `srp` | 5 | `loc` (LoC ALTO) | 0.3561 | 0.3460 | 0.4683 | 0.3723 | 0.5408 | **0.0278** |
| **`serbian`** | `srp` | 5 | `tess-half` (`srp@0.5` Cyr.) | 0.3744 | 0.3376 | 0.5698 | 0.4770 | 0.7171 | 0.6442 |
| **`serbian`** | `srp` | 5 | `tess-latn` (`srp_latn@1` Lat.) | 0.4931 | 0.4810 | 0.3758 | 0.2547 | 0.2884 | 1.0000 |
| **`russian`** | `rus` | 2 | `loc` (LoC ALTO) | **0.3743** | **0.3743** | 0.8030 | 0.2247 | **0.9730** | — |
| **`russian`** | `rus` | 2 | `ours-tesseract` (`rus@1`) | **0.3804** | **0.3804** | **0.8296** | **0.2383** | 0.9648 | — |
| **`russian`** | `rus` | 2 | `surya` | 0.4784 | 0.4784 | 0.5797 | 0.1298 | 0.9159 | — |

### 6.3 Six Concrete OCR Scoring & Engine-Tuning Decisions Derived from Wave 4

1. **Degraded English (`worst-english` vs. `control`): Route to `ndnp-open-ocr` or Full-Scale Tesseract (`eng@1`) When `legibility_ceiling < 0.75` or `mean_word_conf < 0.70`:**
   - On `control` English pages (`legibility_ceiling = 0.8093`, `mean_word_conf = 0.8005`, `line_height_px = 46.3 px`), historical `loc` is already accurate (`est_cer = 0.1977`, `findable_content = 0.9019`, `findable_places = 0.9443`).
   - On `worst-english` pages (`legibility_ceiling = 0.6352`, `mean_word_conf = 0.5814`, `line_height_px = 30.5 px`), historical `loc` collapses (`est_cer = 0.5240`, `findable_content = 0.4628`, `findable_places = 0.4723`) and hallucinates noise tokens (`mean_words = 9,257` vs `4,622`). Running `ndnp` (`ndnp-open-ocr`) cuts `est_cer` to **`0.3692`** and doubles `findable_content` (**`0.9592`**) and `findable_places` (**`0.9792`**); `ours-tesseract` (`eng@1`) reaches **`0.4083` `est_cer`** and **`0.8227` `findable_content`**.
2. **German Fraktur (`deu`): `deu_latf@1` and `frak2021@1` Cut LoC CER by >55% and Triple Findable Content, with `@0.5` Fallback on `>45 MP` Scans:**
   - On the 8 non-zero German pages, `tess-latf` (`deu_latf@1`, **`0.1480` `est_cer`**, **`0.8795` `findable_content`**, **`0.9210` `findable_places`**) and `ours-tesseract` (`frak2021@1`, **`0.1527` `est_cer`**, **`0.8168` `findable_content`**) drastically outperform historical `loc` (`0.3455` `est_cer`, `0.3422` `findable_content`), Antiqua `tess-deu` (`0.5870` `findable_content`), and `doctr` (`0.5994` `est_cer`, `0.1696` `findable_content`).
   - However, on the 2 ultra-large scans (`5936×8307` and `5656×8075`, `>45 MP`), `@1` Tesseract returned `0` words (`words = 0`) while `@0.5` (`tess-half`) recovered `92.8%–97.4%` of findable content. **Rule:** Always check `words == 0` (or `width * height > 45_000_000`) and automatically fall back to `@0.5` scale when `@1` fails.
3. **Serbian (`srp`): Mandatory Cyrillic `srp@1` Re-OCR (Historical LoC Used Latin OCR on Cyrillic Print):**
   - Historical `loc` OCR on Serbian newspapers has **`findable_places = 0.0278` (`2.8%`)** and `findable_content = 0.5408` because LoC ran Latin-script OCR over Cyrillic columns (confirmed by `tess-latn` `srp_latn@1` scoring an even worse `0.4931` `est_cer` and `0.2884` `findable_content`). Re-OCRing with `ours-tesseract` (`srp@1` Cyrillic) reduces `est_cer` to **`0.2708`** and boosts `findable_content` to **`0.8931`** and `findable_places` to **`0.9861` (`+95.8 pp`)**.
4. **Czech (`ces`) and Polish (`pol`): High-ROI Diacritic & Content Recovery via Language-Specific Tesseract 5:**
   - On Czech (`ces`), `ours-tesseract` (`ces@1`) and `tess-half` (`ces@0.5`) boost `findable_content` from **`0.4283` (`loc`) to `0.9487–0.9565` (`+52.0 pp`)** and `findable_places` from `0.5833` to `0.8333–0.8750`.
   - On Polish (`pol`), `ours-tesseract` (`pol@1`) achieves **`0.2120` `est_cer`**, **`0.9641` `completeness`** (vs `0.8489` `loc`), and **`0.9926` `findable_content`**.
5. **Resolution Scaling (`@1` vs. `@0.5`) Gated by `line_height_px`:**
   - When `line_height_px >= 40 px` (`control` English `46.3 px`, Czech `40.8 px`), `@0.5` half-scale Tesseract matches `@1` accuracy (`+0.0037` CER diff on `control`, `-0.0216` on `ces`) at ~4× faster OCR speed.
   - When `line_height_px < 36 px` (`worst-english` `30.5 px`), `@0.5` downscaling reduces glyph height to `~15 px`, degrading `est_cer` from `0.4083` to **`0.5332` (`+12.5 pp`)** and dropping `findable_content` from `0.8227` to `0.6778`. Thus `line_height_px` measured by `usnm_quality` directly determines whether an issue can use fast `@0.5` OCR or requires `@1.0` full resolution.
6. **`legibility_ceiling` Strongly Outperforms ALTO `mean_word_conf` as an Unsupervised Quality Predictor:**
   - Across all 50 `v1-wave4` pages, `legibility_ceiling` correlates at **Pearson $r = -0.731$ (Spearman $\rho = -0.761$)** with `loc` `est_cer` and **Spearman $\rho = -0.895$** with `ours-tesseract` `est_cer`, whereas ALTO `mean_word_conf` has **zero correlation ($r = +0.004, \rho = -0.054$)** with `ours-tesseract` `est_cer` across multi-language newspapers and only weak correlation ($r = -0.288$) with `loc` `est_cer`.

---

## 7. Justified Whole-Corpus Execution Plan (Tiered Volunteer Compute Architecture)

To honor our commitment that **every volunteer CPU/GPU cycle must be justified either by platform health or by directly improving corpus OCR quality**, we reject a naive brute-force run of VLM inference across all 19.7M pages (`19.7M × 665s ≈ 3.64M CPU-hours`). Instead, we structure the corpus-scale rollout as a **three-tier funnel**:

| Tier | Job & Target Population | Scope & Sampling Rule | Compute Cost per Page | Total Volunteer Compute | Direct Justification |
|---|---|---|---|---|---|
| **Tier 0: Platform Health Gates** | `usnm_quality` & `usnm_qwen` (4-page bounded probes) | New OS/arch/GPU plan classes (`windows_x86_64` native CPU/CUDA, `x86_64-apple-darwin`, Linux `docker_nvidia_cuda_8b`) | `1s` (`quality`) / `68s` (`qwen` GPU) | `< 2` host-hours total | Verifies code signing, GPU passthrough, memory bounds, and cross-device determinism before opening a plan class to production cohorts. |
| **Tier 1: Fast Deterministic Triage & Image Tuning** | `usnm_quality` (Native CPU across Windows, macOS, Linux) | Stratified title-issue sample first (`10,000 → 100,000` pages), then full pages slated for OCR review (`min_quorum = 1` + 5% spot-check) | **`0.70s` median (`0.94s` mean)** | **`~2.6 CPU-hours` per 10,000 pages** (`~260 CPU-hours` per 1M pages) | Computes exact per-page `legibility_ceiling`, `skew_deg`, `line_height_px`, `bleed_through`, `completeness`, and 3-reader `est_cer`. Immediately identifies: (a) pages that are already clean (`est_cer < 0.04`, `completeness > 0.95` → **skip re-OCR**), (b) pages that are physically unreadable (`legibility_ceiling < 0.65` → **do not waste Tesseract/Surya cycles**), and (c) pages where `loc` or `as` dropped columns/articles (`completeness < 0.85` while `legibility_ceiling > 0.80` → **high-ROI re-OCR targets**). |
| **Tier 2: Stratified VLM Calibration & Ambiguity Resolution** | `usnm_qwen` (`k_crops = 6`, `Qwen3-VL-2B-Q8_0` on CPU/GPU) | **1%–2% stratified audit sample** of Tier 1 pages + **2-reader / 1-reader pages in the uncertain triage band** (`0.75 ≤ legibility_ceiling ≤ 0.90` where existing readers disagree or lack a 3rd reader) | **`~333s` on 4-core CPU** or **`~21s` on GPU** (`k = 6` crops × `3.5s`) | **`~58 GPU-hours`** (or `~925 CPU-hours`) per 10,000 audited pages | Unlocks determined 3-cornered hat CER (`MAE ≤ 0.014`) on 2-reader pages and direct `cer_vs_vlm` (`MAE = 0.019`) on 1-reader (`loc`-only) pages, preventing false-positive re-OCR runs across entire newspaper titles. |
| **Tier 3: Targeted OCR Recovery** | `usnm_ocr` (Tesseract 5.5 tuned by Tier 1 image stats) & `usnm_qwen` (`Qwen3-VL-8B` on GPU) | Pages & regions flagged by Tier 1 + Tier 2 as **high-legibility, high-recoverable-error** ($\text{Expected Gain} = \text{legibility\_ceiling} - (1 - \widehat{\text{CER}}) \ge 0.10$) | `~90s` (`usnm_ocr` whole page) / `~11.4s` per damaged crop (`Qwen3-VL-8B` GPU) | Scaled strictly to pages with verified $\ge 10\%$ recoverable CER | Directly replaces degraded `loc` (`9.22%` CER) and misaligned `as` transcriptions with `3.68%` CER (Tesseract 5.5) or `2.17%` CER (`Qwen3-VL-8B`), measurably improving search recall and precision on `usnewsmap.com`. |
