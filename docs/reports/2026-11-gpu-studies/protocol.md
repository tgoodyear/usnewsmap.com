# Protocol for the November 2026 GPU studies

Written and committed before any study ran. Samples, splits, metrics and decision rules below don't change after results are seen. A departure is recorded in the report as a departure.

All text is scored on our side. Hosts are rented, untrusted and get only public scans and public models.

## Scoring

- CER and WER: `usnm_ocr_survey::score::cer_wer` (gold-set normalization: NFC, line-end hyphens joined, whitespace runs collapsed; case, punctuation, diacritics, long s and ligatures not folded), pooled over reference characters and words.
- A missing output scores as empty and stays in the denominator. Missing counts are reported.
- Limits: 95% percentile bootstrap resampling whole source pages, 2,000 resamples, seed 202611.
- Comparisons are paired on the same items. A difference is called only when its 95% limits exclude zero.
- Measured CER needs a human transcription. Agreement between readers is reported in separate tables and never as accuracy.

## Invented-text check

Applied to held-out crops with a human reference. The damage metric isn't used here, because it scores fluent invented text as good ([13 §13.7.7](../../design/13-ocr-engines.md#1377-cascade-optional-per-route)).

Tokens are normalized words with outer punctuation removed, lower-cased. Two tokens match when their edit distance is at most 0.34 of the longer length, so ordinary OCR errors still match. Reference and reading are aligned by longest common subsequence under that match.

| Measure | Definition | Source of the limit |
| --- | --- | --- |
| Invented run | 5 or more consecutive reading tokens with no aligned reference token | Set here |
| Over-length guard | Reading characters over reference characters above 1/0.8 | `min_chars_ratio = 0.8`, read as a symmetric bound |
| Under-length guard | That ratio below 0.8 | `min_chars_ratio = 0.8` |
| Word-overlap guard | Share of reference tokens aligned below 0.6 | `min_word_overlap = 0.6` |
| Major hallucination | Invented run, or over-length and word-overlap failing together | Set here |

This is a mechanical count. A repeated line counts as invented text, and so does a long run of garbage, so a count isn't proof of a fluent invention. Flagged crops are listed by id for a person to read.

## Study 1: German Fraktur bake-off (#926, #835)

Readers: Qwen3-VL 8B Q4_K_M on GPU (llama.cpp, request version 2, 1,024 minimum image tokens, temperature 0), Tesseract 5.5 `frak2021` and `deu_latf+deu`, and LoC's text where it exists.

Tesseract's primary setting is PSM 3 at 300 DPI, the production one. PSM 6 is secondary, because #835 found the two differ on crops.

**Measured set.** `reichsanzeiger-de/all-700` of benchmark v1.0.0 and the NZZ crops named in `data/samples/`. Neither is a page from our corpus: Reichsanzeiger is a gazette, NZZ a Swiss newspaper. LoC's text has no reading on these. The Austrian set isn't used here, so the two ONB reserve issues of #835 stay unspent by this study.

**Agreement set.** 100 pages frozen by `scripts/select-pilot.sh` from the pilot list, with 12 crops per page cut by the production recipe (`usnm-boinc qwen local-unit`, seed 522). The pilot list holds pages the database calls German; it doesn't record typeface. There's no transcription, so this set gives agreement and nothing else.

The universe excludes pilot pages with no scan on disk or no ALTO reachable from the batch archives or S3 bucket, so the sample covers those batches only.

Primary comparison: pooled CER of Qwen against `frak2021` PSM 3 on the measured set. No German trust threshold is proposed from this study, and #835 stays open.

## Study 2: LoRA training (#583)

Base: Qwen3-VL-2B at the pinned revision, in the signed training image, BF16. Adapter on language-attention `q_proj` and `v_proj` only, vision frozen.

The split is source-disjoint and is fixed by `data/samples/lora-split.json`, whose SHA-256 is in the commit that adds it, before training.

| Split | Sources | Use |
| --- | --- | --- |
| Train | Austrian newspapers (German), NewsEye BnF (French), NewsEye NLF Swedish | Training only; CC BY 4.0 |
| Validation | NZZ (German), NewsEye NLF Finnish | Choosing a checkpoint, once |
| Test | Reichsanzeiger `all-700` (German), Overproof `all-300` (English) | Scored once |

Training runs rank 8, learning rate 1e-4 with cosine decay, accumulation 4, seed 583, and saves adapters at 250, 500, 1,000 and 2,000 optimizer steps. These exceed the 64-step bound of the volunteer trial contract, which remains unchanged, so this isn't a contract trial.

The contract's image limit (about 128 image tokens) can't resolve newspaper text. Training and every inference here use 1,024 to 2,048 image tokens.

Selection: lowest pooled validation CER, baseline included. If the baseline wins, the adapter chosen for the test is the best checkpoint, and the outcome is reported as a negative result on validation as well.

Test: baseline and the one selected adapter, each once. Primary metric: paired pooled CER difference on Reichsanzeiger. Overproof is secondary and is a language that isn't in the training data. The proposed gate from [the transcription note](../../notes/2026-10-16-boinc-transcription.md) is a 10% relative CER reduction without material regression in omissions or invented text.

Every adapter is marked `untrusted-requires-held-out-evaluation`. Rented hardware doesn't meet #583's closure, so #583 stays open.

## Study 3: engine comparison (#968, #812, #925)

Readers, in order: Surya, Qwen3-VL 8B, CHURRO 3B, Tesseract, American Stories, then GOT-OCR 2.0 and Granite-Docling, then MinerU and dots.ocr.

**Same crops per language**, all human-referenced: English `legacy/overproof-en-100`, German `reichsanzeiger-de/acceptance-100` and the NZZ crops, and French, Finnish and Swedish `multilingual/all-52`. A language with few source pages is reported with its page count, and no engine is ranked on a set under 5 pages.

CHURRO's training data includes Chronicling America, so its English result can't be separated from possible training exposure.

**Reading order (#925).** 20 multi-column pages, the first by the sample's hash rank with at least 3 ALTO columns. The check is Kendall's tau over words occurring once on both sides, against a geometric column order built from LoC's ALTO blocks. That order is a heuristic reference and not a truth.

## Study 4: pages per dollar (#296)

The same 40 pages, the first 40 of the pilot sample by hash rank, on a 3090 and an RTX 4090. Workload: 12 Qwen crops per page and one tiled Surya page read, the two GPU reads in production. Measured: pages per hour during the run, and dollars per 1,000 pages over all instance time, including startup, push, model load and failed hosts.

## Budget

Total at most $40, from about $49.89. Study caps: Fraktur $8, LoRA $14, comparison $10, pages per dollar $8. A study that would pass its cap stops and keeps partial results.
