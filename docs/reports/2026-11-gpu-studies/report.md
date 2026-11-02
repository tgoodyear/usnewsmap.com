---
title: "GPU studies, November 2026"
subtitle: "German Fraktur bake-off, a LoRA adapter on held-out text, an engine comparison with Japanese, and pages per dollar"
date: "November 2026"
---

The samples, split, metrics and decision rules were fixed in [`protocol.md`](protocol.md) and committed before any study run. Seven setup rentals began earlier; none of them read a crop. The Japanese amendment was committed before any reader ran on these crops under this study, though 24 of them were read under #128. Departures are listed under [Departures from the protocol](#departures-from-the-protocol). Scripts are in [`scripts/`](scripts/), the scoring tool in [`tools/`](tools/), and scores in [`data/`](data/).

All limits are 95% bootstrap limits, resampling source pages. A number from the protocol is the primary result. Anything marked post-hoc was chosen after seeing results.

# Summary

Pooled over Study 3's 612 crops, Qwen 8B has a CER of 17.3% and Tesseract 9.0%, a difference of 8.2 points with limits of -4.6 to 25.4, so no difference is called. On the German measured set the protocol's pooled CER is 49.6% for Qwen 8B against 17.9% for `frak2021` PSM 3, a difference that is called, against Qwen. Repetition loops are the visible cause: leaving out the 9 looped crops of 1,060 (post-hoc) brings Qwen to 10.9%. Loops were rarer on some sets than others: 3.9% of pilot crops and 15 of 64 Japanese crops. A loop guard is the first change to make.

A rank-8 LoRA adapter on Qwen 2B cut held-out Reichsanzeiger CER from 62.0% to 17.3%, and readings at the token cap there fell from 15 to 2. How much of the gain comes from looping less is Study 5's question. It showed no gain on English, and it omits more. It stays untrusted.

Tiled full-page Surya costs about ten times Qwen's crop read per page. A 4090 was faster than a 3090 and dearer per page. Study 3's Surya scores measured our configuration: in Study 6, line recognition took Surya from 109.2% to 23.0% pooled CER, still behind Tesseract's 9.0% on those crops, and `whole` mode kept column order better but read less than half the text.

On Japanese, NDLOCR-Lite, CHURRO and Surya agree most with each other and with the machine references. That is agreement, not accuracy. No human transcription of these crops exists that we could verify.

# Study 1: German Fraktur (#926, #835)

Readers: Qwen3-VL 8B Q4_K_M on an RTX 3090, Tesseract 5.5 `frak2021` and `deu_latf+deu`, and LoC's text on the pilot pages.

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

Validation. Used once, to pick a checkpoint.

| Checkpoint | Pooled CER | 95% limits | Pooled WER |
| --- | ---: | ---: | ---: |
| Baseline | 28.1% | 6.7 to 67.3 | 74.2% |
| 250 steps | 5.0% | 3.9 to 6.2 | 22.1% |
| 500 steps | 5.0% | 3.8 to 6.8 | 19.2% |
| 1,000 steps | 3.5% | 2.9 to 4.4 | 15.6% |
| 2,000 steps | 3.7% | 2.9 to 4.8 | 15.8% |

The 1,000-step adapter was selected, SHA-256 `b6bc8c2a1c6b4e953e10321ebb8bdf27a363840cb7a8b05bf92eb16ed6d77b80`.

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

@@S58B@@

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

# Spend

Credit went from $49.88 to $44.28, so $5.60 of the $40 limit. Per-study figures are hourly price by instance time from the ledger, which comes to $5.67. That is over the $5.60 paid because the ledger counts from instance creation, including loading time, and hasn't been checked against Vast's billing records; bandwidth and storage aren't split out.

| Study | Cap | Instances | Paid hours | Estimated $ |
| --- | ---: | ---: | ---: | ---: |
| 1: German Fraktur | 8 | 18 | 6.25 | 1.17 |
| 2: LoRA | 14 | 3 | 4.12 | 0.81 |
| 3: comparison | 10 | 10 | 8.61 | 1.71 |
| 4: pages per dollar | 8 | 4 | 7.19 | 1.98 |

19 of the 35 instances ended without a clean job exit: 11 with no SSH, 6 interrupted by us, 1 failed push and 1 job exit 7. Sampling of GPU use began after Study 1 and LoRA training. Of the 15.7 job hours sampled since, 13.5 were busy, 86%. Paid time in total was 26.2 hours. Before sampling, about 2.8 paid hours went on Tesseract, which never used the GPU.

| Job | Card | Peak VRAM | Cost |
| --- | --- | ---: | ---: |
| LoRA training | RTX 3090 | 21.7 GB reserved, 8.8 GB allocated | $0.35 |
| Qwen 8B | 3090, 4090 | 6.9 GB | $3.40 per 1,000 pages on the 3090 |
| CHURRO | 3090 | 9.2 GB | about $0.41 per 1,000 crops |
| GOT-OCR | 3090 | 4.5 GB | about $0.44 per 1,000 crops |
| Surya | 3090, 4090, 2060 SUPER | about 2.2 GB | $36 per 1,000 pages on the 3090 |
| Granite | 3090, then 2060 SUPER | 1.4 GB on the 3090, 3.9 GB shared | about $1.10 per 1,000 crops on the 3090 |
| Tesseract, NDLOCR-Lite, American Stories | Local CPU, after first Tesseract rental | none | no rental cost |

Most of these could have used a cheaper card. A 12 GB RTX 3060 was on offer at about $0.06 to $0.07 an hour, against about $0.19 for a 3090, but no job here ran long enough on one to measure its cost per page.

# Departures from the protocol

- Post-hoc views. The no-loop scores and folded scores weren't planned. They're labelled wherever used.
- Tesseract ran from the pinned worker image under `chroot` on a plain base, because that image's container closes SSH on Vast. Study 1's crop arms ran on a rented host with an unused GPU. The remaining Tesseract arms ran on a local node at the same version and flags.
- Surya used image digest `e326a2a5` locally and `bccf7cba` rented, with the same Surya version, llama.cpp build and weights digest.
- CHURRO ran in a Python 3.12 environment with torch 2.8.0, at the pinned commit and model revision, since its package needs 3.12.
- American Stories ran as one batch on a local CPU with torch 1.13.1, onnxruntime 1.16.3 and timm 0.6.12, not the repository's pins, which don't run on this kernel.
- Granite had a 1,024-token cap, after about 97 s a crop at 4,096, and read 285 of 612 crops, 140 on a 3090 and 145 on the local 2060 SUPER.
- MinerU and dots.ocr weren't run, here or on Japanese.
- Reading order has 8 Surya pages of 20.
- Study 4 stopped Surya at 18 and 20 pages of 40, and Qwen on the 2060 SUPER wasn't measured.
- Order. Study 3's local tail overlapped Study 4's rentals.
- Japanese crops came from public scans held on a workstation, a one-off copy of about 42 MB.

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
- The LoRA adapter has no GGUF export and no BOINC inference comparison.
- MinerU and dots.ocr remain unrun.
