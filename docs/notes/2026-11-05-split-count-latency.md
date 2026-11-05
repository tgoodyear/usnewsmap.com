# What the split count costs an uncached search

**Status:** measured; nothing changed in production · **Date:** 5 November 2026 · **Measured on:** the public API serving `pages-v20261026-1` (399 splits, 23.77M pages), uncached; and synthetic indexes of 120,000 pages on the shared dev host (CT 240, 4 cores, load average 10 to 30), Quickwit 0.9.1 with production's searcher settings · **Code:** `crates/usnm-ingest/tests/split_latency.rs`, `crates/usnm-ingest/tests/synthetic/mod.rs`; [05 §5.5.1](../design/05-search-and-storage.md#551-quickwit-index-config-validated-in-s-2) · **Issues:** #1430, #1518

## The question

#1430 lists four ways to cut the latency of uncached searches. #1518 did the first: a typical search is now one full pass (summary and cube together) and two one-day lookups for the earliest and latest pages. The other three are fewer splits at the next rebuild, warming common words' data when the searcher starts, and a cheaper path for very rare phrases. Before picking `--split-docs` for the next rebuild, we wanted to know how much of an uncached search the splits cost, and how that cost grows for each kind of search.

## What we did

1. **Production, from outside.** A handful of `/v1/aggregate` searches on the public API, one at a time and 30 s apart, reading `timing_ms.backend`. Each window ends a different number of days before the corpus's end, so neither the API's caches nor Quickwit's partial-request cache can answer. To isolate the per-split floor we searched for made-up words, a new one each time, over the full range and over one decade, which decade pruning (§5.5.5) cuts to that decade's splits.
2. **The harness.** `split_latency.rs`, an ignored test, builds one synthetic corpus into one index per split target. Each index is laid out by decade and sent a decade at a time, as a release does. One read-only searcher with `infra/quickwit/searcher.yaml`'s settings and the 4 threads `scripts/onprem/serving/gen-env.sh` gives production serves them all. It plans each search with `plan::aggregate`, as `/v1/aggregate` does. After one round to load the caches, each round's window ends a day earlier, so Quickwit's partial-request cache never answers. Its corpus generator is the one `decade_measure.rs` used, moved to `tests/synthetic` and shared, so the planted words are reproducible. The run was:

   ```sh
   QUICKWIT_BIN=…/quickwit-v0.9.1/quickwit USNM_MEASURE_DOCS=120000 \
     USNM_MEASURE_SPLITS=1000,4000,12000 USNM_MEASURE_ROUNDS=9 \
     USNM_MEASURE_DIR=… cargo test -p usnm-ingest --test split_latency -- --ignored --nocapture
   ```

3. **Options 3 and 4.** We read how the searcher reads a phrase and what it keeps between requests, and changed no code for either. The reasons are under [What is left](#what-is-left).

## What we measured

**Production** (public API, 5 November 2026, backend ms):

| Search                                     | Pages matched | Backend ms                  |
| ------------------------------------------ | ------------: | --------------------------- |
| A word on no page, full range              |             0 | 331, 207                    |
| A word on no page, 1840s (about 5 splits)  |             0 | 5, 6                        |
| A word on no page, 1900s (about 74 splits) |             0 | 83                          |
| `phrenological`                            |        24,028 | 350                         |
| `lindbergh`                                |        79,328 | 506                         |
| `society`                                  |     6,955,195 | 599                         |
| `railroad`                                 |     8,982,236 | 802                         |
| "phrenological society"                    |           618 | 1,922 (first), 1,206, 1,360 |
| "phrenological lecture"                    |           112 | 1,490, 1,025                |

The decades' split counts are `pages-v20261022-1`'s ([decade pruning note](2026-10-25-decade-pruning-acceptance.md)). The version now serving has 6 more splits.

**Synthetic** (120,000 pages of about 600 words, 9 rounds; searcher CPU is the mean, the times are medians):

| Search                           | Pages matched | Splits | Searcher CPU ms | Total ms | First pass ms | Edges ms |
| -------------------------------- | ------------: | -----: | --------------: | -------: | ------------: | -------: |
| A word on no page                |             0 |    133 |              22 |       31 |            31 |        0 |
|                                  |               |     46 |              11 |       18 |            18 |        0 |
|                                  |               |     27 |               7 |       13 |            13 |        0 |
| A rare word                      |         1,277 |    133 |              86 |      171 |           160 |       14 |
|                                  |               |     46 |              38 |      106 |            93 |        9 |
|                                  |               |     27 |              27 |       93 |            69 |       12 |
| A common word                    |        18,897 |    133 |              94 |      227 |           212 |       15 |
|                                  |               |     46 |              42 |      167 |           147 |       16 |
|                                  |               |     27 |              37 |      105 |            96 |        9 |
| A very common word               |        47,880 |    133 |             110 |      262 |           231 |       19 |
|                                  |               |     46 |              51 |      174 |           151 |       14 |
|                                  |               |     27 |              39 |      145 |           112 |       16 |
| A rare phrase with a common word |            40 |    133 |             144 |      254 |           241 |       14 |
|                                  |               |     46 |              54 |      114 |            95 |       16 |
|                                  |               |     27 |              33 |       75 |            45 |       17 |

The indexes held 133, 46 and 27 splits for targets of 1,000, 4,000 and 12,000 pages, about 0.7 to 0.8 GB each.

- **Production's floor is most of a rare word's search.** A search that matches nothing still opens every split. Over the full range that takes 0.2 to 0.33 s, about 0.5 to 0.8 ms a split, and over the 1840s 5 ms. `phrenological` took 350 ms in all. Halving the split count, to about 200 splits of 120,000 pages, would take about 0.1 to 0.17 s off every full-range search from the floor alone.
- **The synthetic searcher's CPU grows in a straight line with the splits, and a phrase pays the most per split.** From 133 to 27 splits, each split cost about 0.14 ms of CPU for a word on no page, 0.55 to 0.67 ms for a word on some pages and 1.05 ms for the rare phrase with a common word in it. The phrase's CPU fell from 144 ms to 33 ms. The 46-split index falls close to the same line for every kind (within 5 ms). The corpus is the same in every index, so the slope is what a split costs, not what the search reads.
- **The edges are small.** Each looks at one decade's splits. On a quiet run they took 4 to 6 ms (an earlier run of 5 rounds), and 9 to 19 ms here. They aren't worth folding into the first pass, which would need hits, sorting and snippets in it (#1518).
- **The elapsed times on the dev host are noisy.** The earlier 5-round run, at a lower load, took between a fifth and a third of these times with about the same CPU (rare word 53, 35 and 29 ms in all; the phrase 46, 25 and 18 ms). The CPU time is what to compare.

What the numbers don't show:

- The synthetic corpus is about 200 times smaller than production's and was all in the page cache, so a synthetic split costs less than a production one: 0.14 ms of CPU for a word on no page, against production's 0.5 to 0.8 ms of elapsed time.
- It doesn't reproduce the weight of a production common word's positions (`society` on 7M pages). That is why production's "phrenological society" takes 2 to 3 times as long as `society` alone, while the synthetic phrase is cheaper than its common word.
- Production's elapsed times include whatever else the searcher was doing, and were taken a handful at a time.

## What is left

- **Option 2, fewer splits.** The next full rebuild's split target is the lever, and it needs no code: `scripts/onprem/release.sh --split-docs N` (`USNM_SPLIT_DOCS`). The current version was built with a 60,000-page target. 120,000 would make about 200 splits and, from the floor alone, save about 0.1 to 0.17 s on every full-range search, probably more on phrases, whose per-split cost was the highest on the synthetic indexes. The bounds on the target are the writer's memory while a split uploads and the scratch held by the largest merge ([Larger splits](../operations/rebuild-and-index-storage.md#full-rebuild), 05 §5.5.1). Picking N is the maintainer's decision at the next rebuild. To size it against phrase latency first, run the harness with a larger corpus (`USNM_MEASURE_DOCS`, `USNM_MEASURE_WORDS`) on a quiet host, or measure a dev release at two targets as that section describes.
- **Option 3, warming.** Between requests the searcher keeps only split footers, fast fields and partial and predicate results in memory ([Searcher caches](../operations/search-serving.md#searcher-caches)). It reads postings and positions from storage on every search, which on-prem means from ZFS's ARC when they're there. ARC survives a searcher restart, and the 1 November trace found that a larger ARC didn't change the times. So warming at start helps only after a host restart or an eviction, and only until ARC evicts the data again. The first "phrenological society" above, 0.6 s slower than the next two, is about the size of that gain. A warm-up after a host restart is an operations decision, not an API change.
- **Option 4, rare phrases.** A phrase reads every word's positions in every split. The common-word pairs (05 §5.5.3) avoid that only for a phrase with one of the 89 function words before another word, so "phrenological society", which has none, reads all of `society`'s positions. We considered and left out two paths in the API:
  - A first pass for the words without the phrase, to find the decades with candidates, then the phrase over just those decades. It adds back a full pass of its own (`society` alone took 0.6 s) to save part of a 1.2 s one.
  - Checking the phrase in the API over that pass's pages. The summary and cube would then be computed outside Quickwit, and the API would have to reproduce the analyzer exactly for the counts to stay the same.

  The fix is index-side: pairs for frequent content words, or pairs in both directions. That means a new `common_grams` version, a larger index and a full rebuild.

- **The searcher's 4 threads.** `scripts/onprem/serving/gen-env.sh` caps the searcher at 4 threads (`QW_NUM_CPUS`, `RAYON_NUM_THREADS`, `QW_TOKIO_RUNTIME_NUM_THREADS`). Per-split work is CPU, so the cap bounds how fast a full-range search can go. We didn't measure the serving host's spare cores.
