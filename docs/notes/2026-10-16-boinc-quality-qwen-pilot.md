# BOINC OCR quality (`usnm_quality`) and vision-language crop (`usnm_qwen`) pilot and calibration

## Native generation diagnostics (schema 1)

New native workers retain per-crop generation metadata in the atomic checkpoint
and upload it as JSON under `== generation_v1/<crop-id>` sections in `diag.txt`.
The existing readings/result wire contract and scientific validator are unchanged.
Old checkpoints without metadata still load; they do not acquire invented finish
reasons. Older workers cannot resume checkpoints containing the new field.

Each section records the server finish reason (missing means unknown), reported
completion tokens, requested budget, exact response/content SHA-256 and byte
counts, whether existing text cleaning changed content, longest identical-token
run, and descriptive budget-exhaustion/duplicate-line/empty/uncertainty-only flags.
Repetition metrics inspect the complete raw response content before existing
8 KiB reading cleaning/truncation, including degeneration beyond the retained prefix.
Unexpected non-string or oversized finish reasons are labeled invalid rather than
successful stops; the complete response hash still identifies their source.
Hashes are **not** copies of raw HTTP responses. The readings remain unchanged.
Metadata follows existing diagnostic limits: 2 KiB per section, 64 KiB total with
explicit cut notices; large bundles can have incomplete uploaded diagnostics,
not inferred successful completion for missing sections.

No stop strings or repetition penalty are explicitly requested; this does not
assert that server defaults disable repetition controls. Prompt 1, area-based
token budget, temperature 0, top-p 1, seed 42, cleaning and validation stay unchanged.
Agreement, including high Dice on repeated text, is not accuracy evidence.
Token exhaustion or repetition is a failure/uncertainty label requiring
image-grounded review, not a reason to lower validation thresholds or discard a
reading. For c07-like loops, compare crop/prompt/budget/stop/repetition variants
against multilingual, damaged and clipped image-grounded examples before changing
generation policy; simply raising the budget can prolong degeneration.

**Status:** multi-platform BOINC pilot collected (5 `usnm_quality` campaigns, 61 canonical workunits, 0 failures); `usnm_qwen` multi-host quorum and `crops.parquet` join verified; gold-set calibration complete (`overproof-ca`, `bln600`, `reichsanzeiger`, `Qwen3-VL-2B-Q8_0`, `Qwen3VL-8B-Q4_K_M`) · **Date:** 2026-10-16 · **Measured on:** `boinc.usnewsmap.com` (CT 100/101 Linux x86_64 native & rootless Podman, macOS `arm64-apple-darwin` native), `usnm-ocr-node3-arcs-prod` (`overproof-ca`, `bln600`, `reichsanzeiger`), Apple Silicon Metal GPU (`llama-server`) · **Full report & preserved data:** [docs/reports/2026-10-boinc-quality-pilot/report.md](../reports/2026-10-boinc-quality-pilot/report.md) · **Issues:** [#522](https://github.com/tgoodyear/usnewsmap.com/issues/522), [#531](https://github.com/tgoodyear/usnewsmap.com/issues/531), [#533](https://github.com/tgoodyear/usnewsmap.com/issues/533), [#559](https://github.com/tgoodyear/usnewsmap.com/issues/559)

## The question

Before submitting corpus-scale quality scoring (`usnm_quality`) and vision-language transcription (`usnm_qwen`) jobs to volunteer machines, we needed to verify cross-platform determinism, resolve scheduler/feeder starvation and LXC container provisioning bottlenecks, calibrate the Three-Cornered Hat estimator against human ground truth (`overproof-ca`, `bln600`, `reichsanzeiger`), and establish a volunteer-compute justification framework where every CPU/GPU cycle either verifies platform health or directly improves corpus OCR quality.

## What we measured

1. **Live BOINC Pilot (`usnm_quality` & `usnm_qwen`):**
   - `usnm_quality` completed **125 validated tasks** across **61 canonical workunits** with **0 client errors** and **100.0% bit-for-bit agreement** across macOS `arm64-apple-darwin` native, Linux `x86_64-pc-linux-gnu` native, and Linux Docker (`x86_64-pc-linux-gnu__docker`), in **0.70s median internal runtime** (`11.4s` p50 BOINC turnaround, `1.1s` p50 CPU time, `<256 MB` RSS).
   - `usnm_qwen` (`qwen3-vl-2b-q8` on 4-core CPU containers) completed **28 validated results** with **100% cross-host quorum agreement** (word-bag Dice `0.865–0.909` vs `0.500` probe floor; `665.2s` p50 elapsed for 12 crops, `2,539 MB` p90 RSS). Passing `QWEN_SPOOL=/project/usnmocr/results/12` to `quality-collect` joined the validated `v1-wave2` readings into `crops.parquet`, unlocking a determined 3-cornered hat (`hat_est_cer`: `vlm = 0.1596`, `loc = 0.1802`, `ours-tesseract = 0.4411`) on a 2-reader 1866 page (`sn82014761_1866-08-04_ed-1_seq-4`) whose page-level hat was previously undetermined.
2. **Gold-Set Ground Truth & Three-Cornered Hat Calibration (`overproof-ca` clean 345 crops & stratified sample-35):**
   - Excluding the 9 stretched-box conversions ([#531](https://github.com/tgoodyear/usnewsmap.com/issues/531)) on `overproof-ca` (`345` clean crops, `94,627` reference chars): `Qwen3VL-8B-Q4_K_M` achieved **2.17% True CER**, `Qwen3-VL-2B-Q8_0` **2.71% True CER**, `Tesseract 5.5 (eng-psm6)` **3.68% True CER**, `docTR 1.1.0` **5.25% True CER**, `loc-alto` **9.22% True CER**, and `american-stories` **130.08% True CER** on ALTO crop boxes.
   - In the 3-reader box-aligned crop hat (`loc + tesseract-psm6 + Qwen3-VL-2B`, excluding `as`), CER estimation MAE is **`0.0140` for `loc` (`r = 0.787`), `0.0133` for `tesseract-psm6` (`r = 0.948`, Spearman `ρ = 0.967`), and `0.0091` for `vlm` (`r = 0.924`)**. On 1-reader (`loc`-only) pages, direct `cer_vs_vlm` against `Qwen3-VL-2B` estimates `loc` true CER with **`MAE = 0.0189`** (`Bias = +0.0136`).
3. **Crop Subsampling (`k_crops`) & Multi-Page Workunit Batching:**
   - Reducing `k_crops` from `12` (2 per stratum) to **`6` (1 per stratum across `wc-{low,mid,high} × {top,bottom}`)** preserves full-page CER accuracy within **`0.0067–0.0087` MAE (`<0.9%` CER)** while cutting volunteer `usnm_qwen` compute time per page by **50%**.
   - Because `usnm_quality` takes `0.70s`–`1.06s` median internal compute vs `9.1s`–`10.7s` of BOINC RPC/spawn/upload overhead (`90%–94%` overhead at 1 page/WU), corpus-scale `usnm_quality` should bundle **32–50 pages per workunit** (cutting BOINC DB rows and file fanout by **32×–50×** to `~394k–615k` WUs). For `usnm_qwen` (`k=6`), bundling **3–4 pages (18–24 crops/WU)** on CPU (`~16–22 min`) and **15–20 pages (90–120 pre-cropped PNGs/WU)** on GPU (`~5–20 min`) amortizes `llama-server` model load (`<2%` overhead) and cuts volunteer image download bandwidth by **>95%**.
4. **Multi-Reader Wave 4 (`v1-wave4`: 50 Workunits / 100 Replica Tasks across 6 Languages & 7 Readers) & Engine Routing Rules:**
   - All **100 / 100 replica tasks (`50/50` canonical workunits)** completed with **0 errors** and **100.0% bit-exact cross-platform quorum agreement** across `arm64-apple-darwin` (`50` tasks, `0.62 s` median internal compute) and Linux `x86_64` (`50` tasks, `1.29 s` median internal compute) over **259 reader-page evaluations** (`5.18` readers/page).
   - **ALTO Confidence Fails as a Triage Signal:** `mean_word_conf` has zero correlation ($\rho = -0.054$) with actual Tesseract error across languages, whereas `legibility_ceiling` correlates strongly ($\rho = -0.761$ on `loc`, $\rho = -0.895$ on `ours-tesseract`).
   - **Actionable Transcription Routing:**
     - **Clean English (`legibility_ceiling >= 0.78`):** Skip re-OCR (`90.2%–96.2%` findable content already in `loc`/`ndnp`).
     - **Degraded / Small-Print English (`legibility_ceiling < 0.75` or `line_height_px < 36 px`):** Historical `loc` collapses (`0.5240` CER, `46.3%` findable content). Route to `ndnp-open-ocr` (`0.3692` CER, `95.9%` findable content) or full-scale Tesseract `eng@1` (`0.4083` CER, `82.3%` findable content); never downscale to `@0.5` when `line_height_px < 36 px` (`+12.5 pp` CER penalty).
     - **German Fraktur (`deu`):** Route to `deu_latf@1` (`0.1480` CER, `88.0%` findable content) or `frak2021@1` (`0.1527` CER), cutting `loc` CER (`0.3455`, `34.2%` findable) by **57%** and beating Antiqua `deu@1` by **+29.3 pp** findable content.
     - **Serbian (`srp@1`), Czech (`ces@0.5`/`@1`), and Polish (`pol@1`):** High-ROI re-OCR targets: `srp@1` fixes LoC's Latin-on-Cyrillic failure (`findable_places` **`2.8% → 98.6%`**, `findable_content` **`54.1% → 89.3%`**); `ces` lifts findable content from **`42.8% → 94.9%–95.7%`**; `pol@1` reaches **`99.3%` findable content** (`0.2120` CER).
     - **Oversized Scans (`width * height > 45,000,000`):** Run `@0.5` fallback if `@1` returns `<50` words (`0% → 92.8%–97.4%` findable content on `5700×8200` scans).

## What is left

See the full interim report and tiered corpus rollout plan in [docs/reports/2026-10-boinc-quality-pilot/report.md](../reports/2026-10-boinc-quality-pilot/report.md).
