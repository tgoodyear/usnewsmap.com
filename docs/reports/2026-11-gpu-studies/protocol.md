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

### Amendment, 1 November 2026: Japanese

Added on the maintainer's approval, before any reader ran on these crops under this study.

**Crops.** The 24 crops of the #128 comparison (12 typeset, 12 camp papers), plus 40 new crops: 20 typeset and 20 camp, one per page, from the same 124 public scans, taken in order of `sha256("gpu-studies-202611:ja:"+page_id)` from pages not already cut. The box rule is `crops.py`'s: a quarter of the width and a fifth of the height, at a position seeded by the same hash. Typeset means the Rocky Shimpo and Colorado Times; the rest are camp papers, mostly hand-lettered mimeograph.

**Readers.** NDLOCR-Lite, Qwen3-VL 8B, CHURRO, Surya, Tesseract `jpn_vert`, and dots.ocr if it runs. GOT-OCR, Granite-Docling and American Stories don't claim Japanese and aren't run on these crops. NDLOCR-Lite and Tesseract are CPU engines and run on a local node.

**Measures.** Character bigrams over Japanese characters only (kana and kanji, `usnm_core::ja::is_ja`), with old forms folded by `usnm_core::ja::fold_char`, as F1. Never whitespace word bags ([why](../../boinc-ndlocr-lite.md)). The 23 references are machine-made and barely checked (#135), so a score against them is **agreement with a machine reference, not accuracy**, in every table. Pairwise F1 between readers is agreement too. This study has no measured accuracy for Japanese unless a verifiable public human transcription of these crops turns up, and the report says which it is.

**Invented text and vertical layout.** Counted per reader:

- reading fails: under 35% of its non-space characters are Japanese, the page-language rule of the #128 sample;
- loop: the reading hits the token budget, or its Japanese bigrams repeat so that distinct bigrams are under half of all bigrams;
- invented text: over half of its Japanese bigrams occur in no other reader's text for that crop and in no reference, with at least 20 characters;
- vertical-layout failure: reading fails on a crop where at least two other readers produced 20 or more Japanese characters.

These are mechanical signals. A person who reads Japanese has not checked them.

## Study 4: pages per dollar (#296)

The same 40 pages, the first 40 of the pilot sample by hash rank, on a 3090 and an RTX 4090. Workload: 12 Qwen crops per page and one tiled Surya page read, the two GPU reads in production. Measured: pages per hour during the run, and dollars per 1,000 pages over all instance time, including startup, push, model load and failed hosts.

## Amendment, 1 November 2026: Study 5, loop guard, 8B training and the deployed path

Added on the maintainer's approval, after Studies 1 to 4 and before any Study 5 run. The readings of Studies 1 to 4 have been seen, and this design is informed by them.

**Held-out data.** Study 2 said its test set would be scored once. Using Reichsanzeiger `all-700` and Overproof `all-300` again is a departure. They stay as the like-for-like comparison and every result on them is labelled **second use**. Clean confirmation comes from two human-referenced sets not used by any study here, frozen before any reading:

- **BLN600**, English: 200 excerpts by `sha256("gpu-studies-202611:bln600:"+crop_id)` order. CC BY-NC-ND 4.0, research use, original image bytes.
- **GT4HistOCR `dta19`**, 19th-century German Fraktur books: 300 lines from the first 30 documents by `sha256("gpu-studies-202611:dta19:"+document)` order, at most 10 lines per document by the same hash. CC BY 4.0. These are books and not newspapers.

If a set can't be fetched or converted, the report says so and the comparison stands as second use only.

**Step 1, loop guard.** Candidates, all on Qwen3-VL-2B in the Study 2 settings, one crop at a time:

| Name | Method |
| --- | --- |
| `none` | No guard, Study 2's reading |
| `rp105` | Repetition penalty 1.05 |
| `rp110` | Repetition penalty 1.10 |
| `stop` | Stop on repeat: stop when `tail_repetition` of the contract fires, and keep the text before the repeated run plus one period |
| `stop-retry` | `stop`, then one retry at penalty 1.10, capped at twice the tokens used before the loop |

Selection uses the 378 validation crops of Study 2 only, once, with no use of the test sets or the new sets. The guard is the candidate with the lowest pooled CER among those whose under-length and word-overlap counts each stay within 2 of `none`. If several are within 0.2 points, the first in this order wins: `stop`, `rp105`, `rp110`, `stop-retry`. The same guard applies to 8B without further selection.

Scored for 2B: baseline with guard and adapter with guard, against Study 2's baseline and adapter without guard, on the second-use sets and the new sets. New sets also get both unguarded readers. Scored for 8B on the same guard: Study 1's measured set (second use) and the pilot pages' loop rate. Primary: the share of Study 2's -44.7 point Reichsanzeiger difference that baseline with guard gives alone, and the guarded paired difference between adapter and baseline. Under-length, word-overlap and invented-text counts are reported for every reader, since a guard can cut real text.

**Step 2, 8B training.** Study 2's data, split, method and hyperparameters on Qwen3-VL-8B-Instruct, at a pinned revision: rank 8, learning rate 1e-4 with cosine decay, accumulation 4, seed 583, 2,000 steps, checkpoints at 250, 500, 1,000 and 2,000, 1,024 to 2,048 image tokens, BF16. Any change is recorded. Checkpoint chosen on validation once, by the Study 2 rule, read with the chosen guard. Test once on the second-use sets and the new sets, with the guard. Peak memory is measured first, and the card is the cheapest that fits.

**Step 3, deployed path.** The selected adapter or adapters are merged into their base and converted to GGUF with the llama.cpp commit of the pinned server image, 2B at Q8_0 and 8B at Q4_K_M, the quantizations `usnm_qwen` runs, with the unchanged vision projectors. They are read in the `usnm_qwen` image with request version 2, on the same test crops, next to the unmodified GGUF. The result is the loss from export and quantization, as paired CER differences. Nothing is published, signed or sent to BOINC.

Adapters stay `untrusted-requires-held-out-evaluation`. Study 5 runs under the same total limit of $40.

## Budget

Total at most $40, from about $49.89. Study caps: Fraktur $8, LoRA $14, comparison $10, pages per dollar $8. A study that would pass its cap stops and keeps partial results.
