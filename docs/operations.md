# Operations

Running the ingest pipeline in Azure: the backfill, releases, full rebuilds, and how to watch them. The pipeline is two Container Apps jobs, `caj-usnm-ingest-{env}` (`usnm-ingest run`: new LoC batches, the title catalog and a release) and `caj-usnm-backfill-{env}` (parallel `curate` workers for the initial corpus). Both run inside the VNet as `id-usnm-ingest`; the storage and Cosmos accounts stay private throughout. How they are deployed is in [`infra/README.md`](../infra/README.md); the design is in [08](design/08-azure-infrastructure.md) and [04](design/04-data-sources-and-ingestion.md).

Every command below uses your own `az` sign-in. CI can only roll the API onto new images, so starting jobs and changing settings is always a person's action.

## Deploy the jobs

`scripts/bootstrap.sh <env> --ingest` deploys both jobs (or `scripts/settings.sh <env> USNM_INGEST_JOBS true` and `scripts/provision.sh <env>` on an environment already on the registry). `USNM_BACKFILL_WORKERS` sets the backfill's workers (default 4), and `USNM_BACKFILL_CRON` an optional schedule for it (a UTC cron such as `"0 9 2-4 10 *"`; empty is manual), which every deployment keeps. The ingest job's Quickwit writer works on an NFS share, `USNM_INGEST_SCRATCH_GIB` in size (default 128; 0 removes it, 08 §8.4). The weekly schedule comes after the first release ([Weekly updates](#weekly-updates)).

## The title catalog

The catalog (`reference/catalog/titles.json`, `places.json`) is built by `titles-sync`, which `run` calls before every release. A batch is released only once its titles are in the catalog. LoC rate limits title records, often well under its published 20 a minute; titles-sync then sends nothing for 65 minutes and resumes more slowly, until 8 h into the run (`--titles-max-runtime-secs`), and the next run continues from where it stopped. `/v1/status` shows `titles.pipeline.awaiting_sync` (titles still to fetch) and `batches_waiting_for_titles`. Place corrections live in git and ship in the ingest image; the next run applies them (04 §4.6):

- **A misspelled or renamed town** that should share a place with the town's other titles: add `{"added": "2026-10-06", "from": "Skaguay", "reason": "old spelling", "ref": 190, "source": "…", "state": "AK", "to": "Skagway"}` to `catalog/overrides/place-aliases.json`. Every override entry carries its audit trail: `added` (the date), `reason`, `source` (what shows the change is right: a URL or a precise citation) and `ref` (the issue or PR), plus `updated` and `history` when it changes later (`catalog/overrides/README.md`). `from` is the city as LoC spells it (spelling variants such as "St."/"Saint" or a trailing state abbreviation already match); `to` is the town's name, which becomes the place's name. Keep the entries sorted by state, then `from`, and each entry's keys in alphabetical order; `cargo test -p usnm-ingest places` checks both, and refuses an alias whose `to` is another alias's `from`. Only merge towns that are one place on the map; a town that was separate (East Providence, Winston before 1913) keeps its own place.
- **Wrong coordinates or name**: add `{"added": "…", "city": "Lusk", "lat": 42.7625, "lon": -104.4522, "reason": "…", "ref": 123, "source": "…", "state": "WY"}` to `catalog/overrides/places.json`, sorted by state, then city, keys in alphabetical order, with `"name"` to change the place's name and `"precision"` (`city`, `county` or `state`) if needed. `source` says where the coordinates come from, `reason` why the override exists. Take the coordinates from the gazetteer (`catalog/gazetteer/us-places.csv`) or another public source, not from LoC, unless the gazetteer lacks the town and one of the place's own LoC records has its point (New Echota, #223). The `geocode` log lists places where LoC's point and the gazetteer's are more than 25 km apart and LoC's was kept (`furthest` in its warning, `disagreement_examples` in the report): start there. A LoC point more than 100 km from the gazetteer's town of that name (when the name isn't ambiguous there) is replaced by the gazetteer's already (`gazetteer_over_loc_places`); check that list too, since the gazetteer can have the name for a different town than the paper's. It also lists places at the median of LoC points more than 25 km apart (`loc_apart_examples`, with the two furthest points and their LCCNs): one of those records likely has another town's point. Each place in `catalog/places.json` records the rule its coordinates came from (`coordinates_from`) and the other names its titles use with the rule that merged each (`variants`).
- **A title whose record lists several places** (a paper that moved): add `{"added": "…", "city": "Chicago", "lccn": "sn84024055", "reason": "…", "ref": 190, "source": "…", "state": "IL"}` to `catalog/overrides/title-places.json`, sorted by `lccn`, keys in alphabetical order.
- **Check the effect first**: run `curl -s https://api.usnewsmap.com/v1/places > /tmp/places.json`, then `cargo run -p usnm-ingest --example place-report -- /tmp/places.json`; the report lists the merges, renamed places and places that move.

A corrected point or name ships with the next release, delta or full. A change that moves a published title to another place (most aliases, a title place, new merging rules) waits for the next full release: incremental releases keep published titles where they were published, and log `N published titles' place changes wait for the next full release`. Run a [full rebuild](#full-rebuild) on the E4 profile to apply them; the weekly run never starts one for this, since on the Consumption profile a full release runs out of memory (#172). To refresh the gazetteer for a newer Census year, run `scripts/build-gazetteer.py --year YYYY` and commit `catalog/gazetteer/us-places.csv`.

## Backfill and the first version

Backfill, then publish the first version:

```sh
RG=$(scripts/settings.sh prod AZURE_RESOURCE_GROUP)
# Queue every batch from LoC's listing and curate it. Downloads are paced to
# LoC's limit (10 bulk requests per 10 minutes per IP), one every 75 s across
# all workers, so this takes about 2.5 days. Each worker stops claiming 22 h
# after it starts and exits 0 once its current batch is done (by about
# 22 h 45 min). Start the job again when an execution has ended, until the
# queue is empty (it resumes where it left off).
az containerapp job start -n "$(scripts/settings.sh prod BACKFILL_JOB)" -g "$RG"
# When that has finished: curate any leftovers, build the base index, publish.
az containerapp job start -n "$(scripts/settings.sh prod INGEST_JOB)" -g "$RG"
```

Watch with `az containerapp job execution list -n <job> -g "$RG" -o table` and the saved log queries:

```sh
scripts/logs.sh prod list                      # the saved queries (ops/queries/)
scripts/logs.sh prod release-progress 6h       # docs sent, rate, retries, free disk, memory, merges
scripts/logs.sh prod index-layout 2d           # splits, docs and size of each index a release published
scripts/logs.sh prod curation-throughput 1d    # batches and pages per hour
scripts/logs.sh prod curate-replicas 6h        # each worker's last line, batch and stage
scripts/logs.sh prod errors-by-batch 1d
scripts/logs.sh prod job-executions 2d
```

The workspace id is the stack output `LOG_ANALYTICS_WORKSPACE_ID`; the queries run with your own sign-in (Log Analytics Reader or more). Traces (`release`, `curate`) and metrics are in Application Insights `appi-usnm-<env>`. With alert emails set, a failed job, a stalled release, a stalled backfill or a backfill replica silent for 15 minutes sends an email (08 §8.1.2). The same views, as charts: the "usnewsmap pipeline" workbook under Application Insights `appi-usnm-<env>` → Workbooks.
The last lines of each worker say why it stopped: `max runtime reached; claiming no more batches` (with the number of batches still queued) when the execution should be started again. `curation finished` on its own means the worker found nothing more it could claim. A batch that failed in this execution is queued again but skipped by that worker, so check `scripts/logs.sh prod errors-by-batch 1d` and start the job once more if it lists batches that weren't curated later.

## Switch the API to the published indexes

Once a version is published (`current.json` exists in `reference/`): `scripts/settings.sh <env> USNM_SEARCH_BACKEND quickwit`, then `scripts/provision.sh <env>`. Until then the API would have nothing to serve ([`infra/README.md`](../infra/README.md#what-this-slice-runs)).

## Weekly updates

Poll LoC weekly for new batches: `scripts/settings.sh <env> USNM_INGEST_CRON "17 3 * * 1"` (Mondays 03:17 UTC), then `scripts/provision.sh <env>` while no ingest execution is running. The setting lives only in `.azure/<env>/.env`. Each run fetches LoC's listing once, curates and releases what is new, and exits 0 when there is nothing (08 §8.4). It stops curating 6 h in and releases what it has; if its log says `max runtime reached; claiming no more batches` with batches still queued, start the backfill job to curate the rest. A scheduled job can still be started by hand.

## Check a release's splits

Before it publishes, a release waits for the writer's merges: `release merges` lines (every 30 s: step `settle` then `finalize`, splits, merges running and queued, free disk), then `merges settled; closing the index for its final merges`, then `merged; the index is closed to further writes` with the new index's split count. It then logs one `index layout` line per index of the new version and records the same under `indexes` in `reference/<version>/manifest.json`. `scripts/logs.sh prod index-layout 2d` lists them. A merged index has at most `pages / 30,000` (rounded down) `+ 7` splits (`split_num_docs_target` in `infra/quickwit/pages-index.yaml`); the release fails rather than publish more, or if merges don't finish within `USNM_MERGE_TIMEOUT_SECS` (default 14400, 4 hours; a setting, `scripts/settings.sh <env> USNM_MERGE_TIMEOUT_SECS <secs>` then `scripts/provision.sh <env>`, budgeted under the ingest job's 48-hour replica timeout (the backfill and Japanese OCR jobs keep 24 hours) in `infra/modules/ingestjobs.bicep`), or if the writer reports a full disk or a failed merge, or exits. A writer the kernel killed for memory shows as `the Quickwit writer was killed by signal 9 (SIGKILL): it ran out of memory (…)` in the `command failed` line; `quickwit_rss_mb` in `release progress` is its memory (08 §8.4).

## Searcher caches

The Quickwit sidecar keeps the split footers, fast fields and partial and predicate results in memory, with the capacities in `infra/quickwit/searcher.yaml` (08 §8.4). The API reads the sidecar's metrics every minute and reports them as `api.searcher_cache_*` (08 §8.1.2):

```sh
scripts/logs.sh prod searcher-caches 1d        # per replica and cache: MB and items held, hits, misses, evictions, hit rate
```

The same table, and charts of MB held and of misses and evictions, are in the "usnewsmap API" workbook. In Log Analytics:

```kusto
AppMetrics
| where AppRoleName == "usnm-api" and Name startswith "api.searcher_cache_"
| where tostring(Properties.cache) == "split_footer"
| summarize Count = sum(Sum), Held = max(Sum) by Name, Replica = AppRoleInstance
```

For `api.searcher_cache_bytes` and `api.searcher_cache_items` (gauges) read `Held`, the most the cache held in the span; for hits, misses and evictions read `Count`, their total in the span. Split footers are one item per split. A replica's first report counts everything since its sidecar started, the warm-up included.

**Sizing `split_footer_cache_capacity`.** Size it from the release's footer total, and check it against what the cache does after the warm-up. Evictions on their own don't show the footers don't fit: Quickwit 0.9.1 also counts replacing an entry as an eviction, and after a publish the cache drops the old version's footers. The API logs `split footer cache evictions seen` once per searcher run, for the record, without a cause. The footers don't fit when the total is above the capacity, or when footer misses and evictions keep coming hours after the warm-up, with no publish or restart in between. To size it:

1. Add up `footer_bytes` on the version's `index layout` lines (`scripts/logs.sh prod index-layout 2d`, column `FooterMb`), or under `indexes` in `reference/<version>/manifest.json`, over every index the version lists: the base, its deltas and the Japanese index.
2. Set `split_footer_cache_capacity` in `infra/quickwit/searcher.yaml` to that total plus about 25% for the deltas the next weekly releases add, rounded up. Quickwit reads `256MB` as 256,000,000 bytes, the unit `FooterMb` uses.
3. Check it fits the sidecar's 4 GiB next to the fast field cache (1 GB), the aggregation memory limit (768 MB) and the partial request and predicate caches (32 MB each). If it doesn't, the fast field cache is the one to shrink first; then the container's memory (`infra/modules/containerapp.bicep`).

The change goes live when it merges (CI applies `searcher.yaml` to the running app). Confirm with `searcher-caches` a day later: after the warm-up, footer misses and evictions stay near 0 (each replica start misses about once per split), and the footer cache's MB held is about the footer total.

## Searcher threads

The sidecar runs each split's search on its `search` thread pool, and downloads from Blob and opens the splits on its main Tokio runtime. The API reads both with the caches, every minute, and reports them as `api.searcher_pool_tasks` and `api.searcher_runtime_*` (08 §8.1.2):

```sh
scripts/logs.sh prod searcher-threads 2h       # per replica and minute: search pool ongoing and pending, main runtime threads and busy share
```

Read it while cold searches run (`scripts/load-cold-searches.py`):

- `SearchPending` above 0 with `SearchOngoing` at the pool's size: searches wait for cores. The pool has `RAYON_NUM_THREADS` threads (4, set on the sidecar in `infra/modules/containerapp.bicep`; without it, one per CPU Rust counts, which rounds the 3.75 vCPU quota down to 3). Quickwit doesn't report the size; it is the most `SearchOngoing` reaches.
- `MainBusyPercent` near 100 with the search pool below its size and nothing pending: searches wait on the main runtime. It has `QW_TOKIO_RUNTIME_NUM_THREADS` threads (4, same file; `MainThreads`).
- Neither: look at Blob reads, and at `max_num_concurrent_split_searches` in `infra/quickwit/searcher.yaml`, which limits the split searches downloading at once.

The pool numbers are a reading once a minute, not an average, so a short burst can fall between two. `MainBusyPercent` is busy time over the thread time the runtime had (`api.searcher_runtime_capacity_ms`), so it stays a share however the reports fall into the minutes; it is empty for a replica's first minute and after a searcher restart.

## Cache warm-up

Each API start, and each publish before it swaps the new version in, warms the caches with the home page's examples and the most frequent logged searches (06 §6.5). One line per run:

```sh
scripts/logs.sh prod warm-up 2d                # per run: examples warm, cached, computed, skipped, ms
```

Read `Warm` against `Examples` first: equal means every example's search was in the in-process cache when the run ended (it is counted then, so evictions show). Then:

- **A start that read everything:** `Cached` is the examples plus the logged searches, `Computed` and `Skipped` are 0, and `Ms` is a few seconds. This is the usual start once a version and release have been warmed once.
- **A start that computed some:** `Computed` above 0 means searches no cache held: examples added since, or every search after a release that changed the response format (`RESPONSE_FORMAT` in `crates/usnm-api/src/routes/mod.rs`). Each takes about 11 s, so about 26 fit in the 5-minute budget. `Skipped` above 0 means the budget ran out with that many searches left; the next start reads what this one computed and goes on from there, and a visitor who opens a skipped example computes and persists it.
- **A publish:** `Cached` is 0 or close to it (nothing is cached for a new version yet, unless a visitor computed it meanwhile), and `Computed` is the searches reached in 15 minutes, about 75 of the 100 examples. The next start computes the rest.
- **`GaveWayMs`** is how long a start's computations waited because every visitor slot was taken. Large values mean visitors kept the searcher busy during the start.
- **`TimedOut` or `Failed`** above 0: the `slow warm-up query` and `warm-up query failed` lines of that run name the example (`api-errors` lists the failures).

Each computed search also logs `slow warm-up query` with its `ms` when it takes 1 s or more, and `api.prewarm_query_seconds` (by `endpoint` and `source`, `cache` or `computed`) has the time of every warm-up query.

## Full rebuild

Rebuild the indexes as one merged base (one-off, or a compaction). A release also builds a full base by itself, without `USNM_INGEST_FULL`, when the published version was built with another search backend or another version of the common-word pairs (05 §5.5.3), or when `USNM_AMERICAN_STORIES` is on and the published version doesn't have American Stories' text (below); the steps below force one. It is also how merged places and other place changes of published titles reach the site (04 §4.6). Indexes built before the merge settings (September 2026) keep their many small splits: their splits are past any maturation period, so nothing merges them in place, and they are sealed. A full release builds a new base from every curated batch, merged, and publishes a version with no deltas; the previous version stays in Blob for rollback (`current.json`). It takes the writer lock like any release, so it never runs alongside another writer (08 §8.4.1). Run it when no backfill or ingest execution is running and the backfill has finished:

```sh
RG=$(scripts/settings.sh prod AZURE_RESOURCE_GROUP)
JOB=$(scripts/settings.sh prod INGEST_JOB)
scripts/settings.sh prod USNM_INGEST_FULL true
scripts/settings.sh prod USNM_MERGE_TIMEOUT_SECS 14400   # the default; set it only to change it
scripts/settings.sh prod USNM_DEDICATED_PROFILE true     # the E4 profile (#172)
scripts/settings.sh prod USNM_INGEST_ON_DEDICATED true   # and the ingest job on it
scripts/provision.sh prod
curl -s https://api.usnewsmap.com/v1/status | jq .titles.pipeline   # awaiting_sync: titles to fetch first
az containerapp job start -n "$JOB" -g "$RG"
```

A full run releases only once titles-sync has fetched every title the listing and the curated batches name (titles LoC doesn't have excepted; a failed fetch counts as left); a base built without them would leave their batches out. With titles to fetch (about 4.5 s each, plus 65 minutes for each time LoC blocks), the first execution may spend its 8 hours on titles-sync and then fail with `titles-sync reached its deadline with N of M titles left … nothing was released` (or `LoC rate limited titles-sync …`). That is expected: start the job again, and it continues where it stopped. The job-failed alert leaves this stop out; the severity 3 *ingest not progressing* alert fires if no execution follows within 3 hours, or if 3 stops in a row leave as many titles as the first. `scripts/logs.sh prod ingest-endings` lists each execution's ending, and the stop's `command failed` line carries `outcome: titles_left`. Once titles-sync finishes, the same execution builds the base. Watch it:

```sh
scripts/logs.sh prod release-progress 6h    # docs sent, rate, memory (quickwit_rss_mb), merges
az containerapp job execution list -n "$JOB" -g "$RG" -o table
```

- `title records` lines every 100 titles, and a `title records` report at the end (`left: 0` and no `throttled` or `out_of_time` when done).
- `release progress` every 30 s: about 750 pages a second before the common-word pairs (#154), so 23.8M pages took about 9 h; with the pairs (each page's index about 1.6× larger) the 2026-10-05 rebuild sent 230 to 460 pages a second, about 15 to 29 h for 23.7M pages (#172). `quickwit_rss_mb` should stay under about 5,500 on the Consumption profile (7,680 MiB container), or about 20,000 on the E4 profile (25 GiB).
- `release merges`: step `settle`, then `finalize`; `splits` falls by 9 every minute or two after ingest ends. About 1.7 h for 23.8M pages in October 2026; with the 60,000-page splits and the common-word pairs, expect about 2.7 h (#156), and at most `USNM_MERGE_TIMEOUT_SECS`. American Stories' text about doubles the index (step 4 of #218 measures the merge time).
- `merged; the index is closed to further writes`, `index layout` (about 800 splits for 23.8M pages at the 30,000-page target) and `released`. About 11 h from the start when the catalog was complete, in the October 2026 rebuild before #154. With the pairs, the 2026-10-05 rebuild sent only 230 to 460 pages a second, so plan for up to about 29 h of sending and about 41 h in all (up to 8 h of titles-sync, sending, up to 4 h of merges), inside the ingest job's 48 h replica timeout (#172, #173).

**Laying the base out by decade (#123, 05 §5.5.5).** To have the rebuild keep each decade in its own splits, so that a date-limited search skips the others, add the setting to the first provision above:

```sh
scripts/settings.sh prod USNM_PARTITION_DECADE true   # the job's --partition-decade
```

Only a full base takes it. The release then logs `decade layout` when it creates the index, a `sent a decade's pages` line each time a decade's pages go to the writer (at most every 30,000 pages of a decade; until then they wait in `/scratch/usnm/quickwit/decade-spill`, a few GB at most), and `sent the pages a decade at a time` with the totals before the merges. The `index layout` line of the base has `splits_by_decade`. Check afterwards: `current.json` has `decades: 1`, and `reference/<version>/manifest.json` has `build.features.decades` and `splits_by_decade` under `indexes`. Leaving the setting on or off changes nothing until the next full rebuild: deltas follow the published version (with the field and decade tags on a version laid out by decade, without on one that isn't). To stop searches naming decades at once, without a rebuild, publish a `current.json` without the key.

Then turn the full rebuild off, and the E4 profile with it, in two provisions: the job has to leave the profile before the profile can go.

```sh
scripts/settings.sh prod USNM_INGEST_FULL ""
scripts/settings.sh prod USNM_INGEST_ON_DEDICATED ""
scripts/provision.sh prod
scripts/settings.sh prod USNM_DEDICATED_PROFILE ""
scripts/provision.sh prod
```

Don't leave `USNM_INGEST_FULL` set: every run, scheduled ones included, would rebuild the whole corpus. Don't leave `USNM_DEDICATED_PROFILE` set either: while the environment has the E4 profile it pays the Dedicated plan management fee every hour (about $0.10 an hour in East US 2 in October 2026), whether or not a node runs. An E4 node itself (4 vCPU, 32 GiB) costs about $0.39 an hour while the job runs.

**Why the E4 profile.** The first full rebuild with the common-word pairs (#154) was killed when the Quickwit writer ran out of memory at the Consumption profile's limit (7.5 GiB with the scratch share's init container), 6.5 h in, at 26.5% (#172). On the E4 profile the ingest container gets 3.0 vCPU and 25 GiB, and the writer a 6 GiB indexing heap, a 120 s commit timeout and a 4 GiB ingest queue (`USNM_WRITER_HEAP`, `USNM_WRITER_COMMIT_SECS`, `USNM_WRITER_QUEUE`, set by `infra/modules/ingestjobs.bicep`), so it writes splits at the 30,000-page target instead of cutting small ones every 30 s and merging them (#172). The `writer tuning` line at the start of the release shows them. A full run also takes longer than the October 2026 rebuild (about 230 to 460 pages a second instead of 750), which is why the ingest job's replica timeout is 48 h (#173). Benchmark afterwards with `scripts/bench-cold-searches.py`. The commit timeout was 600 s at first: the ingest queue's write-ahead log is only truncated when a commit is published, so it filled in about 4 minutes and Quickwit refused pages ("no shards available") for the rest of each 10 minutes, and the October 6 rebuild was stopped at 15% after averaging 132 pages/s. At 120 s the queue holds about 2 minutes of pages.

## American Stories' text

The release indexes American Stories' text beside LoC's (04 §4.9, 05 §5.5.4, #218) only with `USNM_AMERICAN_STORIES` (the job's `--american-stories`), off by default. The first release with it builds a full base, so turn it on for a full rebuild, after step 4 of #218 has measured the index size and rebuild time, and with an explicit go-ahead:

1. Run the writer to the end: `scripts/ja-ocr/start-quality.sh prod american-stories-write` on the one-replica job. It marks each finished year with `curated/american-stories/years/{year}.json`; start it again until all 180 years have one (its log's `american stories write finished` line counts them). A release reads only the years with a marker, and a year finished after the base reaches the base's pages only at the next full rebuild.
2. Do the [full rebuild](#full-rebuild), with the setting added to its first provision:

   ```sh
   scripts/settings.sh prod USNM_AMERICAN_STORIES true
   scripts/settings.sh prod USNM_INGEST_FULL true          # optional: the setting forces a full base anyway
   ```

   With no year marked yet, the run fails at its start, before curation and titles-sync (`--american-stories (USNM_AMERICAN_STORIES) is set, but the curated store has no finished year of American Stories' text`). Its log has `American Stories' text` (years, parts) near the start, then `American Stories' text for the batch` per batch (pages, `text_mb`: what the release holds in memory for the batch), and `American Stories' text indexed` (documents with the text, and those with only it) before the merges.
3. Check the result: `current.json` has `american_stories: 1`; `reference/<version>/manifest.json` has `built_from.american_stories` and `build.features.american_stories`, and `american_stories.json` lists the parts read. On the site, a word from an American Stories headline finds pages, and a hit whose match is only there shows its snippet from that text.

Keep `USNM_AMERICAN_STORIES` set afterwards: weekly deltas then write the text for their pages too. Clearing it makes the next release publish a version without `american_stories`, so searches leave the text out (the quick way to switch it off, without a rebuild); setting it again rebuilds in full. To switch it off at once, without a release, publish a `current.json` without the key (05 §5.5.4).

## Japanese OCR: mixed pages and English text

**Mixed pages** (#204, 04 §4.8). The OCR job's targets (`targets-v3`) include the Japanese titles' pages whose LoC text is from 0.35 to under 0.65 word-like (`loc_text = mixed`, about 2,000 pages). List them once on the one-replica job, then start the OCR job as usual:

```sh
RG=$(scripts/settings.sh prod AZURE_RESOURCE_GROUP)
scripts/ja-ocr/start-quality.sh prod targets            # caj-usnm-jaone: writes ocr-ja/targets-v3.jsonl
# its log ends with `targets written` and the pages by loc_text (mixed: about 2,000); then
az containerapp job start -n caj-usnm-jaocr-prod -g "$RG"   # `run`: OCRs the targets not done yet
```

Listing streams the archive of every batch with a Japanese title again, as the first listing did, to find the pages LoC ships without text; pages already read are skipped by the run. Another cut: `start-quality.sh prod targets --mixed-below 0.8`, then `start-quality.sh prod run --mixed-below 0.8` (its own list and claims, `targets-v3-mixed80`); 0.35 or less lists no mixed pages. At the job's pace in October 2026 (a 2-page issue in about 21 s and a 4-page one in about 38 s per replica, mostly loc.gov's pacing) the 2,000 pages take about 5 hours on the two replicas, about $4.50 of Consumption time (2 × 4 vCPU and 8 GiB at about $0.43 an hour each), plus the listing. The next release publishes their Japanese text (an overlay-only release when nothing else is new). A mixed page joins the Japanese index only with Japanese on it: expect about 200 (Rocky Shimpo's front pages).

**English text of our OCR** (#203, 04 §4.8). Off by default. With `USNM_JA_LATIN` (the job's `--ja-latin`), a release gives the pages LoC ships without text a main-index document with the Latin-script text our OCR read on them (their English ads, mastheads and sections), and gives a page LoC read badly our text in place of LoC's where ours reads more words. No full rebuild is forced:

```sh
scripts/settings.sh prod USNM_JA_LATIN true
scripts/provision.sh prod
```

- The next release that builds a main index (the weekly delta) adds the pages LoC ships without text, a few thousand documents. An overlay-only release adds none. Its log has `indexed our Japanese OCR's Latin text` with `missing` and `in_place_of_locs`; `reference/<version>/ocr_ja.json` has `latin: {added, pages, hidden}`, `ja_latin.json` lists the pages, and the build record has `features.ja_latin`.
- Our text in place of LoC's reaches a batch when a release indexes it, so for the published Japanese batches at the next [full rebuild](#full-rebuild) (with the setting on).
- Check: an English search for a word in a Colorado Times ad (for example `"Larimer Street"` over 1945) finds pages marked "Our OCR", whose viewer link has no highlight. The page totals don't change: these pages were counted since #139.
- Clearing the setting stops new documents; a delta keeps the ones already indexed, and the next full rebuild leaves them out.

## Check Japanese search

Our OCR of the Japanese pages LoC ships without text (04 §4.8) goes live only with a release: each release reads the OCR job's output once, near its start, and logs `Japanese OCR overlay` with the pages it took. Pages the job reads after that wait for the next release; when nothing else is new, a run releases them on their own ("releasing the new Japanese OCR on the same indexes"). After a release publishes, check from outside:

```sh
scripts/check-ja-search.py --expect <issue> … --late <issue> …
```

Issues are named as the OCR job logs them (`ocr issue done`, `<lccn>_<date>_ed-<n>`): `--expect` ones the job finished before the release's `Japanese OCR overlay` line, `--late` ones after it. The script checks that `/v1/meta` names a Japanese index and its pages, that common words (日本, 戦争, 米国, 真珠湾, 収容所) return pages (a version without the index answers 422), that hits are marked as our OCR with the engine, that each `--expect` issue has pages of our OCR (the paper's hits on that day for any of の, に, は, を, in the issue's edition), and that each `--late` issue has none yet. Every request is pinned to the version `/v1/meta` named and doesn't follow redirects, so a release that publishes mid-check fails it rather than mixing versions. It sends Do Not Track, so its searches stay out of the search log, and exits 1 if a check fails.

## OCR quality audit

`jaocr.py quality` (`ja-ocr/quality.py`) measures how good LoC's OCR text is, by language and decade, to show where reading pages again would pay off, and how often a page's language differs from its title's first catalog language. It runs in the Japanese OCR job's image because the curated store is reachable only from the VNet. It reads every curated part of every batch in the published version once, keeps a fixed sample of pages (2% by default; a page is in it when a hash of its `doc_id` falls under the cut, so a rerun takes the same pages) and scores each sampled page's stored text. Word tokens are letters only (surrounding punctuation stripped, a word hyphenated across a line break joined), at least 2, case-folded.

The default metric, v2, first finds the page's language:

- Candidates are the title's catalog languages with a word list, plus English: wordfreq's, or for Hawaiian and Yiddish our own (`ja-ocr/wordlists/`, built by `scripts/ja-ocr/build-wordlists.py`; sources and licenses in its README). Each language's function words are its 100 most frequent words of 2 letters or more. The winner is the language whose function words are the largest share of the page's tokens, if that share is at least 0.10, at least 10 different function words appear, and among the function words only one of the two has, the winner has at least twice the runner-up's tokens. Otherwise the page is `und`: fewer than 20 word tokens, garbled, or in a language with no list (Dakota, Choctaw, Navajo, Latin, Welsh and others).
- A page whose words are mostly in another script takes the title's language in that script: Hebrew script is `yid` or `heb` (if the title lists both, the one whose function words clearly fit better), Cyrillic is Russian, Ukrainian, Bulgarian or Serbian. Serbian in Cyrillic is scored letter for letter in Latin script, against wordfreq's Serbo-Croatian list. Words are read with Yiddish ligatures (װ, ױ, ײ) as their two letters. A title that lists no language in that script gets `und`.
- `mixed`: a page of 200 words or more is cut into 50-word windows (a last, shorter window counts if it has at least 25 words; a shorter tail is left out), and each window gets a language if one clearly wins (the same test, with 5 different function words). Two languages that each win at least 20% of the decided windows, and at least 2 windows, make the page mixed. Each decided window is scored against its own language; windows no language wins (garbled, or too few function words) aren't scored. A second script holding at least 20% of the words in another of the title's languages also makes a page mixed (an English column on a Yiddish page): that part has no word minimum of its own, only the 5 different function words a window needs.

Then it scores the page against its language:

- `function_share`: the share of its word tokens that are the language's function words. Text in one language has a similar share from page to page, different for each language: on a handful of LoC pages checked while building it, about 0.41 to 0.46 for English, 0.25 to 0.31 for German, 0.36 to 0.39 for Spanish, and about 0.2 for Polish and Lithuanian, whose most common words are single letters and don't count. Compare a language's pages with its own median and read the low tail as damage.
- `damage_rate`: near-misses / (near-misses + tokens that are one of the language's 20 most frequent words). A near-miss is one OCR edit from one of those 20: a letter substituted or deleted, a thin letter (c f i j l n r t v) inserted, or one letter read as two thin ones ("h" as "li"). English "tbe", "tlie", "thc", "aud", "nnd", "ot", "iu"; German "bie", "unb", "ift", "fich", "dcr". Real words don't count: anything in the top 5,000 words of the language, of the title's other languages or of English ("she", "an", "or", "hand", "tho"). Danish and Norwegian also accept each other's words and the "aa" spelling of "å" ("paa", "aar"); Czech and Slovak each other's. A page with `damage_rate` over 0.1 counts as damaged, over 0.25 as badly damaged.
- `garbage_share`: the share of its tokens that rmgarbage-style rules flag: over 40 characters, mostly punctuation, the same letter 3 times in a row, 4 or more Latin letters with no vowel or only vowels, or a capital inside a lower-case word ("tHe").

`--metric v1` runs the first version: `dict_share`, the share of word tokens in wordfreq's top 200,000 words for the title's first catalog language. It is too lenient to rank pages: those lists hold common OCR errors ("tbe", "aud", "ift", "bie") and no frequency cut separates them from rare real words, so medians were about 0.95 for English and 0.86 for German. It also scored pages of multilingual titles against the first language listed, so Yiddish, Serbian and Polish papers catalogued as English first came out worst.

Start it as one execution of the job with `quality` and its options as the arguments; the script prints the execution's name:

```sh
scripts/ja-ocr/start-quality.sh prod --sample-pct 10 --min-pages 50
```

`az containerapp job start --args quality` alone doesn't work: when any container flag is given, the CLI sends a container override named after the job, with no image, settings or resources. The script sends the job's own template with only the arguments changed.

The job's replicas (`USNM_JA_OCR_REPLICAS`, 2) share the work. The parts are cut into chunks of whole batches (about 40 parts each); a replica claims a chunk under `audit/ocr-quality-v2-<version>-<pct>pct.run/<execution>/claims/`, scans it, and writes its sampled pages to `pages/<chunk>.jsonl.gz` and a `done/<chunk>.json` marker. A replica renews its claims every 5 minutes; a claim not renewed for `JAOCR_AUDIT_LOCK_MINUTES` (60) is taken over, and the replica that lost it drops its copy (a chunk's pages are the same whoever scans it). So a replica that dies costs only its chunks' time. When every chunk is done, the replica that creates `reduce.lock` tallies all the chunks, writes the outputs and the final status, renewing the lock as it goes, and then writes `finished.json`. The other waits for `finished.json` (taking over a reduce lock that goes stale), logs `ocr quality results written by another replica` and exits; a retried replica of a finished run exits at once. Progress writes stop once every chunk is done, and a waiting replica restores the final status if a late one replaced it. A new execution starts a new run directory, so a rerun redoes the work and overwrites the outputs. The outputs are shared by every execution with the same version and sample size, so the reducer publishes under `audit/ocr-quality-v2-<version>-<pct>pct.publish.lock` (released after, taken over when stale): two overlapping runs publish one after the other, and the later one's files win. Each replica scores parts in 6 worker processes (`JAOCR_QUALITY_WORKERS`; reading the parts is partly I/O, so more workers than the 4 vCPUs pays) and needs well under the job's 8 GiB: each worker holds a few small word lists and a cache of the last 262,144 distinct tokens' scores. The 2% run took 69 minutes on one replica before the work was shared and the token cache added (October 2026). The job's 24-hour replica timeout is its limit. `ja-ocr/bench_quality.py` times the per-page scoring on synthetic pages.

Output, in the curated container (the data account is reachable only through its private endpoint, like the search log below):

- `audit/ocr-quality-v2-<version>-<pct>pct.json`: every table, the summary (with the language agreement shares), the word lists and thresholds used.
- `audit/ocr-quality-v2-<version>-<pct>pct-<table>.csv`, one per table:
  - `by-language-decade`, `by-language`, `by-decade`, by detected language (`mixed`, `und`, and `no_text` for empty and short pages are rows of their own): pages sampled, shares empty and short, `text_pages`, shares `und` and `mixed`, mean, median and 10th, 25th, 75th and 90th percentiles of `function_share`, mean and median `damage_rate`, shares damaged (over 0.1) and badly damaged (over 0.25), mean `garbage_share`.
  - `language-agreement`: the crosstab of the title's first catalog language against the detected page language (`mixed:ger+eng` names both), with pages and the share of the row.
  - `agreement-summary`: for all pages with text, those of single-language titles, of multilingual titles, with no title, and of each first language: the shares detected as the first language, that differ, and detected as another language, `und` and `mixed`, each of the pages with text.
  - `worst-titles` (100) and `worst-batches` (50) by median `damage_rate`, with the detected-language mix (`ger:40 eng:3 und:2`); `undetermined-titles` (100), the titles with the highest `und` share.
- `audit/ocr-quality-v2-<version>-<pct>pct-pages.csv.gz`: every sampled page but Japanese titles', one row each: `doc_id`, title, date, batch, the title's languages, the detected language and decision, the winner's and runner-up's function-word shares, the script, `function_share`, `damage_rate`, `garbage_share` and word count. About 475,000 rows at 2%, for spot checks and for deciding how to tag page languages. It is compressed as the run goes; a sample of more than 3,000,000 pages (over about 12%) gets no page file (`ocr quality pages file dropped`), only the tables.

Titles and batches enter the worst lists with at least `--min-pages` pages with a `damage_rate`, and `undetermined-titles` with that many pages with text. The same rows are in Log Analytics, one `ocr quality` line each with `metric` and `table` fields, then `ocr quality finished` with the totals; `ocr quality progress` lines come about once a minute from each replica, with the chunks and batches done so far across the run. `scripts/ja-ocr/quality-rows.sh prod <execution>` prints a run's rows as JSON lines:

```sh
scripts/logs.sh prod ocr-quality 2d
```

A v2 run also writes `status/ocr-quality.json` in the reference container for the status page: `{batches: {done, total}, finished_at, metric, pages_sampled, sample_pct, started_at, summary, updated_at, version}`, at the start, with each progress line and at the end. At the end `finished_at` and `summary` are set: `summary.agreement` (`differs_share`, `mixed_share`, `und_share` of all pages with text, `multilingual_differs_share` of multilingual titles' pages) and `summary.languages`, each detected language with at least 200 sampled pages (`pages`, `function_share_median`, `damage_rate_median`, `damaged_share`). Every field is present, and null when there is nothing to measure: the agreement shares when no sampled page has text, `multilingual_differs_share` when no page of a multilingual title does (a small sample), and a language's three scores when none of its pages was scored (a language with no word list, such as Cherokee). A run that fails leaves its last progress, with no `finished_at`. A failed write is logged and the audit goes on.

Read the numbers with these limits in mind:

- It is a sample. At 2% a small title or a single decade of a rare language has few pages; check `text_pages` and `damage_pages` before trusting a row.
- `damage_rate` has a floor: real words outside the top 5,000 that are one edit from a common word ("ana", "hen", "ion" in English) count as misses, so clean English scores about 0.04 to 0.05. Old spellings do too, so compare decades within a language more than languages with each other. Misreads that are real words ("tho" for "the", "ber" for "der") and misreads two edits away ("bcr", "beu" for "der", "den") don't count, so it undercounts damage.
- `und` mixes three things: pages too short to tell, pages so garbled no language fits, and pages in languages with no word list. `garbage_share` and the `undetermined-titles` list help tell them apart. Heavily damaged pages can fall to `und` instead of their language, so a language's damage shares leave out its worst pages.
- Detection only chooses among the title's catalog languages and English: a German page in a title catalogued as English only is `und`, not German. English is always a candidate, so a page counted as English in a title that doesn't list English is English text (ads, a column) by its function words.
- Pages of titles that list Japanese are left out (our own OCR covers them) and counted in the `japanese_skipped` row.

## Text the Japanese titles' pages hold that search can't reach

`jaocr.py mixed` (`ja-ocr/mixed.py`) measures two gaps the Japanese OCR leaves. The OCR job read a page only when LoC's text for it was missing, empty, short or under 35% word-like; a Japanese query searches only our OCR, any other query only LoC's text. Its October 2026 results led to the `mixed` targets and the English text of our OCR in the main index ([above](#japanese-ocr-mixed-pages-and-english-text)); the command still measures by the 35% test.

- Japanese on pages LoC read as words: a page with an English column and a Japanese one, read in English, can pass the 35% test, and its Japanese column is then garbled Latin. For every page with LoC text of a title that lists Japanese, in the batches that hold such titles, it counts pages by word-like share (bands from under 0.35 to 0.9 and over); the other titles in those batches, same scanning and OCR but English only, are the control.
- English on the pages we read: NDLOCR-Lite reads some English (mastheads, short lines), but only into the Japanese index. For every page of our OCR it counts the tokens that are among English's 5,000 most frequent words (3 letters or more).

```sh
scripts/ja-ocr/start-quality.sh prod mixed          # prints the execution's name
scripts/ja-ocr/quality-rows.sh prod <execution>     # its "mixed pages" rows, once it has finished
```

It writes `audit/mixed-pages-<version>-<execution>.json` and a CSV per table in the curated container (named by execution, so two runs never write the same files): `loc-text-bands` (Japanese titles against the control), `loc-text-by-title`, `loc-text-samples` (15 page ids per band, with their loc.gov links, for looking at the scans), `our-ocr-english` (by why LoC's text wasn't used), `our-ocr-by-title` and `our-ocr-samples`. It runs on the one-replica audit job, `caj-usnm-jaone-<env>`, which `start-quality.sh` picks for it. Should a second replica run it anyway (the ja-ocr job), one does the work, renewing a lock; the other waits for its finished marker (`mixed pages written by another replica`) and takes over if the lock goes stale, and a retried replica of a finished execution exits at once. `our-ocr-english` and `our-ocr-by-title` also give the median Latin share of our OCR's letters and the share of pages over half Latin.

## American Stories against LoC's text

`jaocr.py american-stories --year Y [--year …] [--sample-pct 10]` (`ja-ocr/american_stories.py`, #205) compares [American Stories](https://huggingface.co/datasets/dell-research-harvard/AmericanStories) (Dell et al. 2023, CC BY 4.0), a re-OCR of Chronicling America, with LoC's text on our pages. It streams each year's tarball from Hugging Face (0.4 to 7.6 GB; nothing is written to disk), joins scans to our pages by their file names, read in a first pass (a scan named `<date>_p<page>_<lccn>_<reel>_<date><ed>_<image>.json` is our `<lccn>_<date>_ed-<ed>_seq-<page>`; one with no page, `pNone`, about 3% of 1865, takes its image's place in the issue, a rule that agrees with 92.8% of 1865's named pages; a second copy of a page is skipped), keeps the audit's fixed sample (`quality.sampled`), and reads LoC's text for the sampled pages of those years from the batches whose dates overlap them. For pages of titles whose first language is English it reports the join, words per page, the audit's English damage rate and function-word share for each text, pages matching a fixed term list in either text, and LoC's damage rate by the share of regions American Stories marked Illegible.

```sh
scripts/ja-ocr/start-quality.sh prod american-stories --year 1865 --year 1925
scripts/ja-ocr/quality-rows.sh prod <execution>    # its "american stories" rows
```

It writes `audit/american-stories-<version>-<execution>.json`. It runs on the one-replica audit job, `caj-usnm-jaone-<env>` (`ja-ocr/solo.py` still keeps a second replica out, as for `jaocr.py mixed`). A failed run raises *Japanese OCR or audit job failed* (severity 3), not the ingest alert.

### Indexing only where American Stories differs (`--diff`, #251)

`--diff` adds a measurement for indexing American Stories' text only where it differs from LoC's, to cut the second text field's cost to cold searches (#251). On the same pages it tokenizes both texts as the index does (`usnm_core::text`: hyphen rejoining, ligatures, split on what isn't a letter or digit, ASCII folding, tokens up to 40 characters), aligns them (a patience diff: tokens unique to both texts as anchors, `difflib.SequenceMatcher` with `autojunk=False` on small gaps without one), and builds each page's delta for k = 0, 2, 3 and 5: every region where American Stories' tokens differ from LoC's, with k of its tokens each side (where LoC has tokens American Stories hasn't, just the k each side), overlapping or touching windows merged. A term in American Stories' text is in LoC's or in the delta, and a phrase of n words that matches only in American Stories' text is inside one window once k is at least n - 1; the run checks both. New `american stories` tables:

- `diff_summary` (per year and k): American Stories', LoC's and the delta's tokens, the delta's share of American Stories' tokens (in total, and the median and 90th percentile per page), unique terms per page summed for each text and the delta (postings grow with them), `terms_lost` (should be 0), every page's distinct bigrams and trigrams that match only in American Stories' text and how many the delta keeps (`ngram2_loss`, `ngram3_loss`), the same for those whose words are all in LoC's text (`ngram*_edge*`: together only in American Stories'), n-grams that would match falsely if the windows ran together with no position gap (`ngram*_spurious_if_joined`), and `secs_per_1000_pages` for the diff.
- `diff_phrase` (per year, for the term list and 10 common phrases such as "new york" and "cross of gold") and `diff_phrase_group` (per year, for those two groups and the bigrams and trigrams of the run's `only_american_stories` contexts): pages matching in LoC's text, in American Stories', only in American Stories' (`only_words_all_in_loc` of those), and per k the ones the delta keeps (`kept_k*`, `loss_k*`, `edge_kept_k*`, `spurious_if_joined_k*`).
- `diff_edge_examples`: up to 10 pages a year with a phrase whose words are all in LoC's text but together only in American Stories', with loc.gov links.

```sh
scripts/ja-ocr/start-quality.sh prod american-stories --year 1865 --year 1925 --diff
```

The diff runs after the comparison and takes about 40 s per 1,000 pages of 8,000 tokens (measured on synthetic pages; the run logs its own figure in `american stories diff year`).

## American Stories' text for our pages

`jaocr.py american-stories-write [--year Y …]` (`ja-ocr/american_stories_write.py`, #218) writes American Stories' text (Dell et al. 2023, CC BY 4.0) for our pages to the curated container, for the release to index beside LoC's. It streams each year's tarball from Hugging Face once, places every scan on our page as `american-stories` does, and writes `american-stories/pages/<lccn>/<year>-<nnn>.parquet`: one row per page with `doc_id`, `lccn`, `date`, `text` (each article's headline, byline and text in American Stories' order, separated by blank lines), `articles` (JSON: each article's headline, byline, `start`/`end` in `text`, bounding boxes in scan pixels and the legibility of its regions), `legibility` (the page's text regions by legibility) and the scan's `width` and `height`. A page's second copy is skipped. Each year ends with a marker `american-stories/years/<year>.json` holding its counts; a rerun skips marked years, so an interrupted run resumes. A download that goes 5 minutes without data, or breaks off, fails and the year starts again from the top (up to 3 tries, a minute apart, each logged as `american stories year failed; retrying`); a retry writes the same parts under the same names. A year counts as read only when the gzip stream reached its end marker and, when Hugging Face sends one, the byte count matches `Content-Length`, so a download that stops at a file boundary inside the tarball is retried rather than marked. All 180 years are about 340 GB to stream.

```sh
scripts/ja-ocr/start-quality.sh prod american-stories-write          # every year
scripts/ja-ocr/start-quality.sh prod american-stories-write --year 1865
```

It runs on the one-replica job and logs `american stories year written` per year. `jaocr.py american-stories` also lists, per term and year, up to 8 pages that match only in American Stories' text with the words around the match (`only_american_stories`), for checking those matches by hand.

## Search log

The API keeps every search from the site, with only its filters, page count and UTC day, in the `searches` container ([ADR-0012](design/adr/0012-anonymous-search-log.md), 06 §6.8). Nothing expires `searches/days/` and `searches/import/`; staged batches are deleted after 7 days and stay in soft delete for 14 more. To read it:

```sh
scripts/settings.sh prod USNM_SEARCH_LOG_READERS "$(az ad signed-in-user show --query id -o tsv)"
scripts/provision.sh prod
scripts/searches.sh prod 30      # searches per day, top queries, queries that found nothing
```

The data account is reachable only through its private endpoint, so run `searches.sh` from a network that reaches it. The first day file appears an hour after the first full UTC day with the log deployed.

## Search cluster experiment

An experimental Quickwit cluster on Container Apps (#238, #239), measured on a 1% sample: indexing with 1 and 2 indexers, then searching with 1, 2 and 3 searchers. It runs in its own environment (`dev`), never in prod, and everything it adds sits behind `USNM_SEARCH_CLUSTER`. With the setting off, the stack is exactly what it was (the module is `if (searchCluster && useAcr)`).

**What it deploys** (`infra/modules/searchcluster.bicep`):

- **Nodes** `ca-usnm-qw-{i}`, `USNM_SEARCH_CLUSTER_NODES` of them (0 to 4), each exactly one replica of `USNM_SEARCH_CLUSTER_NODE_VCPU` vCPU (default 2, the API sidecar's size) with 2 GiB per vCPU, on the Consumption profile. Node 0 is the only metastore (the single writer of the file-backed metastore in `qw-cluster`), the control plane, the janitor, an indexer, a searcher, and the root: searches enter there through its internal ingress, `http://ca-usnm-qw-0` inside the environment. Nodes 1 to `USNM_SEARCH_CLUSTER_INDEXERS - 1` also index; the rest only search. Their node config is `infra/quickwit/cluster-node.yaml`; its searcher settings are the sidecar's (`searcher.yaml`), which a test checks.
- **Addresses.** Quickwit gossips over UDP and calls leaves over gRPC, both on replica IPs, which change whenever a replica restarts (#239). Each node starts as `usnm-qwcluster node`. That writes its replica IP to `qw-bench/seeds/qw-{i}.json`, reads the other nodes' entries as `QW_PEER_SEEDS`, sets `QW_ADVERTISE_ADDRESS`, and then becomes Quickwit. Every node writes before it reads, so of two nodes starting together at least one sees the other. A node that restarts at a new IP joins through the others, and they learn its new address by gossip.
- **Bench job** `caj-usnm-qwbench-{env}` (manual, 4 vCPU / 8 GiB, 6 h timeout), which runs `usnm-qwcluster` with each step's arguments (`scripts/start-job.sh`).
- **Storage, Entra only.** Containers `qw-cluster` (splits and metastore; Blob Data Contributor for each node's system-assigned identity, since Quickwit can use no other) and `qw-bench` (seeds, the sample, reports; Contributor for `id-usnm-qwnode-{env}` and `id-usnm-qwbench-{env}`). The bench identity also reads `curated` and `reference`. Nothing can write `qw-index`, `reference` or `curated`. Turning the setting off deletes both containers (soft delete keeps them 14 days).

**The steps.** Each `usnm-qwcluster` step logs one `qwcluster report` line and stores the full report in `qw-bench/runs/`.

| Step | Command | What it measures |
|---|---|---|
| `sample` | `sample --name N --pct P [--american-stories]` | Takes `P`% of the published version's pages (SHA-256 of `doc_id`, mod 10,000, below `P` × 100, as the OCR audit samples) as the documents a full release builds, with duplicate copies settled as the release does. Reads every curated part once. |
| `load` | `load --sample N --index I [--split-docs 3000] [--senders 4]` | Creates `I` from `pages-index.yaml` with `split_num_docs_target` set and one ingest shard per indexer, sends the sample, and times sending, all documents committed, merges settled, and sealed. Also records documents by node, splits and footer sizes, and retries on 429 and 503. |
| `bench` | `bench --label L --index I --sample N --expect-searchers K` | The 13 benchmark searches of `bench-cold-searches.py` and `load-cold-searches.py`, run as `/v1/aggregate` runs them (`plan::aggregate`) against node 0: `first`, `warm`, then concurrency 1, 2, 4 and 10, each with its windows a day further back. Reports median, p90, throughput, and each node's leaf searches and splits. |
| `members` | `members` | The cluster as node 0 sees it: members, addresses, generations, services. |
| `dump` | `dump [--hold-secs 600]` | Prints every stored report, for a log stream when Log Analytics is over its cap. |

**Split target.** At the production 30,000 pages, a 1% sample makes about 8 splits, too few for a root to spread over three nodes the way production spreads about 800. The loader's default of 3,000 makes about 80 splits of a tenth the size. Per-split fixed costs then weigh more than in production, so a second index at 30,000 (`--split-docs 30000`) brackets the result.

**Run it** (in `dev`; prod's data account is private, so dev curates its own 1% from LoC):

```sh
scripts/settings.sh dev USNM_INGEST_JOBS true
scripts/settings.sh dev USNM_INGEST_SCRATCH_GIB 0     # 1% fits the replica's disk
scripts/settings.sh dev USNM_LOG_DAILY_CAP_GB 1       # blob write logs; clear afterwards
scripts/provision.sh dev
RG=$(scripts/settings.sh dev AZURE_RESOURCE_GROUP); ACR=$(scripts/settings.sh dev ACR_NAME)
TAG=qwc-$(git rev-parse --short HEAD)
az acr build -r "$ACR" -t usnewsmap-ingest:$TAG -f Dockerfile.ingest --build-arg USNM_GIT_SHA=$(git rev-parse HEAD) .
az acr build -r "$ACR" -t usnewsmap-api:$TAG -f Dockerfile .
scripts/settings.sh dev USNM_IMAGE_TAG $TAG
scripts/settings.sh dev USNM_USE_ACR true
scripts/provision.sh dev

# The 1%: every 100th batch of LoC's listing by name, from the 40th (offset 39):
# 30 batches, 238,057 pages of 23,838,683 (0.999%), 26 awardees, 23.8 GB of archives.
B=arhi_beatles_ver01,az_fireant_ver01,ct_fairfield_ver01,curiv_plasse_ver01,dlc_alpha_ver03,dlc_debaptiste_ver01,dlc_goldenrod_ver01,dlc_leibovitz_ver01,dlc_saluki_ver01,gu_drteeth_ver01,iahi_hypno_ver01,in_irvington_ver01,khi_garwood_ver02,lu_juggernaut_ver01,me_calais_ver03,mnhi_dassel_ver01,mohi_berenice_ver01,mthi_goldeneye_ver01,ncu_cotton_ver04,nhd_lafayette_ver01,nn_keddy_ver01,ohi_himilco_ver01,oru_longspur_ver01,rp_hobgoblin_ver02,tu_eddie_ver02,uuml_anderson_ver01,vi_fezza_ver01,vnstcsc_duggan_ver01,whi_brie_ver01,wvu_jolie_ver02
scripts/start-job.sh dev INGEST_JOB enqueue --batches $B
scripts/start-job.sh dev BACKFILL_JOB curate --max-runtime-secs 14400
scripts/start-job.sh dev INGEST_JOB run --batches $B --full --curate-max-runtime-secs 21600 \
  --titles-max-runtime-secs 28800 --quickwit-bin /usr/local/bin/quickwit \
  --quickwit-metastore azure://qw-index --quickwit-index-root azure://qw-index
scripts/logs.sh dev release-progress 6h    # the single-writer baseline on the same pages

# One node, one indexer.
scripts/settings.sh dev USNM_SEARCH_CLUSTER true
scripts/settings.sh dev USNM_SEARCH_CLUSTER_NODES 1
scripts/settings.sh dev USNM_SEARCH_CLUSTER_INDEXERS 1
scripts/provision.sh dev
J() { scripts/start-job.sh dev SEARCH_CLUSTER_JOB "$@"; }
J members
J sample --name dev1pct --pct 100          # dev's whole version is the 1%
J load --sample dev1pct --index s1ix
J bench --label n1 --index s1ix --sample dev1pct --expect-searchers 1
# Two nodes, both indexing.
scripts/settings.sh dev USNM_SEARCH_CLUSTER_NODES 2
scripts/settings.sh dev USNM_SEARCH_CLUSTER_INDEXERS 2
scripts/provision.sh dev
J load --sample dev1pct --index s2ix
J bench --label n2 --index s1ix --sample dev1pct --expect-searchers 2
# Three nodes; node 2 only searches. Then the restart test.
scripts/settings.sh dev USNM_SEARCH_CLUSTER_NODES 3
scripts/provision.sh dev
J bench --label n3 --index s1ix --sample dev1pct --expect-searchers 3
J members
az containerapp revision restart -n ca-usnm-qw-2 -g "$RG" \
  --revision "$(az containerapp revision list -n ca-usnm-qw-2 -g "$RG" --query "[?properties.active].name | [0]" -o tsv)"
J bench --label n3-restart --index s1ix --sample dev1pct --expect-searchers 3 --levels 1,4 --levels-only
J members                                  # qw-2 has a new address and generation

# The report: Log Analytics, Azure Monitor per node app, list prices.
scripts/qwcluster-report.py dev --since 2d --out qwcluster-report.md
```

Start each step once the previous execution has ended (`az containerapp job execution list -n "$(scripts/settings.sh dev SEARCH_CLUSTER_JOB)" -g "$RG" -o table`). For a cold `first` pass, restart the node revisions before a bench. The first provision with the cluster may restart node 0 a few times while its identity's role on `qw-cluster` propagates. If Log Analytics is over its cap, stream the stored reports with `J dump --hold-secs 600` and `az containerapp job logs show -n <job> -g "$RG" --container qwbench --follow > lines.txt`, then run `scripts/qwcluster-report.py dev --from-file lines.txt`. Collect the report before scaling down: a deleted app's metrics can't be queried.

**Idle and teardown.** `USNM_SEARCH_CLUSTER_NODES 0` and a provision remove the node apps but keep the containers and the bench job, so nothing is billed but storage. `USNM_SEARCH_CLUSTER ""` and a provision remove everything the setting added, both containers included. Clear `USNM_LOG_DAILY_CAP_GB` afterwards.
