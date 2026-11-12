# Protocol for the reader comparison (#968)

Written and committed on 11 November 2026, before any reader ran for this comparison. The sets, readers, scoring and decision rules below don't change after results are seen. Values that can only be known once the sets are frozen and the units staged (manifest hashes, image digests, model hashes) go in the [freeze record](#freeze-record), which is committed before the first unit is submitted. Any later change is a departure and goes in the [departures log](#departures-log).

## Purpose

This comparison produces one table of CER and WER by language and reader, with every reader on the same human-transcribed crops, held out where a held-out set exists. The readers are LoC's ALTO, Tesseract, Qwen3-VL 8B, Qwen3-VL 2B, Xiaomi-OCR-0 and American Stories.

The numbers compared so far come from different sets. Tesseract's 9.0% is from the GPU studies' 612 crops, mostly NZZ and Reichsanzeiger, where LoC wasn't scored. LoC's 9.22% and Tesseract's 3.68% are from English `overproof-ca`. Qwen 8B's 2.17% is from 35 of those crops, not 345. Tesseract ran at PSM 6 in the GPU studies and the October pilot, while gold units run PSM 3. This comparison puts every reader on the same crops, with the same scorer.

## Sets

One `gold.py`-format directory per set (`manifest.jsonl`, `crops.jsonl`, `images/`, `LICENSES/`), written by `gold.py public-convert`, published together as benchmark release v1.2.0 ([bench.md](../../ocr/bench.md)). Each crop's source page is recorded, because the bootstrap limits resample whole source pages.

"Second use" means the GPU studies' readers have already read these crops or pages ([GPU studies protocol](../2026-11-gpu-studies/protocol.md)). Our readers are stock models and engines, so nothing was trained on these sets. Those earlier readers' results were seen, though, and Study 5 chose the loop guard on NZZ and NewsEye Finnish crops. A second-use result is labelled as such in every table.

| Language | Set | Source | Crops (pages) | Use | Role |
| --- | --- | --- | --- | --- | --- |
| English | `en-op` | Overproof dataset 3 (Chronicling America), `public-convert --dataset overproof-ca`, less the 9 stretched-box crops of #531 | 345 (18) | Second use: benchmark v1.0.0's `overproof-en` crops come from the same 18 pages (Studies 2, 3, 5, 6 and 9), and the October pilot scored LoC, Tesseract and Qwen on these crops | Primary for the LoC comparison; secondary for ranking |
| English | `en-bln` | BLN600 | 400 (400) | Held out: the 400 excerpts outside Study 5's 200. The October pilot read all 600 with Tesseract only | Dropped for the vision-language readers (see the departures log) |
| French | `fr-52` | NewsEye BnF in v1.1.0 `multilingual/all-52` | 24 (8) | Second use: Studies 3, 6, 7 and 9 | Secondary |
| French | `fr-new` | NewsEye BnF, held-out draw | up to 60 (at least 5) | Held out | Primary |
| Finnish | `fi-52` | NewsEye NLF Finnish in `multilingual/all-52` | 18 (6) | Second use: Studies 3, 6, 7 and 9; Study 5's guard was chosen on NLF Finnish crops | Secondary |
| Finnish | `fi-new` | NewsEye NLF Finnish, held-out draw | up to 60 (at least 5) | Held out | Primary |
| Swedish | `sv-52` | NewsEye NLF Swedish in `multilingual/all-52` | 10 (4) | Second use: Studies 3, 6, 7 and 9. Under 5 pages, so unranked | Secondary |
| Swedish | `sv-new` | NewsEye NLF Swedish, held-out draw | up to 60 (at least 5) | Held out | Primary |
| German | `de-rz81` | Reichsanzeiger-GT, the 81 pages outside `all-700` | up to 324 (81) | Not used by Studies 1–9. Study 11's readers, including Xiaomi-OCR-0, Qwen 8B and Tesseract PSM 6, read them | Primary, provisional |
| German | `de-rz700` | Reichsanzeiger-GT, v1.0.0 `reichsanzeiger-de/all-700` | 700 (20) | Second use: Studies 1, 2, 3, 5, 6 and 9 | Secondary |
| German | `de-nzz` | NZZ, Study 3's crops | 360 (167) | Second use: Studies 1, 2, 3, 5, 6 and 9 | Secondary |
| German | `de-onb` | Austrian newspapers, Study 7's crops | 60 (pages counted at freeze) | Second use: Study 7. Its pages trained Study 2's adapter, which isn't a reader here | Secondary |

**`en-op`.** The `public-convert` output of 13 October 2026, 354 crops from 18 pages, less `pub-overproof-ca-006`, `010`, `025`, `027`, `158`, `159`, `234`, `237` and `314`, as in the [October pilot](../2026-10-boinc-quality-pilot/report.md). It is the only set on Chronicling America pages, so the only one with a LoC column. Its boxes come from LoC's ALTO lines, matched to Overproof's lines through LoC's own OCR text, so the box fits LoC's segmentation. This protocol names no held-out Overproof draw. The October conversion found 18 pages. If `public-fetch` finds more dataset 3 pages at freeze, their crops, cut by the same `public-convert` rule, form a held-out set `en-op-new`, which is then primary for the LoC comparison and goes in the freeze record.

**`en-bln`.** BLN600's 600 excerpts less the 200 Study 5 took by `sha256("gpu-studies-202611:bln600:"+crop_id)` order. Each excerpt is its own image, so it counts as its own page. The images are copied unchanged, as BLN600's No-Derivatives terms require.

**Held-out NewsEye draws (`fr-new`, `fi-new`, `sv-new`).** Drawn from the publisher's training archives only. The exclusion list is every page of the publisher's validation archive for the language, which is a superset of `multilingual/all-52` and of the Finnish crops Study 5's guard was chosen on. Study 2's row lists aren't in the GPU studies' data release, and its adapter isn't a reader here, so they aren't used. The list is written by `docs/reports/2026-11-reader-comparison/scripts/newseye_exclusions.py`, sorted, and hashed before the draw. `scripts/ocr-survey/reader_draw.py` draws from the remaining pages, in order of `sha256("reader-cmp-202611:"+dataset+":"+page_id)`, up to 3 regions per page in order of `sha256("reader-cmp-202611:"+dataset+":"+region)`, inside `public-convert`'s length window for that dataset, until it has 60 crops or no pages are left. A set with under 5 pages is unranked. A page outside the list may still share an issue with pages in the publisher's validation archive: #835 found the ONB archives' train and validation labels share issues, and NLF may do the same. The draw is by page, not by issue, and the report says so.

**`de-rz81`.** Study 11's `de-rz` pages, in its frozen set (`SHA256SUMS` SHA-256 `24a42c994a70a4fa85b4df6108b6b687388be0f330f47d2a03e5c9cfb2187662`): on each page, the first 4 transcribed regions in Study 11's `sha256("s11:de-rz:"+id+":"+region)` order. It is converted to `gold.py` format with the page images Study 11 fetched. If it can't be converted, it's dropped and German has second-use sets only. No German newspaper set with human transcriptions remains that the readers here haven't read. The two ONB reserve issues of #835 (4 crops) are too few to rank anything and stay unspent. Chronicling Germany, Study 11's other German set, isn't a scoring source under the [source policy](../../ocr/gold-set.md#source-policy) and isn't used.

**`de-nzz` and `de-onb`.** The crop ids Study 3 and Study 7 used, from the GPU studies' data release. NZZ's 167 front pages are all in Study 3, and the Austrian set has 9 issues, all used by Study 2, so neither has unread pages.

**Freezing.** For each set, the SHA-256 of `manifest.jsonl` and `crops.jsonl`, and the exclusion list's SHA-256 for the draws, go in the freeze record. So do the release's content digest and each subset's digest. `gold.py validate` must pass with 0 errors on every set. A set isn't changed after it's frozen. A wrong crop found later stays in, and is listed by id.

## Readers

| Reader | Where it runs | Settings |
| --- | --- | --- |
| LoC ALTO | On our side, no BOINC | `scripts/ocr-survey/loc_column.py`, below. `en-op` only |
| Tesseract (version recorded at run time; 5.5.0 today, 5.5.4 after #1835) | Local dev node, CPU | `eng`, `fra`, `fin`, `swe`, or `frak2021` for German, by the set's language. PSM 6 primary, PSM 3 secondary. 300 DPI, LSTM engine |
| Qwen3-VL 8B Q4_K_M | BOINC `usnm_qwen`, GPU hosts | Request version 3 (the loop guard, `stop`) on the llama.cpp b12084 runtime of #1774, staged with an explicit request version. No language hint |
| Qwen3-VL 2B Q8_0 | BOINC `usnm_qwen`, GPU hosts | As 8B, staged with the 2B model pin so both read the same crops |
| Xiaomi-OCR-0, Q8_0 GGUF | BOINC `usnm_qwen`, GPU hosts | As shipped by #1776: request version 5, Study 9's region prompt, at most 2,048 output tokens, no loop guard. Marked pending in the results until that release exists; no production release is made for this comparison |
| American Stories | Published dataset text, no host | `en-op` only: the dataset's article text for the same Chronicling America pages, matched to each crop by box. Reported on the crops it aligns to, with the unaligned crops scored as empty beside it, as for LoC |

**Tesseract PSM.** PSM 6 is primary because every earlier crop comparison used it, Tesseract's 3.68% and 9.0% included. PSM 3 is what gold units run, so it's secondary. Each set uses one language profile. Study 3's combined `fra+fin+swe` profile isn't repeated.

**Identities recorded, per reader.**

- LoC: the commit of `loc_column.py`, the SHA-256 of the `usnm-ocr-survey` binary it ran, and per page the ALTO's SHA-256, its `processingSoftware` entries, its URL and retrieval date (`loc/pages.json` from `public-fetch`), and the page image's SHA-256 checked against the manifest.
- Tesseract: the worker image digest, `tesseract --version`, each traineddata file's SHA-256 against `ocr/engines/tesseract/models.lock`, PSM, DPI and engine mode.
- Qwen 8B, Qwen 2B and Xiaomi-OCR-0: the app name and version numbers, plan classes, the worker image digest, the llama.cpp build, the GGUF and vision projector SHA-256 of each model, the request and prompt versions, the engine JSON passed to `benchmark-stage` and its SHA-256, and the staged inputs' SHA-256.
- Scorer: the commits of `usnm-ocr-survey` and of the GPU studies' `gpu-study` tool.

## How readers run

The vision-language readers run on BOINC volunteer hosts. Hosts are untrusted. They get public crop images and public models, and never a reference transcription. Every score is computed on the server.

- **Qwen 8B, Qwen 2B and Xiaomi-OCR-0** use `usnm-boinc qwen benchmark-stage` and `benchmark-collect`. Stage writes one unit per crop with the crop bytes and the source notice. The references stay in the server's `benchmark.json`, and the unit's `loc_text` is empty. Collect reads the canonical results back and scores them. One campaign per reader and set.
- **Tesseract** has no hypothesis-mode BOINC unit, and gold units can't be used: they run PSM 3 only and ship the references so that hosts score themselves. It runs on a local dev node with `scripts/ocr-survey/tesseract_crops.py`, from the pinned traineddata, and is scored by the same scorer.
- **LoC** runs `loc_column.py` over `en-op`'s manifest. It needs no host.

These must be in place before staging:

1. `usnm_qwen` on b12084 (#1774), and `qwen benchmark-stage` options that select and freeze the request version and the model pin in each staged job. Today it stages request version 1 with no model pin, which can't stage guarded Qwen or Xiaomi jobs.
2. Xiaomi-OCR-0 in `usnm_qwen` (#1776).

**Host classes.** The vision-language readers run on GPU plan classes only. No `usnm_qwen` CPU app version is live (the Compute Coordinator, 12 November 2026), so a CPU unit would never dispatch. Quorum and validation are those of the app's production units, and the canonical result of each unit is the one scored. Tesseract runs on CPU on our own dev nodes, not BOINC, with the version and traineddata hashes recorded. There is no CPU-host comparison for the vision-language readers: if a CPU app version is released, a CPU batch needs its own campaigns, with host-class workunit identities distinct from the GPU ones, and joins by amendment.

## LoC column

`scripts/ocr-survey/loc_column.py --manifest M --alto-dir D --out O` runs `usnm-ocr-survey alto-crop` on every Chronicling America crop of a `gold.py` manifest. For each crop it uses LoC's ALTO for the page, the crop's box and the page image's size, and keeps the clipped ALTO's text (`alto_text`). The text is the words whose box centre is in the crop, in the ALTO's reading order, with a kept line hyphen at the end of its line ([A crop against its text](../../ocr/gold-set.md#a-crop-against-its-text)). It writes `outputs.jsonl`, one row per Chronicling America crop, `{"crop_id", "id", "text"}` that every scorer here reads, and `loc-column.json`, with each crop's status and the hashes above.

The ALTO is the one `gold.py public-fetch` fetched for `en-op`, the same files its boxes came from. A crop the tool can't align (missing ALTO, page image checksum mismatch, or a refusal from `alto-crop`) gets an explicit empty-text row, because the scorers skip ids with no row, and scores as empty by the rule below. Its id and reason are listed. A missing LoC reading means our alignment failed, and LoC's text isn't at fault. So if any crop is missing, LoC is also reported on the aligned crops, labelled as such.

The October pilot's 9.22% for LoC came from a different extraction. A different number here can come from the extraction rule alone, and isn't a change in LoC's text.

## Scoring

- **Normalization.** The gold set's normalization is primary: `gold.py normalize`, which `usnm_ocr_survey::score` matches byte for byte. That is NFC, line-end hyphens joined, whitespace runs collapsed, and nothing else folded: case, punctuation, diacritics, long s and ligatures all count. In our own transcriptions `[?x]` reads as `x` and `[?]` is dropped. `raw-v1`, with no normalization, is reported beside it for CER, through `usnm-ocr-survey bench-score --rule raw-v1` on the v1.2.0 release. A folded view (long s, double hyphen) isn't planned. If it's shown, it's labelled post-hoc.
- **Empty output.** A crop with no scored output scores as empty and stays in the denominator. That covers a unit that failed, timed out or reached no canonical result, a result that didn't parse, and a LoC crop that didn't align. An empty reading scores the same way. Both counts are reported per reader and set.
- **CER and WER.** Pooled over reference characters and over reference words (whitespace-separated, after normalization), with median crop CER beside them.
- **Limits.** 95% percentile bootstrap resampling whole source pages, 2,000 resamples, seed 202611. These come from the GPU studies' `gpu-study score` ([tools](../2026-11-gpu-studies/tools/)), with references as rows `{"id", "page", "ref", "source", "lang"}`. Paired differences use the same tool's `--baseline`.
- **Invented text.** The GPU studies' check (`gpu-study invent`), by reader and set. Flagged crops are listed by id for a person to read.
- **Human references only.** Only crops with a human transcription are scored here. Agreement between readers isn't reported as accuracy anywhere.

## Primary comparisons

On each language's primary set:

1. Each reader against Tesseract PSM 6: the paired pooled CER difference with limits.
2. Xiaomi-OCR-0 against Qwen 8B, and Qwen 2B against Qwen 8B.
3. On `en-op`, each reader against LoC.

Secondary: the same on each second-use set; Tesseract PSM 3 against PSM 6; `raw-v1` CER; WER.

## Decision rules

1. **Better.** Reader A is better than reader B on a set when the paired pooled CER difference A − B has 95% limits below zero. A set with under 5 source pages decides nothing and is reported as descriptive.
2. **Reader per language.** The recommended reader for a language is the one with the lowest pooled CER on its primary set among readers better than Tesseract PSM 6 by rule 1. If none is, Tesseract stays. If the lowest reader isn't better by rule 1 than one or more other candidates, the smallest model among the lowest and those candidates is recommended, in the order Xiaomi-OCR-0, Qwen 2B, Qwen 8B, because it costs less host time. A recommended reader with more major hallucinations than Tesseract on that set has its flagged crops read by a person before any change to production.
3. **CPU hosts.** Not assessed. The vision-language readers run on GPU hosts only, so this comparison makes no claim about CPU hosts.
4. **LoC.** A reader improves on LoC's text for Chronicling America pages when it is better than LoC by rule 1 on `en-op`. `en-op` is second use, 18 pages from 5 titles printed 1871 to 1922, so this supports re-reading pages like those and says nothing about the rest of the corpus.
5. **Held out over second use.** A result on second-use sets alone decides nothing. Where a held-out set and a second-use set point different ways, the held-out result decides and the disagreement is reported.
6. **German.** Every German set has been read before, so German results are provisional: `de-rz81` is labelled "Study 11 use" and the others "second use". They stand until the German transcriptions of plan step 8 can confirm them.
7. **Scope.** Too few crops to decide is a valid result. This protocol makes recommendations; the maintainer decides any change to production.

CHURRO isn't a reader in this comparison. The Czech, Serbian, Polish, Russian and Yiddish crops out for transcription join by amendment once their transcriptions are checked.

## Budget

Volunteer hosts for the vision-language readers, and our own CPU dev nodes for Tesseract. No rented GPUs, and the local RTX 2060 SUPER isn't used.

## Freeze record

Frozen 12 November 2026 on usnm-dev2 (`/root/work/sets`), before any result was scored. The sets were converted with `gold.py public-convert` (en-op and the `-52` sets, `de-nzz`) and `scripts/ocr-survey/reader_draw.py` (the held-out draws and `de-rz81`), and each passes `gold.py validate` with 0 errors. The Tesseract runs started before this record was committed; the sets didn't change in between (see the departures log).

| Set | Crops | Pages | `manifest.jsonl` SHA-256 | `crops.jsonl` SHA-256 |
| --- | ---: | ---: | --- | --- |
| `en-op` | 345 | 18 | `249dc92e87309f8c5bcf487859bda16904a4d0dab40922dba528ed77dfbf7b71` | `58840d33d8402a312bad0fa30f71440f0358e92a7850e680792a050aeb89c559` |
| `fr-52` | 24 | 8 | `5676554f206a0fe324f12ada6265e49b5589841db85b48b4c57dd2a7fded3b86` | `fe4db972e8584ba00af3b1916371e5c3fe62da677813879db316fb91ebb074b3` |
| `fi-52` | 18 | 6 | `2efcd634169398b2451320acdc1fe9e25b7432d2e3c557bafc6635df68d7fa25` | `ccf867f744471aa628b5f05b502f6c5da5a7bb54b3399f3514fbde7354cd4eff` |
| `sv-52` | 10 | 4 | `b478110f324ca1b9988becfeb5d57cf90603478d01341f6bd666167fb5dbf851` | `876f3767fe07668e96d36d4b8ee9905961d1917c0592a48320f1d41f16c9c3f8` |
| `fr-new` | 60 | 20 | `35b28f768f475c955d2b9ac9e1f82fedefcc7e1fbc187420111f1d0aebf804a2` | `48be1ee69194e89fc6b9e1ade5af2e74f7904eb74250184cd4da90ec8e6c4dfb` |
| `fi-new` | 60 | 20 | `147a5a6eb7e2f64ffc08f30013a53735867c730ef19e3dc28cf46761df635c79` | `9acd2413fa7120a09e5abcee82405b1b716603268f8a4d968f96beeca99932da` |
| `sv-new` | 60 | 22 | `14cdd58437aff087a7eaef81e2dc7a53f145bd7d8e8de1f9f692d05a8fea9927` | `5bba97e03b77a3173ce979567f39375beda6bbe3cb7b0d9466a246bf71277607` |
| `de-rz81` | 322 | 81 | `3424d29a6dcf4ea55b86292981780a70d1db1851f67a8c3508464cdfc5fe6944` | `1b8325512e7d0b78b3dc8ebc1f1b0b8c37ff766bca9209a544a01dcf919982f5` |
| `de-nzz` | 360 | 167 | `0a3ebeb3b91abfe7dbf3fbc31a336688f4b12e7df2e4b272bd848eee61792f03` | `b36c876b3da5ea0486d79edd5674887141997dc5a2cecf4d0350b90b94463e0e` |

Not frozen, so not in the comparison: `en-bln`, `de-rz700`, `de-onb` (see the departures log).

| Item | Value |
| --- | --- |
| Benchmark v1.2.0 content digest | Not published. The sets are licence-restricted crops and stay on usnm-dev2; the digests above identify them |
| NewsEye exclusion lists (validation-archive page ids, SHA-256) | BnF `2d7ee67d7078bd38c862d5041d10ae29e52f6b0cf9b1af4e3ebe3f19d89fe218` (8 pages), NLF Finnish `040eb60a9075a5b3b0b62af8086b65c727222e7ab217a0a77f40c79abe638043` (8), NLF Swedish `5913471b6570aef63fcb656139ad6daad6ec321238e48a6b1bf8a057907555c9` (4) |
| Reichsanzeiger `all-700` pages excluded (SHA-256) | `9b442030c800e6c2dee99fe5326923074d8bfe00967d689ea9dc32cb18de0201` (20 pages) |
| `loc_column.py` commit and `usnm-ocr-survey` SHA-256 | #1778 merge `9fa02847`; binary `6e935b81e8db522cbaf74c68b95038632b1d6bd4196a79317db2a2ec0842f1f7`; `outputs.jsonl` `60886f9ce8a9460f381cfea7281d049788d525c3e5c8d7e8e6033700d7370127`, 345 of 345 aligned |
| `usnm_qwen` app versions, plan classes, worker image | Pending: no Qwen campaign has run. Engine block from the Compute Coordinator, 12 November: worker `sha256:a25da41a10f266e5c2d02206bf9f0e40b54d476609f85d70a72e4e11792d0efe`, llama `sha256:d962a8228c42c84583150e809d654f9ec995cdf9f4e02e37365293642210a309`, weights `sha256:969266d1c24656977e06a6efacae04deac6d5093c77227fe55ac28975035cddd` |
| GGUF and projector SHA-256: Qwen 8B, Qwen 2B, Xiaomi-OCR-0 | 8B Q4_K_M `67d1659bfe71b89d50b45a4ad1a9e5b997e5bb16ce5da66a6a6167abd569e9e2`, 8B projector `c6ba85508d82f42590e6eb77d5340369ab6fecf107a7561d809523d8aa5f3bfd`; 2B and Xiaomi-OCR-0 pending |
| Engine JSON SHA-256 per campaign | Pending |
| Tesseract build, version, traineddata SHA-256 | Debian 13 `tesseract-ocr` 5.5.0 with Leptonica 1.84.1, run on usnm-dev2, not the worker image. `eng` `8280aed0782fe27257a68ea10fe7ef324ca0f8d85bd2fd145d1c2b560bcb66ba`, `fra` `907743d98915c91a3906dfbf6e48b97598346698fe53aaa797e1a064ffcac913`, `fin` `96745dd0900fe997541516863d859616df6120430db546424feae72923828423`, `swe` `360303308aa5d4a912ac3b3637691152b7532d9bd6e960639db1affb83db7ea9`, `frak2021` `1da2384254fa8462c776faf3b43307fe19ce51be0931c623b2fdd560f96e299a`, each checked against `ocr/engines/tesseract/models.lock` |
| American Stories | Study 3's readings (`data/s3/american-stories.jsonl` of the GPU studies' data release, SHA-256 of the release archive `7c347ebce155158515a9a4256e25fc6e00351a455f648bc81e4713ce6e132884`), second use, unchanged |
| Scorer commits | `gpu-study` from `docs/reports/2026-11-gpu-studies/tools` at #1778's merge `9fa02847`, with two `model: None` initialisers added to build against the current contract; seed 202611, 2,000 resamples |

## Departures log

Each entry: the date, what changed, why, and whether any result had been seen.

- **12 November 2026, before any run.** The vision-language readers are GPU only, and Tesseract runs locally, not on BOINC, as the Compute Coordinator ruled: no Qwen CPU app version is live and there is no hypothesis-mode Tesseract unit. The CPU-host tables and rule 3 are dropped. No result had been seen.
- **12 November 2026, before any run.** The NewsEye exclusion list is every page of the publisher's validation archives (a superset of `all-52` and of Study 5's Finnish crops), and the held-out draw is from the training archives. Study 2's train and validation rows aren't in the GPU studies' data release, and Study 2's adapter isn't a reader here. No result had been seen.
- **12 November 2026, before any run.** `en-bln` is dropped for the vision-language readers. BLN600's images are bilevel TIFFs, its No-Derivatives licence allows only the unchanged file, and a unit carries a JPEG, so the file can't go out as a crop workunit. English then has `en-op` only, a second-use set, so English results decide nothing by rule 5 and are provisional. No result had been seen.
- **12 November 2026, before any run.** `de-rz81` takes up to 4 regions per page in this protocol's hash order (`reader_draw.py`), not Study 11's order, because Study 11's frozen set isn't in the GPU studies' data release. The pages are the same 81. The 20 pages inside `all-700` are the first 20 of `gold.py public-fetch`'s seeded order (`rz_exclusions.py`). No result had been seen.
- **12 November 2026, before any score was seen.** The Tesseract runs started on the frozen sets before this freeze record was committed. The sets, their hashes and the runner didn't change.
- **12 November 2026, before any score was seen.** `de-nzz` was rebuilt with `public-convert --count 360 --per-page 3` because the GPU studies' data release holds the crop ids (`pub-nzz-001` to `360`) but not the conversion command. The conversion gives 360 crops from 167 pages, the counts Study 3 reports, but the crop ids haven't been checked against Study 3's.
- **12 November 2026, before any score was seen.** The Qwen campaigns can't run yet. `qwen benchmark-stage` stages request version 1 only, and the benchmark submission path dispatches container workers that read request version 1 only, so guarded 8B and Xiaomi-OCR-0 can't be dispatched. The Compute Coordinator is adding native benchmark routing. Until then the table has no Qwen or Xiaomi column.
- **12 November 2026, before any score was seen.** `de-rz700` and `de-onb` aren't frozen. The GPU studies' data release holds hashes of their row lists but not the lists, and rebuilding the 700 `all-700` crops and Study 7's 60 Austrian crops would need the benchmark v1.0.0 release and Study 7's draw. Both were second-use sets, so German keeps `de-rz81` (Study 11 pages, provisional) and `de-nzz` (second use).
