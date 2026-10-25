# Full rebuilds and index storage

Part of the [operations guide](README.md).

## Full rebuild

For the #283 account move, follow [Index storage cutover](#index-storage-cutover)
**before starting**: the new account stages its pointer rather than publishing
it to the old searcher. The ordinary full-rebuild commands below otherwise
publish immediately.

Rebuild the indexes as one merged base (one-off, or a compaction). A release also builds a full base by itself, without `USNM_INGEST_FULL`, when the published version was built with another search backend, without common-word pairs or with a version of them newer than the release knows (05 §5.5.3), or when `USNM_AMERICAN_STORIES` is on and the published version doesn't have American Stories' text ([American Stories' text](american-stories.md#american-stories-text)); the steps below force one. It is also how merged places and other place changes of published titles reach the site (04 §4.6). Indexes built before the merge settings (September 2026) keep their many small splits: their splits are past any maturation period, so nothing merges them in place, and they are sealed. A full release builds a new base from every curated batch, merged, and publishes a version with no deltas; the previous version stays in Blob for rollback (`current.json`). A full base is also what moves a version to the latest analyzer versions (05 §5.5.3): a delta folds its words as the published version's indexes do, so a change to the folding, such as vulgar fractions as one word (`½`, #168), reaches the site only with a full rebuild. Afterwards `current.json` has the latest `common_grams` and `ja.fold` (2 since #168), as does `build.features` in `reference/<version>/manifest.json`, and the API parses queries for them. It takes the writer lock like any release, so it never runs alongside another writer (08 §8.4.1). Run it when no backfill or ingest execution is running and the backfill has finished:

```sh
RG=$(scripts/azure/settings.sh --env prod --key AZURE_RESOURCE_GROUP)
JOB=$(scripts/azure/settings.sh --env prod --key INGEST_JOB)
scripts/azure/settings.sh --env prod --key USNM_INGEST_FULL --value true
scripts/azure/settings.sh --env prod --key USNM_MERGE_TIMEOUT_SECS --value 14400   # the default; set it only to change it
scripts/azure/settings.sh --env prod --key USNM_DEDICATED_PROFILE --value true     # the E4 profile (#172)
scripts/azure/settings.sh --env prod --key USNM_INGEST_ON_DEDICATED --value true   # and the ingest job on it
scripts/azure/settings.sh --env prod --key USNM_INGEST_SCRATCH_GIB --value 128     # the writer's scratch share (08 §8.4)
scripts/azure/provision.sh --env prod
curl -s https://api.usnewsmap.com/v1/status | jq .titles.pipeline   # awaiting_sync: titles to fetch first
az containerapp job start -n "$JOB" -g "$RG"
```

**Replica timeout (#233, #283).** Explicit full rebuilds (`USNM_INGEST_FULL=true`) and index-account staging runs have a **96-hour** replica timeout; ordinary ingest retains **48 hours** and backfill retains 24. The 1% dev candidate with American Stories, one text field, decade partitions, 60,000-page splits and page languages sent 113.79 documents/s, projecting about **58 hours of sending and flushing alone** for 23,766,177 pages. Budget another 8 hours for preparation (the curation and title deadlines are counted from the same start), up to 4 hours for merges, and 20% send-time variation: about 82 hours. The 96-hour limit adds margin; it doesn't guarantee completion, especially with production NFS I/O and OCR overlays, which this sample didn't time. Inspect the deployed job's `replicaTimeout` before starting. Keep automatic replica retries at zero: a release does not resume after timeout, and platform maintenance can still interrupt it.

A full run releases only once titles-sync has fetched every title the listing and the curated batches name (titles LoC doesn't have excepted; a failed fetch counts as left); a base built without them would leave their batches out. With titles to fetch (about 4.5 s each, plus 65 minutes for each time LoC blocks), the first execution may spend its 8 hours on titles-sync and then fail with `titles-sync reached its deadline with N of M titles left … nothing was released` (or `LoC rate limited titles-sync …`). That is expected: start the job again, and it continues where it stopped. The job-failed alert leaves this stop out; the severity 3 _ingest not progressing_ alert fires if no execution follows within 3 hours, or if 3 stops in a row leave as many titles as the first. `scripts/azure/logs.sh --env prod --query-name ingest-endings` lists each execution's ending, and the stop's `command failed` line carries `outcome: titles_left`. Once titles-sync finishes, the same execution builds the base. Watch it:

```sh
scripts/azure/logs.sh --env prod --query-name release-progress --span 6h    # docs sent, rate, memory (quickwit_rss_mb), merges
az containerapp job execution list -n "$JOB" -g "$RG" -o table
```

- `title records` lines every 100 titles, and a `title records` report at the end (`left: 0` and no `throttled` or `out_of_time` when done).
- `release progress` every 30 s: about 750 pages a second before the common-word pairs (#154), so 23.8M pages took about 9 h; with the pairs (each page's index about 1.6× larger) the 2026-10-05 rebuild sent 230 to 460 pages a second, about 15 to 29 h for 23.7M pages (#172). `quickwit_rss_mb` should stay under about 5,500 on the Consumption profile (7,680 MiB container), or about 20,000 on the E4 profile (25 GiB).
- `release merges`: step `settle`, then `finalize`; `splits` falls by 9 every minute or two after ingest ends. About 1.7 h for 23.8M pages in October 2026; with the 60,000-page splits and the common-word pairs, expect about 2.7 h (#156), and at most `USNM_MERGE_TIMEOUT_SECS`. American Stories' text about doubles the index (step 4 of #218 measures the merge time).
- `merged; the index is closed to further writes`, `index layout` (about 800 splits for 23.8M pages at the 30,000-page target) and `released`. About 11 h from the start when the catalog was complete, in the October 2026 rebuild before #154. With the pairs, the 2026-10-05 rebuild sent only 230 to 460 pages a second, so plan for up to about 29 h of sending and about 41 h in all (up to 8 h of titles-sync, sending, up to 4 h of merges), inside the historical 48 h limit (#172, #173). The measured #283 layout is slower; use the 96 h limit above for explicit full rebuilds and staging runs, not this historical estimate.

**Laying the base out by decade (#123, 05 §5.5.5).** To have the rebuild keep each decade in its own splits, so that a date-limited search skips the others, add the setting to the first provision above:

```sh
scripts/azure/settings.sh --env prod --key USNM_PARTITION_DECADE --value true   # the job's --partition-decade
```

Only a full base takes it. The release then logs `decade layout` when it creates the index, a `sent a decade's pages` line each time a decade's pages go to the writer (at most every 30,000 pages of a decade, or the split target with `USNM_SPLIT_DOCS`, below; until then they wait in `/scratch/usnm/quickwit/decade-spill`, a few GB at most at 30,000), and `sent the pages a decade at a time` with the totals before the merges. The `index layout` line of the base has `splits_by_decade`. Check afterwards: `current.json` has `decades: 1`, and `reference/<version>/manifest.json` has `build.features.decades` and `splits_by_decade` under `indexes`. Leaving the setting on or off changes nothing until the next full rebuild: deltas follow the published version (with the field and decade tags on a version laid out by decade, without on one that isn't). To stop searches naming decades at once, without a rebuild, publish a `current.json` without the key.

**One searched text field (#283, 05 §5.5.7).** To have the rebuild search LoC's and American Stories' text in one field (`text_all`), so a search looks each word up once, add the setting to the first provision above, alongside `USNM_AMERICAN_STORIES`:

```sh
scripts/azure/settings.sh --env prod --key USNM_SINGLE_TEXT_FIELD --value true   # the job's --single-text-field
```

Only a full base takes it. The release logs `one searched text field` when it creates the index, and `building index` with `text_layout: 2`. Check afterwards: `current.json` has `text_layout: 2`; `reference/<version>/manifest.json` has `build.features.text_layout` and, in `build.templates.pages`, the `text_all` and `text_all_cg` fields; `/v1/meta` says `"text_layout": 2`. The aggregate then has no `total.american_stories_only` (the site drops its page-count note), while hits keep `matched_in` and the American Stories badge. `USNM_AMERICAN_STORIES_SEARCH` has no effect on such a version ([American Stories](american-stories.md)). Leaving the setting on or off changes nothing until the next full rebuild: deltas follow the published version. To go back at once, publish a `current.json` of a version with a field per text (rollback); a full rebuild without the setting does it for good. Turning `USNM_AMERICAN_STORIES` off on such a version makes the next release a full base: the text can't be left out of the field it shares with LoC's in a delta.

**Page languages (#289, 05 §5.5.8).** To have the rebuild give each page its own languages, so `lang=` filters by them, add the setting to the first provision above:

```sh
scripts/azure/settings.sh --env prod --key USNM_PAGE_LANGUAGE --value true   # the job's --page-language
```

Only a full base takes it. The release logs `building index` with `page_language: true`, and before the merges `pages whose own languages differ from their title's, in the version` with their count (expect about 1.5 to 2 million: mostly pages of multilingual titles). Check afterwards: `current.json` has `page_language: 1`; `reference/<version>/manifest.json` has `build.features.page_language`, the two fields in `build.templates.pages`, `page_languages.pages`, and lists `page_language_baselines.json` (perhaps 10% to 20% more entries than `language_baselines.json`) and `page_languages.json` (about 15 to 20 MB); `/v1/meta` says `"language_filter": "page"`. Compare the release's pages a second with the last rebuild's (`release progress`): detection runs on up to 4 threads beside the writer. Leaving the setting on or off changes nothing until the next full rebuild: deltas follow the published version and carry the moves forward. To go back to title languages at once, roll back to a version without page languages (publish its `current.json`); a full rebuild without the setting does it for good. Don't edit the key out of the serving version's `current.json`: the API keeps the language filter it loaded the version with, and its caches are keyed by the version (it logs `current.json changed page_language without a new version`).

**Larger splits (#283).** To have the rebuild's merges stop at more pages per split than the template's 30,000, so a cold search opens fewer splits, add the setting to the first provision above:

```sh
scripts/azure/settings.sh --env prod --key USNM_SPLIT_DOCS --value 120000   # the job's --split-docs; the value comes from the dev measurement below
```

Only a full base takes it: a delta's few splits keep the template's target. The release logs `split target` when it creates the index and `building index` with `split_docs`. Check afterwards: the base's `index layout` line has about pages ÷ target splits (plus up to 7, per decade with `USNM_PARTITION_DECADE`) and a `largest_split_docs` near the target; `reference/<version>/manifest.json` has `build.features.split_docs` and `split_num_docs_target` at the target in `build.templates.pages`. The release holds the merged index to that target (`docs / target + 5 + 2`) before it publishes. `current.json` doesn't change, and the API needs nothing: it searches whatever splits the indexes have. What bounds the target is the writer (05 §5.5.1, 08 §8.4): a split holds min(its size, 10 GB) of the writer's memory while it uploads, and merged splits can reach about twice the target, at about 74 KB a page with American Stories' text; and the scratch share holds, at once, the largest merge (about twice the target's pages, about 18 GB at 120,000) and, with `USNM_PARTITION_DECADE`, the decade spill (under one target per populated decade, compressed; count decades from the actual corpus bounds rather than assuming the 1% sample covers every decade). For example, bounds 1736–1963 span 24 possible decades, so at 60,000 the conservative held-page cap is 1.44 million. The sample's compressed bytes per page is an estimate, not a hard bound for the full corpus. Both the spill and the concurrent merge must fit inside 128 GiB, so watch the free scratch while pages are sent and merged. So a target above 30,000 needs the E4 profile. Clear the setting after the rebuild: a full base the release forces by itself (a new analyzer or American Stories version, say) runs on the Consumption profile, whose 7.5 GiB only the template's target fits.

**Settings checklist for the next rebuild (#283).** Every setting the rebuild reads, its value, and what to check afterwards. Set them all before the first provision above; the release reads them only when it builds the base.

| Setting                                                        | For the rebuild                                                                | What it does                                                                                                                                                       | Check afterwards                                                                                                                                                                                                                                                                 |
| -------------------------------------------------------------- | ------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `USNM_INDEX_STORAGE`                                           | `true` for the account move; **keep on permanently**                           | Creates the private index account and a parallel read-only searcher; the writer stages full rebuilds until cutover                                                 | `INDEX_STORAGE_ACCOUNT`; `reference/index-storage-pending.json`; old `current.json` unchanged                                                                                                                                                                                    |
| `USNM_INDEX_STORAGE_SERVE`                                     | `false` during rebuild, `true` only after pointer promotion                    | Switches the API's new revision to the new-account searcher; later releases publish normally                                                                       | New API revision ready, `/v1/meta` names staged version; see cutover below                                                                                                                                                                                                       |
| `USNM_LAKE_BLOB_SOFT_DELETE_DAYS`                              | Leave at `3`; restore `14` after the old index is retired and pruned           | Restores the lake's recovery window without retaining future deleted splits                                                                                        | Lake's blob service retention; new account still has neither soft delete nor versioning                                                                                                                                                                                          |
| `USNM_INGEST_FULL`                                             | `true`, cleared afterwards                                                     | Every run builds a full base (`--full`)                                                                                                                            | `released` with `full: true`; `current.json` lists one main index                                                                                                                                                                                                                |
| `USNM_DEDICATED_PROFILE`, `USNM_INGEST_ON_DEDICATED`           | `true`, cleared afterwards in two provisions (below)                           | The E4 profile and the ingest job on it: 25 GiB for the container and the writer tuning (`USNM_WRITER_*`, set by the Bicep)                                        | The `writer tuning` line (`heap: 6GiB`, `commit_timeout_secs: 120`, `queue: 4GiB`); `quickwit_rss_mb` in `release progress` under about 20,000                                                                                                                                   |
| `USNM_AMERICAN_STORIES`                                        | `true` (as now)                                                                | American Stories' text with LoC's (04 §4.9); item 1 needs it                                                                                                       | `current.json` has `american_stories: 1`; the `American Stories` lines of the release ([below](american-stories.md#american-stories-text))                                                                                                                                       |
| `USNM_SINGLE_TEXT_FIELD`                                       | `true`                                                                         | One searched text field, `text_layout: 2` (item 1, #285)                                                                                                           | As in [One searched text field](#full-rebuild) above: `text_layout: 2` in `current.json`, `build.features` and `/v1/meta`; no `total.american_stories_only`                                                                                                                      |
| `USNM_SPLIT_DOCS`                                              | The target the dev measurement picks; empty until then (30,000), cleared after | Pages per split of the base (item 2)                                                                                                                               | As in [Larger splits](#full-rebuild) above: `split target` line, `build.features.split_docs`, the `index layout` split count                                                                                                                                                     |
| `USNM_PARTITION_DECADE`                                        | `true`                                                                         | Decade partitions (item 3, #123)                                                                                                                                   | `decades: 1` in `current.json`; `splits_by_decade` in the manifest                                                                                                                                                                                                               |
| (none)                                                         | Automatic                                                                      | A full base moves to the latest analyzer, version 2 (item 4, #168, #278; `½` is one word, #287's characters fold as themselves)                                    | `current.json` has `common_grams: 2` and `ja.fold: 2`; `build.features` has `common_grams: 2` and `ja_fold: 2`                                                                                                                                                                   |
| `USNM_PAGE_LANGUAGE`                                           | `true` once measured on dev (below)                                            | Page-level language (item 5, #289): each page gets its own languages, found from its words, so `lang=` and the counts by language use them                         | As in [Page languages](#full-rebuild) above: `page_language: 1` in `current.json` and `build.features`, the `page_language` and `page_language_set` fields in `build.templates.pages`, `page_languages.json` listed in the manifest; `/v1/meta` says `"language_filter": "page"` |
| `USNM_OCR_LOCAL`                                               | `true`                                                                         | Our OCR in place of LoC's where rule v1 prefers it, on every batch the base reindexes (item 6, #296, [Our OCR in releases](onprem-ocr.md#our-ocr-in-releases-296)) | The `our OCR in ocr-local` line; `reference/<version>/ocr_local.json` with the pages replaced; `build.features.ocr_local: 1`                                                                                                                                                     |
| `USNM_JA_LATIN`                                                | `true` if the published version has the Latin text (as now)                    | Our OCR's Latin text on pages LoC ships without text (#203); a full base without it drops those pages                                                              | `build.features.ja_latin: 1`; `reference/<version>/ja_latin.json` holds as many pages as before                                                                                                                                                                                  |
| `USNM_CONGRESS`                                                | `true` if the published version has a Congress set (none yet)                  | The Congress pages (#229); a release without it publishes a version without them (even with no new batch), and one that moves to a new analyzer rebuilds them      | `build.features.congress: 1`; `current.json` has `congress`                                                                                                                                                                                                                      |
| (none)                                                         | Automatic                                                                      | The catalog on `main` (`catalog/overrides/title-places.json`), the 17 title moves of #281 included (item 7); only a full base moves published titles               | `/v1/titles/{lccn}` of a moved title (such as the Anti-Slavery Bugle) names its new place                                                                                                                                                                                        |
| `USNM_MERGE_TIMEOUT_SECS`                                      | `14400` (the default), unless dev shows larger splits merge for longer         | How long the release waits for the merges                                                                                                                          | `release merges` reaches `merges settled` well inside it                                                                                                                                                                                                                         |
| `USNM_WRITER_INGEST`                                           | `v1`                                                                           | The writer's ingest API ([Duplicate-safe ingest](#duplicate-safe-ingest)): each accepted page once; the release restarts the writer for its final merges           | `writer ingest api` says `v1`; `restarting the writer for the final merges`, then the base's `index layout`; the page count matches the pages sent exactly                                                                                                                       |
| `USNM_INGEST_SCRATCH_GIB`                                      | `128`, cleared afterwards (below)                                              | The writer's scratch share, its account and private endpoint                                                                                                       | Free disk in `release merges` stays well above the largest merge (about twice the split target's pages), plus the decade spill's `peak_disk_mb` while pages are sent                                                                                                             |
| `USNM_AMERICAN_STORIES_SEARCH`                                 | `true` (as now)                                                                | No effect on a `text_layout: 2` version                                                                                                                            | No `USNM_AMERICAN_STORIES_SEARCH=false has no effect` warning in the API's log                                                                                                                                                                                                   |
| `split_footer_cache_capacity` (`infra/quickwit/searcher.yaml`) | Resized after the release if needed                                            | The searcher's footer cache must hold every footer of the version ([Searcher caches](search-serving.md#searcher-caches))                                           | The new version's `footer_bytes` total against the capacity, then footer misses after the warm-up                                                                                                                                                                                |

Afterwards, benchmark with `scripts/ops/bench-cold-searches.py` and compare with `pages-v20261008-1` (1,162 GB; median search 21.4 s with both texts' fields searched, #251), and note the index size, the send rate and the total time on #283.

**Before the go-ahead: what to measure on dev (#275).** The rebuild runs only once these are measured and the owner says go. Each needs the dev environment stood up with a 1% version (#275, the stand-up flow in PR #351) and American Stories' text:

- **One field and larger splits (items 1 and 2):** a full release on dev with `USNM_SINGLE_TEXT_FIELD`, `USNM_PARTITION_DECADE` and a few values of `USNM_SPLIT_DOCS` (30,000 and larger), each against a release without them: index size, pages per second, merge time, `quickwit_rss_mb` at its peak while the largest split uploads, the decade spill's `peak_disk_mb` (in `sent the pages a decade at a time`) beside the largest merge's scratch use, and cold search times. The search cluster's loader (`load --split-docs`, [below](environments-and-deploys.md#search-cluster-experiment)) sends documents with a field per text only, so it can measure the split target but not the one-field layout; a dev release measures both. Pick the rebuild's `USNM_SPLIT_DOCS` from the memory peak and the cold-search gain.
- **Time and profile (#233):** the last full rebuild took 35.8 h on E4. Confirm E4 (rather than Consumption, which can't take a larger target) from the dev send rate scaled to the corpus, against the deployed replica timeout (96 h for explicit full rebuilds and staging runs, 48 h otherwise); include preparation, merge and throughput-variance margin, not just send time. Whether two writers are worth it is still open (#233), and the release has one writer.
- **Page-level language (item 5, #289):** a full release on dev with `USNM_PAGE_LANGUAGE` against one without: pages per second (detection runs on up to 4 threads beside the writer), the count of pages whose own languages differ from their title's, the sizes of `page_language_baselines.json` and `page_languages.json`, and the cold `/v1/aggregate` times of 05 §5.5.8 with `lang=` on the complement side and with `USNM_PAGE_LANGUAGE_FILTER=match`.

Then turn the full rebuild off, and the E4 profile and the scratch share with it, in two provisions: the job has to leave the profile before the profile can go. Run them only once the rebuild's execution has ended (`az containerapp job execution list`), since the first one takes the share away from the job.

```sh
scripts/azure/settings.sh --env prod --key USNM_INGEST_FULL --unset
scripts/azure/settings.sh --env prod --key USNM_SPLIT_DOCS --unset   # if it was set: a forced full base on Consumption takes only the template's target
scripts/azure/settings.sh --env prod --key USNM_INGEST_ON_DEDICATED --unset
scripts/azure/settings.sh --env prod --key USNM_INGEST_SCRATCH_GIB --unset    # deletes the share, its account, endpoint and DNS zone
scripts/azure/provision.sh --env prod
scripts/azure/settings.sh --env prod --key USNM_DEDICATED_PROFILE --unset
scripts/azure/provision.sh --env prod
```

Don't leave `USNM_INGEST_FULL` set: every run, scheduled ones included, would rebuild the whole corpus. Don't leave `USNM_INGEST_SCRATCH_GIB` set either: the share, its private endpoint and its DNS zone cost about $25 a month (#308), and deltas don't need them. Its contents are scratch: a release empties the writer's data before it starts, so deleting the share loses nothing, and the next rebuild's provision creates it again, empty, under the same names. Don't leave `USNM_DEDICATED_PROFILE` set either: while the environment has the E4 profile it pays the Dedicated plan management fee every hour (about $0.10 an hour in East US 2 in October 2026), whether or not a node runs. An E4 node itself (4 vCPU, 32 GiB) costs about $0.39 an hour while the job runs.

**The WAL stays local even with scratch enabled.** Mounted jobs set the internal
`USNM_WRITER_WAL_DIR=/work`: the ingest image's writable ephemeral disk, not
`/scratch`. Each writer creates a fresh private directory there and links
`qwdata/wal` (Quickwit 0.9.1's v2 WAL) and `qwdata/queues` (its v1 ingest
queue, [Duplicate-safe ingest](#duplicate-safe-ingest)) to it. Startup requires at least
8 GiB free locally for the bounded 4 GiB WAL plus headroom; `writer WAL on local
disk` records the actual path and free MiB. `wal_disk_free_mb` in
`release progress` and `release merges` tracks the local volume separately from
the NFS `disk_free_mb`. The directory lives until the writer
stops, through the restart of a v1 release's final merges, and is not replayed
by the next run. Split cache, merge files and decade
spill remain on the large NFS share and retain their existing caps/checks. Jobs
without scratch keep the existing all-local layout. Neither layout resumes an
interrupted rebuild: the next run clears its writer data before starting.

This separates the WAL's latency-sensitive persist lock from NFS. The first
#283 production attempt failed after 25 h at 43.9%: a persist held that lock for
45.51 s against Quickwit 0.9.1's hard 6 s persist timeout (five router attempts),
then returned HTTP 500. It was not a full-disk or confirmed OOM failure. Retrying
that HTTP 500 blindly is unsafe because acceptance is ambiguous and can duplicate
documents. A failed staged run publishes nothing; keep the serving version and
rollback intact while fixing and restarting it.

### Duplicate-safe ingest

**Quickwit 0.9.1's ingest v2 is at least once, and a full rebuild can keep a
page twice.** The second #283 attempt (`si5kesp`, October 2026) sent all
23,740,914 pages, each request answered 200 once, but its base held
23,741,035 documents, and the release's exact count refused to publish it
(57 h lost). A scan of every `doc_id` found 52 pages held more than once, 121
extra copies, in two runs of consecutive pages: 23 pages 5 times and 29 pages
twice. Quickwit's router gives each persist 6 s and tries it up to 5 times
(`PERSIST_REQUEST_TIMEOUT`, `MAX_PERSIST_ATTEMPTS`; neither can be set), and a
persist that timed out still appends once the ingester's lock frees: the logs
show the lock held for 26 s at the time. The client cannot see this, and
Quickwit does not deduplicate by `doc_id`. Nor can such a base be repaired: a
delete by `doc_id` removes every copy, the original too, and the delete
planner skips splits that are not yet mature (7 days).

`USNM_WRITER_INGEST=v1` sends pages through Quickwit's v1 ingest API
(`use_legacy_ingest=true`) instead. It appends a request to the node's local
queue once, with no internal retry, and answers `num_docs_for_processing`.
The sink then:

- Sends a request again only after a 429 (queue full) or a 404 (the index's
  queue doesn't exist yet), which come before anything is appended.
- Never resends after any other answer, a transport error or a timeout. The
  release fails instead, since the request may have been taken.
- Requires `num_docs_for_processing` to equal the request's pages. v1 drops
  pages the mapping refuses without saying so.

v1's merge pipeline reads the merge policy only when the writer starts, so
the final merges can't lower the factor in place as v2's do.
`seal_restarting`:

1. Lets the merges settle.
2. Puts the index's config back with `merge_factor: 2`.
3. Restarts the writer on the same data directory and port. It sends
   SIGTERM, then SIGKILL after 60 s, and starts the new writer only once the
   old one has exited and been reaped. If that isn't confirmed 30 s after the
   SIGKILL, the release fails without starting a second writer.
4. Waits for the new merge pipeline's `spawning merge pipeline` line to show
   `merge_factor: 2`. It fails without publishing if the line shows another
   factor or if no pipeline starts within the finalize stall.
5. Lets the merges settle again, restores the original config, and checks the
   layout as before.

If the release fails or times out after step 2, it puts the original config
back before failing and says so in its error. A release that is cancelled
outright tries the same from a background task and logs the outcome. If the
error says the index "keeps merge_factor 2 in the metastore", the restore
failed. Such an index is never published: prune it, or PUT the config from
its `GET /api/v1/indexes/<id>` (`index_config`) back with the original
`merge_factor` before reusing it.

The page count and the layout check are unchanged: they still refuse an index
whose documents differ from the pages sent. v1 needs `--quickwit-bin`, the
writer the release starts itself, and is refused with `--quickwit-url`.

`USNM_WRITER_INGEST` is a public setting (`infra/env/<env>.json`), passed to
the ingest job. `v2` is the default; set `v1` for full rebuilds. Test it on
dev first (a 1% release with the rebuild's settings) and compare its pages
per second with v2's before a long run.

**Why the E4 profile.** The first full rebuild with the common-word pairs (#154) was killed when the Quickwit writer ran out of memory at the Consumption profile's limit (7.5 GiB with the scratch share's init container), 6.5 h in, at 26.5% (#172). On the E4 profile the ingest container gets 3.0 vCPU and 25 GiB, and the writer a 6 GiB indexing heap, a 120 s commit timeout and a 4 GiB ingest queue (`USNM_WRITER_HEAP`, `USNM_WRITER_COMMIT_SECS`, `USNM_WRITER_QUEUE`, set by `infra/modules/ingestjobs.bicep`), so it writes splits at the 30,000-page target instead of cutting small ones every 30 s and merging them (#172). The `writer tuning` line at the start of the release shows them. A full run also takes longer than the October 2026 rebuild (about 230 to 460 pages a second instead of 750), which is why ordinary ingest has a 48 h replica timeout (#173). Explicit full rebuilds and staging runs now get 96 h, from the slower #283 dev measurement above. Benchmark afterwards with `scripts/ops/bench-cold-searches.py`. The commit timeout was 600 s at first: the ingest queue's write-ahead log is only truncated when a commit is published, so it filled in about 4 minutes and Quickwit refused pages ("no shards available") for the rest of each 10 minutes, and the October 6 rebuild was stopped at 15% after averaging 132 pages/s. At 120 s the queue holds about 2 minutes of pages.

### Full rebuild on-prem

A full rebuild can run on pve-big instead of the ingest job (#283, #917): the release container, CT 215 `usnm-release` (home-infra `scripts/lab/release-ct.sh`, connected to Arc as `usnm-release-arcs-<env>` by `release-ct-arc.sh`), builds a staged version with the Quickwit writer on its own disk, and `usnm-ingest publish-staged` copies it into the index account and the lake. It takes nothing from the E4 profile, the scratch share or the 96-hour replica timeout, and no Azure compute runs. The writer works on a local ZFS dataset (`/srv/usnm`), not NFS. Settings and checks are those of the [checklist](#full-rebuild) above; [Index storage cutover](#index-storage-cutover) applies from its step 3.

What it uses in Azure, as the container's Arc identity (`USNM_ONPREM_RELEASE`, `USNM_ONPREM_RELEASE_HOST`, `modules/onprem-release.bicep`): read on `curated`; read on `reference`, with writes only under `pages-v*/` and to `index-storage-pending.json`; writes (no delete) on the index account's `qw-index`; AcrPull for the pinned Quickwit image. The lake and the index account open to `USNM_OCR_NODE_IPS` while the setting is on. Turning the setting off removes the grants and closes the index account; the writer role itself (`modules/release-writer-role.bicep`) stays while `USNM_ONPREM_RELEASE_HOST` is set, since the stack can't delete a custom role whose assignable scope carries its own deny assignment. Pipeline state is a local file, so the writer lock in Cosmos isn't taken: keep the ingest and prune jobs stopped and their schedules off, as the cutover already requires.

In the container, as `usnm`, each step in one run directory (`scripts/onprem/release.sh`); start the long one as a unit so it outlives the session:

```sh
cd /srv/usnm/src && git pull --ff-only     # the commit to build; then cargo build --release -p usnm-ingest
scripts/onprem/install-quickwit.sh --registry "$(scripts/azure/settings.sh --env prod --key ACR_NAME)"   # once per Quickwit pin
R=/srv/usnm/runs/full-$(date -u +%Y%m%d)
scripts/onprem/release.sh --run-dir "$R" --step mirror    # current.json, catalog/, the serving version's snapshot
scripts/onprem/release.sh --run-dir "$R" --step seed      # batch records from the serving version's batches.json
# as root in the container:
systemd-run --unit usnm-release --uid usnm --gid usnm -p SupplementaryGroups=himds \
  -p StandardOutput=append:$R/release.log -p StandardError=append:$R/release.log \
  /srv/usnm/src/scripts/onprem/release.sh --run-dir "$R" --step release
scripts/onprem/release.sh --run-dir "$R" --step publish --dry-run
scripts/onprem/release.sh --run-dir "$R" --step publish
```

Before promotion ([Index storage cutover](#index-storage-cutover) step 4), record the staged run in Cosmos from the snapshot's `index_run.json`, in the ingest job (the host has no Cosmos access):

```sh
scripts/azure/start-job.sh --env prod --job INGEST_JOB --wait-mins 20 -- import-staged-run --version pages-v<date>-<n>
```

Prune then keeps the version as a newer published run, so a rollback after promotion can still roll forward. The import replaces a failed earlier attempt of the same name that wrote no snapshot, and stops on another published version of it. A Cosmos-backed release or prune also imports the run when `current.json` names a version that has none. After the switch, once the OCR side is ready for the version, the same command with `--current` makes `ops/current` name the version ([Index storage cutover](#index-storage-cutover) step 5).

**Through Kafka (faster).** One writer node's v1 ingest API takes about 130 pages/s, however many requests are in flight. With `USNM_WRITER_KAFKA=127.0.0.1:9092` and `USNM_WRITER_INGEST=v1` (required: the release refuses Kafka with v2 before its writer starts), a decade-partitioned main index's decade files go to a Kafka topic instead, `usnm-<index id>`, with `USNM_WRITER_KAFKA_PARTITIONS` partitions (default 8). The index gets a Kafka source with one pipeline per partition, and the writer node gets the indexer capacity for them. Each file goes to one partition, the next file to the next, so splits stay one decade each. On pve-big, 1, 2 and 4 pipelines indexed 341, 509 and 754 pages/s of build pages. Everything else stays on v1: the last pages and the forced commit, the Japanese and Congress indexes, the exact count, the restart-based final merges. The source records its position in the metastore with each split, so the restart doesn't index anything twice. A produce whose answer was lost is worked out from the partition's end: the release is the topic's only producer, so the end shows whether the batch was written (go on), wasn't (send it again) or neither (fail). The topic's retention (3 hours) deletes records whether or not the index read them, so the producer keeps within reach of the index. Before a produce, at most every 10 s, it reads the index's Kafka checkpoint from the writer node:

- If a partition's oldest held record is past the index's position, records expired unread, and the release fails at once.
- While the index is more than `USNM_WRITER_KAFKA_MAX_LAG` records behind (default 1,000,000, about half an hour of sending), the producer waits.
- If the checkpoint stands still for `USNM_WRITER_KAFKA_STALL_SECS` (default 900 s, never less than the commit timeout plus 600 s) with records waiting, the release restarts its writer node, and the source resumes from its checkpoint. A restarted writer gets the consumer session timeout plus 2 minutes to show progress. There are at most 3 restarts, each only after progress since the one before; after that the release fails. A checkpoint read that fails or takes over 30 s counts as no progress, so a frozen writer is restarted too.
- After the last file, the release waits under the same rules until the index holds every record, then counts.

The source's consumers use a 300 s session timeout (librdkafka's default, 45 s, ended run e, #283, when the broker stalled) and a 900 s maximum poll interval. Once the count matches, the release removes the source and deletes the topic.

The broker is a single-node Apache Kafka in the release container (home-infra `scripts/lab/release-ct-kafka.sh`): KRaft, loopback only, data on the release dataset. Retention is 3 hours with 256 MB segments, so consumed records are dropped while the build runs. If indexing ever fell 3 hours behind, the records would be gone and the exact count would fail, rather than an index quietly missing pages. A release refuses a topic that already exists, since its records would be indexed again: delete a failed run's topic (`kafka-topics.sh --delete`) before building the same version name again.

**Threads.** A batch's curated parts are downloaded 8 at a time. Each part's rows are then built into documents, given their page languages and written as JSON lines on `USNM_RELEASE_THREADS` threads (default: the CPUs), 256 rows at a time, and merged back in row order, so the version is the same as when one thread built it. The `batch timing` line gives each batch's seconds by step (`fetch`, `build`, `lang`, `add`, `queue_wait`), which shows what bounds a run.

`seed` takes the batches the serving version was built from, not Cosmos's list: a batch curated after that version waits for the next release. `--sample 1pct` seeds the 1% set (every 100th batch by name from the 40th: 30 batches, 238,082 pages) for a rehearsal. A rehearsal stops before `publish`. `release` refuses a run directory that already holds a staged version; a failed one is started again in a new directory, since nothing in Azure changed. `publish` refuses unless the lake still serves the version the release took as its rollback (mirror and build again otherwise), every index's splits are published (none staged), and nothing of the same name in Azure differs; a rerun continues an interrupted copy. A version the lake already serves is copied with [`restore`](#restoring-serving-to-azure) instead. Each request to Azure is tried again after a failure that may pass (a dropped or hung connection, 408, 429, a 5xx): 12 tries over about 5 minutes, and a Put Block times out after 30 s plus its 8 MiB at 64 KiB/s. A split whose blocks run out of tries is sent again, as many as 3 more times, 5 minutes apart, and a split sent again (in the same run or a rerun) sends only the blocks Azure doesn't hold already. A 403 or a different object of the same name stops it at once. In the log, `index copy progress` (every minute) and `split copied` show the copy moving; `store request failed; trying again` and `split upload failed` show it waiting out a failure. Resize the container's memory and cores live with `release-ct.sh --resize`.

The merge wait's `release merges` line, every 30 s, says how far the merges have to go: `immature_splits` and `immature_mb` (the splits Quickwit still merges), `pending_merge_mb`, `merge_write_mb_per_sec`, and over the last 30 minutes `immature_mb_per_hour` and `eta_mins` at that rate. The wait is `USNM_MERGE_TIMEOUT_SECS` (default 4 hours).

#### Finishing a release

If a release stops after its main index passed the exact count, most often because the merge wait ran out, `release-finish` (`scripts/onprem/release.sh --step finish --index-version pages-v<date>-<n>`) finishes it in the same run directory, into the version the release would have published. It needs the release's settings (the same `--split-docs`, and the Kafka settings the release had) and the run's own log, `release.log`; log the finish to another file. The run doesn't record whether it was staged, so `release-finish` takes `--stage-index-storage` (the step passes it) or `--publish-serving`, one of them, never neither. It takes the writer lock and refuses unless the run failed (or stopped while building) before writing its snapshot, its log has the merge wait's lines for its main index, neither the catalog nor any curated batch's record has changed since the run started, and the index's published splits hold the documents the run logged sending (its last release progress, a 30 s sample, is a floor).

It then plans the release again, and the run record it comes to must be the run's (names, batches, pages, rollback, features, templates, writer tuning, Quickwit). Each title's documents in the index must carry the catalog's place, place shard, state, languages and ordinal. The curated store's listings are read as of the run's start (`--listed-before` sets another time), so our OCR's parts written since don't count. Its writer starts on the run's data and merges from the start; the splits a stopped writer left staged are marked for deletion, and a merge factor of 2 left in the index's config goes back to the template's.

Meanwhile the finish rebuilds every document the release sent, without sending it: the page languages, the pages given our Latin text or our OCR, and American Stories' record exist only in the stopped process, and only the documents give them back. This reads every batch's curated parts and American Stories' parts from the lake again, as the release did. Each batch's `our OCR for the batch's titles` and `American Stories' text for the batch` line, the planning lines and the end-of-build totals must match the run's log, the count must match the index's, and once the index is sealed, decade by decade. Then it waits for the merges (`USNM_FINISH_MERGE_TIMEOUT_SECS`, default 24 hours) exactly as the release does, removes the Kafka source and topic if they are still there, and runs the rest of the release: our OCR's files, the Japanese index, the layout, the snapshot, the pending pointer and the run record. A finish that stops can be run again. One that finds a Japanese index from the run asks for it to be deleted first.

#### Restoring serving to Azure

After Phase 4 of [ADR-0027](../design/adr/0027-onprem-serving-behind-cloudflare.md) nothing in Azure serves. The index account, its endpoint and the API and searcher apps are deleted, and the lake's `current.json` names the version that serves on-prem. A disaster recovery ([09 §9.6](../design/09-operations-security-cost.md#96-backup-and-disaster-recovery)) provisions them again and copies that version back. `publish` can't do the copy: it refuses unless `current.json` names the release's rollback, and `current.json` now names the release's own version. `restore` is the same copy for that case (#1061). Run it as `publish` is run, as `usnm` in the release container, with `DIR` the run directory that built the version: its `state.json`, `reference/index-storage-pending.json` and `qw/`, which are kept on `tank` for this. As for `publish`, the copy needs the container's grants and the index account open to the node IPs (`USNM_ONPREM_RELEASE` with `USNM_INDEX_STORAGE`), so provision with both set.

```sh
scripts/onprem/release.sh --run-dir "$R" --step restore --dry-run
scripts/onprem/release.sh --run-dir "$R" --step restore
scripts/onprem/release.sh --run-dir "$R" --step ready --app-url "$APP_URL" --index-version pages-v<date>-<n>
```

`restore` runs `usnm-ingest publish-staged --restore`. It checks what `publish` checks (every index's splits are published, none staged, and nothing of the same name in Azure differs), and it copies the same things: the splits, the metastores, the account's manifest, and the version's reference snapshot with its `index_run.json`. A rerun continues an interrupted copy. It differs in three ways:

- It refuses unless the lake's `current.json` names this release's own version and the indexes the release's pointer names. If `current.json` names the release's rollback, the release hasn't been promoted, and `publish` is the step; if it names another version, run `restore` from that version's run directory.
- It writes neither `current.json` nor `index-storage-pending.json`. The lake keeps pointing at the version, so the apps load it as they start.
- `--dry-run` runs every check and writes nothing to Azure, as it does for `publish`.

`ready` doesn't copy anything and needs no `usnm-ingest`, only `curl` and `jq`. It polls the app at `--app-url` (the app's default `azurecontainerapps.io` URL) until `/readyz` passes and `/v1/meta` names `--index-version`, and exits 1 if that hasn't happened after `USNM_READY_TRIES` tries (default 60), `USNM_READY_WAIT_SECS` apart (default 10). Run it after the copy, before the Worker is pointed at the app.

Limits:

- The restore reads the indexes only from `DIR/qw`, so a version whose indexes sit in several run directories can't be restored from one. That matters once releases have a delta mode (ADR-0027 decision 8): the restore then needs every index its pointer names under the one `--local-index-root`, and refuses, naming the missing metastore, if one isn't there.
- It copies the version's index and its own reference snapshot, nothing else. The lake's `current.json`, catalog and other snapshots must still be in Azure. Once Phase 5 moves reference on-prem, the Azure lake no longer has a `current.json` that names the serving version, so Phase 5's restore point has to cover reference too.
- It doesn't write Cosmos. The release host records each run in `index_runs` when it releases (ADR-0027 settled decision 5).

Rehearsal. Before Phase 4 the apps and the index account still hold the serving version, so `restore` finds everything in place: it copies nothing and writes nothing to Azure (its create-only request for each metastore finds the metastore there). Running it then in the version's run directory, with and without `--dry-run`, proves the checks and that the run directory is whole, and `ready` against the running app proves the readiness check. A copy into an empty index account is a different exercise. Do it on dev, or at Phase 4 itself, as an operator step, with the maintainer's approval for anything provisioned for it.

## Index storage cutover

The move of #283, #245 and #416 is opt-in
([ADR-0021](../design/adr/0021-separate-index-storage.md)). Merging it changes
no environment setting. `USNM_INDEX_STORAGE=false` and
`USNM_INDEX_STORAGE_SERVE=false` are the defaults. Do it on dev first, and in
prod only with the maintainer's rebuild go-ahead. All provisions, starts and
Blob commands here are **operator steps**, not part of CI.

Quickwit 0.9 resolves `azure://qw-index` against its one configured Azure
account; an account cannot be encoded in each index's URI. The writer's
metastore and index-root URIs both stay `azure://qw-index`, but
`QW_AZURE_STORAGE_ACCOUNT` selects the new account. A second, internal
searcher (`usnm-qw-search-ca-{env}`) reads that account with the same YAML
(`account:` replaced, `#polling_interval=30s` unchanged). The old searcher or
sidecar and its lake roles stay intact. The dev experiment (`qw-cluster`,
`qw-bench`) stays in the lake; it is not the serving index.

1. **Prepare and pause schedules.** Wait for ingest, backfill and prune to
   stop, then clear `USNM_INGEST_CRON` and record it for restoration. Save
   the serving `current.json` and the rollback version. Set
   `USNM_INDEX_STORAGE=true`, leave `USNM_INDEX_STORAGE_SERVE` absent or
   false, and set the full-rebuild settings above. Commit and review the
   settings, then provision. This creates `usnmqwst{env}{suffix}`, its Blob
   endpoint in `snet-pe` and DNS registration in the existing Blob private
   zone, the writer and ingest grants, and the new searcher. It grants
   nothing publicly: the account is deliberately **not** in
   `publicAccountIds`, so the existing deny policy covers it. The new
   searcher may restart until the first writer has created the metastore.
2. **Build, without switching readers.** Start the ingest job normally. Its
   template uses the new account, `--full` and `--stage-index-storage`.
   Staging ignores the old pointer for index reuse: main, Japanese and
   Congress indexes are rebuilt, including an unchanged Congress set.
   Reference snapshots stay in the lake. The run publishes
   `reference/index-storage-pending.json`, **not** `current.json`, and leaves
   `ops/current` unchanged (step 5 records it). The run record is
   `published` (to the pending pointer); its `previous_version` retains the
   lake version for rollback.
   A retry stages another fresh full base. Keep schedules off and do not
   start an ordinary release with hand-written arguments during migration.
3. **Verify before promotion.** Check the release's counts, manifest and
   merges; read the pending JSON and verify every main, JA and Congress index
   through the new searcher's `/api/v1/indexes/{id}` and representative
   searches from an in-VNet runner. Wait for its readiness probe and RBAC
   propagation. Confirm the live site still reports the old version. Prune
   protects a staged version as a newer published run, but defer cleanup
   until after cutover.
4. **Promote, then switch the API.** Keep every writer and prune stopped.
   From a private-network-connected operator host with Entra Blob Data
   Contributor on the lake's `reference` container (never enable keys or
   public access), back up and promote the pointers. Work in the repository,
   not a system temporary directory:

   ```sh
   LAKE=$(scripts/azure/settings.sh --env prod --key STORAGE_ACCOUNT)
   mkdir -p .azure/prod/index-storage
   az storage blob download --auth-mode login --account-name "$LAKE" --container-name reference \
     --name current.json --file .azure/prod/index-storage/serving-before.json --overwrite
   az storage blob download --auth-mode login --account-name "$LAKE" --container-name reference \
     --name index-storage-pending.json --file .azure/prod/index-storage/pending.json --overwrite
   jq '{index_version, previous_version, indexes, ja, congress}' .azure/prod/index-storage/pending.json
   az storage blob upload --auth-mode login --account-name "$LAKE" --container-name reference \
     --name current.json --file .azure/prod/index-storage/pending.json --overwrite true --content-type application/json
   scripts/azure/settings.sh --env prod --key USNM_INDEX_STORAGE_SERVE --value true
   # Commit/review/merge the setting, then provision from that commit.
   scripts/azure/provision.sh --env prod
   ```

   The old API may log `reload failed; keeping current version`: its lake
   searcher cannot find the new indexes, so it keeps serving its loaded old
   version. **Do not restart or roll the old API between promotion and the
   switch**: a fresh old-account replica cannot load the new pointer. Avoid
   CI and API image rolls in this window; promote only when the reviewed
   switch commit is ready to provision. If the old revision must restart or
   scale out, restore the saved pointer until the switch can complete.
   Provision changes `USNM_QUICKWIT_URL` to the already-ready new searcher;
   the new API revision loads and warms the promoted version before readiness
   moves traffic. The old searcher is never retargeted or deleted by this
   switch. The cutover is controlled, but it doesn't rule out cold starts
   between promoting the pointer and switching the readers.

5. **Check, record and retain rollback.** Verify `/readyz`, `/v1/meta`,
   counts, representative main, JA and Congress queries, and the index
   version on every ready API replica. Then record the version as the
   pipeline's current one, `ops/current`. The staged release left it naming
   the old version. `/v1/status` (`indexing.current_version`) reads it, and
   so does the OCR pipeline: the seeder and `ocr-enqueue` find pages in the
   version it names, `ocr-sync` checks the nodes' output against that
   version's batches and runs the seeder, and the BOINC export (the
   priority controller's claims and reservations) takes the version's batch
   list and fails with "published snapshot changed during export" if it
   changes during one.

   First get the OCR side ready for the new version. Otherwise every pickup
   run (`ocr-sync`) fails at seeding once `ops/current` names it:

   - **The damage tables' version list**,
     `audit/damage/v2/versions/pages-v<date>-<n>.json` in the curated
     store. The seeder refuses a version without it, and only `ocr-damage`
     writes it; a release doesn't. The pass is incremental: it skips every
     batch version already scored, so it pays only for new ones. Run it
     again until its `ocr damage finished` line shows `left` 0. On 24
     October 2026 the list existed for pages-v20261010-2 and not for
     pages-v20261022-1; it was then written on-prem (#1090). Run the pass
     on-prem first, as the damage writer's own identity (docs/operations/japanese-ocr.md,
     "On-prem"); the ingest job is the fallback:

     ```sh
     scripts/azure/start-job.sh --env prod --job INGEST_JOB -- ocr-damage --version pages-v<date>-<n> --max-runtime-secs 165600
     ```

   - **The prefetch manifest**, with `USNM_OCR_SEED_SOURCE=fetched` (as in
     prod). Without a `USNM_OCR_SEED_MANIFEST` pin the seeder reads the
     manifest of the version `ops/current` names, and only the pages on
     the newest fetched list for that manifest version
     (`ocr-out/fetched/<version>/`). Either build the manifest once the
     version list exists and have the node that publishes the fetched list
     publish one for it (`USNM_OCR_PUBLISH_FETCHED`, or
     `usnm-ocr-worker publish-fetched --manifest pages-v<date>-<n>` once):

     ```sh
     scripts/azure/start-job.sh --env prod --job INGEST_JOB -- ocr-prefetch-manifest --version pages-v<date>-<n>
     ```

     or pin `USNM_OCR_SEED_MANIFEST` to the version whose manifest and
     fetched list the seeder reads now, with `scripts/azure/settings.sh`,
     then provision.

   Then record the version between two ticks of the BOINC priority
   controller. On its host (CT 211, see
   [the batch scheduler](../boinc-batch-scheduler.md#on-prem-controller-host-ct-211)),
   stop the timer and wait until no tick is running, run the command, then
   start the timer again:

   ```sh
   systemctl stop usnm-boinc-priority-controller.timer
   systemctl is-active usnm-boinc-priority-controller.service   # repeat until neither active nor activating
   ```

   ```sh
   scripts/azure/start-job.sh --env prod --job INGEST_JOB --image-tag <sha> --wait-mins 20 \
     -- import-staged-run --version pages-v<date>-<n> --current
   ```

   ```sh
   systemctl start usnm-boinc-priority-controller.timer
   ```

   `--image-tag` takes a commit sha, not an image name: the execution's
   first container runs `usnewsmap-ingest:<sha>` from the job's registry.
   Use it for as long as the job's pinned image
   (`USNM_SCHEDULED_INGEST_IMAGE_TAG`) predates `--current`, with a main
   commit that has `--current` and an image. CI's publish job runs only for
   commits that change the images, and publishes only for the head of main,
   or an older commit when no newer one changes them, so not every commit
   has an image. Check first; it prints the sha when the tag is there:

   ```sh
   ACR=$(scripts/azure/settings.sh --env prod --key ACR_NAME)
   az acr repository show-tags --name "$ACR" --repository usnewsmap-ingest --query "[?@=='<sha>']" -o tsv
   ```

   The command refuses unless `current.json` already names the version and
   its run is published, so it never moves `ops/current` ahead of the live
   pointer, and it takes the writer lock like a release. A rerun changes
   nothing. Run it after the switch, not between promotion and the switch:
   until the new API serves the version, `/v1/status` would announce it
   early. This step records it at once; the next ordinary release would
   also repair `ops/current` from the live pointer before it runs. Check
   `indexing.current_version` in `/v1/status`, and that the next pickup
   run succeeds, in the `job-executions` query of `scripts/azure/logs.sh`.
   Restore the weekly schedule and clear the one-off rebuild, scratch and E4
   settings as above, but **keep both index settings true**. Do not prune
   the lake's rollback yet.

6. **Rollback across accounts, if needed.** Pause the writers and prune,
   restore `serving-before.json` to `reference/current.json` with the same
   Entra upload command, then set only `USNM_INDEX_STORAGE_SERVE=false` and
   provision. The retained lake searcher serves the old indexes again. Until
   then the new API keeps its loaded new version if its reload fails. Once
   the old version serves, record it in `ops/current` again as in step 5,
   between controller ticks and with the same image. First check step 5's
   OCR prerequisites for it: a version that served while seeding ran has
   its version list and manifest, but the seeder also needs a fetched list
   for the manifest version it reads.

   ```sh
   scripts/azure/start-job.sh --env prod --job INGEST_JOB --image-tag <sha> --wait-mins 20 \
     -- import-staged-run --version <rollback version> --current
   ```

   Its run is already in the state, so the import finds it and only
   `ops/current` changes. Nothing else repairs `ops/current` here: with SERVE
   false every release stages, and a staged release leaves it alone. Left
   naming the new version, `/v1/status` keeps announcing that version and
   the OCR pipeline keeps working on a version the site doesn't serve. Keep
   schedules off: with SERVE false, a release stages a full rebuild again;
   it does not send incremental writes back to the lake. Never turn `USNM_INDEX_STORAGE` off: the deployment stack would
   delete the new account and its data. Roll forward by promoting the saved
   pending pointer and setting SERVE true again, then recording the new
   version with `--current` as in step 5. A retained new-account rollback
   needs only its pointer restored, not an account switch, then `--current`
   for the version it restored.

7. **Prune each account explicitly.** After the new version serves, run
   the normal prune against the new account, a dry run and then the delete.
   `kept_absent` for the lake rollback there is expected. For the lake,
   override the account on **one execution**, not the job's saved template
   (the identity retains its lake grant). This preserves serving and rollback
   indexes in whichever account holds them:

   ```sh
   RG=$(scripts/azure/settings.sh --env prod --key AZURE_RESOURCE_GROUP)
   JOB=$(scripts/azure/settings.sh --env prod --key INGEST_JOB)
   LAKE=$(scripts/azure/settings.sh --env prod --key STORAGE_ACCOUNT)
   template=$(az containerapp job show -n "$JOB" -g "$RG" --query properties.template -o json)
   DELETE=false  # dry run first; inspect logs, then repeat with true
   body=$(jq -c --arg lake "$LAKE" --argjson delete "$DELETE" '
     def exec: {name, image, command, args, env, resources} | with_entries(select(.value != null));
     {containers: [.containers[0] |
       .args = (["prune", "--quickwit-bin", "/usr/local/bin/quickwit",
         "--quickwit-metastore", "azure://qw-index", "--quickwit-index-root", "azure://qw-index"]
         + (if $delete then ["--delete"] else [] end)) |
       .env |= map(if .name == "QW_AZURE_STORAGE_ACCOUNT" then .value = $lake else . end) | exec],
      initContainers: []}' <<< "$template")
   az rest --method post --url "https://management.azure.com/subscriptions/$(az account show --query id -o tsv)/resourceGroups/$RG/providers/Microsoft.App/jobs/$JOB/start?api-version=2025-01-01" --body "$body"
   scripts/azure/logs.sh --env prod --query-name prune --span 2h
   ```

   Wait for success and inspect the dry run before deleting. Once another
   verified new-account release is serving, its rollback is also in the new
   account: repeat lake prune to remove the former lake rollback. Never
   delete split blobs or copy just the metastore between accounts. Only now
   set `USNM_LAKE_BLOB_SOFT_DELETE_DAYS=14` and provision. Blobs already
   deleted keep their original retention; old versions drain under the lake
   lifecycle rules. The lake's empty `qw-index` container and grants may
   remain; retiring them is separate cleanup. With a standalone legacy
   searcher, `USNM_SEARCHER_APP=false` can now retire that app while SERVE
   remains true (the selected searcher stays the new-account app). Keep it
   until cross-account rollback is no longer needed.

## Prune old index versions

Releases never delete an index, so `qw-index` keeps every version ever built and every rebuild that stopped part way (in October 2026, about 1.2 TB of the 2.8 TB of current index blobs; #245). `usnm-ingest prune` deletes the Quickwit indexes that no kept version names. It keeps:

- the serving version: every index `current.json` lists, its Japanese (`ja`) and Congress (`congress`) index sets included;
- its rollback, the version `current.json` names as `previous_version`;
- any published version newer than the serving one (there is one only after `current.json` was rolled back by hand, so rolling forward still works). A newer version whose snapshot has a `manifest.json` counts as published even if its run says `building`: a release writes the manifest, then `current.json`, then records the publish, and that last step can fail.

A kept version's indexes are all that its records name: `current.json`, its run in the pipeline state, the `indexes` of `reference/<version>/manifest.json`, and its Japanese and Congress index ids (`pages-ja-{date}-{n}`, `congress-{date}-{n}`). Every other `pages-…` or `congress-…` index is deleted: older versions, and rebuilds that were stopped or failed. Indexes of other names (such as Quickwit's own `otel-…`) are left alone. It deletes through Quickwit (`DELETE /api/v1/indexes/{id}`), which removes the split files and the index's metastore entry together, never with blob deletes.

Without `--delete`, it's a dry run: it lists each index it would delete with its splits, documents and size (its published splits' files), and the total, and changes nothing. It takes the writer lock like a release, so it fails with ``lock `quickwit-writer` is held by …`` while a release runs, and a release can't take the lock while prune holds it. It refuses to run when `current.json` can't be read or names no indexes, when the serving version has no index run, or when neither the rollback's run nor its manifest names its indexes (and when a manifest's `indexes` isn't a list of `index_id`s). With `--delete`, it reads `current.json` and the runs again before each delete and stops if what it keeps has changed. If the writer lock can't be renewed, it gives up the delete under way at once and stops its Quickwit node. So `--delete` needs `--quickwit-bin`, a node prune starts itself; `--quickwit-url` (a node already running, which Quickwit lets finish a delete its caller left) is for dry runs only.

Don't roll `current.json` back by hand while prune runs: a rollback doesn't take the writer lock, and prune checks what it keeps only before each delete, so a version made serving during a delete of its index loses it. Run it in the ingest job, as an execution with other arguments (`scripts/azure/start-job.sh`), after a new version is published and checked, and while no ingest or backfill execution is running. An execution can't mount the scratch share, and prune doesn't need it: its Quickwit node only reads the metastore and deletes, with its data in a temporary directory. You need `az` signed in with access that can start the job (the owner's Contributor on the resource group) and read the logs. Dry run first:

```sh
RG=$(scripts/azure/settings.sh --env prod --key AZURE_RESOURCE_GROUP)
JOB=$(scripts/azure/settings.sh --env prod --key INGEST_JOB)
QW=(--quickwit-bin /usr/local/bin/quickwit --quickwit-metastore azure://qw-index --quickwit-index-root azure://qw-index)
scripts/azure/start-job.sh --env prod --job INGEST_JOB -- prune "${QW[@]}"   # dry run: lists, deletes nothing
az containerapp job execution list -n "$JOB" -g "$RG" -o table  # until it has Succeeded
scripts/azure/logs.sh --env prod --query-name prune --span 2h
```

`prune keeps` names the serving and rollback versions and every index they keep; check that the serving version is the one `/v1/status` reports and that the rollback is the one you want. Then one `prune candidate` line per index it would delete, with `bytes` and `gib`, and `prune plan` with the count and `total_gib` (the execution's console log ends with the whole report as JSON). If the list is what you expect, delete:

```sh
scripts/azure/start-job.sh --env prod --job INGEST_JOB -- prune "${QW[@]}" --delete
scripts/azure/logs.sh --env prod --query-name prune --span 2h    # a "deleted index" line for each, then "pruned"
```

A large index can take minutes to delete: Quickwit deletes each split file before it answers. A failed delete stops the run with the error; the indexes already deleted stay deleted, and running it again carries on. Afterwards the rollback is still there, but the versions before it are gone and can't be rolled back to. In the separate index account deletion is immediate, with no retained blob versions or soft-deleted splits. In the legacy lake account the bill drops later than the delete: the deleted split files become previous blob versions, which the `qw-index/` lifecycle rule deletes after a day, and then sit in the 3-day soft delete (08 §8.1).
