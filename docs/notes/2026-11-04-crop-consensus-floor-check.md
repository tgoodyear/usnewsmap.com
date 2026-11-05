# The crop rule's floor against human transcriptions

**Status:** measured; the rule's floor went from 0.5 to 0.7 on this evidence (#1434, PR #1491), and the rule is still off · **Date:** 4 November 2026 · **Measured on:** Study 3's 612 human-transcribed crops (223 pages, five languages, six sources) from the November 2026 GPU studies (PR #1428), with Xiaomi-OCR-0 readings from five builds and Qwen 8B's readings; and a replay of all retained `usnm_qwen` 8B results on 4 November · **Code:** `crates/usnm-boinc/src/qwen/{grounded,consensus,replay}.rs` · **Issues:** #1434, #1420

## The question

`crop-consensus-v2` calls a crop `agree` when the two readings' character similarity is at least 0.5. That floor was measured on agreement between replicas only ([validation note](2026-11-01-qwen-cross-vendor-validation.md)). Agreement between replicas says nothing about whether a reading is right. Study 3's crops have human transcriptions, so this note checks the floor against them. Does it pass crops whose readings both match the transcription, and fail crops where one of them doesn't?

## What we found

**The floor never fails a good pair and never passes a shuffled one.** On every pair of readers, every crop where both readings are within 10% CER of the transcription agrees: 2,315 of 2,315. No shuffled crop agrees: 0 of 3,060.

**At 0.5 it passes some crops where one reading is far off.** Of 89 crops where one reading is within 10% CER and the other is 50% or worse, 10 agree. The strict rule's Dice passes 17 of the same 89 crops, but the strict rule needs every crop of a page to pass, so it rarely accepts them.

**The crops only the rule passes are the weak ones.** At 0.5, 126 crops pass on similarity and fail on Dice. All are German: 124 from NZZ and 2 from the Reichsanzeiger. In 50 of them one reading is over 25% CER against the transcription. On the CPU build against CUDA that's 34 of 66, and the worse reading of those 66 averages 28% CER. Over all the crops the rule agrees on, the worse reading averages 6 to 10%.

**Page by page, the rule doesn't let in more bad text than the strict rule.** Judging each page's crops as one pair, the rule at 0.5 joins 2,218 crops against the strict rule's 2,619. Of those, 92 have a reading over 25% CER, against 115 under the strict rule, and 21 have one at 50% or more, against 38. Study 3 has about three crops a page, so many pages fall under the rule's 100-character limit. Production pages have 12 crops, so the rule should pass more of them than here.

**Neither rule catches errors both readings share.** On crops where Tesseract's error is 20% or more, 27% of the crops the rule joins and 28% of those the strict rule joins have a reading over 25% CER. Two readers that misread the same print agree.

**Yiddish is not in this set.** It's the language where the rule changes the most, and the floor hasn't been checked against a transcription there.

## The rule's floor: 0.7

The floor was raised to 0.7 in PR #1491. The rule is still off until the wrapper's `validator-rule` file names it. At 0.7 no crop with one good and one far-off reading agrees, every crop whose readings both match still agrees, and fewer true Yiddish replica crop pairs fall under the floor than under the strict rule's Dice.

| Pair                     | Similarity < 0.7 | Both good: agree | One bad: agree | Only the rule passes (a reading > 25% CER) | Shuffled: agree |
| ------------------------ | ---------------: | ---------------: | -------------: | -----------------------------------------: | --------------: |
| cuda-3090 + cuda-4090    |                1 |       529 of 529 |         0 of 0 |                                      0 (0) |        0 of 612 |
| cuda-3090 + vllm         |                5 |       514 of 514 |         0 of 3 |                                     10 (3) |        0 of 612 |
| cuda-3090 + cpu          |               20 |       391 of 391 |         0 of 6 |                                    54 (22) |        0 of 612 |
| cuda-3090 + vulkan-b8799 |              105 |       370 of 370 |        0 of 68 |                                     27 (6) |        0 of 612 |
| vllm + qwen8b            |               22 |       511 of 511 |        0 of 12 |                                     17 (1) |        0 of 612 |

Page by page at 0.7, the rule passes 705 pages against the strict rule's 928 and joins 2,174 crops against 2,619. Of those, 75 have a reading over 25% CER, against 115, and 13 have one at 50% or more, against 38.

## Crop by crop, at 0.5

`cuda-3090` and `cuda-4090` are one build on two cards, `vllm` the reference vLLM run, `cpu` and `vulkan-b8799` the b8799 build on CPU and on Vulkan (the Vulkan run has the known bug), `qwen8b` Qwen 8B's readings. "Good" is within 10% CER of the transcription, "bad" 50% or worse.

| Pair                     | Crops | Similarity < 0.5 | Dice < 0.5 | Both good: agree | One bad: agree (Dice passes) | Only the rule passes (a reading > 25% CER) | Shuffled: agree |
| ------------------------ | ----: | ---------------: | ---------: | ---------------: | ---------------------------: | -----------------------------------------: | --------------: |
| cuda-3090 + cuda-4090    |   612 |                1 |          1 |       529 of 529 |                   0 of 0 (0) |                                      0 (0) |        0 of 612 |
| cuda-3090 + vllm         |   612 |                3 |         12 |       514 of 514 |                   1 of 3 (2) |                                     11 (4) |        0 of 612 |
| cuda-3090 + cpu          |   612 |                6 |         70 |       391 of 391 |                   3 of 6 (4) |                                    66 (34) |        0 of 612 |
| cuda-3090 + vulkan-b8799 |   612 |               95 |        121 |       370 of 370 |                  2 of 68 (3) |                                     30 (9) |        0 of 612 |
| vllm + qwen8b            |   612 |               13 |         27 |       511 of 511 |                  4 of 12 (8) |                                     19 (3) |        0 of 612 |

Outside German, the CUDA, vLLM, CPU and Qwen pairs have no crop under the floor on either measure. The buggy Vulkan run has two, both English.

## Page by page

Each page's crops judged as one pair. The strict rule joins every crop of a page it passes; the rule joins the `agree` crops of a page it passes. Error is the worse of the two readings.

| Pair                     | Pages passed, strict / rule | Crops joined, strict / rule | Over 25% CER, strict / rule | 50% or more, strict / rule |
| ------------------------ | --------------------------: | --------------------------: | --------------------------: | -------------------------: |
| cuda-3090 + cuda-4090    |                   222 / 154 |                   610 / 475 |                     21 / 16 |                      6 / 5 |
| cuda-3090 + vllm         |                   212 / 151 |                   588 / 462 |                     18 / 11 |                      6 / 4 |
| cuda-3090 + cpu          |                   166 / 149 |                   487 / 463 |                     29 / 31 |                      7 / 4 |
| cuda-3090 + vulkan-b8799 |                   132 / 116 |                   402 / 370 |                     26 / 18 |                      9 / 3 |
| vllm + qwen8b            |                   196 / 147 |                   532 / 448 |                     21 / 16 |                     10 / 5 |

## Other floors

The rule at other floors, on the same crops and pairs, with the strict rule held at Dice 0.5. The last column is from the 4 November replay: the share of true Yiddish crop pairs between replicas that fall under the floor. The strict rule's Dice drops 13.4% of them.

|       Rule floor | Both good: agree | One bad: agree | Only the rule passes (a reading > 25% CER) | Crops joined (over 25% CER) | Yiddish replica crops under the floor |
| ---------------: | ---------------: | -------------: | -----------------------------------------: | --------------------------: | ------------------------------------: |
|              0.5 |   2,315 of 2,315 |       10 of 89 |                                   126 (50) |                  2,218 (92) |                                  2.5% |
|              0.6 |   2,315 of 2,315 |        3 of 89 |                                   115 (39) |                  2,202 (83) |                                  5.0% |
|              0.7 |   2,315 of 2,315 |        0 of 89 |                                   108 (32) |                  2,174 (75) |                                  9.1% |
|              0.8 |   2,315 of 2,315 |        0 of 89 |                                     64 (4) |                  2,071 (49) |                                 15.9% |
| strict, Dice 0.5 |              n/a |            n/a |                                        n/a |                 2,619 (115) |                                 13.4% |

At 0.7 the rule passes no crop where one reading is far off, keeps every good pair, and still drops fewer true Yiddish crop pairs than the strict rule does.

## The rule on retained results, 4 November

`qwen replay` over 4,843 retained 8B results, 2,222 jobs and 3,138 pairs, against the 824 pairs of the validation note:

| Measure                                    | Strict | Rule at 0.5 | Rule at 0.7 |
| ------------------------------------------ | -----: | ----------: | ----------: |
| Yiddish pairs agreeing, of 1,613           |    214 |         644 |         471 |
| Other pairs agreeing, of 1,525             |  1,384 |       1,438 |       1,390 |
| Units resolved, of 2,222                   |  1,594 |       1,770 |       1,666 |
| Units resolved by their first two replicas |  1,528 |       1,722 |       1,596 |
| Extra replicas not needed                  |      0 |         139 |          80 |

| Control                        | Pairs | Strict passes | Rule at 0.5 | Rule at 0.7 |
| ------------------------------ | ----: | ------------: | ----------: | ----------: |
| Different pages, same language | 2,000 |             0 |           0 |           0 |
| Crops rotated within a page    | 3,138 |           135 |          93 |          82 |
| Every crop looped              | 3,138 |             0 |           0 |           0 |
| Half the crops looped          | 3,138 |             0 |           0 |           0 |
| One crop replaced              | 2,984 |             0 |           0 |           0 |
| Every crop unread              | 3,138 |            15 |           3 |           3 |

A third replica confirms 98.1% of the crops a rescued pair vouched for at 0.5 (4,128 of 4,210), against 96.1% (414 of 431) for strict pairs. At 0.7 they are 95.7% (2,678 of 2,799) and 92.0% (344 of 374). The check takes the pairs the rule accepts at the floor and counts a third reading as confirming when its similarity to either reading reaches the floor, so figures at different floors don't compare. At either floor, a third replica confirms rescued pairs' crops more often than strict pairs'.

## What the collector does now

`quality collect --qwen-crop-states <dir>` joins a canonical reading only on the crops an agreeing pair marked `agree`. A looped, unread or unjudged crop keeps its rows without the reading, and `crops.parquet`'s `vlm_crop_state` says which. A result no agreeing pair judged crop by crop was accepted by the strict rule and joins whole, as before. The crop-state records are part of the row-cache keys. Details are in [boinc-quality.md](../boinc-quality.md).

## Limits

- The readings are Xiaomi-OCR-0's, a 0.8B model, except `qwen8b`. Production runs Qwen3-VL 8B, whose readings differ between backends in other ways.
- No Yiddish, and no Hebrew-script text.
- Legibility is a stand-in: Tesseract's error on the crop, `clean` under 5%, `fair` under 20%, `poor` from 20%.
- The page-level tables use Study 3's pages, about three crops each.

## Reproducing it

The inputs are Study 3's `refs.jsonl`, `qwen.jsonl` and `tesseract.jsonl` and Study 9's `gguf-cuda-r3090.jsonl`, `gguf-cuda-r4090.jsonl`, `gguf-cpu.jsonl`, `gguf-gpu.jsonl` and `xiaomi.jsonl`, from the GPU studies' working set. SHA-256: refs `6be9b8a9…`, qwen `ce58ff7e…`, tesseract `62c0a42d…`, cuda-r3090 `b45d535e…`, cuda-r4090 `614acb31…`, cpu `3c70ad4c…`, gpu `2ad15f37…`, xiaomi `8bda44b9…`.

```sh
usnm-boinc qwen grounded --refs refs.jsonl \
  --reader cuda-3090=gguf-cuda-r3090.jsonl --reader cuda-4090=gguf-cuda-r4090.jsonl \
  --reader vllm=xiaomi.jsonl --reader cpu=gguf-cpu.jsonl --reader vulkan-b8799=gguf-gpu.jsonl \
  --reader qwen8b=qwen.jsonl \
  --pair cuda-3090:cuda-4090 --pair cuda-3090:vllm --pair cuda-3090:cpu \
  --pair cuda-3090:vulkan-b8799 --pair vllm:qwen8b \
  --legibility tesseract.jsonl --out grounded.jsonl --crops grounded-crops.jsonl
usnm-boinc qwen replay --seen <project>/qwen/seen --out replay.json
```

The floor defaults to the rule's, 0.7; `--floor 0.5`, `0.6` and `0.8` give the others. The crop counts under the floor at 0.5 match Study 9's: 1, 3, 6 and 95 on similarity and 1, 12, 70 and 121 on Dice.
