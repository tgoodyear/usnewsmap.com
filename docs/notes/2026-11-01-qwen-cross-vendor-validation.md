# Why Qwen replicas come back inconclusive

**Status:** measured, not shipped. The loop guard and `crop-consensus-v2` are in the code and off · **Date:** November 2026 · **Measured on:** all retained `usnm_qwen` 8B results on 1 November 2026, 1,142 replicas, 515 jobs, 751 pairs; replica accounting from a BOINC database read earlier that day, before AMD's general admission · **Code:** `crates/usnm-boinc/src/qwen/{consensus,replay}.rs`, `crates/usnm-qwen-app/src/server.rs`, `scripts/boinc/qwen-auto-cohort.py` · **Issues:** #1420, #885, #1434, #1435

## The question

Over seven days, `usnm_qwen` had 162 valid results against 422 inconclusive. Is the validator too strict for GPU output, or do the replicas really differ? Workunit 21128, AMD's first real return, was inconclusive against Apple and Intel. Agreement between replicas is what the quality measurements rest on, so a rule that passes more isn't better unless the passes mean something.

## What we found

**Yiddish is the failure, on every vendor.** Under the rule in force, 16% of Yiddish pairs agree (91 of 558) and 87% of pairs in other languages agree (168 of 193). Of the crops read, 31% of Yiddish readings loop, against 6% elsewhere. The rate is the same on Apple, Intel and NVIDIA.

**Loops are in nearly every failure, but cutting them isn't enough.** 463 of the 492 failing pairs have a looped crop. Cutting loops from both sides fixes only 21, because the text before the loop differs too. 81 failing pairs have a crop that really disagrees. The strict rule is also too lax: it passes 104 pairs containing loops, since two loops of the same words have a high Dice. On WU 21131, c07 scores Dice 1.0 over 882 tokens of repetition.

**One crop can fail a 12-crop page.** A failing pair usually fails on one to four crops. WU 21128's AMD and Apple readings differ on c07, where AMD repeats a word and Apple repeats `[?]`. Their other crops agree.

**Word-bag Dice is the wrong measure for this text.** A one-letter error loses a whole word, and Hebrew-script readings have many. Character similarity with Hebrew points left out separates true pairs from shuffled ones better:

| Crop pairs at floor 0.5                  | Word-bag Dice | Character similarity |
| ---------------------------------------- | ------------: | -------------------: |
| Yiddish true pairs falling short (2,419) |         14.5% |                 3.0% |
| Yiddish shuffled pairs passing (2,000)   |          0.1% |                 0.1% |
| Other true pairs falling short (1,925)   |         0.31% |                0.47% |
| Other shuffled pairs passing (2,000)     |            0% |                   0% |

**Extra replicas rarely help, and most of the cost is elsewhere.** Of 386 units with two or more returns, 204 ended valid: 185 on two replicas and 19 on three. 99 units reached a third replica and 19 resolved; 80 are waiting for a fourth. That is 871 replicas for 204 valid units, 4.3 against the 2 a unit needs. A larger cost is starvation: 905 units had one returned result and an unsent peer once the NVIDIA and Intel windows closed.

**AMD isn't the outlier. One Intel host is.** Share of compared crops that disagree under the rule:

| Pair           | Yiddish pairs |         Disagree |
| -------------- | ------------: | ---------------: |
| AMD + Apple    |           129 | 1.6% (10 of 628) |
| AMD + NVIDIA   |            12 |   1.8% (1 of 56) |
| AMD + Intel    |            13 |   5.2% (3 of 58) |
| Apple + NVIDIA |           100 |  1.3% (6 of 456) |
| Apple + Intel  |           110 | 7.0% (33 of 471) |
| Intel + NVIDIA |           194 | 6.6% (52 of 783) |

The Intel figures come from one host with an Iris Xe GPU, so this may be that host's driver or build and not Intel in general. On WU 21128 c09 it wrote fully pointed Hebrew where AMD and Apple wrote none. AMD agrees on all 57 compared crops of 7 Serbian and Spanish pairs and has read no German, so outside Yiddish its sample is small.

## The rule

`crop-consensus-v2` judges each crop on its own:

- `looped`: a side's reading ends in a repeated run or its result marks the crop looped. It counts for neither side.
- `no_text`: neither side has a letter or digit.
- `agree`: character similarity is at least 0.5, with Hebrew points left out for every vendor. Readings stay as read.
- `disagree`: anything else, including text on one side only. It fails the pair.

A pair agrees when no crop disagrees and `agree` crops are more than half of the crops with text and hold at least 100 characters, or when no crop has text at all, as under the strict rule. The 0.5 floor is the number already in force, not a fitted one. A pair vouches only for its `agree` crops, so the rule claims less than the strict rule does. The 100-character limit came from this data: two issues of one Yiddish paper matched on their mastheads alone.

| Measure, 751 pairs                         | Strict | `crop-consensus-v2` |
| ------------------------------------------ | -----: | ------------------: |
| Yiddish pairs agreeing, of 558             |     91 |                 241 |
| Other pairs agreeing, of 193               |    168 |                 180 |
| Units resolved, of 515                     |    259 |                 333 |
| Units resolved by their first two replicas |    240 |                 316 |
| Extra replicas not needed                  |      0 |                  45 |

Pairs that must fail:

| Control                        | Pairs | Strict passes | Rule passes |
| ------------------------------ | ----: | ------------: | ----------: |
| Different pages, same language | 2,000 |             0 |           0 |
| Crops rotated within a page    |   751 |            21 |          13 |
| Every crop looped              |   751 |             0 |           0 |
| Half the crops looped          |   751 |             0 |           0 |
| One crop replaced              |   706 |             0 |           0 |
| Every crop unread              |   751 |             2 |           2 |

An earlier sample drew one different-page pair that both rules passed. An exhaustive scan of different-job pairs found matches above the 100-character limit on three pairs of jobs, each the same page staged twice. The rotated passes inspected are pages whose crops read alike or blank. The two unread passes are pages with no text on either side.

A third replica gives an independent check. On crops a rescued pair vouched for, it confirms 97.3% (930 of 956), against 98.7% (77 of 78) for strict pairs. Strict's sample is small because units that pass rarely get a third replica.

Leaving Hebrew points out narrows the Intel gap and doesn't close it. On a run of 691 pairs with and without it, Apple + Intel went from 9.7% to 7.3%, Intel + NVIDIA from 9.5% to 6.6% and Apple + NVIDIA from 2.6% to 1.3%, so Intel pairs stay at about five times Apple + NVIDIA. Agreeing pairs went from 368 to 394, Yiddish true pairs falling short from 4.9% to 3.0% and third-replica confirmation from 96.3% to 97.1%, and every control was unchanged.

## The loop guard

Request versions 3 and 4 apply Study 5's `stop` rule to versions 1 and 2. The guard does little for validation on its own, as above. It saves GPU time: repeated text after a loop starts is about 57% of all generated tokens (1.78 million of 3.10 million, 9,727 readings, 2,069 of them looped). That estimate scales each looped reading's tokens by the characters cut. The guard also cuts a printed repeat that ends naturally: 13 of 1,645 repeating readings with generation records.

## AMD admission

`amd_vulkan_8b` went to general admission on 1 November with the maintainer's approval. Admission was the only gate: at the AMD host's next request after that it received 177 tasks in one reply, and its results have validated since. There is no in-flight limit on that class, unlike the refill's ten per host. The published AMD version reads request version 1 only, so request versions 2 to 4 need a new signed release first. `qwen-auto-cohort.py` now closes its window if an open AMD or Apple class can't read the request version.

## What is left

- The rule's floor has only been measured on agreement between replicas, never against image-grounded transcriptions. Do that before it goes live, along with the collector change (#1434).
- A second Intel host would show whether the gap is Intel or this one host.
- Yiddish needs request version 2 or 4 more than it needs any validator change. A rule that vouches for half a page still leaves the other half unread.
- Bundling pages to keep the model loaded is #1435.
