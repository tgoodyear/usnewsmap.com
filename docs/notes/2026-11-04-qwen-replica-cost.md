# What Qwen's extra replicas cost

**Status:** measured; nothing changed · **Date:** 4 November 2026 · **Measured on:** the 3,481 `usnm_qwen` units created from 21 October to 3 November 2026 (the first on 26 October), read from the BOINC database at 22:20 UTC on 4 November, with each unit's language and request version from its job input · **Code:** `scripts/boinc/qwen-replica-cost.py` · **Issues:** #1420, #1477

## The question

A Qwen unit needs two replicas that agree. When the first two disagree, BOINC sends a third and then a fourth (`max_total_results` 4, one replica per host). #1420 asks what that costs: how many replicas are sent per unit that ends valid, against the two it needs, and whether a disputed unit is settled by more replicas or stays unresolved. The [validation note](2026-11-01-qwen-cross-vendor-validation.md) answered why the first pairs disagree. This note counts the cost.

## What we found

**Yiddish request version 1 carries almost all of it.** Its first pair disagreed on 581 of the 747 units with at least two returns. A third replica settled 33 of the 246 units it reached (13%), and a fourth settled 8 of 132 (6%). 126 units hit the cap with no consensus, against 41 settled by a tie-breaker. It ran 4.69 replicas per valid unit on settled units, and 9.1 counting those already run on the 414 disputed units still open. Of the 56.9 GPU-hours spent on replicas that resolved nothing, 54.8 were Yiddish v1.

**Elsewhere, extra replicas are cheap and work.** Serbian versions 1 and 3 settled 1,268 of 1,289 valid units on the first pair, at 2.02 to 2.03 replicas per valid unit, with 1 to 2% of their GPU time resolving nothing. Outside Yiddish, a third replica settled 21 of the 28 units it reached and a fourth settled 2 of 3.

**Disputed units wait on one host.** A unit can't go back to a host that has read it. 398 of the 485 open disputed units with an unsent replica can go only to the one NVIDIA host. It was in contact with the server that evening and had quota to spare, but it took 17 Qwen tasks in the 24 hours before the read. 46 can go only to Intel, whose admission is closed, so they won't move. The same is true of 47 units with one return, where the Apple replica returned and the AMD and NVIDIA replicas both ended in client errors. 45 of them are from the `amd-backlog-20261027` campaign. The errors weren't looked into further.

## Method

`scripts/boinc/qwen-replica-cost.py collect` runs in the BOINC server container as root. It reads the window's workunits and their results in one `READ ONLY` transaction of `LIMIT`-bounded `SELECT`s, and joins each unit's language and request version from its `qwen.json`. It then prints a JSON snapshot. `report` turns the snapshot into the tables below. A unit is **disputed** when its first two successful returns, in the order the server received them, aren't both in the valid pair. The replica that **resolved** a valid unit is the received-order position of its second valid result. **Resolved nothing** covers the losing replicas and errors on units that ended valid, and every replica on units that hit the cap (`error_mask` too many total or successful results). Every valid unit in the window had exactly two valid results, and none was cancelled or ended on too many errors, so the report's columns for those cases are zero and are left out here. **GPU-hours** are the summed `elapsed_time` of successes and errors. The validator's pair log (`qwen/pairs.jsonl`) matched the database on every first pair: 1,572 agreed and 686 disagreed.

## Results

| Language, request version | Units | Created | Sent | Unsent now | Valid | No consensus at cap | Open: disputed | Open: not disputed |
| ------------------------- | ----: | ------: | ---: | ---------: | ----: | ------------------: | -------------: | -----------------: |
| all                       |  3481 |    2.29 | 1.85 |       1550 |  1636 |                 128 |            494 |               1223 |
| srp v3                    |  1416 |    2.03 | 1.55 |        689 |   589 |                   0 |             25 |                802 |
| srp v1                    |  1070 |    2.05 | 1.73 |        343 |   700 |                   1 |             34 |                335 |
| yid v1                    |   753 |    3.07 | 2.51 |        417 |   207 |                 126 |            414 |                  6 |
| spa v1                    |   111 |    2.01 | 1.89 |         13 |    98 |                   0 |              1 |                 12 |
| yid v4                    |    38 |    2.61 | 1.74 |         33 |     4 |                   1 |             15 |                 18 |
| deu v3                    |    31 |    2.16 | 1.90 |          8 |    23 |                   0 |              3 |                  5 |
| eng v1                    |    21 |    3.95 | 3.00 |         20 |     1 |                   0 |              0 |                 20 |
| other                     |    41 |    3.12 | 2.46 |         27 |    14 |                   0 |              2 |                 25 |

Created and Sent are replicas per unit. Every open unit not disputed had one return.

| Language, request version | Valid | On first pair | By replica 3 | By replica 4 | Sent per valid unit | Ran per valid unit, settled |
| ------------------------- | ----: | ------------: | -----------: | -----------: | ------------------: | --------------------------: |
| all                       |  1636 |          1572 |           54 |           10 |                2.05 |                        2.36 |
| srp v3                    |   589 |           573 |           16 |            0 |                2.03 |                        2.03 |
| srp v1                    |   700 |           695 |            3 |            2 |                2.01 |                        2.02 |
| yid v1                    |   207 |           166 |           33 |            8 |                2.25 |                        4.69 |
| deu v3                    |    23 |            21 |            2 |            0 |                2.09 |                        2.09 |

Ran per valid unit, settled: returns and errors on valid and capped units, per valid unit. A unit needs 2.

| Language, request version | First pair disagreed | Replica 3 returned | It resolved | Replica 4 returned | It resolved | No consensus at cap | Waiting for 3 | Waiting for 4 |
| ------------------------- | -------------------: | -----------------: | ----------: | -----------------: | ----------: | ------------------: | ------------: | ------------: |
| all                       |                  686 |                277 |          54 |                135 |          10 |                 128 |           409 |            85 |
| srp v3                    |                   41 |                 19 |          16 |                  0 |           0 |                   0 |            22 |             3 |
| srp v1                    |                   40 |                  7 |           3 |                  3 |           2 |                   1 |            33 |             1 |
| yid v1                    |                  581 |                246 |          33 |                132 |           8 |                 126 |           335 |            79 |
| yid v4                    |                   16 |                  3 |           0 |                  0 |           0 |                   1 |            13 |             2 |

| Language, request version | GPU-hours | On the valid pair | Resolved nothing | Share | Disputed, still open | First return, waiting |
| ------------------------- | --------: | ----------------: | ---------------: | ----: | -------------------: | --------------------: |
| all                       |     253.7 |             106.4 |             56.9 |   22% |                 76.9 |                  13.6 |
| srp v3                    |      46.1 |              37.4 |              0.9 |    2% |                  2.1 |                   5.6 |
| srp v1                    |      57.8 |              51.0 |              0.9 |    1% |                  3.0 |                   2.9 |
| yid v1                    |     135.5 |              11.8 |             54.8 |   40% |                 68.9 |                   0.1 |

The last three tables show the larger groups only; `report` prints all of them.

## Caveats

- Half the units are still open: 802 Serbian v3 units, mostly created on 2 and 3 November, have one return. The settled ratios favour units that settle quickly, so Yiddish v1's 4.69 is a floor.
- GPU-hours are wall time on devices about ten times apart. Median Serbian successes took 27 s on AMD, 79 s on NVIDIA, 236 s on Apple and 317 s on Intel, so Apple's replicas dominate the hours.
- One unit's job input couldn't be read, so its language is unknown. No result ended with no reply.
- The 1 November note's 4.3 replicas per valid unit was measured on a mostly Yiddish sample. This window is mostly Serbian, so the overall 2.36 isn't comparable with it; Yiddish v1's 4.69 is.

## What is left

These are decisions for the maintainer, and nothing was changed:

- Whether to stop tie-breakers for Yiddish v1, or stop Yiddish v1 on Qwen (#1477). The data supports either: 378 tie-breaker returns settled 41 units.
- What to do with the 93 units that can go only to the closed Intel class: reopen it or cancel them.
- Whether one NVIDIA host is enough to settle 398 disputed units. At its last day's rate, and taking nothing else, that would take about three weeks.
