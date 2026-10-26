# Decade pruning: production acceptance

- **Issue:** [#123](https://github.com/tgoodyear/usnewsmap.com/issues/123)
- **Measured:** 25 October 2026 ET (26 October UTC)
- **Tooling:** main `fedabd04`, `scripts/ops/bench-cold-searches.py` and `scripts/ops/load-cold-searches.py`
- **Serving version:** `pages-v20261022-1`, published 24 October 2026
- **Searcher:** Azure Quickwit, 3.75 vCPU / 7.5 GiB; no restart or cache clearing

## Published layout

The authoritative lake `reference/current.json` names this version and
`pages-base-20261022-1`, with `decades: 1`. Its reference manifest records
`build.features.decades: 1`, `text_layout: 2`, `common_grams: 2`,
`page_language: 1` and a 60,000-document split target. The main index has
23,740,914 documents and 393 splits; the Japanese index has 11,163
documents and one unpartitioned split.

| Decade bucket | Main splits |
| ------------- | ----------- |
| 1820          | 4           |
| 1830          | 2           |
| 1840          | 5           |
| 1850          | 9           |
| 1860          | 13          |
| 1870          | 17          |
| 1880          | 26          |
| 1890          | 48          |
| 1900          | 74          |
| 1910          | 88          |
| 1920          | 47          |
| 1930          | 21          |
| 1940          | 19          |
| 1950          | 14          |
| 1960          | 6           |
| **Total**     | **393**     |

The 1820 bucket includes earlier decades. The manifest establishes the
layout, not request-time pruning by itself.

## Request-time pruning

At 01:39:27–01:39:33 UTC on 26 October, queried the **serving**
searcher's loopback REST endpoint through an authenticated Container
Apps exec session. Nothing was exposed publicly or installed. Its exec
endpoint initially returned 429; honored its 600-second Retry-After
before making the consolidated measurement.

Both searches used `max_hits=0` against `pages-base-20261022-1`:

```text
text_all:influenza AND day:[79622 TO 79986]
text_all:influenza AND day:[79622 TO 79986] AND decade:IN [1910]
```

The inclusive day numbers are 1 January through 31 December 1918,
relative to 1 January 1700. No timestamp or `date:` filter was used.
The exact day filter remained unchanged.

| Query              | Hits   | Targeted splits | Quickwit elapsed ms | Errors |
| ------------------ | ------ | --------------- | ------------------- | ------ |
| Without decade tag | 58,353 | 393             | 232.519             | 0      |
| With decade tag    | 58,353 | 88              | 149.235             | 0      |

Targeted splits come from the success-labelled
`quickwit_search_leaf_search_targeted_splits_sum` counter: 372,779
before, 373,172 after the untagged query, 373,260 after the tagged
query. Successful root and leaf request counters each advanced from
4,335 to 4,336 to 4,337, exactly one per query; the corresponding
targeted-splits observation count did likewise. Three-second idle
checks before and after showed no counter movement. Thus no other
successful root/leaf request contaminated those deltas.

The tag preserved hits while reducing targeted splits by **77.6%**
(393 to 88, 4.47 times fewer), matching the manifest's 88 splits in the
1910 bucket. These two ordered queries are a pruning correctness
check, not a controlled latency experiment: the untagged request could
warm data for the tagged one. This proves pruning for this deployed
1918 case, not every possible date range.

## Public API benchmarks

Ran each command once against the live site, sequentially, with no
browser-origin headers:

```sh
python3 scripts/ops/bench-cold-searches.py --base-url https://usnewsmap.com
python3 scripts/ops/load-cold-searches.py \
  --base-url https://usnewsmap.com --levels 1 --pause 30 --offset 7 --verbose
```

The three whole-corpus monthly searches all returned 200 without a 202:

| Search       | First response s | Reported backend s | Repeat response s |
| ------------ | ---------------- | ------------------ | ----------------- |
| radio        | 2.16             | 1.704              | 0.16              |
| television   | 1.09             | 0.750              | 0.12              |
| yellow fever | 4.05             | 3.698              | 0.16              |

The load script always runs an initial concurrency-one pass, then the
requested levels, so `--levels 1` means two sequential ten-search passes.
Its date windows moved back eight and nine days respectively.

| Pass                   | Searches/s | Median s | Slowest s | Mean backend s | Failed |
| ---------------------- | ---------- | -------- | --------- | -------------- | ------ |
| Initial, concurrency 1 | 0.581      | 1.20     | 3.99      | 1.45           | 0/10   |
| Next, concurrency 1    | 0.572      | 1.35     | 4.18      | 1.41           | 0/10   |

Neither pass received a 202, busy response or 429. The date-limited
`influenza` case took 0.46 / 0.63 s end-to-end (0.30 / 0.35 s reported
backend); `cross of gold` took 0.99 / 0.67 s (0.88 / 0.49 s backend).
The site remained ready and reported the same version afterwards.

These are first/repeated API requests on an already-running searcher,
not cold-start or disk-cold measurements. Date shifts distinguish the
two passes' cache keys, but do not prove no earlier visitor or benchmark
used those keys. Reported backend time can belong to a cached result.
No cache was purged. The older section 6.5 results used different index
layouts and compute; they are historical context, not a controlled
before/after estimate of the decade change's speedup.

## Outcome and limits

The deployed layout and same-hit/fewer-split acceptance checks for #123
passed, and the bounded benchmark commands completed without failures.
The API stayed ready on the same version. No rebuild, deployment,
restart, cache purge, resource setting change or BOINC action was needed.
The evidence supports closing the decade-pruning implementation issue;
broader concurrency sizing, cold-start behavior and on-prem migration
remain separate work.
