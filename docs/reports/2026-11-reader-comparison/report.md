# Reader comparison, results (#968)

12 November 2026. Tesseract, LoC and American Stories are scored, and so are Xiaomi-OCR-0 and guarded Qwen3-VL 8B, both as local runs on one GPU host (CT 214, an RTX 2060 SUPER), not on BOINC. Qwen3-VL 2B hasn't run, and a BOINC run of Xiaomi on volunteer hardware is still to come. The sets, readers, scoring and decision rules are those of the [protocol](protocol.md), and the [freeze record](protocol.md#freeze-record) lists the hashes. Departures from the protocol are in its [departures log](protocol.md#departures-log).

## What ran

Every CER below is pooled character error rate against human transcriptions, with missing output kept in the denominator, scored by the GPU studies' `gpu-study score`. Limits are 95% percentile bootstraps resampling whole source pages, 2,000 resamples, seed 202611. The normalization is the gold set's (`gold.py normalize`). Median crop CER and WER are in the tables beside it.

- **Tesseract 5.5.0**, run on our own dev node (usnm-dev2) with the Debian build, the pinned traineddata and `--dpi 300`, at PSM 6 (the protocol's primary) and PSM 3 (what gold units run). No crop failed or timed out. A few came back empty: 2 on `fr-new` and 2 on `de-rz81` at PSM 3, and 7 on `de-nzz` at PSM 6 and 13 at PSM 3. They are scored as empty.
- **Xiaomi-OCR-0** (Q8_0 GGUF, f16 projector) on llama.cpp b12084 Vulkan, local run on CT 214: request version 5 settings (native size, Study 9's region prompt, no loop guard, 2,048 tokens). All 1,259 crops returned text, in 38 minutes.
- **Guarded Qwen3-VL 8B** (Q4_K_M, the `usnm_qwen` files) on the same runtime and host: request version 3 (prompt, loop guard, 2,048 tokens, images over 2,048 px fitted), with a second pass at a larger context for crops the first couldn't read. All 1,259 crops returned text; the guard stopped 3, and 68 crops were fitted.
- **LoC** on `en-op`: the clipped ALTO text, 345 of 345 crops aligned.
- **American Stories** on `en-op`: the readings from Study 3, unchanged. It returned text on 59 of the 345 crops, and the [GPU studies report](../2026-11-gpu-studies/report.md) found the same on its 611 crops.

## Tesseract by set

| Set | Crops | Pages | Reader | CER (95% limits) | WER | Median crop CER | PSM 3 minus PSM 6, points (95% limits) |
| --- | ---: | ---: | --- | --- | ---: | ---: | --- |
| `en-op` (second use) | 345 | 18 | PSM 6 | 4.06% (2.96 to 5.17) | 13.26% | 2.30% | |
| | | | PSM 3 | 4.42% (3.02 to 5.84) | 13.68% | 2.39% | +0.35 (-0.11 to 0.96) |
| `fr-new` (held out) | 60 | 20 | PSM 6 | 11.52% (6.31 to 17.17) | 33.91% | 5.47% | |
| | | | PSM 3 | 14.41% (7.25 to 23.51) | 36.04% | 5.45% | +2.90 (-0.02 to 8.16) |
| `fr-52` (second use) | 24 | 8 | PSM 6 | 5.14% (3.38 to 7.73) | 20.45% | 4.39% | |
| | | | PSM 3 | 8.91% (3.39 to 17.13) | 23.28% | 4.34% | +3.77 (-0.08 to 10.83) |
| `fi-new` (held out) | 60 | 20 | PSM 6 | 10.46% (9.34 to 11.42) | 54.10% | 10.39% | |
| | | | PSM 3 | 10.41% (9.31 to 11.33) | 54.03% | 10.41% | -0.05 (-0.14 to 0.01) |
| `fi-52` (second use) | 18 | 6 | PSM 6 | 7.88% (4.12 to 10.11) | 43.06% | 9.21% | |
| | | | PSM 3 | 7.94% (4.07 to 10.16) | 43.13% | 9.21% | +0.06 (0.00 to 0.19) |
| `sv-new` (held out) | 60 | 22 | PSM 6 | 8.89% (6.93 to 10.48) | 38.23% | 11.02% | |
| | | | PSM 3 | 8.75% (6.79 to 10.42) | 37.98% | 10.40% | -0.13 (-0.25 to -0.03) |
| `sv-52` (second use) | 10 | 4 | PSM 6 | 11.01% (10.60 to 11.41) | 47.10% | 11.41% | |
| | | | PSM 3 | 10.82% (9.85 to 11.22) | 46.46% | 11.14% | -0.19 (-1.34 to 0.00) |
| `de-rz81` (Study 11 pages) | 322 | 81 | PSM 6 | 4.90% (4.02 to 5.89) | 14.68% | 3.43% | |
| | | | PSM 3 | 4.66% (3.63 to 5.90) | 14.47% | 2.68% | -0.23 (-0.91 to 0.69) |
| `de-nzz` (second use) | 360 | 167 | PSM 6 | 13.36% (10.43 to 16.76) | 39.50% | 5.26% | |
| | | | PSM 3 | 15.57% (12.23 to 19.60) | 39.32% | 5.45% | +2.21 (0.06 to 4.64) |

German uses `frak2021` at both PSMs, French `fra`, Finnish `fin`, Swedish `swe`, English `eng`. `sv-52` has 4 pages, so by rule 1 it decides nothing.

PSM 3 and PSM 6 differ with limits that exclude zero in two places: Swedish `sv-new` (PSM 3 lower by 0.13 points) and NZZ (PSM 3 higher by 2.21 points). Every other pair has limits that include zero, or only touch it. The PSM that the live German feed uses (3) is the better of the two on `de-rz81` and the worse on `de-nzz`, and the `de-rz81` difference has limits that include zero.

## Xiaomi-OCR-0 and Qwen 8B against Tesseract (local run)

CER, with the paired difference from Tesseract PSM 6 in points (95% limits). A difference whose limits are below zero is better by rule 1. The last column is Xiaomi minus 8B on the same crops.

| Set | Tesseract PSM 6 | Xiaomi | Xiaomi minus PSM 6 | 8B | 8B minus PSM 6 | Xiaomi minus 8B |
| --- | ---: | ---: | --- | ---: | --- | --- |
| `en-op` (second use) | 4.06% | 2.12% | -1.94 (-2.49 to -1.33) | 3.22% | -0.84 (-2.21 to 0.92) | -1.10 (-2.73 to 0.12) |
| `fr-new` (held out) | 11.52% | 6.08% | -5.43 (-7.26 to -3.70) | 6.05% | -5.47 (-7.28 to -3.70) | +0.03 (-0.17 to 0.24) |
| `fr-52` (second use) | 5.14% | 2.62% | -2.53 (-4.50 to -1.37) | 2.42% | -2.73 (-4.70 to -1.53) | +0.20 (0.09 to 0.34) |
| `fi-new` (held out) | 10.46% | 5.40% | -5.06 (-6.23 to -3.71) | 3.30% | -7.16 (-7.95 to -6.27) | +2.10 (1.22 to 3.13) |
| `fi-52` (second use) | 7.88% | 4.22% | -3.66 (-5.02 to -1.67) | 2.33% | -5.55 (-7.26 to -2.43) | +1.89 (0.97 to 2.65) |
| `sv-new` (held out) | 8.89% | 2.53% | -6.36 (-7.75 to -4.71) | 2.09% | -6.79 (-8.27 to -5.03) | +0.44 (0.14 to 0.74) |
| `sv-52` (second use, 4 pages) | 11.01% | 3.03% | -7.98 (-9.50 to -6.18) | 1.71% | -9.30 (-9.81 to -8.20) | +1.32 (0.20 to 2.84) |
| `de-rz81` (Study 11 pages) | 4.90% | 5.15% | +0.25 (-0.69 to 1.07) | 4.80% | -0.10 (-0.99 to 0.74) | +0.35 (0.14 to 0.56) |
| `de-nzz` (second use) | 13.36% | 7.99% | -5.38 (-8.24 to -2.72) | 8.91% | -4.46 (-6.34 to -2.77) | -0.92 (-3.57 to 1.37) |

Major invented-text counts (the GPU studies' check, flagged crops not yet read by a person) are Tesseract 69, Xiaomi 16 and 8B 47 over all 1,259 crops. On `en-op` they are 16, 9 and 26. On `fr-new` they are 9, 4 and 4, and on `de-rz81` 25, 1 and 1. On these three sets neither vision-language reader has more than Tesseract except 8B on `en-op` (26 against 16).

These two columns are a local run on one host. The protocol planned them as BOINC campaigns on volunteer GPUs. Guarded 8B couldn't be dispatched as a BOINC benchmark until native routing was merged, and the maintainer then chose a local run for 8B. A BOINC run of Xiaomi on 1.20 app versions is being staged, and its results will be reported beside the local column as a cross-hardware replicate.

## Recommended reader per language

Protocol rule 2 picks the reader with the lowest CER on the language's primary set among those better than Tesseract PSM 6 by rule 1. When the lowest isn't better than another candidate by rule 1, it picks the smaller model of them (Xiaomi before 8B).

| Language | Primary set | Recommended | Why |
| --- | --- | --- | --- |
| French | `fr-new` | Xiaomi-OCR-0 | Both beat Tesseract by about 5.4 points. 8B's 0.03-point edge is inside the limits, so the smaller model wins |
| Finnish | `fi-new` | Qwen 8B | 8B beats Tesseract by 7.16 points and beats Xiaomi by 2.10 (1.22 to 3.13) |
| Swedish | `sv-new` | Qwen 8B | 8B beats Tesseract by 6.79 points and Xiaomi by 0.44 (0.14 to 0.74) |
| German | `de-rz81` | Tesseract stays | Neither reader beats it: Xiaomi +0.25 (-0.69 to 1.07), 8B -0.10 (-0.99 to 0.74). Provisional, as every German set has been read before |
| English | `en-op` only | Xiaomi-OCR-0, provisional | It beats Tesseract by 1.94 points and isn't separated from 8B. `en-op` is second use, so by rule 5 this decides nothing |

German on `de-nzz` (second use) points the other way: both readers beat Tesseract (by 5.38 and 4.46 points). No German set is held out, and rule 6 keeps German results provisional, so the German result stays Tesseract for now. Study 11 (draft #1858) found that Xiaomi loses on tall Reichsanzeiger columns when its images are fitted to 2,048 px. This run reads them at native size, which is the request version 5 setting.

The recommendations are for the maintainer to act on. Nothing here changes production. They rest on one GPU host, so any change should wait for the BOINC replicate of Xiaomi, and the flagged invented-text crops should be read by a person.

## English: LoC and American Stories

`en-op` is a second-use set, so it can't decide the English recommendation by rule 5.

| Reader | Crops read | CER (95% limits) | WER | Against Tesseract PSM 6, points (95% limits) |
| --- | ---: | --- | ---: | --- |
| Tesseract PSM 6 | 345 | 4.06% (2.96 to 5.17) | 13.26% | |
| LoC | 345 | 9.54% (7.14 to 11.77) | 29.14% | +5.48 (3.12 to 7.68) |
| American Stories | 59 of 345 | 97.63% over all 345 | 98.54% | not meaningful |

LoC's text is worse than Tesseract's on these crops by rule 1. The October pilot measured 9.22% for LoC on the same crops with a different extraction, so the 0.3 point gap can come from the extraction rule alone and is not a change in LoC's text.

American Stories returns text on 59 crops, so its 345-crop CER is mostly missing output. On those 59 crops, American Stories scores 85.86% CER (limits 83.47 to 88.64), against 2.87% for Tesseract and 10.13% for LoC on the same 59. The GPU studies report reached the same conclusion for crops and notes that American Stories' layout model is built for pages.

## German Tesseract against LoC

The German Tesseract text (`frak2021`, PSM 3, 300 DPI) is live in search, and the question was whether it beats LoC's text by damage band. This report can't answer it, and the band check is parked: the typed-transcription tool for the 154 band crops was dropped, so those crops have no human truth and no truth source is planned for now. LoC has no text for `de-rz81` or `de-nzz`, so neither has an ours-against-LoC pair, and neither is in the corpus, so neither has a damage band. The absolute German numbers are in the table above: 4.66% on Reichsanzeiger and 15.57% on NZZ at PSM 3.

The crops, boxes and per-crop readings (live ALTO, LoC ALTO and Tesseract on the crop) stay on disk, and `scripts/score_de968.py` stays in this change, so the check can run if a truth source appears. All German frak2021 text is revoked at the next full rebuild, and the check was only ever about which bands could earn their way back later.

## English held-out set: `en-as`

13 November 2026. The protocol has no held-out English set now that `en-bln` is dropped. `en-as` is one: the 10 hand-transcribed full pages of the American Stories gold set (Dell et al. 2023, [arXiv:2308.12477](https://arxiv.org/abs/2308.12477)), cut into one crop per transcribed region. It was built and frozen before any reader ran on it, with `scripts/build_en_as.py`, and `gold.py validate` passes with 0 errors.

**What it is.** 507 crops from 10 pages, 194,649 characters after normalization. The dataset has 597 regions, and 90 of them have no text, so they have no crop. The crops are 307 articles, 199 headlines and 1 image caption. By length they are 257 under 100 characters, 173 from 100 to 500 and 77 over 500. The crops total 46.8 MB (median 32 KB, largest 1.6 MB), up to 4,311 px wide and 7,958 px tall. The text comes from the Hugging Face repository `dell-research-harvard/AmericanStoriesTraining`, folder `gold_data/`, taken once on 13 November.

**Provenance.** The dataset doesn't say where its pages come from. Each one is a Chronicling America front page (edition 1, sequence 1). I matched each page's transcription against LoC's OCR for the issue named in the dataset's own metadata: 75% to 99% of the words match the right page, against 56% or less for any other page in the issue.

| File | LCCN | Title | Date |
| --- | --- | --- | --- |
| 0004 | sn82014248 | Daily Kennebec Journal | 1880-07-01 |
| 0015 | sn96060045 | Eskimo Bulletin | 1902-05-01 |
| 0042 | sn89055163 | Long Valley Advocate | 1904-12-01 |
| 0075 | sn83045462 | Evening Star | 1909-05-07 |
| 0102 | 2016270502 | Waterbury Democrat | 1895-10-15 |
| 0131 | sn84026758 | Democratic Messenger | 1881-10-22 |
| 0272 | sn89055091 | Weiser Signal | 1898-03-24 |
| 0424 | sn84029386 | Abilene Weekly Reflector | 1899-09-07 |
| 0810 | sn87057934 | Audubon County Journal | 1905-10-12 |
| 1230 | sn83040592 | Baxter Springs News | 1915-10-28 |

**Licence.** The repository's metadata says Apache-2.0 and its card is empty. The page images are Chronicling America scans, which are public domain. This settles the two open points in the source policy in `docs/ocr/gold-set.md`, so `en-as` is a scoring source.

**Leakage.**
- American Stories isn't held out on this set. It is the set the dataset's authors evaluated on, and the paper doesn't say whether the 10 pages were kept out of the layout detector's training. Any American Stories column here is marked as not held out.
- CHURRO-DS, the training set of the CHURRO reader, holds 5 of the 10 pages as dataset `stories`: 0004 and 0102 in its dev split, 0131, 0810 and 1230 in its test split, none in train. CHURRO isn't a reader in this comparison, so this is a note only. I read the splits through the Hugging Face dataset server, so a page filed under another name would not show.
- The training data of Xiaomi-OCR-0, Qwen3-VL and Tesseract is undisclosed. The text has been public on Hugging Face since 2023, so I can't rule out that a reader saw it.

**Results.** Tesseract 5.5.0 (Debian build, Leptonica 1.84.1, `eng` from tessdata_best, SHA-256 `8280aed0782fe27257a68ea10fe7ef324ca0f8d85bd2fd145d1c2b560bcb66ba`, 300 DPI) ran on CT 245 with 3 threads. LoC is the clipped ALTO text, with 507 of 507 crops aligned. The scorer and limits are those above: pooled CER, 95% percentile bootstrap over whole pages, 2,000 resamples, seed 202611. There are 10 pages, so the limits are wide. Empty outputs, scored as empty, are 1 for PSM 6, 4 for PSM 3 and 3 for LoC.

| Reader | CER (95% limits) | WER | Median crop CER | Against PSM 6, points (95% limits) |
| --- | --- | ---: | ---: | --- |
| Tesseract PSM 6 | 3.62% (2.20 to 5.79) | 12.82% | 1.50% | |
| Tesseract PSM 3 | 3.40% (2.08 to 5.51) | 11.32% | 1.36% | -0.22 (-0.68 to 0.37) |
| LoC | 7.71% (2.88 to 16.41) | 19.16% | 1.72% | +4.09 (0.56 to 10.84) |

By region length, CER with the PSM 3 difference from PSM 6 and LoC's difference from PSM 6:

| Region length | Crops | PSM 6 | PSM 3 | PSM 3 minus PSM 6 | LoC | LoC minus PSM 6 |
| --- | ---: | --- | --- | --- | --- | --- |
| Under 100 characters | 257 | 4.56% (3.25 to 7.36) | 3.50% (2.63 to 4.91) | -1.06 (-2.57 to -0.39) | 7.24% (3.99 to 13.40) | +2.68 (0.16 to 6.07) |
| 100 to 500 | 173 | 2.63% (1.71 to 4.77) | 3.44% (1.79 to 7.34) | +0.80 (-0.40 to 2.87) | 4.28% (2.09 to 9.62) | +1.65 (0.18 to 5.17) |
| Over 500 | 77 | 3.82% (2.09 to 6.24) | 3.38% (1.81 to 5.54) | -0.44 (-0.75 to -0.16) | 8.67% (2.74 to 19.75) | +4.85 (0.54 to 12.73) |

PSM 3 is lower than PSM 6 with limits that exclude zero under 100 characters and over 500. Overall and from 100 to 500 the limits include zero. LoC is worse than Tesseract PSM 6 overall and in every band, with limits that exclude zero.

**Freeze record.** `manifest.jsonl` SHA-256 `eafddeb411ae59af4ebab968b673f1705732a44fe7983b0d9ceeaa3e7ae86f50`, `crops.jsonl` `37f188de2831edffb08b895e25417d35b6f32f0143acc6345db4d8c97a2b7dae`, the images (all 507 `images/*.jpg`, hashed in path order) `06434ad89d11a28ac854829f43fcf2a0a95e342624beb00f24b8c25894491fe2`. Built with Pillow 11.1.0. `usnm-ocr-survey` built from this branch has SHA-256 `6e935b81e8db522cbaf74c68b95038632b1d6bd4196a79317db2a2ec0842f1f7`, the same as the binary the #968 LoC column used. `gpu-study` was built from this branch with the same two `model: None` initialisers as for the other sets. Outputs SHA-256: PSM 6 `96f3e642f0036edfe90a19e82722d836d727ca466ef02d5f53d8e94a660450ff`, PSM 3 `c0b97b0f913b9c02fb9a71d0658759a4de7f8b170e816e8a4c3471163f66ab41`, LoC `9ac9de3284950578dff6a1ed84a7de39db7bb893c3d9c162454f1d7feb86c7d2`. The set and results are on pve-big in `/tank/usnm-lake/as-gold-20261113/en-as/`; the source files are in its parent directory. The crops and page images aren't committed. Xiaomi-OCR-0 and Qwen3-VL 8B haven't run on it; the BOINC benchmark is to run them.

## What is not in this report

- Qwen3-VL 2B, and BOINC runs of Xiaomi and 8B. Xiaomi on volunteer hardware (1.20 app versions) is being staged as a cross-hardware replicate. Guarded 8B ran locally only.
- English held-out set (`en-bln`): dropped, because BLN600's images are bilevel TIFFs that a unit can't carry unchanged under its No-Derivatives licence. `en-as` (above) replaces it as the held-out English set, but Xiaomi and Qwen haven't run on it, so the English recommendation stays provisional.
- `de-rz700` and `de-onb`: not frozen, because their crop lists (Study 7's and the benchmark release's) can't be rebuilt from the GPU studies' data release; see the departures log.
- The GPU and CPU host comparison. No CPU app version is live.

## Reproducing

`scripts/ocr-survey/tesseract_crops.py`, `scripts/ocr-survey/loc_column.py`, `scripts/ocr-survey/reader_draw.py` and the scripts in `scripts/` beside the protocol rebuild every input and score in this report. The sets and per-crop outputs stay on usnm-dev2 (`en-as`: on pve-big, see above) because the source terms of several sets restrict redistribution.
