# Whether Qwen dispatch leaves the Intel host fair work

**Status:** measured; nothing changed · **Date:** 5 November 2026 · **Measured on:** the BOINC database, feeder cache, scheduler log and cohort controller journal on the on-prem server, read at 12:20 UTC on 5 November, before that day's server image roll; `usnm_qwen` replicas sent from 28 October · **Code:** `scripts/boinc/qwen-dispatch-fairness.py` · **Issues:** #786, #1420

## The question

#786 runs one Intel host as a second 8B Qwen backend. A [cohort controller](../boinc-intel-gpu.md#persistent-finite-nvidiaintel-refill) opens Intel's plan class to a small window of fresh units at a time, while the AMD and Apple classes and the generally admitted NVIDIA class take any unsent replica. The issue's acceptance asks for evidence under ordinary work that dispatch moves past a prefix of units no host can take, and that a warm feeder cache doesn't hide work. This note measures both and asks why the Intel host has had no Qwen work since the evening of 4 November.

## What we found

**Blocked units don't hold up the queue.** 1,638 Qwen replicas were unsent, one on each of 1,638 units. On 102 of these units the Apple, AMD and NVIDIA hosts each already hold a replica, so with Intel's class closed nobody can take them. The first 60 units in the feeder's order are among them. In the 24 hours before the read the Apple host was sent 240 Qwen replicas, the AMD host 138 and the Intel host 58, all on units after those 60.

**The feeder cache holds the whole queue.** All 1,638 unsent replicas were cached, 102 of them on blocked units. 2,448 of the 4,096 slots were empty and the oldest Qwen slot was 293 s old, inside the feeder's 5-minute purge. Qwen's weight is 16 of the active apps' 20, a share of about 3,277 slots, twice the queue. At this size blocked units can't crowd other work out of the cache.

**The controller moved past held windows.** From 28 October to 3 November it opened the next window 29 times while a host was starved: 20 times for the Intel host and 9 for the NVIDIA host. It left 76 replicas behind as ordinary unsent work.

**Intel is idle because no fresh pairs are left.** The last Qwen unit was created at 21:02 UTC on 4 November, and the AMD host's last Qwen replica was sent a minute later. The controller admits only units whose two replicas are both unsent, and every tick since then has found none among about 1,500 audited candidates. The Intel host's last Qwen replica was sent at 20:36. It kept asking for work, about 550 scheduler requests a day, and was sent work for the other apps. It holds no replica of 1,582 of the unsent units, but its closed bounds exclude them. Of the open hosts, the Apple host can take 1,134 and the NVIDIA host 1,515. The NVIDIA host took no Qwen after 23:02 on 3 November and made only 13 to 24 requests a day after that, so the Apple host alone was taking Qwen work.

**Intel's recent replicas validate.** Of the 104 replicas sent to it on 4 November, 102 are now valid and 2 inconclusive. Up to 31 October most of its returns were on units that later failed at the replica cap. This note doesn't split them by language, so it doesn't say how much of the change is Intel's.

## Method

`scripts/boinc/qwen-dispatch-fairness.py collect` runs in the BOINC server container. In one `READ ONLY` transaction of `LIMIT`-bounded `SELECT`s it reads each host's Qwen results by send day, the unsent Qwen replicas in the feeder's default order (result priority, then workunit id), the hosts holding a replica of each of those units, and the last 24 hours of sends. It reads each plan class's workunit bounds from `plan_class_spec.xml` and parses `bin/show_shmem` for the feeder cache. `report` prints the tables below. A host **can take** a unit when its plan class's bounds include the unit and it holds no replica of it; one result per host per unit is the project's rule. A unit is **blocked** when no host that sent Qwen work in the period can take it. The **blocked head** is the run of blocked units at the front of the feeder's order. The controller counts come from `usnm-qwen-cohort-auto.service`'s journal, and the request counts from the scheduler log.

## Results

| Device | Plan class       | Bounds    | Can take | Only taker | Could take a blocked unit | Sent, last 24 h | Sent past the blocked head |
| ------ | ---------------- | --------- | -------: | ---------: | ------------------------: | --------------: | -------------------------: |
| Apple  | apple_gpu_8b     | unbounded |     1134 |         21 |                         0 |             240 |                        240 |
| AMD    | amd_vulkan_8b    | unbounded |        0 |          0 |                         0 |             138 |                        138 |
| Intel  | intel_vulkan_8b  | closed    |        0 |          0 |                       102 |              58 |                         58 |
| NVIDIA | nvidia_vulkan_8b | unbounded |     1515 |        402 |                         0 |               0 |                          0 |

Could take a blocked unit: holds no replica of it, but its plan class's bounds exclude it.

The Intel host's Qwen replicas by the day they were sent, as they stood at the read:

| Day sent   | Sent | Valid | Invalid | Inconclusive | Not checked | Errors |
| ---------- | ---: | ----: | ------: | -----------: | ----------: | -----: |
| 28 October |   62 |    22 |      11 |            2 |          27 |      0 |
| 29 October |   94 |    25 |       9 |            7 |          52 |      1 |
| 30 October |   55 |    16 |       2 |           23 |          13 |      1 |
| 31 October |   60 |    14 |       5 |            4 |          37 |      0 |
| 1 November |    0 |     – |       – |            – |           – |      – |
| 2 November |  119 |    88 |       8 |           19 |           0 |      4 |
| 3 November |   71 |    64 |       2 |            4 |           1 |      0 |
| 4 November |  104 |   102 |       0 |            2 |           0 |      0 |
| 5 November |    0 |     – |       – |            – |           – |      – |

Valid to Not checked count successful returns. Not checked: the unit failed before the return was judged, for example by reaching the replica cap with no consensus. On 1 November the controller's enablement had expired, so Intel's window stayed closed all day.

## Caveats

- One read is one moment. The 24-hour sends are measured against the blocked head at the read, not the head at each send.
- Only hosts that sent Qwen work since 28 October count as takers. A host whose class was closed throughout, such as the 6 GB NVIDIA card, isn't listed.
- The server image roll later on 5 November restarts the feeder; whether the policy and cache come back as before is reported on #786.

## What is left

These are decisions for the maintainer, and nothing was changed:

- Whether Intel's window may take a unit's second replica, not only fresh pairs. While no fresh units are produced, the Intel host can take nothing, though 1,582 unsent units are open to it.
- What to do with the 102 units only Intel can take: open Intel's class to them, or cancel them (#1420 counted 93 on 4 November).
- The German c07 repetition on #786 is separate and still open.
