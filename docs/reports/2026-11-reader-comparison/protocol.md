# Protocol for the reader comparison (#968)

Written and committed on 11 November 2026, before any reader ran for this comparison. The sets, readers, scoring and decision rules below don't change after results are seen. Values that can only be known once the sets are frozen and the units staged (manifest hashes, image digests, model hashes) go in the [freeze record](#freeze-record), which is committed before the first unit is submitted. Any later change is a departure and goes in the [departures log](#departures-log).

## Purpose

This comparison produces one table of CER and WER by language and reader, with every reader on the same human-transcribed crops, held out where a held-out set exists. The readers are LoC's ALTO, Tesseract, Qwen3-VL 8B, Qwen3-VL 2B and Xiaomi-OCR-0.

The numbers compared so far come from different sets. Tesseract's 9.0% is from the GPU studies' 612 crops, mostly NZZ and Reichsanzeiger, where LoC wasn't scored. LoC's 9.22% and Tesseract's 3.68% are from English `overproof-ca`. Qwen 8B's 2.17% is from 35 of those crops, not 345. Tesseract ran at PSM 6 in the GPU studies and the October pilot, while gold units run PSM 3. This comparison puts every reader on the same crops, with the same scorer.

## Sets

One `gold.py`-format directory per set (`manifest.jsonl`, `crops.jsonl`, `images/`, `LICENSES/`), written by `gold.py public-convert`, published together as benchmark release v1.2.0 ([bench.md](../../ocr/bench.md)). Each crop's source page is recorded, because the bootstrap limits resample whole source pages.

"Second use" means the GPU studies' readers have already read these crops or pages ([GPU studies protocol](../2026-11-gpu-studies/protocol.md)). Our readers are stock models and engines, so nothing was trained on these sets. Those earlier readers' results were seen, though, and Study 5 chose the loop guard on NZZ and NewsEye Finnish crops. A second-use result is labelled as such in every table.

| Language | Set | Source | Crops (pages) | Use | Role |
| --- | --- | --- | --- | --- | --- |
| English | `en-op` | Overproof dataset 3 (Chronicling America), `public-convert --dataset overproof-ca`, less the 9 stretched-box crops of #531 | 345 (18) | Second use: benchmark v1.0.0's `overproof-en` crops come from the same 18 pages (Studies 2, 3, 5, 6 and 9), and the October pilot scored LoC, Tesseract and Qwen on these crops | Primary for the LoC comparison; secondary for ranking |
| English | `en-bln` | BLN600 | 400 (400) | Held out: the 400 excerpts outside Study 5's 200. The October pilot read all 600 with Tesseract only | Primary for ranking |
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

**Held-out NewsEye draws (`fr-new`, `fi-new`, `sv-new`).** Drawn from the publisher's validation and training archives together. Pages are excluded if they're in the exclusion list: every page of `multilingual/all-52`, every page in Study 2's train and validation rows (`lora-split.json`), and every NewsEye page named anywhere in the GPU studies' data release. The list is written, sorted and hashed before the draw. From the remaining pages, in order of `sha256("reader-cmp-202611:"+dataset+":"+page_id)`, the draw takes up to 3 regions per page in order of `sha256("reader-cmp-202611:"+dataset+":"+region)`, inside `public-convert`'s length window for that dataset, until it has 60 crops or no pages are left. A set with under 5 pages is unranked. A page outside the list may still share an issue with pages in the publisher's other splits: #835 found the ONB archives' train and validation labels share issues, and NLF may do the same. The draw is by page, not by issue, and the report says so.

**`de-rz81`.** Study 11's `de-rz` pages, in its frozen set (`SHA256SUMS` SHA-256 `24a42c994a70a4fa85b4df6108b6b687388be0f330f47d2a03e5c9cfb2187662`): on each page, the first 4 transcribed regions in Study 11's `sha256("s11:de-rz:"+id+":"+region)` order. It is converted to `gold.py` format with the page images Study 11 fetched. If it can't be converted, it's dropped and German has second-use sets only. No German newspaper set with human transcriptions remains that the readers here haven't read. The two ONB reserve issues of #835 (4 crops) are too few to rank anything and stay unspent. Chronicling Germany, Study 11's other German set, isn't a scoring source under the [source policy](../../ocr/gold-set.md#source-policy) and isn't used.

**`de-nzz` and `de-onb`.** The crop ids Study 3 and Study 7 used, from the GPU studies' data release. NZZ's 167 front pages are all in Study 3, and the Austrian set has 9 issues, all used by Study 2, so neither has unread pages.

**Freezing.** For each set, the SHA-256 of `manifest.jsonl` and `crops.jsonl`, and the exclusion list's SHA-256 for the draws, go in the freeze record. So do the release's content digest and each subset's digest. `gold.py validate` must pass with 0 errors on every set. A set isn't changed after it's frozen. A wrong crop found later stays in, and is listed by id.

## Readers

| Reader | Where it runs | Settings |
| --- | --- | --- |
| LoC ALTO | On our side, no BOINC | `scripts/ocr-survey/loc_column.py`, below. `en-op` only |
| Tesseract 5.5.0 | BOINC, CPU hosts, hypothesis-mode unit | `eng`, `fra`, `fin`, `swe`, or `frak2021` for German, by the set's language. PSM 6 primary, PSM 3 secondary. 300 DPI, LSTM engine |
| Qwen3-VL 8B Q4_K_M | BOINC `usnm_qwen`, GPU and CPU hosts | The production request version (3, with the loop guard) on the llama.cpp b12084 runtime of #1774. No language hint |
| Qwen3-VL 2B Q8_0 | BOINC `usnm_qwen`, GPU and CPU hosts | As 8B, with a per-unit model pin (plan step 5) so both read the same staged crops |
| Xiaomi-OCR-0, Q8_0 GGUF | BOINC `usnm_qwen`, GPU and CPU hosts | As shipped by #1776: Study 9's region prompt, at most 2,048 output tokens, the `stop` guard |

**Tesseract PSM.** PSM 6 is primary because every earlier crop comparison used it, Tesseract's 3.68% and 9.0% included. PSM 3 is what gold units run, so it's secondary. Each set uses one language profile. Study 3's combined `fra+fin+swe` profile isn't repeated.

**Identities recorded, per reader.**

- LoC: the commit of `loc_column.py`, the SHA-256 of the `usnm-ocr-survey` binary it ran, and per page the ALTO's SHA-256, its `processingSoftware` entries, its URL and retrieval date (`loc/pages.json` from `public-fetch`), and the page image's SHA-256 checked against the manifest.
- Tesseract: the worker image digest, `tesseract --version`, each traineddata file's SHA-256 against `ocr/engines/tesseract/models.lock`, PSM, DPI and engine mode.
- Qwen 8B, Qwen 2B and Xiaomi-OCR-0: the app name and version numbers, plan classes, the worker image digest, the llama.cpp build, the GGUF and vision projector SHA-256 of each model, the request and prompt versions, the engine JSON passed to `benchmark-stage` and its SHA-256, and the staged inputs' SHA-256.
- Scorer: the commits of `usnm-ocr-survey` and of the GPU studies' `gpu-study` tool.

## How readers run

Every reader except LoC runs on BOINC volunteer hosts. Hosts are untrusted. They get public crop images and public models, and never a reference transcription. Every score is computed on the server.

- **Qwen 8B, Qwen 2B and Xiaomi-OCR-0** use `usnm-boinc qwen benchmark-stage` and `benchmark-collect`. Stage writes one unit per crop with the crop bytes and the source notice. The references stay in the server's `benchmark.json`, and the unit's `loc_text` is empty. Collect reads the canonical results back and scores them. One campaign per reader and set.
- **Tesseract** needs a hypothesis-mode unit: a unit that returns the text for each crop at a given language, profile and PSM, scored on the server the same way. Gold units can't be used. They run PSM 3 only, and they ship the references so that hosts score themselves.
- **LoC** runs `loc_column.py` over `en-op`'s manifest. It needs no host.

These must be in place before staging:

1. `usnm_qwen` on b12084 (#1774), with the per-unit model pin.
2. Xiaomi-OCR-0 in `usnm_qwen` (#1776).
3. The Tesseract hypothesis-mode unit.
4. The benchmark stage's source notice accepts BLN600's licence, CC BY-NC-ND 4.0. Today it accepts BY-NC-SA, BY-NC, BY-SA, BY and CC0 today. The images go out unchanged, which ND allows.

**Host classes.** Each campaign is submitted twice with the same staged inputs: once to GPU plan classes and once to CPU plan classes. Tesseract runs on CPU only. Quorum and validation are those of the app's production units, and the canonical result of each unit is the one scored. The plan class of the app version that produced it records the host class.

- The primary table uses the GPU batch for Qwen 8B, Qwen 2B and Xiaomi-OCR-0, and the CPU batch for Tesseract.
- The secondary table scores the CPU batch of the three vision-language readers, with the paired difference between the two classes on the crops both read.

If the CPU batch of a reader doesn't finish within 14 days of submission, it's reported with what returned, and the gap goes in the departures log.

## LoC column

`scripts/ocr-survey/loc_column.py --manifest M --alto-dir D --out O` runs `usnm-ocr-survey alto-crop` on every Chronicling America crop of a `gold.py` manifest. For each crop it uses LoC's ALTO for the page, the crop's box and the page image's size, and keeps the clipped ALTO's text (`alto_text`). The text is the words whose box centre is in the crop, in the ALTO's reading order, with a kept line hyphen at the end of its line ([A crop against its text](../../ocr/gold-set.md#a-crop-against-its-text)). It writes `outputs.jsonl`, rows `{"crop_id", "id", "text"}` that every scorer here reads, and `loc-column.json`, with each crop's status and the hashes above.

The ALTO is the one `gold.py public-fetch` fetched for `en-op`, the same files its boxes came from. A crop the tool can't align (missing ALTO, page image checksum mismatch, or a refusal from `alto-crop`) has no output and scores as empty, by the rule below. Its id and reason are listed. A missing LoC reading means our alignment failed, and LoC's text isn't at fault. So if any crop is missing, LoC is also reported on the aligned crops, labelled as such.

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

Secondary: the same on each second-use set; Tesseract PSM 3 against PSM 6; the CPU batch against the GPU batch per reader; `raw-v1` CER; WER.

## Decision rules

1. **Better.** Reader A is better than reader B on a set when the paired pooled CER difference A − B has 95% limits below zero. A set with under 5 source pages decides nothing and is reported as descriptive.
2. **Reader per language.** The recommended reader for a language is the one with the lowest pooled CER on its primary set among readers better than Tesseract PSM 6 by rule 1. If none is, Tesseract stays. If the lowest reader isn't better by rule 1 than one or more other candidates, the smallest model among the lowest and those candidates is recommended, in the order Xiaomi-OCR-0, Qwen 2B, Qwen 8B, because it costs less host time. A recommended reader with more major hallucinations than Tesseract on that set has its flagged crops read by a person before any change to production.
3. **CPU hosts.** A vision-language reader is a CPU-host reader for a language only if its CPU batch is better than Tesseract PSM 6 by rule 1 on that language's primary set.
4. **LoC.** A reader improves on LoC's text for Chronicling America pages when it is better than LoC by rule 1 on `en-op`. `en-op` is second use, 18 pages from 5 titles printed 1871 to 1922, so this supports re-reading pages like those and says nothing about the rest of the corpus.
5. **Held out over second use.** A result on second-use sets alone decides nothing. Where a held-out set and a second-use set point different ways, the held-out result decides and the disagreement is reported.
6. **German.** Every German set has been read before, so German results are provisional: `de-rz81` is labelled "Study 11 use" and the others "second use". They stand until the German transcriptions of plan step 8 can confirm them.
7. **Scope.** Too few crops to decide is a valid result. This protocol makes recommendations; the maintainer decides any change to production.

American Stories and CHURRO aren't readers in this comparison. The Czech, Serbian, Polish, Russian and Yiddish crops out for transcription join by amendment once their transcriptions are checked.

## Budget

Volunteer hosts only. No rented GPUs, and the local RTX 2060 SUPER isn't used.

## Freeze record

Filled in and committed before the first unit is submitted.

| Item | Value |
| --- | --- |
| Benchmark v1.2.0 content digest | |
| Subset digests | |
| Manifest and `crops.jsonl` SHA-256, per set | |
| NewsEye exclusion list SHA-256 | |
| `loc_column.py` commit and `usnm-ocr-survey` SHA-256 | |
| `usnm_qwen` app versions, plan classes, worker image | |
| GGUF and projector SHA-256: Qwen 8B, Qwen 2B, Xiaomi-OCR-0 | |
| Engine JSON SHA-256 per campaign | |
| Tesseract image, version, traineddata SHA-256 | |
| Scorer commits | |

## Departures log

Each entry: the date, what changed, why, and whether any result had been seen.

None yet.
