# Strict Qwen agreement on crops with no words

**Status:** measured; the pair log now marks these crops, no rule changed, a decision is needed · **Date:** 4 November 2026 · **Measured on:** the on-prem BOINC server's `qwen/pairs.jsonl` and retained results (`qwen/seen`) for the pairs logged from 28 October 22:16 to 4 November 22:16 UTC, joined to `workunit.canonical_resultid` in a read-only transaction · **Code:** `crates/usnm-boinc/src/qwen/validate.rs`; `qwen::agree` and `boinc::dice` in `usnm-ocr-contract` · **Issues:** #1463, #1420, #1434

## The question

The strict rule (`strict-v1`, [the validator](../boinc-quality.md)) counts a crop as read when its reading isn't blank, and compares two readings by word-bag Dice. A word is a run of letters or digits. A reading with none, such as `[?]` on an illustration or a row of dashes, has an empty bag, and `dice` scores two empty bags 1.0. So two different punctuation-only readings pass at any floor, and the rule never looks at the loop guard's flags. Production WU 27224 had four such crops, one of them loop-stopped. It didn't become canonical, because its other eight crops disagreed. How often does this happen, and does it decide canonical results?

## What we did

- **Characterisation tests** (`characterise_1463_*` in `qwen/validate.rs`) pin today's behaviour on synthetic readings. Two different punctuation-only readings pass, even at a floor of 1.0. A pair with no word on any crop passes. A whitespace-only pair passes, because neither host read the crop. Whitespace-only against punctuation fails ("read by one host only"). A crop the loop guard flagged on one side passes.
- **The pair log gains `wordless`.** It lists each crop both hosts read where neither reading has a letter or digit, with `identical` (the same once whitespace is collapsed) and `looped` per side (the loop-guard bit or a repeated run at the end, as `crop-consensus-v2` marks it). It is computed only when the line is written, and no verdict reads it. A test checks that `compare` returns `qwen::agree`'s verdict with and without a log, and that the line's `agree` matches it. The server runs a pinned build, so production logs won't have the field until a new server image is deployed.
- **Measurement.** A Python script on CT 230 read the pair log and both results' `readings.json` for every pair in the window, and printed counts only. No reading text left the server. It uses the rule's own definitions: read means not blank after trimming, and no word means no alphanumeric character (Python's `isalnum`, close to Rust's `char::is_alphanumeric`; every crop it flagged has Dice 1.0 in the log). Loop flags are the `repetition_loop_crop_bits` bit or a ported `tail_repetition`. A pair compared more than once counts once (2,694 repeat lines). Eleven unparseable lines in the whole log were skipped. A unit's canonical decision is the earliest agreeing pair that holds its canonical result.

## What we measured

| Crops, all compared pairs (3,255 pairs, 1,717 agreeing)      |  Count |
| ------------------------------------------------------------ | -----: |
| Read by both hosts                                           | 33,759 |
| … with no word on either side (Dice 1.0 by definition)       |  4,679 |
| … … both sides only `[?]`                                    |  4,413 |
| … … same once whitespace is collapsed                        |  4,637 |
| … … different                                                |     42 |
| … … a side loop-flagged (386 by the guard's bit)             |    421 |
| … … different and not loop-flagged                           |      1 |
| Read by neither host (blank or whitespace only)              |      0 |
| Read by one host only, no word on either side (rule rejects) |      1 |
| Words on one side, none on the other (Dice 0, rule rejects)  |    241 |

Of the 4,679, 913 were in agreeing pairs:

| Crops with no word, in agreeing pairs   | Count |
| --------------------------------------- | ----: |
| Both only `[?]`, same, not loop-flagged |   885 |
| Other punctuation, same, not flagged    |     3 |
| Same, loop-flagged                      |    23 |
| Different, loop-flagged                 |     2 |
| Different, not loop-flagged             |     0 |

Every one of the 1,717 agreeing pairs set its unit's canonical result:

| Canonical decisions (1,717 units)                                               | Units |
| ------------------------------------------------------------------------------- | ----: |
| The deciding pair had a crop with no word                                       |   193 |
| … one of those crops was loop-flagged (13 by the guard's bit)                   |    15 |
| … one of those crops differed between its readings (both also in the row above) |     2 |
| … the pair had no crop with a word at all                                       |     2 |

The numbers are about agreement, not accuracy. They don't say whether a `[?]` crop is an illustration or legible print the model failed to read. That needs the image, which is the image-grounded step the issue asks for and isn't done here.

## The trade-off

**False positives.** A crop with no word has no wrong words in it, but the strict rule vouches for every crop of an accepted result, so `quality collect` joins the reading into `crops.parquet` and scores the other readers' text on that crop against it. A loop or `[?]` on legible print then counts against the readers that did read it. A pair whose crops have no words at all is accepted with nothing compared. This week that meant 15 canonical results with a loop on such a crop, and 2 with no word compared.

**False negatives.** Most of these crops are `[?]` on both sides (885 of the 913 in accepted pairs), which is the right answer for an illustration, an ornament or a blank area. If every such crop failed, 193 of this week's 1,717 canonical decisions (11%) would have failed too, and those units would have gone out for more replicas. A unit that keeps failing reaches its four-result limit with no canonical result.

## Options, for the maintainer to choose

None of these is implemented. Each count is how many of this week's 1,717 canonical decisions would have gone the other way.

| Option                                                                                      | Decisions changed |
| ------------------------------------------------------------------------------------------- | ----------------: |
| Keep the strict rule as it is                                                               |                 0 |
| Require crops with no word to be the same once whitespace is collapsed                      |                 2 |
| Treat crops with no word as neutral, and require at least one crop with a word in each pair |                 2 |
| Treat crops with no word as neutral, and let a pair with none pass                          |                 0 |
| Fail a crop with no word when either side is loop-flagged                                   |                15 |
| Require them to be the same, and fail them when loop-flagged                                |                15 |

`crop-consensus-v2`, the opt-in rule that is off ([floor check](2026-11-04-crop-consensus-floor-check.md)), already treats these crops as `no_text`, which is neutral, and loop-flagged crops as `looped`, which count against its majority. Turning it on would change this behaviour too, so the choice here interacts with #1434's.

## What is left

- The maintainer's choice of rule, or a decision to keep this one. The thresholds, quorum and admission are unchanged.
- Image-grounded review of the loop-flagged and `[?]` crops, to separate blank print from model failures.
- Once a server image that logs `wordless` is deployed, the counts can come from the pair log alone.
