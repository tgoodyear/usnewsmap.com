---
title: "GPU studies, November 2026"
subtitle: "German Fraktur bake-off, a LoRA adapter on held-out text, an engine comparison with Japanese, and pages per dollar"
date: "November 2026"
---

The samples, split, metrics and decision rules were fixed in [`protocol.md`](protocol.md) and committed before any run. The Japanese amendment was committed before any Japanese reading. Departures are listed under [Departures from the protocol](#departures-from-the-protocol). Scripts are in [`scripts/`](scripts/), the scoring tool in [`tools/`](tools/), and scores in [`data/`](data/).

All limits are 95% bootstrap limits, resampling source pages. A number from the protocol is the primary result. Anything marked post-hoc was chosen after seeing results.

# Summary

Qwen3-VL reads most historical print better than Tesseract, and fails badly on a small share of crops by looping. On the German measured set the protocol's pooled CER is 49.6% for Qwen 8B against 17.9% for `frak2021`, and 9 looped crops of 1,060 account for that. A loop guard is the first change to make.

A rank-8 LoRA adapter on Qwen 2B cut held-out Reichsanzeiger CER from 62.0% to 17.3%, mostly by looping less. It showed no gain on English, and it omits more. It stays untrusted.

Full-page Surya costs about ten times Qwen's crop read per page. A 4090 was faster than a 3090 and dearer per page.

On Japanese, NDLOCR-Lite, CHURRO and Surya agree most with each other and with the machine references. That is agreement, not accuracy. No human transcription of these crops exists that we could verify.

# Study 1: German Fraktur (#926, #835)

Readers: Qwen3-VL 8B Q4_K_M on an RTX 3090, Tesseract 5.5 `frak2021` and `deu_latf+deu`, and LoC's text on the pilot pages.

## Measured CER, on human transcriptions

1,060 crops from 187 source pages: 700 Reichsanzeiger line pairs from 20 gazette pages, and 360 NZZ crops from 167 Swiss newspaper pages. Neither set comes from our corpus. LoC has no reading of them, so LoC isn't in this table.

| Reader | Items | Pooled CER | 95% limits | Median crop CER | Pooled WER |
| --- | ---: | ---: | ---: | ---: | ---: |
| Qwen3-VL 8B | 1,060 | 49.6% | 26.4 to 78.2 | 4.5% | 117.4% |
| `frak2021` PSM 3 | 1,060 | 17.9% | 12.8 to 23.5 | 5.9% | 35.4% |
| `frak2021` PSM 6 | 1,060 | 16.6% | 11.4 to 22.5 | see data | 34.6% |
| `deu_latf+deu` PSM 3 | 1,060 | 24.9% | 18.9 to 31.2 | see data | 58.5% |
| `deu_latf+deu` PSM 6 | 1,060 | 23.0% | 17.1 to 29.1 | see data | 55.6% |

The primary comparison goes against Qwen: its pooled CER is 31.6 points above `frak2021` PSM 3 (limits 9.3 to 58.3).

That result comes from a few crops. 9 of the 1,060 readings ran out of tokens in a repetition loop, each adding up to 2,048 tokens of invented text against a one-line reference. Leaving those 9 crops out for both readers (post-hoc) gives:

| Reader | Items | Pooled CER | 95% limits | Pooled WER |
| --- | ---: | ---: | ---: | ---: |
| Qwen3-VL 8B, no looped crops | 1,051 | 10.9% | 8.3 to 13.7 | 29.3% |
| `frak2021` PSM 3, same crops | 1,051 | 17.7% | 12.6 to 23.2 | 35.1% |

On those crops Qwen is 6.8 points lower (limits 4.0 to 9.9). So Qwen reads most Fraktur better than `frak2021` and fails badly on under 1% of crops. Its pooled number means little until a loop guard exists.

Both views count Qwen's modernizing against it. It writes `s` for `ſ`, and these references keep `ſ`. Folding long s, the double hyphen and superscript e on both sides (post-hoc) gives Qwen a median crop CER of 1.7% and `frak2021` PSM 3 of 3.1%. An engine that modernizes isn't transcribing diplomatically, and whether that suits our search text is a question for #926.

The invented-text check counts 88 major hallucinations for Qwen and 126 for `frak2021` PSM 3, of 1,060 crops. It can't tell fluent invention from garbage, and most of Tesseract's look like garbage. Nobody has read them.

## Agreement, not accuracy, on the pilot pages

100 pages frozen from the pilot list, 1,187 crops. There's no transcription here. These numbers say how far readers differ, and nothing about which is right. The list records no typeface, so these aren't known to be Fraktur pages.

| Pair | Symmetric edit rate | 95% limits | Mean word-bag Dice |
| --- | ---: | ---: | ---: |
| `frak2021` PSM 3 and `deu_latf` PSM 3 | 20.1% | 18.3 to 21.7 | 0.57 |
| Qwen and `frak2021` PSM 3 | 47.0% | 40.6 to 52.9 | 0.53 |
| Qwen and LoC | 55.6% | 50.8 to 60.1 | 0.41 |
| `frak2021` PSM 3 and LoC | 40.5% | 37.8 to 43.3 | 0.34 |
| `deu_latf` PSM 3 and LoC | 43.1% | 40.7 to 45.7 | 0.31 |

46 of the 1,187 pilot readings by Qwen looped, 3.9%, four times the rate on the measured set. On whole pages over all 100, word-bag Dice is 0.26 between `frak2021` and LoC and 0.35 between `deu_latf` and LoC.

No German trust threshold comes out of this study, and #835 stays open.

# Study 2: LoRA training (#583)

A rank-8 adapter on Qwen3-VL-2B, trained for 2,000 optimizer steps on 4,032 crops from the Austrian newspapers, NewsEye BnF and NewsEye NLF Swedish. Validation was NZZ and NewsEye Finnish (378 crops), test Reichsanzeiger and Overproof (1,000 crops, 38 pages). The split was fixed before training: SHA-256 of [`data/samples/lora-split.json`](data/samples/lora-split.json) is `65fe42a25f9e2ee6f30c975b2c7bc9398c5293493a01ccad165930a51606ff81`.

Training took 61 minutes on an RTX 3090, with 8.8 GB allocated at peak and about 21.7 GB reserved. All 4,032 examples encoded.

**Validation.** Used once, to pick a checkpoint.

| Checkpoint | Pooled CER | 95% limits | Pooled WER |
| --- | ---: | ---: | ---: |
| Baseline | 28.1% | 6.7 to 67.3 | 74.2% |
| 250 steps | 5.0% | 3.9 to 6.2 | 22.1% |
| 500 steps | 5.0% | 3.8 to 6.8 | 19.2% |
| 1,000 steps | 3.5% | 2.9 to 4.4 | 15.6% |
| 2,000 steps | 3.7% | 2.9 to 4.8 | 15.8% |

The 1,000-step adapter was selected, SHA-256 `b6bc8c2a1c6b4e953e10321ebb8bdf27a363840cb7a8b05bf92eb16ed6d77b80`.

**Held-out test.** Scored once.

| Source | Reader | Items | Pooled CER | 95% limits | Pooled WER | 95% limits |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Reichsanzeiger | Baseline | 700 | 62.0% | 29.6 to 100.1 | 100.8% | 62.3 to 140.4 |
| Reichsanzeiger | Adapter | 700 | 17.3% | 8.5 to 31.3 | 43.5% | 28.8 to 66.8 |
| Overproof | Baseline | 300 | 8.2% | 1.1 to 21.1 | 10.7% | 3.8 to 22.7 |
| Overproof | Adapter | 300 | 5.3% | 2.1 to 10.6 | 13.9% | 6.3 to 29.9 |

The primary result is a paired CER difference of -44.7 points on Reichsanzeiger (limits -78.0 to -17.8). That passes the proposed 10% relative gate on CER. Much of the gain is fewer loops: 16 test readings hit the 1,024-token cap under the baseline and 3 under the adapter.

On Overproof, English that isn't in the training data, the difference is -2.9 points with limits that include zero (-17.2 to 6.7). So this is no gain shown for English, and its WER moved the wrong way, with wide limits.

**Invented text** on the 1,000 test crops, against the guards of [13 §13.7.7](../../design/13-ocr-engines.md#1377-cascade-optional-per-route):

| Reader | Major hallucinations | Invented runs | Over-length guard | Under-length guard | Word-overlap guard |
| --- | ---: | ---: | ---: | ---: | ---: |
| Baseline | 90 | 88 | 91 | 1 | 15 |
| Adapter | 56 | 56 | 61 | 16 | 20 |

The adapter invents less and omits more. Under-length failures rose from 1 to 16, word-overlap failures from 15 to 20. The gate asks for no material regression in omissions, so that rise needs reading before anyone calls this a pass.

**Status.** The adapter stays `untrusted-requires-held-out-evaluation`. It wasn't exported to the GGUF the BOINC app runs, so nothing here measures the deployed path. Base is 2B, not the 8B of Study 1. Rented hardware doesn't meet #583's volunteer CUDA closure, so #583 stays open. These runs exceed the 64-step bound and the image limit of the volunteer contract: at the contract's roughly 128 image tokens, newspaper text can't be resolved.

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

Read the table with these limits:

- **A crop isn't a page.** Surya's full-page mode and American Stories' layout model are built for pages. On NZZ's one-line crops Surya's median crop CER is 100%, and 317 of 612 crops fail the under-length guard. American Stories returned nothing on 530 of 611 crops. This says those crops don't suit them, and little about their reading.
- **Tesseract** used `eng`, `frak2021` or `fra+fin+swe` by source, at PSM 6. Qwen and CHURRO were given no language.
- **CHURRO's training data includes Chronicling America**, so its Overproof result can't be separated from training exposure. Licence: Qwen Research License.
- **Granite** read 285 crops: all but 327 of NZZ's, so its NZZ score isn't shown. It had a 1,024-token cap.
- **Sets under 5 pages aren't ranked.** Swedish has 4 pages.
- **Cards differed**, which affects speed comparisons and not accuracy.

Across the 612 crops, major hallucinations number 32 for Qwen, 48 for Tesseract, 45 for Surya, 54 for CHURRO, 20 for American Stories and 88 for GOT, and 98 of Granite's 285. Tesseract's are mostly garbage, and American Stories mostly returned nothing.

MinerU and dots.ocr were not run. Their terms in 13 clear this use: MinerU's conditions bind online services, and the maintainer cleared dots on 2026-10-11. They were dropped for time, and remain to do.

## Japanese: agreement, not accuracy

64 crops, 32 typeset and 32 from camp papers. **Nothing here measures accuracy.** The 23 references are machine-made and barely checked (#135). No verifiable public human transcription of these crops turned up. CODH's modern magazines and NDL's set are transcriptions of other material, so they don't cover these crops.

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
| Surya and CHURRO | 0.74 | see data | see data |
| NDLOCR-Lite and Qwen | 0.63 | 0.76 | 0.57 |
| Others with `jpn_vert` | 0.10 to 0.22 | see data | see data |

Failures, by the amendment's mechanical counts, on 64 crops:

| Reader | Under 35% Japanese | Vertical-layout failure | Loop | Invented text |
| --- | ---: | ---: | ---: | ---: |
| NDLOCR-Lite | 5 | 4 | 0 | 1 |
| CHURRO | 5 | 4 | 3 | 2 |
| Surya | 2 | 1 | 2 | 5 |
| Qwen3-VL 8B | 6 | 4 | 15 | 4 |
| Tesseract `jpn_vert` | 0 | 0 | 0 | 59 |

Qwen's 15 loops, nearly a quarter of its crops, are the clearest failure here. `jpn_vert`'s 59 mean its Japanese-script output shares almost nothing with any other reader, which fits garbage more than fluent invention. A vertical-layout count can also be a crop with English on it, as these papers ran English sections. Nobody who reads Japanese has checked this.

## Reading order on multi-column pages (#925)

Kendall's tau over words occurring once on both sides, against a geometric column order built from LoC's ALTO. This is a heuristic reference, not a truth. LoC's score is partly circular, because the reference comes from its own blocks.

| Reader | Pages with 20 or more matched words | Median tau | Lowest | Pages under 0.5 |
| --- | ---: | ---: | ---: | ---: |
| LoC | 20 | 0.97 | 0.57 | 0 |
| `frak2021` PSM 3, whole page | 12 | 0.80 | 0.11 | 3 |
| Surya, tiled | 8 | 0.35 | 0.16 | 7 |

Tiled Surya loses column order on most pages. 3 of 12 `frak2021` pages do too, which fits #925's report. The sample is small, and for Surya it's the 8 pages read before it was stopped.

# Study 4: pages per dollar (#296)

The same pages from the frozen sample. Cost is everything paid for the instance, from creation to destruction, at its hourly price.

| Workload | Card | Pages read | Pages per hour, reading | $/hour | Instance minutes | $ per 1,000 pages |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Qwen 8B, 12 crops a page | RTX 3090 | 40 | 91 | 0.19 | 43 | 3.40 |
| Qwen 8B, 12 crops a page | RTX 4090 | 40 | 141 | 0.37 | 30 | 4.70 |
| Surya, tiled | RTX 3090 | 18 | 5.9 | 0.20 | 190 | 36 |
| Surya, tiled | RTX 4090 | 20 | 7.5 | 0.36 | 168 | 51 |
| Surya, tiled | RTX 2060 SUPER, local | 1 | 3.9 | 0 | n/a | no rental cost |

Pages per hour for Surya compares the first 18 pages on each rented card. The 2060 SUPER figure is one page, 914 s against 400 s on the 3090 and 314 s on the 4090 for that same page. Each rented run sat on a different host, with its own CPU, and CPU load matters to Surya's layout step, so these aren't clean card effects.

Qwen at 467 crops and Surya on whole pages are different work: a Surya page costs about ten times a page of Qwen crops. The 4090 is about 1.5 times as fast on Qwen and 1.3 times on Surya, at about 1.9 times the price, so the 3090 is cheaper per page on both. Failed hosts aren't in this table; they are in [Spend](#spend). The 2060 SUPER has no rental cost, and its electricity wasn't measured. Qwen wasn't measured on it, because pulling the image would block the BOINC client.

# Spend

Credit went from $49.88 to $44.28, so $5.60 of the $40 limit. Per-study figures are hourly price by instance time from the ledger, which comes to $5.67; bandwidth and storage aren't split out.

| Study | Cap | Instances | Paid hours | Estimated $ |
| --- | ---: | ---: | ---: | ---: |
| 1: German Fraktur | 8 | 18 | 6.25 | 1.17 |
| 2: LoRA | 14 | 3 | 4.12 | 0.81 |
| 3: comparison | 10 | 10 | 8.61 | 1.71 |
| 4: pages per dollar | 8 | 4 | 7.19 | 1.98 |

19 of the 35 instances ended without a clean job exit: hosts with no SSH, pushes that failed, setup errors. Sampling of GPU use began after Study 1 and LoRA training. Of the 15.7 job hours sampled since, 13.5 were busy, 86%. Paid time in total was 26.2 hours. Before sampling, about 2.7 paid hours went on Tesseract, which never used the GPU.

| Job | Card | Peak VRAM | Cost |
| --- | --- | ---: | ---: |
| LoRA training | RTX 3090 | 21.7 GB reserved, 8.8 GB allocated | $0.35 |
| Qwen 8B | 3090, 4090 | 6.9 GB | $3.40 per 1,000 pages on the 3090 |
| CHURRO | 3090 | 9.2 GB | about $0.41 per 1,000 crops |
| GOT-OCR | 3090 | 4.5 GB | about $0.44 per 1,000 crops |
| Surya | 3090, 4090, 2060 SUPER | about 2.2 GB | $36 per 1,000 pages on the 3090 |
| Granite | 3090, then 2060 SUPER | 1.4 GB on the 3090, 3.9 GB shared | about $1.10 per 1,000 crops on the 3090 |
| Tesseract, NDLOCR-Lite, American Stories | Local CPU, after first Tesseract rental | none | no rental cost |

Most of these could have used a cheaper card. A 12 GB RTX 3060 was on offer at about $0.06 an hour, against about $0.19 for a 3090, but no job here ran long enough on one to measure its cost per page.

# Departures from the protocol

- **Post-hoc views.** The no-loop scores and folded scores weren't planned. They're labelled wherever used.
- **Tesseract** ran from the pinned worker image under `chroot` on a plain base, because that image's container closes SSH on Vast. Study 1's crop arms ran on a rented host with an unused GPU. The remaining Tesseract arms ran on a local node at the same version and flags.
- **Surya** used image digest `e326a2a5` locally and `bccf7cba` rented, with the same Surya version, llama.cpp build and weights digest.
- **CHURRO** ran in a Python 3.12 environment with torch 2.8.0, at the pinned commit and model revision, since its package needs 3.12.
- **American Stories** ran as one batch on a local CPU with torch 1.13.1, onnxruntime 1.16.3 and timm 0.6.12, not the repository's pins, which don't run on this kernel.
- **Granite** had a 1,024-token cap, after about 97 s a crop at 4,096, and read 285 of 612 crops, 140 on a 3090 and 145 on the local 2060 SUPER.
- **MinerU and dots.ocr** weren't run, here or on Japanese.
- **Reading order** has 8 Surya pages of 20.
- **Study 4** stopped Surya at 18 and 20 pages of 40, and Qwen on the 2060 SUPER wasn't measured.
- **Order.** Study 3's local tail overlapped Study 4's rentals.
- **Japanese crops** came from public scans held on a workstation, a one-off copy of about 42 MB.

# Findings about the rental

- Vast puts a per-instance key in `/root/.vast_api_key` on every rental, readable by the host. It isn't our account key. It got 401 from the account endpoint and 200 from the instance list. Nothing of ours was on the hosts. We didn't test what else that key can do.
- `env` didn't reach the login shell on our images, and `HOME` stayed `/tmp`, so the forced-tmux switch has to be set in `$HOME`.
- The Tesseract worker image closes SSH on Vast, so Tesseract ran elsewhere under `chroot`.
- llama.cpp fell back to CPU on GeForce cards when the image's CUDA compat library came first on the library path. The job now requires a CUDA device and stops otherwise.
- Pulling an image on the BOINC client's machine holds podman's store lock, which delayed BOINC's container starts by several minutes. Nothing should be pulled there while BOINC runs.

# Open

- #583, #835, #926, #968, #812 and #925 all stay open.
- Whether a loop guard explains part of #1420's inconclusive Qwen results.
- A person who reads Japanese hasn't checked any Japanese result (#135).
- Nobody has read the invented-text crops. The ids are in the data.
- The LoRA adapter has no GGUF export and no BOINC inference comparison.
- MinerU and dots.ocr remain unrun.
