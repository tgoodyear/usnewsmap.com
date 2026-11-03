---
title: "GPU studies, November 2026"
subtitle: "German Fraktur, LoRA adapters, an engine comparison with Japanese, pages per dollar, a loop guard, a Surya retest, an image-token sweep and Yiddish"
date: "November 2026"
---

For Studies 1 to 7, the samples, split, metrics and decision rules were fixed in [`protocol.md`](protocol.md) before the corresponding runs. Seven setup rentals began earlier; none of them read a crop. The Japanese amendment was committed before any reader ran on these crops under this study, though 24 of them were read under #128. Study 8 was added later; its method and limitations are below. Departures are listed under [Departures from the protocol](#departures-from-the-protocol). Scripts are in [`scripts/`](scripts/), the scoring tool in [`tools/`](tools/), and the earlier scores in [`data/`](data/).

All limits are 95% bootstrap limits, resampling source pages. For Studies 1 to 7, a number from the protocol is the primary result. Anything marked post-hoc was chosen after seeing results.

# Summary

**Review limitation:** The historical LoRA adapter files were not checked for missing tensors, earlier Surya page-mode runners could discard block errors, and earlier Tesseract wrappers could mask failed reads. The affected comparisons are provisional; the retained evidence does not establish whether these problems affected a result. Study 8 used separately checked runners.

Pooled over Study 3's 612 crops, Qwen 8B has a CER of 17.3% and Tesseract 9.0%, a difference of 8.2 points with limits of -4.6 to 25.4, so no difference is called. On the German measured set the protocol's pooled CER is 49.6% for Qwen 8B against 17.9% for `frak2021` PSM 3, a difference that is called, against Qwen. Repetition loops are the visible cause: leaving out the 9 looped crops of 1,060 (post-hoc) brings Qwen to 10.9%. Loops were rarer on some sets than others: 3.9% of pilot crops and 15 of 64 Japanese crops. A loop guard is the first change to make.

A rank-8 LoRA adapter on Qwen 2B cut held-out Reichsanzeiger CER from 62.0% to 17.3%. Study 5 found that a loop guard alone gives the same gain (-45.0 points against the adapter's -44.7): with the guard on both, the 2B adapter's own gain is -6.9 points on Reichsanzeiger (second use) and -1.0 on unseen Fraktur books, with none on English. An 8B adapter gains -4.5 on Reichsanzeiger and little or nothing on the clean sets. Both lose little or nothing when exported to the GGUF the BOINC app runs. They stay untrusted.

The guard is `stop`: end the reading when the contract's `tail_repetition` fires and keep the text before the repeat. It costs little real text. Study 7 found no image-token setting better than 1,024.

Tiled full-page Surya costs about ten times Qwen's crop read per page. A 4090 was faster than a 3090 and dearer per page. Study 3's Surya scores measured our configuration: in Study 6, line recognition took Surya from 109.2% to 23.0% pooled CER, still behind Tesseract's 9.0% on those crops, and `whole` mode kept column order better but read less than half the text.

On Japanese, NDLOCR-Lite, CHURRO and Surya agree most with each other and with the machine references. That is agreement, not accuracy. No human transcription of these crops exists that we could verify.

On 104 hand-corrected Yiddish book pages, mean page CER is 1.52% for Jochre 3, 8.72% for Tesseract `yid` and 30.15% for Qwen request version 4. Qwen was given the corpus's line segmentation and reading order; the page readers were not. The 100 damaged newspaper crops have no transcriptions, so their high disagreement is not a measure of accuracy. Don't launch a full Yiddish reread at version 4 on this evidence.

# Study 1: German Fraktur (#926, #835)

Readers: Qwen3-VL 8B Q4_K_M on an RTX 3090, Tesseract 5.5 `frak2021` and `deu_latf+deu`, and LoC's text on the pilot pages.

The historical Tesseract wrappers could record failed or timed-out reads as text. The retained Study 1 and 3 outputs lack per-read exit receipts, so job-level success and zero missing counts cannot establish that all reads succeeded. Their Tesseract comparisons remain provisional. Updated wrappers record failures separately; no OCR was rerun.

## Measured CER, on human transcriptions

1,060 crops from 187 source pages: 700 Reichsanzeiger line pairs from 20 gazette pages, and 360 NZZ crops from 167 Swiss newspaper pages. Neither set comes from our corpus. LoC has no reading of them, so LoC isn't in this table.

| Reader | Items | Pooled CER | 95% limits | Median crop CER | Pooled WER |
| --- | ---: | ---: | ---: | ---: | ---: |
| Qwen3-VL 8B | 1,060 | 49.6% | 26.4 to 78.2 | 4.5% | 117.4% |
| `frak2021` PSM 3 | 1,060 | 17.9% | 12.8 to 23.5 | 5.9% | 35.4% |
| `frak2021` PSM 6 | 1,060 | 16.6% | 11.4 to 22.5 | 4.5% | 34.6% |
| `deu_latf+deu` PSM 3 | 1,060 | 24.9% | 18.9 to 31.2 | 11.1% | 58.5% |
| `deu_latf+deu` PSM 6 | 1,060 | 23.0% | 17.1 to 29.1 | 9.3% | 55.6% |

The primary comparison goes against Qwen: its pooled CER is 31.6 points above `frak2021` PSM 3 (limits 9.3 to 58.3).

That result comes from a few crops. 9 of the 1,060 readings ran out of tokens in a repetition loop, each adding up to 2,048 tokens of invented text against a one-line reference. Leaving those 9 crops out for both readers (post-hoc) gives:

| Reader | Items | Pooled CER | 95% limits | Pooled WER |
| --- | ---: | ---: | ---: | ---: |
| Qwen3-VL 8B, no looped crops | 1,051 | 10.9% | 8.3 to 13.7 | 29.3% |
| `frak2021` PSM 3, same crops | 1,051 | 17.7% | 12.6 to 23.2 | 35.1% |

On those crops Qwen is 6.8 points lower than PSM 3 (limits 4.0 to 9.9). This post-hoc view drops the crops where Qwen does worst, so it describes the other 99% and doesn't replace the primary result. By median crop CER, also post-hoc, Qwen is at 4.5% against 5.9% for PSM 3 and ties PSM 6 at 4.5%. On Reichsanzeiger the medians are 5.4% for Qwen, 6.8% for PSM 3 and 3.5% for PSM 6, so PSM 6 beats Qwen there.

Both views count Qwen's modernizing against it. It writes `s` for `ſ`, and these references keep `ſ`. Folding long s, the double hyphen and superscript e on both sides (post-hoc) gives a median crop CER of 1.7% for Qwen, 3.1% for `frak2021` PSM 3 and 2.4% for PSM 6. An engine that modernizes isn't transcribing diplomatically, and whether that suits our search text is a question for #926.

The invented-text check counts 88 major hallucinations for Qwen and 126 for `frak2021` PSM 3, of 1,060 crops. It can't tell fluent invention from garbage, and nobody has read them.

## Agreement on the pilot pages

100 pages frozen from the pilot list, 1,187 crops. There's no transcription here, so this is agreement, not accuracy. These numbers say how far readers differ, and nothing about which is right. The list records no typeface, so these aren't known to be Fraktur pages.

| Pair | Symmetric edit rate | 95% limits | Mean word-bag Dice |
| --- | ---: | ---: | ---: |
| `frak2021` PSM 3 and `deu_latf` PSM 3 | 20.1% | 18.3 to 21.7 | 0.57 |
| Qwen and `frak2021` PSM 3 | 47.0% | 40.6 to 52.9 | 0.53 |
| Qwen and LoC | 55.6% | 50.8 to 60.1 | 0.41 |
| `frak2021` PSM 3 and LoC | 40.5% | 37.8 to 43.3 | 0.34 |
| `deu_latf` PSM 3 and LoC | 43.1% | 40.7 to 45.7 | 0.31 |

46 of the 1,187 pilot readings by Qwen looped, 3.9%, more than four times the rate on the measured set. On whole pages over all 100, word-bag Dice is 0.26 between `frak2021` and LoC and 0.35 between `deu_latf` and LoC.

No German trust threshold comes out of this study, and #835 stays open.

# Study 2: LoRA training (#583)

A rank-8 adapter on Qwen3-VL-2B, trained for 2,000 optimizer steps on 4,032 crops from the Austrian newspapers, NewsEye BnF and NewsEye NLF Swedish. Validation was NZZ and NewsEye Finnish (378 crops), test Reichsanzeiger and Overproof (1,000 crops, 38 pages). The split was fixed before training: SHA-256 of [`data/samples/lora-split.json`](data/samples/lora-split.json) is `65fe42a25f9e2ee6f30c975b2c7bc9398c5293493a01ccad165930a51606ff81`.

Training took 61 minutes on an RTX 3090, with 8.8 GB allocated at peak and about 21.7 GB reserved. All 4,032 examples encoded.

For regeneration, `analyze-2.sh` requires `--phase validation` to score checkpoints for selection or `--phase test` to score the held-out split; it has no default phase.

Validation. Used once, to pick a checkpoint.

| Checkpoint | Pooled CER | 95% limits | Pooled WER |
| --- | ---: | ---: | ---: |
| Baseline | 28.1% | 6.7 to 67.3 | 74.2% |
| 250 steps | 5.0% | 3.9 to 6.2 | 22.1% |
| 500 steps | 5.0% | 3.8 to 6.8 | 19.2% |
| 1,000 steps | 3.5% | 2.9 to 4.4 | 15.6% |
| 2,000 steps | 3.7% | 2.9 to 4.8 | 15.8% |

The 1,000-step adapter was selected, SHA-256 `b6bc8c2a1c6b4e953e10321ebb8bdf27a363840cb7a8b05bf92eb16ed6d77b80`.

The historical loaders did not require a complete adapter tensor-key set. The retained training logs record checkpoint hashes but not tensor keys, and the inspected study data do not contain the checkpoint payloads. Completeness of the 2B and 8B adapters therefore remains unverified, so the adapter comparisons in Studies 2 and 5 are provisional. The updated loaders reject incomplete adapters; no readings were rerun.

Held-out test. Scored once.

| Source | Reader | Items | Pooled CER | 95% limits | Pooled WER | 95% limits |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Reichsanzeiger | Baseline | 700 | 62.0% | 29.6 to 100.1 | 100.8% | 62.3 to 140.4 |
| Reichsanzeiger | Adapter | 700 | 17.3% | 8.5 to 31.3 | 43.5% | 28.8 to 66.8 |
| Overproof | Baseline | 300 | 8.2% | 1.1 to 21.1 | 10.7% | 3.8 to 22.7 |
| Overproof | Adapter | 300 | 5.3% | 2.1 to 10.6 | 13.9% | 6.3 to 29.9 |

The primary result is a paired CER difference of -44.7 points on Reichsanzeiger (limits -78.0 to -17.8). That passes the proposed 10% relative gate on CER. Readings at the 1,024-token cap on Reichsanzeiger fell from 15 to 2. Study 5 measures how much of the gain that explains.

On Overproof, English that isn't in the training data, the difference is -2.9 points with limits that include zero (-17.2 to 6.7). So this is no gain shown for English, and its WER moved the wrong way, with wide limits.

Invented text on the 1,000 test crops, against the guards of [13 §13.7.7](../../design/13-ocr-engines.md#1377-cascade-optional-per-route):

| Reader | Major hallucinations | Invented runs | Over-length guard | Under-length guard | Word-overlap guard |
| --- | ---: | ---: | ---: | ---: | ---: |
| Baseline | 90 | 88 | 91 | 1 | 15 |
| Adapter | 56 | 56 | 61 | 16 | 20 |

The adapter invents less and omits more. Under-length failures rose from 1 to 16, word-overlap failures from 15 to 20. The gate asks for no material regression in omissions, so that rise needs reading before anyone calls this a pass.

Status. The adapter stays `untrusted-requires-held-out-evaluation`. It wasn't exported to the GGUF the BOINC app runs, so nothing here measures the deployed path. Base is 2B, not the 8B of Study 1. Rented hardware doesn't meet #583's volunteer CUDA closure, so #583 stays open. These runs exceed the 64-step bound and the image limit of the volunteer contract: the protocol states that the contract's roughly 128 image tokens can't resolve newspaper text, which wasn't tested here.

# Study 3: engine comparison (#968, #812, #925)

## Latin-script crops, on human transcriptions

612 crops. Pooled CER by source, with median crop CER in brackets. A reader with no output on a crop scores as empty. Limits and WER are in [`data/s3/compare.jsonl`](data/s3/compare.jsonl).

For regeneration, `analyze-3.sh` decodes the American Stories `as.b64` in `${GPU_STUDY_DEV}/local/out/` (or `AMERICAN_STORIES_B64`), joining nonblank `bboxes[].raw_text` in order as the survey runner does. Alternatively, explicitly set `AMERICAN_STORIES_JSONL` to an existing `{id,text}` JSONL file, such as the [retained readings](data/s3/american-stories.jsonl); do not set it with `AMERICAN_STORIES_B64`. Input and override paths are absolute or relative to `GPU_STUDY_ROOT`. A valid empty reading is present; an absent row is missing. Before scoring, the last selected reading for each ID must match those retained readings, or `AMERICAN_STORIES_REFERENCE` if they are elsewhere. A missing input, malformed result, or mismatch stops regeneration rather than changing the historical scores. The raw batch was unavailable for this review, so its comparison has only been exercised on synthetic data.

| Source (crops, pages) | Qwen 8B | CHURRO | Tesseract | Surya | GOT-OCR | Granite | American Stories |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Overproof English (100, 18) | 1.6 (0.4) | 1.9 (0.4) | 3.1 (2.0) | 48.5 (8.0) | 44.1 (2.5) | 55.3 (1.9) | 91.4 (96.8) |
| French (24, 8) | 2.8 (2.0) | 3.2 (2.3) | 7.2 (5.7) | 3.0 (2.3) | 17.9 (13.1) | 99.7 (8.5) | 96.1 (100) |
| Finnish (18, 6) | 2.4 (2.6) | 0.9 (0.9) | 8.2 (9.6) | 7.3 (2.4) | 78.0 (45.8) | 74.7 (26.4) | 76.9 (100) |
| Swedish (10, 4), unranked | 2.1 (2.1) | 2.1 (2.3) | 11.4 (12.4) | 3.0 (2.8) | 91.0 (41.7) | 48.4 (23.3) | 75.6 (95.7) |
| Reichsanzeiger German (100, 20) | 41.6 (5.1) | 18.3 (4.6) | 18.6 (3.5) | 392.1 (31.3) | 385.0 (27.5) | 732.6 (67.6) | 100 (100) |
| NZZ German (360, 167) | 49.9 (2.0) | 13.4 (5.7) | 13.4 (5.3) | 205.9 (100) | 52.3 (27.3) | see below | 100 (100) |

A CER above 100% means the reading is longer than the reference.

Caveats:

- These Surya rows measure our configuration, not the engine at its best: `full_page=True` on line crops, CPU layout and the GGUF through llama.cpp. Study 6 retests it.
- The historical `whole`, `capped` and `tiled` runners discarded block errors like skips. The retained rows do not identify whether any such errors occurred, so the affected Surya scores and page-mode comparisons remain provisional. The updated runner records block failures; no readings were rerun.
- A crop isn't a page. Surya's full-page mode and American Stories' layout model are built for pages. On NZZ's one-line crops Surya's median crop CER is 100%, and 317 of 612 crops fail the under-length guard. American Stories returned nothing on 530 of 611 crops. This says those crops don't suit them, and little about their reading.
- Tesseract used `eng`, `frak2021` or `fra+fin+swe` by source, at PSM 6. Qwen and CHURRO were given no language.
- CHURRO's training data includes Chronicling America, so its Overproof result can't be separated from training exposure. Licence: Qwen Research License.
- Granite read 285 crops: all but 327 of NZZ's, so its NZZ score isn't shown. It had a 1,024-token cap.
- Sets under 5 pages aren't ranked. Swedish has 4 pages.
- Cards differed, which affects speed comparisons and not accuracy.

Across the 612 crops, major hallucinations number 32 for Qwen, 48 for Tesseract, 45 for Surya, 54 for CHURRO, 20 for American Stories and 88 for GOT, and 98 of Granite's 285. The check counts garbage and repeats as well as fluent invention, and nobody has read these. American Stories returned nothing on most crops.

MinerU and dots.ocr were not run. Their terms in [design document 13](../../design/13-ocr-engines.md) clear this use: MinerU's conditions bind online services, and the maintainer cleared dots on 2026-10-11. They were dropped for time, and remain to do.

## Japanese agreement scores

64 crops, 32 typeset and 32 from camp papers. Nothing here measures accuracy. The 23 references are machine-made and barely checked (#135). No verifiable public human transcription of these crops turned up. [The gold set](../../ocr-gold-set.md) lists CODH's modern magazines and NDL's set, which are magazines and books. Neither was checked for these crops.

Character bigram F1 over Japanese characters, old forms folded. Median over crops, against the machine references (23 crops, 12 typeset and 11 camp):

| Reader | All | Typeset | Camp papers |
| --- | ---: | ---: | ---: |
| NDLOCR-Lite | 0.84 | 0.90 | 0.77 |
| CHURRO | 0.81 | 0.84 | 0.79 |
| Surya | 0.78 | 0.88 | 0.76 |
| Qwen3-VL 8B | 0.74 | 0.79 | 0.74 |
| Tesseract `jpn_vert` | 0.29 | 0.36 | 0.06 |

Median pairwise F1, all 64 crops:

| Pair | All | Typeset | Camp |
| --- | ---: | ---: | ---: |
| NDLOCR-Lite and Surya | 0.77 | 0.82 | 0.65 |
| NDLOCR-Lite and CHURRO | 0.75 | 0.80 | 0.68 |
| Surya and CHURRO | 0.74 | 0.80 | 0.68 |
| NDLOCR-Lite and Qwen | 0.63 | 0.76 | 0.57 |
| Others with `jpn_vert` | 0.10 to 0.22 | 0.32 to 0.37 | 0.05 to 0.07 |

Failures, by the amendment's mechanical counts, on 64 crops:

| Reader | Under 35% Japanese | Vertical-layout failure | Loop | Invented text |
| --- | ---: | ---: | ---: | ---: |
| NDLOCR-Lite | 5 | 4 | 0 | 1 |
| CHURRO | 5 | 4 | 3 | 2 |
| Surya | 2 | 1 | 2 | 5 |
| Qwen3-VL 8B | 6 | 4 | 15 | 4 |
| Tesseract `jpn_vert` | 0 | 0 | 0 | 59 |

Qwen's 15 loops, nearly a quarter of its crops, are the clearest failure here. `jpn_vert`'s 59 mean its Japanese-script output shares almost nothing with any other reader. A vertical-layout count may also be a crop with little Japanese on it. The crops weren't inspected. Nobody who reads Japanese has checked this.

## Reading order on multi-column pages (#925)

The Surya row measures our configuration, not the engine at its best: tiled text is joined tile by tile, so the score is mostly our stitching. Study 6 retests it.

Kendall's tau over words occurring once on both sides, against a geometric column order built from LoC's ALTO. This is a heuristic reference, not a truth. LoC's score is partly circular, because the reference comes from its own blocks.

| Reader | Pages with 20 or more matched words | Median tau | Lowest | Pages under 0.5 |
| --- | ---: | ---: | ---: | ---: |
| LoC | 20 | 0.97 | 0.57 | 0 |
| `frak2021` PSM 3, whole page | 12 | 0.73 | 0.11 | 3 |
| Surya, tiled | 8 | 0.34 | 0.16 | 7 |

Tiled Surya loses column order on most pages. 3 of 12 `frak2021` pages do too, which fits #925's report. The sample is small, and for Surya it's the 8 pages read before it was stopped.

# Study 4: pages per dollar (#296)

The same pages from the frozen sample. Cost is everything paid for the instance, from creation to destruction, at its hourly price.

| Workload | Card | Pages read | Pages per hour, reading | $/hour | Instance minutes | $ per 1,000 pages |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Qwen 8B, 12 crops a page | RTX 3090 | 40 | 91 | 0.19 | 43 | 3.40 |
| Qwen 8B, 12 crops a page | RTX 4090 | 40 | 141 | 0.37 | 30 | 4.70 |
| Surya, tiled | RTX 3090 | 18 | 5.9 | 0.20 | 190 | 36 |
| Surya, tiled | RTX 4090 | 20 | 7.5 | 0.36 | 168 | 51 |
| Surya, tiled | RTX 2060 SUPER, local | 2 | 3.5 | 0 | n/a | no rental cost |

Pages per hour for Surya compares the first 18 pages on each rented card. The 2060 SUPER figure is two pages, 2,084 s in all, against 836 s on the 3090 and 614 s on the 4090 for the same two. Each rented run sat on a different host, with its own CPU, and CPU load matters to Surya's layout step, so these aren't clean card effects.

Qwen at 467 crops and Surya on whole pages are different work: a Surya page costs about ten times a page of Qwen crops. The 4090 is about 1.5 times as fast on Qwen and 1.3 times on Surya, at about 1.9 times the price, so the 3090 is cheaper per page on both. No Study 4 rental failed, so the table holds all its paid time. The 2060 SUPER has no rental cost, and its electricity wasn't measured. Qwen wasn't measured on it, because pulling the image would block the BOINC client.

# Study 5: loop guard, 8B training and the deployed path

Study 2's test set was to be scored once. Reusing Reichsanzeiger and Overproof here is a departure, so those results are labelled second use. Two sets no study had used give the clean confirmation: 200 BLN600 English excerpts and 300 GT4HistOCR `dta19` Fraktur lines from 30 books, frozen before any reading. BLN600 is cut from London newspapers, and `dta19` is from books, so it says less about newspaper layout.

## Loop guard

Candidates were chosen on Study 2's 378 validation crops on Qwen 2B. Rule: lowest pooled CER among those keeping omission counts within 2 of no guard, ties going to the simplest.

| Candidate | Pooled CER | 95% limits | Median crop | Major hallucinations | Under-length | Word-overlap |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `none` | 28.1% | 6.7 to 67.3 | 4.3% | 15 | 1 | 10 |
| `stop` | 7.9% | 6.4 to 9.9 | 4.4% | 14 | 1 | 9 |
| `rp105` | 16.0% | 6.7 to 37.1 | 4.5% | 15 | 1 | 8 |
| `rp110` | 16.1% | 6.9 to 36.8 | 4.5% | 15 | 1 | 7 |
| `stop-retry` | 8.1% | 6.6 to 10.3 | 4.4% | 14 | 1 | 8 |

`stop` was selected: when `tail_repetition` fires, generation stops and the text before the repeated run is kept, with one period of it. Repetition penalty barely helps, and a retry adds nothing.

The guard scores on the test sets were computed by applying the same cut to finished readings, which needs no GPU, instead of rereading. On validation the cut gave 7.7% and the live guard 7.9%. 37 of 378 texts differ between those two runs, which were on different cards.

**Qwen 2B, BF16.** Second use, Reichsanzeiger 700 and Overproof 300:

| Source | Baseline | Baseline, guard | Adapter | Adapter, guard |
| --- | ---: | ---: | ---: | ---: |
| Reichsanzeiger | 62.0% | 17.1% | 17.3% | 10.2% |
| Overproof | 8.2% | 2.3% | 5.3% | 2.9% |

On Reichsanzeiger the guard alone gives -45.0 points (limits -83.6 to -14.5), which is 101% of the adapter's -44.7. So Study 2's headline gain was almost all fewer loops. With both guarded, the adapter's own gain is -6.9 points (-12.9 to -2.0) on Reichsanzeiger, and on Overproof it is +0.6 (0 to 1.3), so it may do slight harm to English.

The clean sets, new to all readers:

| Source | Baseline | Baseline, guard | Adapter | Adapter, guard | Guarded difference | 95% limits |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `dta19` German, 300 lines, 30 books | 22.5% | 7.4% | 21.5% | 6.4% | -1.0 | -1.4 to -0.7 |
| BLN600 English, 200 excerpts | 3.2% | 3.2% | 3.1% | 3.1% | -0.2 | -1.4 to 0.8 |

The adapter has a real, small gain on unseen Fraktur and none shown on English. That's well short of the 10% relative gate on English and meets it on `dta19`.

**Qwen 8B.** Second use of Study 1's measured set, 1,060 crops: unguarded 49.6%, with guard 11.1% (8.3 to 13.8). Against `frak2021` PSM 3 the difference goes from +31.6 points to -6.8 (-10.1 to -3.9). The guard fired on 47 of the 1,187 pilot crops, 46 of them at the token cap.

**What the guard costs.** It cuts the repeated tail and little else. Second use, 1,000 crops: under-length failures rise from 1 to 4 for the baseline, and 16 stay 16 for the adapter. Word-overlap failures don't change. Major hallucinations fall from 90 to 87 and stay 56, so most of what that count flags isn't loops. On 8B, under-length failures rise from 3 to 5 of 1,060.

## Deployed path

The adapter was merged into its base and converted with llama.cpp commit `091b6a7fc`, the pinned server image's, to Q8_0 (SHA-256 `57ef4592`), then read in the `usnm_qwen` 2B image with request version 2, next to the stock GGUF. Nothing was signed, published or sent to BOINC. All with guard:

| Source | Stock GGUF | Adapter GGUF | Difference | 95% limits |
| --- | ---: | ---: | ---: | ---: |
| Reichsanzeiger, second use | 15.5% | 10.1% | -5.4 | -12.1 to -1.2 |
| Overproof, second use | 2.2% | 2.4% | +0.2 | -0.6 to 0.8 |
| `dta19` | 7.0% | 6.3% | -0.7 | -1.1 to -0.3 |
| BLN600 | 2.4% | 2.2% | -0.1 | -1.1 to 0.5 |

Export and quantization lost nothing measurable. The same adapter, GGUF against BF16: Reichsanzeiger -0.1 points (-0.4 to 0.3), `dta19` -0.1 (-0.2 to 0.1), Overproof -0.6 (-1.3 to -0.1), BLN600 -0.8 (-1.7 to 0). The GGUF path was better than BF16 where a difference shows, which isn't an export gain: the runtimes also differ in image handling. Peak VRAM was 3.5 GB on 2B Q8_0.

**Qwen 8B adapter.** The Study 2 recipe on Qwen3-VL-8B-Instruct (revision [`0c351dd01ed87e9c1b53cbc748cba10e6187ff3b`](data/s5/8b-train-report.json), pinned in the training script), on an RTX 4090: 2,000 steps in 76 minutes, 22.5 GB allocated at peak and 24 GB reserved, so a 3090 just fits it and a 12 GB card doesn't. The first run, on a 3090, finished training and validation, but its files were lost when the host refused SSH during the copy and the wrapper destroyed the instance anyway. It was rerun with the same seed and settings, and the wrapper now copies results back during the job.

Validation, with guard, 378 crops: baseline 5.3% (3.8 to 7.5); 250 steps 3.5%; 500 steps 2.6%; 1,000 steps 2.5%; 2,000 steps 2.1% (1.6 to 2.8). The 2,000-step adapter was selected.

Test, once, with guard, BF16:

| Source | Baseline | Adapter | Difference | 95% limits |
| --- | ---: | ---: | ---: | ---: |
| Reichsanzeiger, second use | 12.4% | 7.9% | -4.5 | -6.1 to -2.9 |
| Overproof, second use | 2.0% | 1.4% | -0.6 | -1.6 to 0.2 |
| `dta19` | 5.4% | 5.2% | -0.2 | -0.3 to 0.0 |
| BLN600 | 3.7% | 3.0% | -0.7 | -2.2 to 0.4 |

Major hallucinations fall from 102 to 45 of 1,500, under-length failures go from 11 to 12, and word-overlap failures from 23 to 15. The 8B adapter clears the 10% relative gate on Reichsanzeiger only. On the clean sets its gain is small (`dta19`) or not shown (BLN600).

**8B deployed path.** Merged and converted to Q4_K_M (SHA-256 `990f3d6a`), read in the `usnm_qwen` 8B image next to the stock GGUF, with guard:

| Source | Stock GGUF | Adapter GGUF | Difference | 95% limits |
| --- | ---: | ---: | ---: | ---: |
| Reichsanzeiger, second use | 12.2% | 8.4% | -3.7 | -5.7 to -2.0 |
| Overproof, second use | 1.8% | 1.6% | -0.1 | -1.0 to 0.6 |
| `dta19` | 5.5% | 5.3% | -0.2 | -0.4 to -0.1 |
| BLN600 | 2.2% | 2.5% | +0.3 | 0.2 to 0.4 |

Against BF16, the same adapter lost a little in Q4_K_M on Reichsanzeiger, +0.5 points (-0.1 to 1.1), and Overproof, +0.2 (0.1 to 0.3), with `dta19` and BLN600 within noise. On the deployed path the 8B adapter is slightly worse than stock on BLN600, +0.3 (0.2 to 0.4), so it isn't neutral on English. Peak VRAM was 7.2 GB for 8B Q4_K_M and 18.8 GB for 8B BF16 inference.


The adapters stay `untrusted-requires-held-out-evaluation`.

# Study 6: a fair Surya retest

Study 3's Surya rows measure our configuration. This retest ran the same inputs the way Surya is meant to run, on a 12 GB RTX 3060. The crops and pages were already seen, so this is a second use, with no training.

## Line recognition on the 612 crops

`full_page` off, each crop one block. Pooled CER, with limits, median crop CER and Study 3's pooled figure for the same crops:

| Source (crops, pages) | Pooled CER | 95% limits | Median crop | Study 3 pooled |
| --- | ---: | ---: | ---: | ---: |
| All (612) | 23.0% | 16.3 to 30.8 | 7.1% | 109.2% |
| Overproof English (100, 18) | 16.6% | 11.0 to 22.8 | 1.5% | 48.5% |
| Reichsanzeiger German (100, 20) | 23.6% | 14.9 to 32.4 | 7.9% | 392.1% |
| NZZ German (360, 167) | 15.9% | 11.5 to 22.8 | 7.4% | 205.9% |
| French (24, 8) | 18.4% | 7.5 to 27.2 | 3.0% | 3.0% |
| Finnish (18, 6) | 45.6% | 14.4 to 64.2 | 10.3% | 7.3% |
| Swedish (10, 4), unranked | 31.9% | 1.7 to 47.5 | 10.0% | 3.0% |

Against Tesseract's 9.0% over the same crops, Surya is 14.0 points higher (limits 7.8 to 21.0). Line mode fixes the line crops and does worse on the region crops of the NewsEye sets, where full-page mode had worked. Major hallucinations number 49 of 612, and 48 crops fail the under-length guard. Nobody has read them. It took 849 s, 1.4 s a crop.

## Page modes on the reading-order pages

| Mode | Pages read | Pages scored | Median tau | Pages under 0.5 | Matched words a page | Pages per hour |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `whole` | 20 | 15 | 0.75 | 3 | 70 | 11.7 |
| `capped` | 8 | 5 | 0.20 | 4 | 63 | 6.7 |
| `tiled`, Study 3 | 8 | 8 | 0.34 | 7 | 213 | 5.9 on a 3090 |
| LoC | 20 | 20 | 0.97 | 0 | 1,709 | |

Layout ran on the GPU here, with a CUDA torch in the image. Surya's own order in `whole` mode scores far better than our tile stitching, so Study 3's 0.34 was mostly our stitching. It also reads much less of the page: median 11,189 characters against LoC's 26,120, and 5 of 20 pages had too few matched words to score. So `whole` keeps order and loses text. These are heuristic scores on a small sample. `capped` was stopped at 8 pages at about 9 minutes a page.

## Native weights against the GGUF

50 crops by the frozen hash order, line mode, native weights (revision `3b3d4cdf`, OpenRAIL) on vLLM 0.20.1 with Surya's server settings scaled to a 12 GB card (`max-num-seqs` 16, `max-num-batched-tokens` 4096), 9.9 GB peak VRAM. Pooled CER is 61.4% (37.6 to 95.4) for native and 18.9% (10.0 to 30.7) for the GGUF, 42.5 points worse (19.6 to 76.9). A few readings run far past their reference, such as 552 characters on a 57-character crop. This wasn't investigated further, and it may come from our server settings, so it isn't a verdict on the native model.

All 50 archived native rows have successful status and nonempty output, and their IDs match the selection reconstructed from the retained 612 line IDs. The original native input list was not found in the inspected worktree, so it cannot independently verify that reconstruction.

# Study 7: image-token sweep

Only default and 1,024 minimum image tokens had been compared (#529), and 1,024 came from llama.cpp's warning. This sweep ran the stock Qwen 8B Q4_K_M GGUF at six values, with the `stop` guard applied to every reading. It ran on a rented RTX 3060 in the `usnm_qwen` image's CUDA build 11610, because the native Vulkan build of the installed app found no device there. So the build differs from the 1.14 to 1.17 Vulkan builds, and agreement between two vendors wasn't measured. The Yiddish crops typed in #529 couldn't be found, so Yiddish has no word recall here.

Crops: the 39 of #529 (34 Yiddish, 3 Serbian, 2 English, no transcriptions), and 112 with human transcriptions (French 24, Finnish 18, Swedish 10, German Austrian newspapers 60). No gold-set crop has a transcription yet, and no Study 5 test set was used. Context size stayed 4,096 up to 1,536, then rose to fit: 4,352 at 2,048 and 5,376 at 3,072.

| Minimum image tokens | CER, 112 crops | 95% limits | Against 1,024 | 95% limits | Yiddish loops, of 34 | Yiddish at token cap | Yiddish text kept | Seconds a crop, 151 | Peak VRAM |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Default | 3.0% | 2.4 to 3.7 | +0.3 | 0.1 to 0.6 | 15 | 16 | 77% | 4.8 | 6.9 GB |
| 768 | 2.7% | 2.2 to 3.2 | 0.0 | -0.1 to 0.2 | 7 | 8 | 100% | 5.0 | 6.9 GB |
| 1,024 | 2.7% | 2.2 to 3.2 | | | 9 | 11 | 100% | 5.8 | 6.9 GB |
| 1,536 | 2.8% | 2.2 to 3.4 | +0.1 | -0.1 to 0.4 | 6 | 8 | 106% | 6.7 | 6.9 GB |
| 2,048 | 2.8% | 2.2 to 3.5 | +0.1 | -0.1 to 0.4 | 7 | 10 | 111% | 8.4 | 6.9 GB |
| 3,072 | 3.0% | 2.4 to 3.6 | +0.3 | 0.1 to 0.5 | 8 | 9 | 98% | 11.8 | 7.2 GB |

Text kept is characters after the guard, against the same crops at 1,024. No value ran out of memory on the 12 GB card. Loops on the transcribed crops were zero at every value, so these CERs say nothing about loops. The 5 English and Serbian crops didn't loop at any value.

The default is worse than 1,024, which confirms #529. Above that, nothing beats 1,024. Under the protocol's rule, 768 is the best value by a hair, with a difference of 0.0 points (limits -0.1 to 0.2) against 1,024, 7 Yiddish loops against 9, and 14% less GPU time. That doesn't meet the bar for a new request version: the limits include zero, and on 34 Yiddish crops the gap between 6 and 9 loops can't be told from noise. There's also no measured accuracy on Yiddish here. 3,072 costs twice the GPU time and is worse. Keep 1,024.

The Study 2 and Study 5 adapters trained at 1,024 to 2,048 image tokens (`min_pixels` 1,048,576, `max_pixels` 2,097,152, 32 pixels a token), and are served in BF16 with the same range. Through llama.cpp, request version 2 sets the minimum at 1,024 with no such maximum, so a crop whose native size exceeds 2,048 tokens is read at a resolution the adapter never trained on. No test here shows harm from that.

# Study 8: Yiddish test set (2 November 2026)

The six typed Yiddish crops from #529 could not be found. This study tests the staged version 4 request against hand-corrected books, then measures agreement on untranscribed newspaper crops. It was added after the earlier studies, not as a preregistered amendment to their protocol.

## Book accuracy

The [Yiddish OCR Corpus](https://gitlab.com/jochre/corpora/jochre-yiddish-corpus), by Assaf Urieli and the Yiddish Book Center, is [CC BY-NC-SA 4.0](https://gitlab.com/jochre/corpora/jochre-yiddish-corpus/-/blob/bd69b412947e03cca18fdd5c72826207946d777f/LICENSE.md); see [Urieli et al.](https://arxiv.org/abs/2501.08442). We used its test split at commit `bd69b412947e03cca18fdd5c72826207946d777f`: 104 book pages from 60 books, annotated to glyph level and reviewed by a second annotator. No adapter was trained on the corpus in this study. Jochre's previously trained model is not independent of that corpus.

Jochre 3 1.1.4 and Tesseract 5.5.0 `yid` read whole pages, the latter with `tessdata_best`, PSM 3 and 300 dpi. `yid+eng` is a second whole-page setting. Qwen3-VL 8B Q4_K_M ran through the installed `usnm_qwen` 1.17 app and llama-server b8799 on an RTX 2060 SUPER, outside BOINC. Version 1 uses the default image-token minimum without the loop guard; version 3 adds the guard; version 4 combines the guard with a 1,024-token minimum.

Qwen and the Tesseract PSM 7 control read the corpus's 4,011 ALTO line boxes and had their readings joined in ALTO order. They therefore received segmentation and reading order that the whole-page readers had to find themselves; the line readers' page scores measure recognition under that advantage. No page or line results were missing, although Tesseract PSM 7 returned empty text for 139 lines.

The book measure is the mean of page CERs, after Jochre's YIVO simplification on both sides and collapsed paragraph breaks. Our simplifier matched the corpus's simplified text on 103 of 104 pages, with one edit across 167,980 characters. Limits are 95% percentile bootstrap intervals over pages, 2,000 resamples with seed 202611; they do not resample books. CER can exceed 100% when insertions are numerous.

| Reader | Input | Mean page CER | 95% limits |
| --- | --- | ---: | ---: |
| Jochre 3 | Whole page | 1.52% | 1.17 to 1.90 |
| Tesseract `yid`, PSM 3 | Whole page | 8.72% | 7.14 to 10.47 |
| Tesseract `yid+eng`, PSM 3 | Whole page | 16.94% | 15.14 to 18.91 |
| Tesseract `yid`, PSM 7 | ALTO lines | 13.57% | 11.32 to 16.12 |
| Qwen v1 | ALTO lines | 99.04% | 59.00 to 147.96 |
| Qwen v3 | ALTO lines | 28.82% | 26.53 to 31.34 |
| Qwen v4 | ALTO lines | 30.15% | 27.59 to 32.77 |

The paired v4-minus-v3 difference is **+1.33 percentage points** (95% limits +0.35 to +2.23), so v4 was worse on these book pages. Version 1 hit the repetition-loop-at-budget flag on 162 of 4,011 lines; the guard stopped 162 at v3 and 202 at v4. These flags are mechanical, not a human assessment of each reading.

## Newspaper agreement

We selected 100 crops from Yiddish units in `qwen-nonenglish-v1`: 20 pages per LoC title across five titles, with one crop per page. Pages were ranked within title by `sha256("yid-silver-202611:" + doc_id)` and the crop by `sha256(doc_id + ":" + crop_id)`. The selection predates the readers. The candidate pages came from a damaged-OCR pool, so they aren't representative of all Yiddish newspapers.

There is no human transcription for these crops. Distances below are pooled Levenshtein edits over the longer normalized reading, with YIVO simplification, Hebrew points removed and whitespace collapsed. The limits resample pages. **These numbers measure agreement, not accuracy:** no reader is established as correct.

| Pair | Symmetric edit distance | 95% limits |
| --- | ---: | ---: |
| Jochre 3 and Tesseract `yid` | 69.7% | 63.4 to 75.6 |
| Tesseract `yid` and Qwen v4 | 69.9% | 62.3 to 77.5 |
| Jochre 3 and Qwen v4 | 74.1% | 68.1 to 79.4 |
| Qwen v3 and v4 | 59.9% | 49.4 to 68.7 |

The book result doesn't establish which reader is best on newspapers. A human-transcribed newspaper test set is needed before choosing one or launching a full version 4 reread.

## Data and provenance

The [aggregate-only Study 8 supplement](https://github.com/tgoodyear/usnewsmap.com/releases/tag/data-report-data-s8-supplement-20261102-b7f7371) includes the complete summary scores, line diagnostics, newspaper agreement and provenance, but no images, transcriptions, OCR text or per-page or per-crop results. Its [archive](https://github.com/tgoodyear/usnewsmap.com/releases/download/data-report-data-s8-supplement-20261102-b7f7371/report-data.tar.gz) has SHA-256 `eff92070058bf0d065e3febcde6f215dc4f2f955e4a1dca002189de1e44e5030`; verify it against the published [checksums](https://github.com/tgoodyear/usnewsmap.com/releases/download/data-report-data-s8-supplement-20261102-b7f7371/SHA256SUMS) before extracting at the repository root. The [member list](https://github.com/tgoodyear/usnewsmap.com/releases/download/data-report-data-s8-supplement-20261102-b7f7371/report-data.members.txt) identifies the files.

The supplement is **not** the cumulative `report-data` set and does not change `ops/data-assets.json`. The normal cumulative packer would also carry forward the earlier release and tracked November study data, including textual reader outputs. Rights review and a safe migration or exclusion plan for that earlier material remain necessary before publishing a cumulative replacement or completing that data step for merge.

The retained test-ID and newspaper-selection digests were recorded after scoring. We did not locate a per-file split hash manifest, so the pinned revision and retained lists do not establish the exact bytes used for each page. This is a byte-level reproducibility limitation, not by itself a blocker for the aggregate supplement.

The checked-in runners were hardened after scoring to retry failed Jochre and Tesseract reads, exclude readings without successful timing receipts, check Jochre ZIP integrity, and stop native workloads before restoring BOINC GPU mode. The native runner supervises its whole batch and publishes a unit's result marker only after its output copies complete. The window supervisor reaps descendants, including detached sessions, and confirms cleanup before restoring GPU mode. If confirmation is missing, it attempts a permanent `never` hold and requires an operator to inspect and manually resume. The native runner and deadline helper require either a BOINC hold expiring at least 60 seconds after their deadline or an explicit `--no-boinc` for a standalone host. They do not themselves establish or verify the initial hold: on a BOINC host, use the window wrapper.

If cleanup approaches the timed suspension's expiry, the supervisor retries extending the hold. A failed BOINC RPC can prevent either extension or the permanent hold; an uncatchable termination of the window wrapper or a standalone supervisor can prevent cleanup or hold extension. In those cases, a surviving workload can outlast a timed suspension and needs operator intervention; inspect the processes before any manual GPU-mode change.

The historical Jochre runs have HTTP 200 receipts and valid text-containing ZIPs for all 104 book pages and 100 newspaper crops. Study 8 calls its own Tesseract runner, not the earlier studies' Tesseract wrappers. Both whole-page settings have 104 of 104 successful receipts, the line setting has 4,011 of 4,011, and the newspaper run has 100 of 100; the scored readings match the retained outputs. The 139 empty line readings are noted above. An exit code alone cannot establish that a reading is semantically complete. No readers were rerun for these changes.

Run the [synthetic regression tests](../../../scripts/ci/test_gpu_study_yiddish.py) from the repository root with `python3 -B -I scripts/ci/test_gpu_study_yiddish.py`. The descendant tests require Linux, `/proc` and pidfd support; they use mock commands rather than BOINC, GPUs or OCR.

# Spend

Credit went from $49.88 to $37.97, so $11.91 of the $40 limit. The ledger's estimate, hourly price times time from instance creation to destruction, comes to $11.40. That estimate differs from what was paid because it counts loading time and leaves out bandwidth and storage, and it hasn't been checked against Vast's billing records.

| Study | Cap | Instances | Paid hours | Estimated $ |
| --- | ---: | ---: | ---: | ---: |
| 1: German Fraktur | 8 | 18 | 6.25 | 1.17 |
| 2: LoRA | 14 | 3 | 4.12 | 0.81 |
| 3: comparison | 10 | 10 | 8.61 | 1.71 |
| 4: pages per dollar | 8 | 4 | 7.19 | 1.98 |
| 5: guard, 8B, deployed path | | 30 | 28.52 | 5.36 |
| 6: Surya retest | | 3 | 4.10 | 0.24 |
| 7: image tokens | | 1 | 1.90 | 0.13 |
| 8: Yiddish, local | | 0 | 0 | 0.00 |
| All | 40 | 69 | 60.70 | 11.40 |

Study 5's figure includes the lost 8B run, about 4 hours and $0.85, and the 8B deployed reads, which stopped once when four parallel rentals together tripped one rental's spend cap. The cap compares account credit, so it counts other rentals' spend; it should track its own instance.

Of 69 instances, 37 ended with a clean job exit. The others: 20 with no SSH within 10 minutes, 6 interrupted by us, 2 failed pushes, and 4 setup failures (job exits 7, 9 and others). GPU use was sampled from midway through Study 1 onward: of 44.0 job hours sampled, 38.8 were busy, 88%. Before sampling, about 2.8 paid hours went on Tesseract, which never used the GPU.

| Job | Card | Peak VRAM | Cost |
| --- | --- | ---: | ---: |
| 2B LoRA training | RTX 3090 | 21.7 GB reserved, 8.8 GB allocated | $0.35 |
| 8B LoRA training | RTX 4090 | 24 GB reserved, 22.5 GB allocated | about $0.53 for 76 minutes |
| 8B BF16 inference | 3090, 4090 | 18.8 GB | |
| 2B BF16 inference | RTX 3060 | about 5 GB | |
| Qwen 8B Q4_K_M | 3090, 4090, 3060 | 6.9 to 7.2 GB | $3.40 per 1,000 pages on the 3090 |
| Qwen 2B Q8_0 | RTX 3060 | 3.5 GB | |
| CHURRO | 3090 | 9.2 GB | about $0.41 per 1,000 crops |
| GOT-OCR | 3090 | 4.5 GB | about $0.44 per 1,000 crops |
| Surya GGUF | 3090, 4090, 3060, 2060 SUPER | 2.1 to 2.2 GB | $36 per 1,000 tiled pages on the 3090 |
| Surya native on vLLM | RTX 3060 | 9.9 GB | |
| Granite | 3090, then 2060 SUPER | 1.4 GB on the 3090, 3.9 GB shared | about $1.10 per 1,000 crops on the 3090 |
| Tesseract, NDLOCR-Lite, American Stories | Local CPU, after the first Tesseract rental | none | no rental cost |

From Study 5 on, jobs that fit in 12 GB ran on RTX 3060s at $0.058 to $0.078 an hour. Readers ran on different cards, which affects speed comparisons.

# Departures from the protocol

- Post-hoc views. The no-loop scores and folded scores weren't planned. They're labelled wherever used.
- Tesseract ran from the pinned worker image under `chroot` on a plain base, because that image's container closes SSH on Vast. Study 1's crop arms ran on a rented host with an unused GPU. The remaining Tesseract arms ran on a local node at the same version and flags.
- The earlier rental unpacker ignored OCI layer whiteouts, so the historical rental rootfs could have retained files removed by the pinned image; its exact filesystem fidelity is unverified. The updated unpacker checks each layer and applies its whiteouts before additions, requires an empty, exclusively used rootfs, and refuses symlink ancestors, filesystem name aliases, and unsupported extended or sparse metadata. Non-ASCII paths also fail closed on name-aliasing filesystems. The pinned image was not fetched or retested, and no historical run was repeated.
- The updated earlier-study Tesseract wrappers require verified model pins. `jpn_vert` has no approved pin in the worker lock, so that Japanese run is unavailable until one is reviewed.
- Surya used image digest `e326a2a5` locally and `bccf7cba` rented, with the same Surya version, llama.cpp build and weights digest.
- CHURRO ran in a Python 3.12 environment with torch 2.8.0, at the pinned commit and model revision, since its package needs 3.12.
- American Stories ran as one batch on a local CPU with torch 1.13.1, onnxruntime 1.16.3 and timm 0.6.12, not the repository's pins, which don't run on this kernel.
- Granite had a 1,024-token cap, after about 97 s a crop at 4,096, and read 285 of 612 crops, 140 on a 3090 and 145 on the local 2060 SUPER.
- MinerU and dots.ocr weren't run, here or on Japanese.
- Reading order has 8 Surya pages of 20.
- Study 4 stopped Surya at 18 and 20 pages of 40, and Qwen on the 2060 SUPER wasn't measured.
- Order. Study 3's local tail overlapped Study 4's rentals.
- Japanese crops came from public scans held on a workstation, a one-off copy of about 42 MB.
- Study 5 applied the guard by cutting finished readings, not by rereading, except on validation, where the live guard was run and compared.
- Study 5's 8B baseline and adapter test reads were split across machines to save time; each crop was scored once.
- Study 5's 8B training ran on a 4090 after the 3090 run's files were lost.
- Study 5's BLN600 images were converted from TIFF to PNG, pixel for pixel, for llama.cpp; the request had labelled them JPEG.
- Study 6 ran the native weights with Surya's vLLM settings scaled to a 12 GB card.
- Study 7 ran the image's CUDA build, not the native Vulkan build, and raised context above 4,096 for 2,048 and 3,072 image tokens. The typed Yiddish crops of #529 weren't found.
- Study 6 started alongside Study 5 rather than after it.
- Study 8 was added after the original protocol. Its line readers used corpus segmentation and order, unlike the page readers; its book results do not establish newspaper accuracy.

# Findings about the rental

- Vast puts a per-instance key in `/root/.vast_api_key` on the rental we inspected, readable by the host. It isn't our account key. It got 401 from the account endpoint and 200 from the instance list. Nothing of ours was on the hosts. We didn't test what else that key can do.
- `env` didn't reach the login shell on our images, and `HOME` stayed `/tmp`, so the forced-tmux switch has to be set in `$HOME`.
- The Tesseract worker image closes SSH on Vast, so Tesseract ran elsewhere under `chroot`.
- llama.cpp fell back to CPU on GeForce cards when the image's CUDA compat library came first on the library path. The job now requires a CUDA device and stops otherwise.
- Pulling an image on the BOINC client's machine holds podman's store lock, which delayed BOINC's container starts by at least 3 minutes, going by process start times on the client. Nothing should be pulled there while BOINC runs.

# Open items

- #583, #835, #926, #968, #812 and #925 all stay open.
- Whether looping explains part of #1420's inconclusive Qwen results.
- A person who reads Japanese hasn't checked any Japanese result (#135).
- Nobody has read the invented-text crops. The ids are in the data.
- The adapters are exported and read in the `usnm_qwen` images, but nothing was signed, published or sent to BOINC, and no volunteer has run them.
- The guard shipped in `usnm_qwen` 1.17; its effect on omissions in Yiddish newspapers needs a transcribed sample.
- Podman temp files from a cancelled pull remain on the BOINC client container; a permission check blocked their removal, so they still need deleting.
- Surya's native weights scored worse than the GGUF on 50 crops, with server settings that may be the cause.
- Study 7's Yiddish crops have no transcriptions, so it can't say whether any image-token value reads them better. Study 8 adds book accuracy, not newspaper accuracy.
- MinerU and dots.ocr remain unrun.
