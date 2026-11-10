# Full rebuilds and index storage

Part of the [operations guide](README.md).

## Full rebuild

Prod has no Azure ingest job (`USNM_INGEST_JOBS` is `false`), so prod rebuilds run [on-prem](#full-rebuild-on-prem). The ingest-job commands in this document (`--env <env>`) apply only to an environment where `USNM_INGEST_JOBS` is `true`; the settings and checks apply to both paths. For the #283 account move, follow [Index storage cutover](#index-storage-cutover) before starting, so the new account stages its pointer; otherwise an ingest-job full rebuild publishes immediately (the on-prem release always stages).

A full rebuild builds a new merged base from every curated batch and publishes a version with no deltas; the previous version stays in Blob for rollback (`current.json`). It is the only way to move published titles' places, merged places included (04 §4.6), or to move a version to the latest analyzer (05 §5.5.3): deltas fold words as the published indexes do, so a change such as `½` as one word (#168) needs a rebuild. Afterwards `current.json` and `build.features` in `reference/<version>/manifest.json` have the latest `common_grams` and `ja.fold` (2 since #168), and the API parses queries for them. A release forces a full base itself, without `USNM_INGEST_FULL`, when the published version used another search backend, lacks common-word pairs or has a newer version of them than the release knows, or lacks American Stories' text while `USNM_AMERICAN_STORIES` is on ([American Stories' text](american-stories.md#american-stories-text)). Indexes built before the merge settings (September 2026) keep their many small splits: they are past any maturation period, so nothing merges them in place.

It takes the writer lock (08 §8.4.1). Run it after the backfill finishes, with no backfill or ingest execution running:

```sh
RG=$(scripts/azure/settings.sh --env <env> --key AZURE_RESOURCE_GROUP)
JOB=$(scripts/azure/settings.sh --env <env> --key INGEST_JOB)
scripts/azure/settings.sh --env <env> --key USNM_INGEST_FULL --value true
scripts/azure/settings.sh --env <env> --key USNM_MERGE_TIMEOUT_SECS --value 14400   # the default; set it only to change it
scripts/azure/settings.sh --env <env> --key USNM_DEDICATED_PROFILE --value true     # the E4 profile (#172)
scripts/azure/settings.sh --env <env> --key USNM_INGEST_ON_DEDICATED --value true   # and the ingest job on it
scripts/azure/settings.sh --env <env> --key USNM_INGEST_SCRATCH_GIB --value 128     # the writer's scratch share (08 §8.4)
scripts/azure/provision.sh --env <env>
curl -s https://api.usnewsmap.com/v1/status | jq .titles.pipeline   # awaiting_sync: titles to fetch first (the environment's API)
az containerapp job start -n "$JOB" -g "$RG"
```

**Replica timeout (#233, #283).** 96 h for explicit full rebuilds (`USNM_INGEST_FULL=true`) and index-account staging runs, 48 h for ordinary ingest, 24 h for backfill. The 1% dev candidate (American Stories, one text field, decade partitions, 60,000-page splits, page languages) sent 113.79 documents/s: about 58 h of sending for 23,766,177 pages, about 82 h with 8 h of preparation (the curation and title deadlines count from the same start), 4 h of merges and 20% send variation. NFS I/O and OCR overlays weren't timed, so 96 h leaves margin; it doesn't guarantee completion. Check the job's `replicaTimeout` first and keep automatic replica retries at zero: a release doesn't resume after a timeout, and platform maintenance can still interrupt it.

A full run releases only once titles-sync has fetched every title the listing and curated batches name (titles LoC lacks excepted; failed fetches count as left), since a base built without them would leave their batches out. At about 4.5 s a title plus 65 minutes per LoC block, the first execution may spend its 8 hours on titles-sync and fail with `titles-sync reached its deadline with N of M titles left … nothing was released` (or `LoC rate limited titles-sync …`). Start the job again; it continues, then builds the base. The job-failed alert ignores this stop; the severity 3 _ingest not progressing_ alert fires if no execution follows within 3 hours or 3 stops in a row leave as many titles as the first. `scripts/azure/logs.sh --env <env> --query-name ingest-endings` lists endings (`outcome: titles_left` on this one's `command failed` line). Watch the run:

```sh
scripts/azure/logs.sh --env <env> --query-name release-progress --span 6h    # docs sent, rate, memory (quickwit_rss_mb), merges
az containerapp job execution list -n "$JOB" -g "$RG" -o table
```

- `title records` every 100 titles; the final report has `left: 0` and no `throttled` or `out_of_time`.
- `release progress` every 30 s: 230 to 460 pages/s with common-word pairs, which make each page's index about 1.6× larger (#154, #172). `quickwit_rss_mb` stays under about 5,500 on Consumption (7,680 MiB container) or 20,000 on E4 (25 GiB).
- `release merges`: step `settle`, then `finalize`; `splits` falls by 9 every minute or two after ingest ends. About 2.7 h with 60,000-page splits and the pairs (#156), at most `USNM_MERGE_TIMEOUT_SECS`. American Stories' text about doubles the index (#218 step 4).
- `merged; the index is closed to further writes`, `index layout` (about 800 splits for 23.8M pages at the 30,000 target), `released`.

**Layout settings.** The four below apply only to a full base; add them to the first provision. Deltas follow the published version's layout, so changing one has no effect until the next full rebuild.

**Decade layout (#123, 05 §5.5.5)** keeps each decade in its own splits, so date-limited searches skip the others.

```sh
scripts/azure/settings.sh --env <env> --key USNM_PARTITION_DECADE --value true   # the job's --partition-decade
```

The release logs `decade layout`; `sent a decade's pages` each time a decade's pages go to the writer (at most every 30,000 pages of a decade, or the `USNM_SPLIT_DOCS` target; until then they wait in `/scratch/usnm/quickwit/decade-spill`, a few GB at most at 30,000); `sent the pages a decade at a time` with totals; and `splits_by_decade` in `index layout`. Check `decades: 1` in `current.json`, and `build.features.decades` and `splits_by_decade` under `indexes` in the manifest. To stop decade searches without a rebuild, publish `current.json` without the key.

**One searched text field (#283, #925, 05 §5.5.7)** puts LoC's and American Stories' text in one field (`text_all`), so each word is looked up once. Set it with `USNM_AMERICAN_STORIES`.

```sh
scripts/azure/settings.sh --env <env> --key USNM_SINGLE_TEXT_FIELD --value true   # the job's --single-text-field
```

The base gets `text_layout: 3`. The release logs `one searched text field` and `building index` with `text_layout: 3`. Check `text_layout: 3` in `current.json` and `/v1/meta`; `build.features.text_layout` and the `text_all`, `text_all_cg`, `text_loc` fields in `build.templates.pages`. The aggregate drops `total.american_stories_only`; hits keep `matched_in` and the American Stories badge; `USNM_AMERICAN_STORIES_SEARCH` has no effect ([American Stories](american-stories.md)). To revert, publish a field-per-text version's `current.json`, or rebuild without the setting. Turning `USNM_AMERICAN_STORIES` off forces a full base, since a delta can't leave the text out of the field it shares with LoC's. With `USNM_OCR_LOCAL` ([Our OCR](../design/12-onprem-ocr-pipeline.md#step-9-release-reads-ocr-local-provenance-file-l)), pages rule v1 replaced (about 19.8 thousand of 23.7 million in `pages-v20261022-1`, #925; not Japanese-OCR Latin pages) also get LoC's text in `text_all` after ours and American Stories', 32 positions apart, stored in `text_loc`. Pages count once; `matched_in` says `loc_original` (06 §6.3.4). The serving API must know layout 3 before publish, or it refuses the version and keeps the previous one. A version published in layout 2 keeps layout 2 for its deltas, so the third text appears only with the next full rebuild.

**Page languages (#289, 05 §5.5.8)** gives each page its own languages, found from its words, for `lang=`.

```sh
scripts/azure/settings.sh --env <env> --key USNM_PAGE_LANGUAGE --value true   # the job's --page-language
```

The release logs `building index` with `page_language: true`, then `pages whose own languages differ from their title's, in the version` (expect about 1.5 to 2 million). Check `page_language: 1` in `current.json`; in the manifest `build.features.page_language`, the two fields in `build.templates.pages`, `page_languages.pages`, and `page_language_baselines.json` (perhaps 10% to 20% more entries than `language_baselines.json`) and `page_languages.json` (about 15 to 20 MB); `/v1/meta` says `"language_filter": "page"`. Detection uses up to 4 threads beside the writer; compare pages/s with the last rebuild. To revert, publish a version without page languages, or rebuild without the setting. Don't edit the key out of the serving `current.json`: the API keeps its loaded filter, its caches are keyed by version, and it logs `current.json changed page_language without a new version`.

**Larger splits (#283)** mean fewer splits per cold search.

```sh
scripts/azure/settings.sh --env <env> --key USNM_SPLIT_DOCS --value 120000   # the job's --split-docs; the value comes from the dev measurement below
```

The release logs `split target` and `building index` with `split_docs`, and refuses more than `docs / target + 5 + 2` splits. Check `index layout` (about pages ÷ target splits, plus up to 7 per decade with `USNM_PARTITION_DECADE`; `largest_split_docs` near the target) and `build.features.split_docs` and `split_num_docs_target` in the manifest. `current.json` doesn't change and the API needs nothing: it searches whatever splits exist. Limits (05 §5.5.1, 08 §8.4): an uploading split holds min(its size, 10 GB) of writer memory; merged splits can reach about twice the target, at about 74 KB a page with American Stories' text; the 128 GiB scratch holds the largest merge (about 18 GB at 120,000) plus the decade spill (under one target per populated decade). Count decades from the corpus's actual bounds, not the 1% sample: 1736–1963 spans 24, so 1.44 million held pages at 60,000. The sample's compressed bytes per page is an estimate, not a bound for the full corpus, so watch free scratch. A target above 30,000 needs E4. Unset it afterwards: a forced full base runs on Consumption (7.5 GiB), which fits only 30,000.

**Settings checklist for the next rebuild (#283).** Set all before the first provision; the release reads them only when it builds the base. "Item" numbers refer to the list in [#283](https://github.com/tgoodyear/usnewsmap.com/issues/283).

| Setting                                                        | For the rebuild                                                      | What it does                                                                                                                                     | Check afterwards                                                                                                                                  |
| -------------------------------------------------------------- | -------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| `USNM_INDEX_STORAGE`                                           | `true`; **keep on permanently**                                      | Private index account and read-only searcher; full rebuilds stage until cutover                                                                  | `INDEX_STORAGE_ACCOUNT`; `reference/index-storage-pending.json`; old `current.json` unchanged                                                     |
| `USNM_INDEX_STORAGE_SERVE`                                     | `false` during rebuild, `true` after pointer promotion               | API uses the new-account searcher; releases publish normally                                                                                     | New API revision ready, `/v1/meta` names staged version                                                                                           |
| `USNM_LAKE_BLOB_SOFT_DELETE_DAYS`                              | `14` in prod since 5 Nov 2026 (#308), after the old index was pruned | Lake recovery window without retaining future deleted splits                                                                                     | Lake blob retention; new account has no soft delete or versioning                                                                                 |
| `USNM_INGEST_FULL`                                             | `true`, cleared after                                                | Every run builds a full base (`--full`)                                                                                                          | `released` with `full: true`; one main index in `current.json`                                                                                    |
| `USNM_DEDICATED_PROFILE`, `USNM_INGEST_ON_DEDICATED`           | `true`, cleared after in two provisions                              | E4 profile: 25 GiB, writer tuning (`USNM_WRITER_*` from Bicep)                                                                                   | `writer tuning` (`heap: 6GiB`, `commit_timeout_secs: 120`, `queue: 4GiB`); `quickwit_rss_mb` under 20,000                                         |
| `USNM_AMERICAN_STORIES`                                        | `true` (as now)                                                      | American Stories' text (04 §4.9); item 1 needs it                                                                                                | `american_stories: 1` in `current.json`; release's `American Stories` lines ([American Stories' text](american-stories.md#american-stories-text)) |
| `USNM_SINGLE_TEXT_FIELD`                                       | `true`                                                               | `text_layout: 3` (item 1, #285, #925)                                                                                                            | [One searched text field](#full-rebuild)                                                                                                          |
| `USNM_SPLIT_DOCS`                                              | Dev-measured target; empty (30,000) until then, cleared after        | Pages per split (item 2)                                                                                                                         | [Larger splits](#full-rebuild)                                                                                                                    |
| `USNM_PARTITION_DECADE`                                        | `true`                                                               | Decade partitions (item 3, #123)                                                                                                                 | `decades: 1` in `current.json`; `splits_by_decade` in the manifest                                                                                |
| (none)                                                         | Automatic                                                            | Analyzer version 2 (item 4, #168, #278; `½` one word, #287's characters fold as themselves)                                                      | `common_grams: 2`, `ja.fold: 2` in `current.json`; `common_grams: 2`, `ja_fold: 2` in `build.features`                                            |
| `USNM_PAGE_LANGUAGE`                                           | `true` once measured on dev                                          | Page-level language for `lang=` and language counts (item 5, #289)                                                                               | [Page languages](#full-rebuild); `page_language` and `page_language_set` fields                                                                   |
| `USNM_OCR_LOCAL`                                               | `true`                                                               | Our OCR where rule v1 prefers it, on every batch the base reindexes (item 6, #296, [Our OCR in releases](onprem-ocr.md#our-ocr-in-releases-296)) | `our OCR in ocr-local`; `reference/<version>/ocr_local.json`; `build.features.ocr_local: 1`                                                       |
| `USNM_JA_LATIN`                                                | `true` if published version has it (as now)                          | Our Latin text on pages LoC ships without text (#203); a full base without it drops them                                                         | `build.features.ja_latin: 1`; `reference/<version>/ja_latin.json` page count unchanged                                                            |
| `USNM_CONGRESS`                                                | `true` if published version has a Congress set (none yet)            | Congress pages (#229); without it a release drops them (even with no new batch); a new analyzer rebuilds them                                    | `build.features.congress: 1`; `congress` in `current.json`                                                                                        |
| (none)                                                         | Automatic                                                            | Catalog on `main` (`catalog/overrides/title-places.json`), incl. #281's 17 title moves (item 7)                                                  | `/v1/titles/{lccn}` of a moved title (such as the Anti-Slavery Bugle) names its new place                                                         |
| `USNM_MERGE_TIMEOUT_SECS`                                      | `14400` unless dev shows longer merges                               | Release's merge wait                                                                                                                             | `merges settled` well inside it                                                                                                                   |
| `USNM_WRITER_INGEST`                                           | `v1`                                                                 | [Duplicate-safe ingest](#duplicate-safe-ingest)                                                                                                  | `writer ingest api` `v1`; `restarting the writer for the final merges`, then the base's `index layout`; page count equals pages sent              |
| `USNM_INGEST_SCRATCH_GIB`                                      | `128`, cleared after                                                 | Writer scratch share, its account and private endpoint                                                                                           | Free disk in `release merges` well above the largest merge (about twice the split target's pages), plus spill `peak_disk_mb`                      |
| `USNM_AMERICAN_STORIES_SEARCH`                                 | `true` (as now)                                                      | No effect on `text_layout` 2 or 3                                                                                                                | No `USNM_AMERICAN_STORIES_SEARCH=false has no effect` warning in the API's log                                                                    |
| `split_footer_cache_capacity` (`infra/quickwit/searcher.yaml`) | Resize after the release if needed                                   | Must hold every footer ([Searcher caches](search-serving.md#searcher-caches))                                                                    | Version's `footer_bytes` vs capacity; footer misses after warm-up                                                                                 |

Afterwards benchmark with `scripts/ops/bench-cold-searches.py` against `pages-v20261008-1` (1,162 GB; median search 21.4 s with both texts' fields searched, #251) and record size, send rate and total time on #283.

**Before the go-ahead: what to measure on dev (#275).** The rebuild waits for these and the maintainer's go. Use dev with a 1% version (stand-up flow in PR #351) and American Stories' text:

- **Items 1 and 2:** full releases with `USNM_SINGLE_TEXT_FIELD`, `USNM_PARTITION_DECADE` and several `USNM_SPLIT_DOCS` values against one without: index size, pages/s, merge time, peak `quickwit_rss_mb` while the largest split uploads, spill `peak_disk_mb` against the largest merge's scratch, cold search times. The search-cluster loader (`load --split-docs`, [Search cluster experiment](environments-and-deploys.md#search-cluster-experiment)) can measure the split target but not the one-field layout. Pick `USNM_SPLIT_DOCS` from the memory peak and cold-search gain.
- **Time and profile (#233):** the last rebuild took 35.8 h on E4. Confirm E4 (Consumption can't take a larger target) from the dev rate scaled to the corpus, with preparation, merge and throughput-variance margin, against the replica timeout. Whether two writers help is still open (#233); the release has one writer.
- **Item 5 (#289):** with and without `USNM_PAGE_LANGUAGE`: pages/s, differing-page count, sizes of `page_language_baselines.json` and `page_languages.json`, cold `/v1/aggregate` times (05 §5.5.8) with `lang=` on the complement side and with `USNM_PAGE_LANGUAGE_FILTER=match`.

Once the execution has ended (`az containerapp job execution list`), turn the rebuild, E4 and scratch off in two provisions: the job must leave the profile before the profile can go, and the first provision takes the share away from the job.

```sh
scripts/azure/settings.sh --env <env> --key USNM_INGEST_FULL --unset
scripts/azure/settings.sh --env <env> --key USNM_SPLIT_DOCS --unset   # if it was set: a forced full base on Consumption takes only the template's target
scripts/azure/settings.sh --env <env> --key USNM_INGEST_ON_DEDICATED --unset
scripts/azure/settings.sh --env <env> --key USNM_INGEST_SCRATCH_GIB --unset    # deletes the share, its account, endpoint and DNS zone
scripts/azure/provision.sh --env <env>
scripts/azure/settings.sh --env <env> --key USNM_DEDICATED_PROFILE --unset
scripts/azure/provision.sh --env <env>
```

Don't leave these set. `USNM_INGEST_FULL` rebuilds the corpus on every scheduled run. Scratch costs about $25 a month (#308); deleting it loses nothing, and the next rebuild's provision creates it again, empty, under the same names. The E4 profile costs about $0.10 an hour in management fees even when idle, plus about $0.39 an hour per running node (4 vCPU, 32 GiB; East US 2, October 2026).

**WAL on local disk.** Mounted jobs set `USNM_WRITER_WAL_DIR=/work` (ephemeral disk, not `/scratch`). Each writer creates a fresh private directory there and links `qwdata/wal` (v2 WAL) and `qwdata/queues` (v1 queue) to it, which keeps the WAL's latency-sensitive persist lock off NFS. Startup needs 8 GiB free for the bounded 4 GiB WAL plus headroom; `writer WAL on local disk` logs path and free MiB; `wal_disk_free_mb` tracks it beside NFS `disk_free_mb`. The directory lives until the writer stops, through a v1 release's final-merge restart, and isn't replayed. Split cache, merge files and decade spill stay on NFS; jobs without scratch keep the all-local layout. Neither layout resumes an interrupted rebuild: the next run clears its writer data before starting. The first #283 attempt failed at 43.9% when a persist held that lock for 45.51 s against Quickwit 0.9.1's 6 s timeout and returned HTTP 500; don't retry such a 500 blindly, since it can duplicate documents. A failed staged run publishes nothing; keep the serving version and rollback intact while fixing and restarting it.

### Duplicate-safe ingest

Quickwit 0.9.1's ingest v2 is at least once: a persist that timed out (6 s, up to 5 tries; `PERSIST_REQUEST_TIMEOUT`, `MAX_PERSIST_ATTEMPTS`, not settable) still appends later. Quickwit doesn't deduplicate by `doc_id`, and a delete by `doc_id` removes every copy (and skips splits under 7 days old), so such a base can't be repaired. The second #283 attempt (`si5kesp`, October 2026) held 121 extra copies and the exact count refused it.

`USNM_WRITER_INGEST=v1` uses the v1 API (`use_legacy_ingest=true`): one append per request, no internal retry, answering `num_docs_for_processing`. The sink resends only after a 429 (queue full) or 404 (queue not created yet), both before any append; it fails on any other answer, a transport error or a timeout, and requires `num_docs_for_processing` to equal the pages sent (v1 drops refused pages silently). v1 reads the merge policy only at start, so `seal_restarting`:

1. Lets merges settle.
2. Sets the index config to `merge_factor: 2`.
3. Restarts the writer (same directory and port): SIGTERM, SIGKILL after 60 s, new writer only after the old one is reaped; fails if that isn't confirmed 30 s after SIGKILL.
4. Waits for `spawning merge pipeline` with `merge_factor: 2`; fails without publishing otherwise or if no pipeline starts within the finalize stall.
5. Settles again, restores the config, checks the layout.

On a failure or timeout after step 2 the release restores the config (a cancelled one tries from a background task). If the error says the index "keeps merge_factor 2 in the metastore", it is never published: prune it, or PUT its `GET /api/v1/indexes/<id>` `index_config` back with the original `merge_factor`. The page count and layout check still refuse an index whose documents differ from the pages sent. v1 needs `--quickwit-bin` and is refused with `--quickwit-url`. `USNM_WRITER_INGEST` is public (`infra/env/<env>.json`) and passed to the ingest job, default `v2`; set `v1` for full rebuilds after a 1% dev comparison of pages/s.

**Why E4.** On Consumption (7.5 GiB with the scratch share's init container) the writer ran out of memory 6.5 h into a rebuild (#172). E4 gives 3.0 vCPU, 25 GiB, a 6 GiB heap, 120 s commit timeout and 4 GiB queue (`USNM_WRITER_HEAP`, `USNM_WRITER_COMMIT_SECS`, `USNM_WRITER_QUEUE`, from `infra/modules/ingestjobs.bicep`), so splits reach the 30,000 target instead of being cut small every 30 s. The queue's WAL truncates only on commit; at 600 s it filled and Quickwit refused pages with "no shards available". At 120 s it holds about 2 minutes of pages.

### Full rebuild on-prem

The on-prem release container `usnm-release` (#283, #917; home-infra `scripts/lab/release-ct.sh`, on Arc as `usnm-release-arcs-<env>` via `release-ct-arc.sh`) builds a staged version on local ZFS (`/srv/usnm`, not NFS), and `usnm-ingest publish-staged` copies it to the index account and lake. No Azure compute runs; E4, scratch and the 96 h timeout don't apply. Use the [checklist](#full-rebuild); [Index storage cutover](#index-storage-cutover) applies from step 3.

Its Arc identity (`USNM_ONPREM_RELEASE`, `USNM_ONPREM_RELEASE_HOST`, `modules/onprem-release.bicep`) gets read on `curated`; read on `reference` with writes only under `pages-v*/` and to `index-storage-pending.json`; write without delete on the index account's `qw-index`; Cosmos DB Data Contributor on `index_runs` only (which can also delete its items); AcrPull for the pinned Quickwit image. The lake and index account open to `USNM_OCR_NODE_IPS`; Cosmos needs `USNM_COSMOS_ONPREM_ACCESS` (on in prod) or `USNM_ONPREM_SERVING`, or the record gets 403. Provision before the first release. Turning it off removes the grants and closes the index account; `modules/release-writer-role.bicep` stays while `USNM_ONPREM_RELEASE_HOST` is set (the stack can't delete a custom role with its own deny assignment). The Cosmos writer lock isn't taken, so keep ingest and prune jobs and schedules off.

As `usnm` in the container, use one run directory per release, and start the long step as a unit so it outlives the session:

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
scripts/onprem/release.sh --run-dir "$R" --step publish \
  --cosmos-endpoint "$(scripts/azure/settings.sh --env prod --key COSMOS_ENDPOINT)"
```

- `seed` uses the serving version's batches, not Cosmos's list, so newer curated batches wait. `--sample 1pct` seeds 30 batches (every 100th by name from the 40th, 238,082 pages) for a rehearsal that stops before `publish`.
- `release` refuses a directory that already holds a staged version; rerun a failed one in a new directory.
- `publish` refuses unless the lake still serves the release's rollback (mirror and build again otherwise), every split is published, and no same-name object in Azure differs; reruns resume. Use [`restore`](#restoring-serving-to-azure) for a version the lake already serves.
- `publish` records the run in Cosmos `index_runs` (ADR-0027 settled decision 5) with `usnm-ingest import-staged-run`, from the snapshot's `index_run.json`, so the version is listed before promotion. It needs `--cosmos-endpoint` or `USNM_COSMOS_ENDPOINT` (the `COSMOS_ENDPOINT` setting, `https://<account>.documents.azure.com/`) and checks it and the staged pointer before copying; a dry run doesn't. It doesn't set `ops/current` (no `ops` role; cutover step 5 does). If only the record failed (Cosmos firewall still closed, role not provisioned), fix it and rerun `--step publish`.

After promotion `publish` refuses, so record a missing run from the batch container (`USNM_INGEST_ONPREM` grants it `index_runs` and `ops`), as root on its host:

```sh
pct exec 215 -- /usr/local/bin/usnm-ingest-run --command import-staged-run --wait -- --version pages-v<date>-<n>
```

The import is idempotent: it creates a missing run, keeps an identical one, replaces a failed same-name attempt with no snapshot, and stops on a different published one. Prune then keeps the version as a newer published run, so rollback can roll forward. A Cosmos-backed release or prune also imports a run `current.json` names but lacks. Check `/v1/versions` lists it (`serving` false until the switch) and `/v1/status` (on `/datasets`) has it under `indexing.runs`; the API rereads `index_runs` about once a minute.

**Through Kafka.** One writer's v1 API takes about 130 pages/s. With `USNM_WRITER_KAFKA=127.0.0.1:9092` and `USNM_WRITER_INGEST=v1` (required), a decade-partitioned main index's decade files go round-robin to topic `usnm-<index id>` with `USNM_WRITER_KAFKA_PARTITIONS` partitions (default 8), keeping splits one decade each. The index gets a Kafka source with one pipeline per partition, and the writer node the indexer capacity for them; on the on-prem release host 1, 2 and 4 pipelines gave 341, 509 and 754 pages/s. The last pages and forced commit, the Japanese and Congress indexes, the exact count and the restart-based final merges stay on v1. The source checkpoints in the metastore, so restarts don't duplicate. A produce with a lost answer is resolved from the partition's end (written: go on; not written: resend; neither: fail). Before producing, at most every 10 s, the release reads the index's Kafka checkpoint:

- Oldest held record past the index's position (expired unread): fail.
- More than `USNM_WRITER_KAFKA_MAX_LAG` behind (default 1,000,000, about half an hour of sending): wait.
- No progress for `USNM_WRITER_KAFKA_STALL_SECS` (default 900, at least commit timeout plus 600) with records waiting: restart the writer, and the source resumes from its checkpoint; allow the session timeout plus 2 minutes to progress. At most 3 restarts, each only after progress since the one before, then fail. A checkpoint read failing or over 30 s counts as no progress.
- After the last file, wait the same way until every record is indexed, then count.

Consumers use a 300 s session timeout (librdkafka's 45 s default broke #283 run e) and a 900 s max poll interval. Once the count matches, the release removes the source and topic. The broker is single-node KRaft Kafka on loopback in the container (home-infra `scripts/lab/release-ct-kafka.sh`), data on the release dataset, 3-hour retention, 256 MB segments; falling 3 hours behind fails the count. A release refuses an existing topic: delete it (`kafka-topics.sh --delete`) before reusing the version name.

**Threads and copy.** Curated parts download 8 at a time; documents are built on `USNM_RELEASE_THREADS` threads (default: CPUs), 256 rows at a time, in row order, so output matches a one-thread build. `batch timing` gives seconds per step (`fetch`, `build`, `lang`, `add`, `queue_wait`). Azure requests retry transient failures (dropped connection, 408, 429, 5xx) 12 times over about 5 minutes; a Put Block times out after 30 s plus 8 MiB at 64 KiB/s. A failed split is resent up to 3 more times, 5 minutes apart, sending only missing blocks. A 403 or differing object stops it at once. Watch `index copy progress`, `split copied`, `store request failed; trying again`, `split upload failed`. Resize live with `release-ct.sh --resize`. `release merges` (every 30 s) shows `immature_splits`, `immature_mb`, `pending_merge_mb`, `merge_write_mb_per_sec`, and over the last 30 minutes `immature_mb_per_hour` and `eta_mins`; the wait is `USNM_MERGE_TIMEOUT_SECS` (default 4 hours).

#### Finishing a release

When a release stops after its main index passed the exact count (usually the merge wait ran out), `scripts/onprem/release.sh --step finish --index-version pages-v<date>-<n>` (`release-finish`) completes it in the same run directory. Use the release's settings (`--split-docs`, Kafka) and its `release.log`, logging the finish elsewhere, with exactly one of `--stage-index-storage` (the step passes it) or `--publish-serving`. It takes the writer lock and refuses unless the run stopped before writing its snapshot, its log has the merge-wait lines, the catalog and curated batch records are unchanged since the run started, and published splits hold the documents last logged as sent (that 30 s sample is a floor).

It replans and requires the same run record (names, batches, pages, rollback, features, templates, writer tuning, Quickwit) and that each title's documents carry the catalog's place, place shard, state, languages and ordinal. For a title with places by date, every document's `day` must fall in a period of its place, so a catalog that gained a period or moved a boundary past a built page is refused. Curated listings are read as of the run's start (`--listed-before` overrides), so our OCR's later parts don't count. It restarts the writer on the run's data, marks leftover staged splits for deletion, and resets a leftover `merge_factor` 2. It rebuilds every document without sending it, from the lake, and requires each batch's `our OCR for the batch's titles` and `American Stories' text for the batch` lines, planning lines, totals and per-decade counts to match. It then waits for merges (`USNM_FINISH_MERGE_TIMEOUT_SECS`, default 24 hours), removes any Kafka source and topic, and writes our OCR's files, the Japanese index, the layout, the snapshot, the pending pointer and the run record. It can be rerun; delete any Japanese index from the run first if asked.

#### Restoring serving to Azure

`restore` (#1061) copies a version that serves on-prem into the index account, as in a disaster recovery ([09 §9.6](../design/09-operations-security-cost.md#96-backup-and-disaster-recovery)) after the account and apps are reprovisioned. [ADR-0027](../design/adr/0027-onprem-serving-behind-cloudflare.md) Phase 4 planned to delete them; under [ADR-0029](../design/adr/0029-onprem-primary-azure-failover.md) the index account keeps one current release for the Azure spare. `publish` can't do this, since after promotion `current.json` names the release itself. Run it like `publish`, from the version's run directory (keep its `state.json`, `reference/index-storage-pending.json` and `qw/` on the release container's dataset), with `USNM_ONPREM_RELEASE` and `USNM_INDEX_STORAGE` provisioned.

```sh
scripts/onprem/release.sh --run-dir "$R" --step restore --dry-run
scripts/onprem/release.sh --run-dir "$R" --step restore
scripts/onprem/release.sh --run-dir "$R" --step ready --app-url "$APP_URL" --index-version pages-v<date>-<n>
```

`restore` (`usnm-ingest publish-staged --restore`) makes `publish`'s checks and copy (splits, metastores, manifest, reference snapshot with `index_run.json`), resumable, but refuses unless `current.json` names this release's own version and indexes (if the rollback, use `publish`; if another version, use its run directory). It writes neither `current.json` nor `index-storage-pending.json`: the lake keeps pointing at the version, so the apps load it as they start. `--dry-run` writes nothing. `ready` (only `curl` and `jq`) polls `--app-url` (the `azurecontainerapps.io` URL) until `/readyz` passes and `/v1/meta` names `--index-version`, exiting 1 after `USNM_READY_TRIES` (default 60) tries `USNM_READY_WAIT_SECS` (default 10) apart. Run it before pointing the Worker at the app.

Limits: indexes come only from `DIR/qw` (with a delta mode, ADR-0027 decision 8, all under one `--local-index-root`, or it refuses naming the missing metastore); `current.json`, catalog and other snapshots must still be in Azure (Phase 5's restore point must cover reference); it doesn't write Cosmos.

**Rehearsal.** While the index account holds the serving version, `restore` writes nothing to Azure, so running it with and without `--dry-run`, then `ready`, proves the checks pass and the run directory is whole. A copy into an empty account belongs on dev, or as an operator step with the maintainer's approval.

## Index storage cutover

The #283/#245/#416 move ([ADR-0021](../design/adr/0021-separate-index-storage.md)) is opt-in: merging it changes no setting (`USNM_INDEX_STORAGE` and `USNM_INDEX_STORAGE_SERVE` default `false`). Do it on dev first, and in prod only with the maintainer's go-ahead. Every step is an operator step, not CI. Quickwit 0.9 uses one account per process, so URIs stay `azure://qw-index` and `QW_AZURE_STORAGE_ACCOUNT` picks the account. A second internal searcher (`usnm-qw-search-ca-{env}`) reads the new account with the same YAML (`account:` replaced, `#polling_interval=30s`). The old searcher and its lake roles stay; `qw-cluster` and `qw-bench` stay in the lake.

1. **Prepare.** Wait for ingest, backfill and prune to stop; where the ingest job exists, clear and record `USNM_INGEST_CRON`; save the serving `current.json` and rollback version. Set `USNM_INDEX_STORAGE=true`, `USNM_INDEX_STORAGE_SERVE` false, and the rebuild settings; commit, review, provision. This creates `usnmqwst{env}{suffix}` (endpoint in `snet-pe`, DNS in the Blob private zone), grants and the new searcher, which may restart until the metastore exists. The account is not in `publicAccountIds`, so the existing deny policy covers it.
2. **Build.** With the ingest job (where `USNM_INGEST_JOBS` is true), start it normally (its template adds `--full` and `--stage-index-storage`); on-prem, build and publish with [Full rebuild on-prem](#full-rebuild-on-prem), which stages the same way. Staging ignores the old pointer for index reuse, so main, Japanese and Congress indexes are rebuilt, an unchanged Congress set included. Reference snapshots stay in the lake. The run publishes `reference/index-storage-pending.json`, not `current.json`, leaves `ops/current` alone, and records `published` with `previous_version` set. A retry stages another fresh full base. Keep schedules off, and don't start an ordinary release with hand-written arguments during the move.
3. **Verify.** Check counts, manifest and merges; check every index in the pending JSON via the new searcher's `/api/v1/indexes/{id}` and test searches from an in-VNet runner, after readiness and RBAC propagation. The live site must still show the old version. Defer cleanup.
4. **Promote and switch.** With writers and prune stopped, from a private-network host with Entra Blob Data Contributor on `reference` (never keys or public access), working in the repository:

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

   The old API logs `reload failed; keeping current version` and keeps serving. **Don't restart or roll it, or let CI roll images, until the switch**: a fresh old-account replica can't load the new pointer. If it must restart or scale out, restore the saved pointer. Promote only when the switch commit is ready. Provisioning points `USNM_QUICKWIT_URL` at the new searcher and the new revision warms before taking traffic; the old searcher is left alone. Cold starts between promotion and the switch are still possible.

5. **Check and record.** Verify `/readyz`, `/v1/meta`, counts, main, JA and Congress queries and the version on every replica. Then set `ops/current`, which `/v1/status` (`indexing.current_version`), the seeder, `ocr-enqueue`, `ocr-sync` and the BOINC export read (the export fails with "published snapshot changed during export" if it changes mid-export). First prepare OCR, or `ocr-sync` fails at seeding:

   - **Damage version list** `audit/damage/v2/versions/pages-v<date>-<n>.json` (curated store), written only by `ocr-damage`. The pass is incremental (it skips batch versions already scored); rerun it until `ocr damage finished` shows `left` 0. Run it as the damage writer ([Japanese OCR](japanese-ocr.md), "On-prem"; #1090), as root on the batch container's host:

     ```sh
     pct exec 215 -- /usr/local/bin/usnm-ingest-run --command ocr-damage -- --version pages-v<date>-<n> --max-runtime-secs 165600
     ```

   - **Prefetch manifest**, with `USNM_OCR_SEED_SOURCE=fetched`. Unpinned, the seeder reads the `ops/current` version's manifest and its newest fetched list (`ocr-out/fetched/<version>/`). Build the manifest after the version list and publish a fetched list (`USNM_OCR_PUBLISH_FETCHED`, or `usnm-ocr-worker publish-fetched --manifest pages-v<date>-<n>`):

     ```sh
     pct exec 215 -- /usr/local/bin/usnm-ingest-run --command ocr-prefetch-manifest -- --version pages-v<date>-<n>
     ```

     or pin `USNM_OCR_SEED_MANIFEST` to the version whose manifest and fetched list the seeder reads now, and provision.

   Record between BOINC priority-controller ticks. On the controller host ([the batch scheduler](../boinc/server/batch-scheduler.md#on-prem-controller-host-ct-211)):

   ```sh
   systemctl stop usnm-boinc-priority-controller.timer
   systemctl is-active usnm-boinc-priority-controller.service   # repeat until neither active nor activating
   ```

   As root on the batch container's host:

   ```sh
   pct exec 215 -- /usr/local/bin/usnm-ingest-run --command import-staged-run --wait -- --version pages-v<date>-<n> --current
   ```

   Back on the controller host:

   ```sh
   systemctl start usnm-boinc-priority-controller.timer
   ```

   This runs the batch container's installed release (`/usr/local/lib/usnm-ingest/current`), which must have `--current`. For another commit, install it (home-infra `scripts/lab/batch-ct-ingest.sh --install --tag <sha>`) and pass `--tag <sha>`. CI publishes an image only for commits that change the images, and only for main's head or an older commit when no newer one changes them, so not every commit has one. Check first; this prints the sha when the tag is there:

   ```sh
   ACR=$(scripts/azure/settings.sh --env prod --key ACR_NAME)
   az acr repository show-tags --name "$ACR" --repository usnewsmap-ingest --query "[?@=='<sha>']" -o tsv
   ```

   It refuses unless `current.json` names the version and its run is published, takes the writer lock, and is idempotent. Run it after the switch: before it, `/v1/status` would announce the version early. The next ordinary release would also repair `ops/current` from the live pointer. Check `indexing.current_version` and the next pickup (`journalctl -u usnm-ocr-sync` in the batch container). Where the ingest job exists, restore the weekly schedule; clear rebuild, scratch and E4 settings, **keep both index settings true**, and keep the lake's rollback.

6. **Rollback across accounts.** Stop writers and prune, upload `serving-before.json` as `reference/current.json`, set only `USNM_INDEX_STORAGE_SERVE=false`, provision. Then, with step 5's OCR prerequisites met for that version (it also needs a fetched list for its manifest), record it between ticks with the same image:

   ```sh
   pct exec 215 -- /usr/local/bin/usnm-ingest-run --command import-staged-run --wait -- --version <rollback version> --current
   ```

   Nothing else fixes `ops/current` while SERVE is false (every release stages); left alone, `/v1/status` keeps announcing the new version and the OCR pipeline works on a version the site doesn't serve. Keep schedules off: with SERVE false a release stages another full rebuild rather than sending incremental writes to the lake. **Never turn `USNM_INDEX_STORAGE` off**: the stack deletes the account and data. Roll forward by promoting the saved pending pointer, SERVE true, and `--current`. A new-account rollback needs only the pointer and `--current`.

7. **Prune each account** (ingest job only). Prune the new account normally (dry run, then delete; `kept_absent` for the lake rollback is expected). For the lake, override the account on one execution, not the template (the identity keeps its lake grant):

   ```sh
   RG=$(scripts/azure/settings.sh --env <env> --key AZURE_RESOURCE_GROUP)
   JOB=$(scripts/azure/settings.sh --env <env> --key INGEST_JOB)
   LAKE=$(scripts/azure/settings.sh --env <env> --key STORAGE_ACCOUNT)
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
   scripts/azure/logs.sh --env <env> --query-name prune --span 2h
   ```

   Wait for success and inspect the dry run before deleting. After the next new-account release, prune the lake again to drop the old lake rollback. Never delete split blobs or copy only a metastore between accounts. Only now set `USNM_LAKE_BLOB_SOFT_DELETE_DAYS=14` and provision; blobs already deleted keep their original retention, and old versions drain under the lake lifecycle rules. The empty lake `qw-index` and grants can be retired separately; `USNM_SEARCHER_APP=false` retires a standalone legacy searcher once cross-account rollback isn't needed.

## Prune old index versions

Releases never delete indexes, so `qw-index` accumulates old versions and stopped rebuilds (about 1.2 of 2.8 TB in October 2026; #245). `usnm-ingest prune` deletes the Quickwit indexes no kept version names. It keeps:

- the serving version: every index in `current.json`, with its `ja` and `congress` sets;
- its rollback (`previous_version`);
- any published version newer than the serving one (after a hand rollback). A version with a `manifest.json` counts as published even if its run says `building`, since the publish record is written last.

A kept version's indexes are those named by `current.json`, its pipeline-state run, its manifest's `indexes`, and its `pages-ja-{date}-{n}` and `congress-{date}-{n}` ids. Other `pages-…` and `congress-…` indexes are deleted; other names (such as `otel-…`) are ignored. Deletes go through Quickwit (`DELETE /api/v1/indexes/{id}`), which removes split files and metastore entry together, never raw blob deletes.

Without `--delete` it is a dry run listing each candidate (splits, documents, size) and the total. It takes the writer lock, so it fails with ``lock `quickwit-writer` is held by …`` during a release, and a release can't take the lock while prune holds it. It refuses when `current.json` is unreadable or empty, the serving version has no run, the rollback's indexes aren't named by its run or manifest, or a manifest's `indexes` isn't a list of `index_id`s. With `--delete` it rechecks before each delete and stops if what it keeps has changed, and stops its Quickwit node if the lock can't be renewed, so `--delete` needs `--quickwit-bin`; `--quickwit-url` is for dry runs only.

Prune is the one ingest command not moved on-prem (#917): it deletes in Azure's `qw-index`, where no on-prem identity may delete. It needs the ingest job, which prod no longer has (`USNM_INGEST_JOBS` is `false`), so the commands below apply only where the job exists. In prod the retired versions were removed under [#245](https://github.com/tgoodyear/usnewsmap.com/issues/245) instead, and the index account keeps one current release for the Azure spare ([ADR-0029](../design/adr/0029-onprem-primary-azure-failover.md) decisions 5 and 6).

Don't roll `current.json` back by hand during prune: rollbacks don't take the lock, and prune checks what it keeps only before each delete, so a version made serving during a delete of its index loses it. Run it as an ingest-job execution (`scripts/azure/start-job.sh`) after a release is checked, with no ingest or backfill running; it needs no scratch share. You need `az` access to start the job (the maintainer's Contributor on the resource group) and read logs. Dry run first:

```sh
RG=$(scripts/azure/settings.sh --env <env> --key AZURE_RESOURCE_GROUP)
JOB=$(scripts/azure/settings.sh --env <env> --key INGEST_JOB)
QW=(--quickwit-bin /usr/local/bin/quickwit --quickwit-metastore azure://qw-index --quickwit-index-root azure://qw-index)
scripts/azure/start-job.sh --env <env> --job INGEST_JOB -- prune "${QW[@]}"   # dry run: lists, deletes nothing
az containerapp job execution list -n "$JOB" -g "$RG" -o table  # until it has Succeeded
scripts/azure/logs.sh --env <env> --query-name prune --span 2h
```

Check `prune keeps` names the serving version `/v1/status` reports and the rollback you want, then review the `prune candidate` lines (`bytes`, `gib`) and `prune plan` (count, `total_gib`; the full report ends the console log as JSON). Then delete:

```sh
scripts/azure/start-job.sh --env <env> --job INGEST_JOB -- prune "${QW[@]}" --delete
scripts/azure/logs.sh --env <env> --query-name prune --span 2h    # a "deleted index" line for each, then "pruned"
```

Large indexes take minutes. A failed delete stops the run; rerun to continue. Versions older than the rollback are then gone. The index account deletes immediately; in the legacy lake, deleted splits become blob versions removed after a day by the `qw-index/` lifecycle rule, then sit 3 days in soft delete (08 §8.1).
