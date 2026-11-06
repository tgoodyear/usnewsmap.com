# Stopping Yiddish v1 tie-breakers

**Status:** code merged, not enabled; measured read-only · **Date:** 5 November 2026 · **Measured on:** the 3,838 `usnm_qwen` units created from 21 October to 5 November 2026, read from the BOINC database at 12:21 UTC on 5 November · **Code:** `scripts/boinc/qwen-tiebreak-policy.py`, `scripts/boinc/qwen-replica-cost.py` · **Issues:** #1477, #1420, #1479

## The question

When a Qwen unit's first pair disagrees, BOINC sends a third replica and then a fourth. For Yiddish request version 1 these tie-breakers rarely settle the unit: the [replica cost note](2026-11-04-qwen-replica-cost.md) found that a third replica settled 13% of the units it reached and a fourth 6%. #1477's first option is to stop them. This note measures what that saves if it's turned on today. The code is described in [`docs/boinc-quality.md`](../boinc-quality.md) under "Stopping tie-breakers by language and request version".

## What we found

**410 Yiddish v1 units would be settled at once.** 417 Yiddish v1 units are open, and in 411 of them the first pair disagreed. 410 have a tie-breaker waiting to be sent. The other one has a replica running, so it waits for a later pass. Settling the 410 cancels 410 unsent replicas. Without the policy, the units could run up to 738 more replicas before reaching `max_total_results`.

**This saves about 700 replicas and 27 to 50 GPU-hours, at the cost of about 66 Qwen consensuses.** 334 of the units are waiting for a third replica and 76 for a fourth. At the measured rates (a third replica settled 33 of 246 units, a fourth 8 of 135), they would run about 700 more replicas and settle about 66 units. Four hosts read Qwen, one per vendor, and a host reads a unit at most once. 349 of the 410 units can still go to the NVIDIA host, and 328 of those to the Intel host as well. 45 can go only to Intel and 16 only to Apple. The NVIDIA host's 41 Yiddish v1 tie-breakers averaged 2.3 minutes each, which puts the cost at about 27 GPU-hours. At the 4.3-minute average across all vendors it would be 50. That's 0.4 to 0.8 GPU-hours per page settled.

**The consensuses forgone are worth little.** A Qwen consensus isn't accuracy. In Study 8, Qwen v4 had a 30.15% CER on held-out Yiddish pages. The CPU Tesseract `yid` model had 8.72%, and the trained `yidj2` had 4.03% in Study 10 (#1479). The quality router reads Yiddish with `yid` whatever Qwen does. A settled unit ends with no canonical result, so no Qwen reading joins its page and the page keeps the readings it has.

**Yiddish v4 hardly matters.** 33 Yiddish v4 units are open, and 15 are disputed. 14 could be settled now, cancelling 14 replicas, out of at most 25. That saves about 1 GPU-hour. v4 is the request version the live feed stages Yiddish at, so stopping it would change what new Yiddish work does. On 3 November its first pairs had agreed 4 of 19 times under the strict rule, against 165 of 745 for v1 (#1477). That is too few to judge it.

## Results

`plan --stop-tiebreakers yid:1 --stop-tiebreakers yid:4`, window 21 October to 6 November:

| Group  | Open | Disputed | Settle now | Waiting: replica running | Unsent replicas cancelled | Most replicas forgone | Mean hours per return | GPU-hours forgone, low–high |
| ------ | ---: | -------: | ---------: | -----------------------: | ------------------------: | --------------------: | --------------------: | --------------------------: |
| yid v1 |  417 |      411 |        410 |                        1 |                       410 |                   738 |                 0.072 |                   29.5–53.2 |
| yid v4 |   33 |       15 |         14 |                        1 |                        14 |                    25 |                 0.043 |                     0.6–1.1 |

All 424 units are in batch 8, the native 8B GPU target. Low means the unsent replicas only, and high means every replica up to the cap, both at the group's mean return time on all vendors.

Yiddish v1 tie-breakers, the third and fourth returns in received order, by plan class:

| Plan class         | Returns | Mean minutes | Median minutes | Valid |
| ------------------ | ------: | -----------: | -------------: | ----: |
| `amd_vulkan_8b`    |     181 |          0.9 |            0.9 |    14 |
| `apple_gpu_8b`     |     158 |          8.1 |            7.2 |    24 |
| `nvidia_vulkan_8b` |      41 |          2.3 |            1.9 |     7 |
| `intel_vulkan_8b`  |       1 |          6.7 |            6.7 |     0 |

Yiddish v1 tie-breakers returned per day: 19 on each of 28 to 30 October, 9 on 31 October, 46 on 1 November, 184 on 2 November, 58 on 3 November, 25 on 4 November and 2 on 5 November until the read. At 25 a day, the 700 replicas would take about four weeks.

## Method

`qwen-tiebreak-policy.py plan` ran as root in the BOINC server container. It read the window's workunits and results in one `READ ONLY` transaction of `LIMIT`-bounded `SELECT`s, and joined each unit's language and request version from its `qwen.json`. It wrote nothing. `qwen-replica-cost.py collect` read the same window two minutes later for the resolution rates, plan classes and pace. The estimate takes a unit waiting for a third replica as running it, then a fourth if the third doesn't settle it. A unit waiting for a fourth runs that one. Each replica settles its unit at the measured rate. Nothing was settled or cancelled, and no configuration was changed. The server image hadn't been rolled for #1449 yet when the database was read.

## Caveats

- The rates come from the 246 and 135 disputes that tie-breakers have reached so far. The NVIDIA host's tie-breakers came back valid 7 times in 41, above the 13% average for a third replica. If it takes most of the remaining ones, there would be more than 66 consensuses for about the same GPU time.
- GPU-hours are wall time on devices that differ by up to ten times. The NVIDIA figure is the better guide while that host takes most of the remaining replicas. A replica the Intel or Apple host takes costs 7 to 10 minutes.
- Settling takes the units out of the feed's `unresolved_native` count. The feed can then admit as many more pages as it settles under `--max-unresolved` (2,000 by default): 410 for `yid:1`. About 1,790 Qwen units are open today.

## What is left

The maintainer has to decide:

- Whether to turn it on. The recommendation is yes, for `yid:1` only. Run `settle --apply` over the backlog after the #1449 roll is finished. Then repeat the run until `plan` reports nothing left to settle: no new Yiddish v1 units are made, so the backlog only shrinks. Yiddish v4 should stay as it is until it has enough first pairs to judge.
- Whether Qwen should keep reading Yiddish at all, given the CER gap. That is #1479, together with the Tesseract `yidj2` route.
