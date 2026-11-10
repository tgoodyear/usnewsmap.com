# Full rebuilds and index storage

Part of the [operations guide](README.md).

## Full rebuild

A full rebuild builds a new merged base from every curated batch and publishes a version with no deltas; the previous version stays for rollback (`previous_version` in `current.json`). It is the only way to move published titles' places, merged places included (04 §4.6), to move a version to the latest analyzer (05 §5.5.3; deltas fold words as the published indexes do, so a change such as `½` as one word, #168, needs a rebuild), or to change the layout settings below. Afterwards `current.json` and `build.features` in `reference/<version>/manifest.json` have the latest `common_grams` and `ja.fold`, and the API parses queries for them. A release also forces a full base itself when the published version used another search backend, lacks common-word pairs or has a newer version of them than the release knows, or lacks American Stories' text while `USNM_AMERICAN_STORIES` is on ([American Stories' text](american-stories.md#american-stories-text)).

Prod builds full releases [on-prem](#full-rebuild-on-prem); the latest, `pages-v20261026-1` (23,766,177 pages), was published on 28 October 2026 and was serving on 10 November 2026 (`/v1/versions`), after #283's `pages-v20261022-1` of 24 October. The [ingest-job path](#full-rebuild-on-the-ingest-job) applies only where `USNM_INGEST_JOBS` is `true`. The settings and checks below apply to both.

**Settings a full base reads.** The release reads these when it builds a base; deltas follow the published version's layout, so changing one has no effect until the next full rebuild. `scripts/onprem/release.sh` passes the layout settings itself (`--split-docs` from its command line); the ingest job takes them from `infra/env/<env>.json`. Prod's values are in `infra/env/prod.json`.

| Setting                                                        | Prod                     | What it does                                                                                    | Check afterwards                                                                                                                                      |
| -------------------------------------------------------------- | ------------------------ | ----------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| `USNM_INDEX_STORAGE`, `USNM_INDEX_STORAGE_SERVE`               | `true`, keep both        | The index account ([Index storage cutover](#index-storage-cutover))                             | `INDEX_STORAGE_ACCOUNT`; `reference/index-storage-pending.json` after a staged build                                                                  |
| `USNM_AMERICAN_STORIES`                                        | `true`                   | American Stories' text (04 §4.9)                                                                | `american_stories: 1` in `current.json`; the release's `American Stories` lines ([American Stories' text](american-stories.md#american-stories-text)) |
| `USNM_SINGLE_TEXT_FIELD`                                       | `true`                   | One searched text field, `text_layout: 3` (below)                                               | `text_layout: 3` in `current.json` and `/v1/meta`                                                                                                     |
| `USNM_PARTITION_DECADE`                                        | `true`                   | Decade partitions (below)                                                                       | `decades: 1` in `current.json`; `splits_by_decade` in the manifest                                                                                    |
| `USNM_SPLIT_DOCS`                                              | `60000`                  | Pages per split (below)                                                                         | `build.features.split_docs` and `split_num_docs_target` in the manifest                                                                               |
| `USNM_PAGE_LANGUAGE`                                           | `true`                   | Page-level languages for `lang=` (below)                                                        | `page_language: 1` in `current.json`                                                                                                                  |
| `USNM_OCR_LOCAL`                                               | `true`                   | Our OCR where rule v1 prefers it ([Our OCR in releases](onprem-ocr.md#our-ocr-in-releases-296)) | `our OCR in ocr-local`; `reference/<version>/ocr_local.json`; `build.features.ocr_local: 1`                                                           |
| `USNM_JA_LATIN`                                                | `true`                   | Our Latin text on pages LoC ships without text (#203); a full base without it drops them        | `build.features.ja_latin: 1`; `reference/<version>/ja_latin.json` page count unchanged                                                                |
| `USNM_CONGRESS`                                                | off                      | Congress pages (#229); without it a release drops them                                          | `build.features.congress: 1`; `congress` in `current.json`                                                                                            |
| `USNM_WRITER_INGEST`                                           | `v1` (on-prem always)    | [Duplicate-safe ingest](#duplicate-safe-ingest)                                                 | `writer ingest api` `v1`; page count equals pages sent                                                                                                |
| `USNM_MERGE_TIMEOUT_SECS`                                      | default 14400            | The release's merge wait                                                                        | `merges settled` well inside it                                                                                                                       |
| `split_footer_cache_capacity` (`infra/quickwit/searcher.yaml`) | resize after the release | Must hold every footer ([Searcher caches](search-serving.md#searcher-caches))                   | Version's `footer_bytes` against capacity; footer misses after warm-up                                                                                |

Indexes built before the merge settings (September 2026) keep their many small splits: they are past any maturation period, so nothing merges them in place. After a rebuild, benchmark with `scripts/ops/bench-cold-searches.py` and record size, send rate and total time on the rebuild's issue.

**Decade layout (#123, 05 §5.5.5)** keeps each decade in its own splits, so date-limited searches skip the others. The release logs `decade layout`; `sent a decade's pages` each time a decade's pages go to the writer (at most every split target's worth of a decade; until then they wait in a decade spill directory, a few GB at most); `sent the pages a decade at a time` with totals; and `splits_by_decade` in `index layout`. To stop decade searches without a rebuild, publish `current.json` without the `decades` key.

**One searched text field (#283, #925, 05 §5.5.7)** puts LoC's and American Stories' text in one field (`text_all`), so each word is looked up once. Set it with `USNM_AMERICAN_STORIES`. The release logs `one searched text field` and `building index` with `text_layout: 3`; check `build.features.text_layout` and the `text_all`, `text_all_cg`, `text_loc` fields in `build.templates.pages`. The aggregate drops `total.american_stories_only`; hits keep `matched_in` and the American Stories badge; `USNM_AMERICAN_STORIES_SEARCH` has no effect ([American Stories](american-stories.md)). With `USNM_OCR_LOCAL` ([our OCR](../design/12-onprem-ocr-pipeline.md#step-9-release-reads-ocr-local-provenance-file-l)), pages rule v1 replaced (about 19.8 thousand of 23.7 million in `pages-v20261022-1`, #925) also get LoC's text in `text_all` after ours and American Stories', 32 positions apart, stored in `text_loc`; `matched_in` says `loc_original` (06 §6.3.4). The serving API must know layout 3 before publish, or it refuses the version and keeps the previous one. Turning `USNM_AMERICAN_STORIES` off forces a full base, since a delta can't leave the text out of the field it shares with LoC's. To revert, publish a field-per-text version's `current.json`, or rebuild without the setting.

**Page languages (#289, 05 §5.5.8)** give each page its own languages, found from its words, for `lang=`. The release logs `building index` with `page_language: true`, then `pages whose own languages differ from their title's, in the version` (expect about 1.5 to 2 million). In the manifest check `build.features.page_language`, the two fields in `build.templates.pages`, `page_languages.pages`, `page_language_baselines.json` and `page_languages.json` (about 15 to 20 MB); `/v1/meta` says `"language_filter": "page"`. Detection uses up to 4 threads beside the writer. To revert, publish a version without page languages, or rebuild without the setting. Don't edit the key out of the serving `current.json`: the API keeps its loaded filter, its caches are keyed by version, and it logs `current.json changed page_language without a new version`.

**Larger splits (#283)** mean fewer splits per cold search. The release logs `split target` and `building index` with `split_docs`, and refuses more than `docs / target + 5 + 2` splits. Check `index layout` (about pages ÷ target splits, plus up to 7 per decade with decades; `largest_split_docs` near the target). `current.json` doesn't change and the API needs nothing. Limits (05 §5.5.1, 08 §8.4): an uploading split holds min(its size, 10 GB) of writer memory; merged splits can reach about twice the target, at about 74 KB a page with American Stories' text, so scratch must hold the largest merge plus the decade spill. Count decades from the corpus's actual bounds (1736–1963 spans 24). On the ingest job a target above 30,000 needs E4.

### Full rebuild on-prem

The on-prem release container `usnm-release` (#283, #917; home-infra `scripts/lab/release-ct.sh`, on Arc as `usnm-release-arcs-<env>` via `release-ct-arc.sh`) builds a staged version on local ZFS (`/srv/usnm`, not NFS), and `usnm-ingest publish-staged` copies it to the index account and the lake. No Azure compute runs.

**Grants.** Its Arc identity (`USNM_ONPREM_RELEASE`, `USNM_ONPREM_RELEASE_HOST`, `modules/onprem-release.bicep`) gets read on `curated`; read on `reference` with writes only under `pages-v*/` and to `index-storage-pending.json`; write without delete on the index account's `qw-index`; Cosmos DB Data Contributor on `index_runs` only; AcrPull for the pinned Quickwit image. The lake and index account open to `USNM_OCR_NODE_IPS`; Cosmos needs `USNM_COSMOS_ONPREM_ACCESS` (on in prod) or `USNM_ONPREM_SERVING`, or the record gets 403. **`USNM_ONPREM_RELEASE` is `false` in prod between releases**: set it and provision before the next one, and turn it off after its publish, which removes the grants and closes the index account. `modules/release-writer-role.bicep` stays while `USNM_ONPREM_RELEASE_HOST` is set (the stack can't delete a custom role with its own deny assignment). The Cosmos writer lock isn't taken, so keep any ingest and prune jobs and schedules off.

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
  /srv/usnm/src/scripts/onprem/release.sh --run-dir "$R" --step release --split-docs 60000
scripts/onprem/release.sh --run-dir "$R" --step publish --dry-run
scripts/onprem/release.sh --run-dir "$R" --step publish \
  --cosmos-endpoint "$(scripts/azure/settings.sh --env prod --key COSMOS_ENDPOINT)"
```

- `seed` uses the serving version's batches, not Cosmos's list, so newer curated batches wait. `--sample 1pct` seeds 30 batches (every 100th by name from the 40th, 238,082 pages) for a rehearsal that stops before `publish`. Run it the day the release starts: it reserves the version names for today and the next two days.
- `release` builds with the #283 settings (one text field, decades, page languages, American Stories, our OCR, the Latin text, v1 ingest), writes `reference/index-storage-pending.json` in the run directory and nothing in Azure. It refuses a directory that already holds a staged version; rerun a failed one in a new directory. Settings come from the environment, with prod's defaults (`USNM_LAKE`, `USNM_INDEX_ACCOUNT`, `USNM_INGEST`, `USNM_QUICKWIT_BIN`); `USNM_CURATED` reads curated from a local copy instead of the lake (#308).
- `publish` refuses unless the lake still serves the release's rollback (mirror and build again otherwise), every split is published, and no same-name object in Azure differs; reruns resume. It records the run in Cosmos `index_runs` (ADR-0027 settled decision 5) with `usnm-ingest import-staged-run`, from the snapshot's `index_run.json`, so the version is listed before promotion. It needs `--cosmos-endpoint` or `USNM_COSMOS_ENDPOINT` (`https://<account>.documents.azure.com/`) and checks it and the staged pointer before copying; a dry run doesn't. It doesn't set `ops/current` (the [cutover](#index-storage-cutover)'s record step does). If only the record failed (Cosmos firewall closed, role not provisioned), fix it and rerun `--step publish`.
- Promotion is a separate operator step ([Index storage cutover](#index-storage-cutover), step 1).

After promotion `publish` refuses, so record a missing run from the batch container (`USNM_INGEST_ONPREM` grants it `index_runs` and `ops`), as root on its Proxmox host:

```sh
pct exec 215 -- /usr/local/bin/usnm-ingest-run --command import-staged-run --wait -- --version pages-v<date>-<n>
```

The import is idempotent: it creates a missing run, keeps an identical one, replaces a failed same-name attempt with no snapshot, and stops on a different published one. Prune then keeps the version as a newer published run, so rollback can roll forward. A Cosmos-backed release or prune also imports a run `current.json` names but lacks. Check `/v1/versions` lists it (`serving` false until the switch) and `/v1/status` (on `/datasets`) has it under `indexing.runs`; the API rereads `index_runs` about once a minute.

**Through Kafka.** One writer's v1 API takes about 130 pages/s. With `USNM_WRITER_KAFKA=127.0.0.1:9092` and `USNM_WRITER_INGEST=v1` (required), a decade-partitioned main index's decade files go round-robin to topic `usnm-<index id>` with `USNM_WRITER_KAFKA_PARTITIONS` partitions (default 8), keeping splits one decade each. The index gets a Kafka source with one pipeline per partition; on the release host 1, 2 and 4 pipelines gave 341, 509 and 754 pages/s. The last pages and forced commit, the Japanese and Congress indexes, the exact count and the restart-based final merges stay on v1. The source checkpoints in the metastore, so restarts don't duplicate. A produce with a lost answer is resolved from the partition's end (written: go on; not written: resend; neither: fail). Before producing, at most every 10 s, the release reads the index's Kafka checkpoint:

- Oldest held record past the index's position (expired unread): fail.
- More than `USNM_WRITER_KAFKA_MAX_LAG` behind (default 1,000,000, about half an hour of sending): wait.
- No progress for `USNM_WRITER_KAFKA_STALL_SECS` (default 900, at least commit timeout plus 600) with records waiting: restart the writer, and the source resumes from its checkpoint. At most 3 restarts, each needing progress since the one before, then fail. A checkpoint read failing or over 30 s counts as no progress.
- After the last file, wait the same way until every record is indexed, then count.

Consumers use a 300 s session timeout (librdkafka's 45 s default broke an earlier #283 run) and a 900 s max poll interval. Once the count matches, the release removes the source and topic. The broker is single-node KRaft Kafka on loopback in the container (home-infra `scripts/lab/release-ct-kafka.sh`), data on the release dataset, 3-hour retention, 256 MB segments; falling 3 hours behind fails the count. A release refuses an existing topic: delete it (`kafka-topics.sh --delete`) before reusing the version name.

**Threads and copy.** Curated parts download 8 at a time; documents are built on `USNM_RELEASE_THREADS` threads (default: CPUs), 256 rows at a time, in row order, so output matches a one-thread build. `batch timing` gives seconds per step (`fetch`, `build`, `lang`, `add`, `queue_wait`). Azure requests retry transient failures (dropped connection, 408, 429, 5xx) 12 times over about 5 minutes; a Put Block times out after 30 s plus 8 MiB at 64 KiB/s. A failed split is resent up to 3 more times, 5 minutes apart, sending only missing blocks. A 403 or differing object stops it at once. Watch `index copy progress`, `split copied`, `store request failed; trying again`, `split upload failed`. Resize the container live with `release-ct.sh --resize`. `release merges` (every 30 s) shows `immature_splits`, `immature_mb`, `pending_merge_mb`, `merge_write_mb_per_sec`, and over the last 30 minutes `immature_mb_per_hour` and `eta_mins`.

#### Finishing a release

When a release stops after its main index passed the exact count (usually the merge wait ran out), `scripts/onprem/release.sh --run-dir "$R" --step finish --index-version pages-v<date>-<n>` (`release-finish`) completes it in the same run directory. Use the release's settings (`--split-docs`, Kafka) and its `release.log`, logging the finish to another file, with exactly one of `--stage-index-storage` (the step passes it) or `--publish-serving`. It takes the writer lock and refuses unless the run stopped before writing its snapshot, its log has the merge-wait lines, the catalog and curated batch records are unchanged since the run started, and published splits hold the documents last logged as sent (that 30 s sample is a floor).

It replans and requires the same run record (names, batches, pages, rollback, features, templates, writer tuning, Quickwit) and that each title's documents carry the catalog's place, place shard, state, languages and ordinal. For a title with places by date, every document's `day` must fall in a period of its place, so a catalog that gained a period or moved a boundary past a built page is refused. Curated listings are read as of the run's start (`--listed-before` overrides), so our OCR's later parts don't count. It restarts the writer on the run's data, marks leftover staged splits for deletion, and resets a leftover `merge_factor` 2. It rebuilds every document without sending it, from the lake, and requires each batch's `our OCR for the batch's titles` and `American Stories' text for the batch` lines, planning lines, totals and per-decade counts to match. It then waits for merges (`USNM_FINISH_MERGE_TIMEOUT_SECS`, default 24 hours), removes any Kafka source and topic, and writes our OCR's files, the Japanese index, the layout, the snapshot, the pending pointer and the run record. It can be rerun; delete any Japanese index from the run first if asked.

#### Restoring serving to Azure

`restore` (#1061) copies a version that serves on-prem into the index account, as in a disaster recovery ([09 §9.6](../design/09-operations-security-cost.md#96-backup-and-disaster-recovery)) after the account and apps are reprovisioned. Under [ADR-0029](../design/adr/0029-onprem-primary-azure-failover.md) the index account keeps one current release for the Azure spare, so this is for a lost or emptied account. `publish` can't do it, since after promotion `current.json` names the release itself. Run it from the version's run directory (keep its `state.json`, `reference/index-storage-pending.json` and `qw/` on the release container's dataset), with `USNM_ONPREM_RELEASE` and `USNM_INDEX_STORAGE` provisioned:

```sh
scripts/onprem/release.sh --run-dir "$R" --step restore --dry-run
scripts/onprem/release.sh --run-dir "$R" --step restore
scripts/onprem/release.sh --run-dir "$R" --step ready --app-url "$APP_URL" --index-version pages-v<date>-<n>
```

`restore` (`usnm-ingest publish-staged --restore`) makes `publish`'s checks and copy (splits, metastores, manifest, reference snapshot with `index_run.json`), resumable, but refuses unless `current.json` names this release's own version and indexes (if the rollback, use `publish`; if another version, use its run directory). It writes neither `current.json` nor `index-storage-pending.json`, and nothing to Cosmos: the lake keeps pointing at the version, so the apps load it as they start. `ready` (only `curl` and `jq`) polls `--app-url` (the app's default hostname) until `/readyz` passes and `/v1/meta` names `--index-version`, exiting 1 after `USNM_READY_TRIES` (default 60) tries `USNM_READY_WAIT_SECS` (default 10) apart. Run it before pointing the Worker at the app.

Limits: indexes come only from `DIR/qw` (with a delta mode, ADR-0027 decision 8, all under one `--local-index-root`, or it refuses naming the missing metastore); `current.json`, the catalog and other snapshots must still be in Azure.

**Rehearsal.** While the index account holds the serving version, `restore` writes nothing to Azure, so running it with and without `--dry-run`, then `ready`, proves the checks pass and the run directory is whole. A copy into an empty account belongs on dev, or as an operator step with the maintainer's approval.

### Full rebuild on the ingest job

Where `USNM_INGEST_JOBS` is `true` (not prod), the ingest job builds the base. It takes the writer lock (08 §8.4.1). Run it after the backfill finishes, with no backfill or ingest execution running, and turn on the E4 profile and the scratch share for it:

```sh
RG=$(scripts/azure/settings.sh --env <env> --key AZURE_RESOURCE_GROUP)
JOB=$(scripts/azure/settings.sh --env <env> --key INGEST_JOB)
scripts/azure/settings.sh --env <env> --key USNM_INGEST_FULL --value true
scripts/azure/settings.sh --env <env> --key USNM_DEDICATED_PROFILE --value true     # the E4 profile (#172)
scripts/azure/settings.sh --env <env> --key USNM_INGEST_ON_DEDICATED --value true   # and the ingest job on it
scripts/azure/settings.sh --env <env> --key USNM_INGEST_SCRATCH_GIB --value 128     # the writer's scratch share (08 §8.4)
scripts/azure/provision.sh --env <env>
az containerapp job start -n "$JOB" -g "$RG"
```

With `USNM_INDEX_STORAGE` on and `USNM_INDEX_STORAGE_SERVE` off, the job stages the build (`--stage-index-storage`) instead of publishing it ([Index storage cutover](#index-storage-cutover)).

**Replica timeout (#233, #283).** 96 h for explicit full rebuilds (`USNM_INGEST_FULL=true`) and index-account staging runs, 48 h for ordinary ingest, 24 h for backfill. That leaves margin over the 1% dev measurement scaled to the corpus, but doesn't guarantee completion. Keep automatic replica retries at zero: a release doesn't resume after a timeout, and platform maintenance can still interrupt it.

**Titles first.** A full run waits to release until titles-sync has fetched every title the listing and curated batches name (titles LoC lacks excepted; failed fetches count as left). The first execution may spend its 8 hours on titles-sync and fail with `titles-sync reached its deadline with N of M titles left … nothing was released` (or `LoC rate limited titles-sync …`): start the job again. The job-failed alert ignores this stop; the severity 3 _ingest not progressing_ alert fires if no execution follows within 3 hours or 3 stops in a row leave as many titles as the first. `scripts/azure/logs.sh --env <env> --query-name ingest-endings` lists endings.

**Watch it** with `scripts/azure/logs.sh --env <env> --query-name release-progress --span 6h` and `az containerapp job execution list -n "$JOB" -g "$RG" -o table`:

- `title records` every 100 titles; the final report has `left: 0` and no `throttled` or `out_of_time`.
- `release progress` every 30 s. `quickwit_rss_mb` stays under about 5,500 on Consumption (7,680 MiB container) or 20,000 on E4 (25 GiB).
- `release merges`: step `settle`, then `finalize`, at most `USNM_MERGE_TIMEOUT_SECS`.
- `merged; the index is closed to further writes`, `index layout`, `released`.

**Afterwards**, once the execution has ended, turn the rebuild, E4 and scratch off in two provisions: the job must leave the profile before the profile can go, and the first provision takes the share away from the job.

```sh
scripts/azure/settings.sh --env <env> --key USNM_INGEST_FULL --unset
scripts/azure/settings.sh --env <env> --key USNM_INGEST_ON_DEDICATED --unset
scripts/azure/settings.sh --env <env> --key USNM_INGEST_SCRATCH_GIB --unset    # deletes the share, its account, endpoint and DNS zone
scripts/azure/provision.sh --env <env>
scripts/azure/settings.sh --env <env> --key USNM_DEDICATED_PROFILE --unset
scripts/azure/provision.sh --env <env>
```

Don't leave these set. `USNM_INGEST_FULL` rebuilds the corpus on every scheduled run. Scratch costs about $25 a month (#308); the next rebuild's provision creates it again, empty, under the same names. The E4 profile costs about $0.10 an hour in management fees even when idle, plus about $0.39 an hour per running node (East US 2, October 2026). A forced full base on Consumption fits only the template's 30,000-page target, so unset a larger `USNM_SPLIT_DOCS` there too.

**Why E4.** On Consumption (7.5 GiB with the scratch share's init container) the writer ran out of memory 6.5 h into a rebuild (#172). E4 gives 3.0 vCPU, 25 GiB, a 6 GiB heap, 120 s commit timeout and 4 GiB queue (`USNM_WRITER_HEAP`, `USNM_WRITER_COMMIT_SECS`, `USNM_WRITER_QUEUE`, from `infra/modules/ingestjobs.bicep`; the release logs them as `writer tuning`), so splits reach the target instead of being cut small every 30 s. The queue's WAL truncates only on commit; at 600 s it filled and Quickwit refused pages with "no shards available".

**WAL on local disk.** Mounted jobs set `USNM_WRITER_WAL_DIR=/work` (ephemeral disk, not `/scratch`). Each writer creates a fresh private directory there and links `qwdata/wal` (v2 WAL) and `qwdata/queues` (v1 queue) to it, which keeps the WAL's latency-sensitive persist lock off NFS. Startup needs 8 GiB free; `writer WAL on local disk` logs path and free MiB, and `wal_disk_free_mb` tracks it beside NFS `disk_free_mb`. Split cache, merge files and decade spill stay on NFS. Neither layout resumes an interrupted rebuild: the next run clears its writer data first. A persist that held the lock for 45.51 s against Quickwit 0.9.1's 6 s timeout returned HTTP 500 and failed the first #283 attempt; don't retry such a 500 blindly, since it can duplicate documents.

### Duplicate-safe ingest

Quickwit 0.9.1's ingest v2 is at least once: a persist that timed out (6 s, up to 5 tries; `PERSIST_REQUEST_TIMEOUT`, `MAX_PERSIST_ATTEMPTS`, not settable) still appends later. Quickwit doesn't deduplicate by `doc_id`, and a delete by `doc_id` removes every copy (and skips splits under 7 days old), so such a base can't be repaired. The second #283 attempt (October 2026) held 121 extra copies and the exact count refused it.

`USNM_WRITER_INGEST=v1` uses the v1 API (`use_legacy_ingest=true`): one append per request, no internal retry, answering `num_docs_for_processing`. The sink resends after a 429 (queue full) or 404 (queue not created yet), both before any append; it fails on any other answer, a transport error or a timeout, and requires `num_docs_for_processing` to equal the pages sent (v1 drops refused pages silently). v1 reads the merge policy only at start, so `seal_restarting`:

1. Lets merges settle.
2. Sets the index config to `merge_factor: 2`.
3. Restarts the writer (same directory and port): SIGTERM, SIGKILL after 60 s, then a new writer once the old one is reaped; fails if that isn't confirmed 30 s after SIGKILL.
4. Waits for `spawning merge pipeline` with `merge_factor: 2`; fails without publishing otherwise or if no pipeline starts within the finalize stall.
5. Settles again, restores the config, checks the layout.

On a failure or timeout after step 2 the release restores the config (a cancelled one tries from a background task). If the error says the index "keeps merge_factor 2 in the metastore", it is never published: prune it, or PUT its `GET /api/v1/indexes/<id>` `index_config` back with the original `merge_factor`. The page count and layout check still refuse an index whose documents differ from the pages sent. v1 needs `--quickwit-bin` and is refused with `--quickwit-url`. `USNM_WRITER_INGEST` is public (`infra/env/<env>.json`) and passed to the ingest job, default `v2`; the on-prem release always uses `v1`.

## Index storage cutover

The index account ([ADR-0021](../design/adr/0021-separate-index-storage.md), #283, #245, #416), `usnmqwst{env}{suffix}`, holds the searchable indexes apart from the lake. Prod moved to it on 24 October 2026, when `pages-v20261022-1` was promoted, and the legacy lake searcher was retired on 25 October (#1136); `USNM_INDEX_STORAGE` and `USNM_INDEX_STORAGE_SERVE` are both `true`. **Never turn `USNM_INDEX_STORAGE` off**: the stack deletes the account and its data. Quickwit 0.9 uses one account per process, so URIs stay `azure://qw-index` and `QW_AZURE_STORAGE_ACCOUNT` picks the account. Reference snapshots stay in the lake.

A new environment moves the same way: set `USNM_INDEX_STORAGE=true` with `USNM_INDEX_STORAGE_SERVE` false and provision (this creates the account, its grants and a searcher that may restart until the metastore exists); build a full release, which stages; verify; then promote and switch as below. Staging ignores the old pointer for index reuse, so main, Japanese and Congress indexes are rebuilt. A staged run publishes `reference/index-storage-pending.json`, not `current.json`, leaves `ops/current` alone, and records `published` with `previous_version` set.

Promoting a staged version, the same for every full release since:

1. **Promote.** With writers and prune stopped, from a host with Entra Blob Data Contributor on `reference` (never keys or public access), working in the repository, save the serving pointer and upload the pending one as `current.json`:

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
   ```

   On-prem serving reads `current.json` from the lake, and its `/readyz` fails while its searcher lacks an index `current.json` names (`USNM_READY_REQUIRES_PUBLISHED_INDEXES`, [`scripts/onprem/serving/`](../../scripts/onprem/serving/README.md)); the Worker then sends visitors to the spare. Promote after the on-prem searcher has the new version. How a new version reaches the serving container is still open (#1100). On a first switch, also set `USNM_INDEX_STORAGE_SERVE=true` through a pull request and provision from that commit; until then the old API logs `reload failed; keeping current version`, and a fresh old-account replica can't load the new pointer, so don't restart or roll it.

2. **Check and record.** Verify `/readyz`, `/v1/meta`, counts, main, Japanese and Congress queries. Then set `ops/current`, which `/v1/status` (`indexing.current_version`), the seeder, `ocr-enqueue`, `ocr-sync` and the BOINC export read (the export fails with "published snapshot changed during export" if it changes mid-export). First prepare OCR, or `ocr-sync` fails at seeding:
   - **Damage version list** `audit/damage/v2/versions/pages-v<date>-<n>.json` (curated store), written only by `ocr-damage`. The pass is incremental; rerun it until `ocr damage finished` shows `left` 0 ([OCR damage tables](japanese-ocr.md#ocr-damage-tables-296)):

     ```sh
     pct exec 215 -- /usr/local/bin/usnm-ingest-run --command ocr-damage -- --version pages-v<date>-<n> --max-runtime-secs 165600
     ```

   - **Prefetch manifest**, with `USNM_OCR_SEED_SOURCE=fetched`. Unpinned, the seeder reads the `ops/current` version's manifest and its newest fetched list (`ocr-out/fetched/<version>/`). Build the manifest after the version list and publish a fetched list (`USNM_OCR_PUBLISH_FETCHED`, or `usnm-ocr-worker publish-fetched --manifest pages-v<date>-<n>`), or pin `USNM_OCR_SEED_MANIFEST` (prod pins it) to the version whose manifest and fetched list the seeder reads now, and provision:

     ```sh
     pct exec 215 -- /usr/local/bin/usnm-ingest-run --command ocr-prefetch-manifest -- --version pages-v<date>-<n>
     ```

   Record between BOINC priority-controller ticks. On the controller host ([the batch scheduler](../boinc/server/batch-scheduler.md#on-prem-controller-host-ct-211)), `systemctl stop usnm-boinc-priority-controller.timer` and repeat `systemctl is-active usnm-boinc-priority-controller.service` until it is neither active nor activating. Then, as root on the batch container's host:

   ```sh
   pct exec 215 -- /usr/local/bin/usnm-ingest-run --command import-staged-run --wait -- --version pages-v<date>-<n> --current
   ```

   and `systemctl start usnm-boinc-priority-controller.timer` on the controller host. This runs the batch container's installed release (`/usr/local/lib/usnm-ingest/current`), which must have `--current`. For another commit, install it (home-infra `scripts/lab/batch-ct-ingest.sh --install --tag <sha>`) and pass `--tag <sha>`. CI publishes an image only for commits that change the images, so check first; this prints the sha when the tag is there:

   ```sh
   az acr repository show-tags --name "$(scripts/azure/settings.sh --env prod --key ACR_NAME)" --repository usnewsmap-ingest --query "[?@=='<sha>']" -o tsv
   ```

   It refuses unless `current.json` names the version and its run is published, takes the writer lock, and is idempotent. Run it after the switch: before it, `/v1/status` would announce the version early. Check `indexing.current_version` and the next pickup (`journalctl -u usnm-ocr-sync` in the batch container). Keep the lake's rollback pointer.

3. **Rollback.** Stop writers and prune, upload `serving-before.json` as `reference/current.json`, and, with step 2's OCR prerequisites met for that version, record it between ticks: `pct exec 215 -- /usr/local/bin/usnm-ingest-run --command import-staged-run --wait -- --version <rollback version> --current`. Nothing else fixes `ops/current`: left alone, `/v1/status` keeps announcing the new version and the OCR pipeline works on a version the site doesn't serve. Roll forward by promoting the saved pending pointer and `--current` again. A rollback to a version in the lake's old `qw-index` would also need `USNM_INDEX_STORAGE_SERVE=false` and the legacy searcher, both gone since #1136 and #245.

## Prune old index versions

Releases never delete indexes, so `qw-index` accumulates old versions and stopped rebuilds. `usnm-ingest prune` deletes the Quickwit indexes no kept version names. It keeps:

- the serving version: every index in `current.json`, with its `ja` and `congress` sets;
- its rollback (`previous_version`);
- any published version newer than the serving one (after a hand rollback). A version with a `manifest.json` counts as published even if its run says `building`, since the publish record is written last.

A kept version's indexes are those named by `current.json`, its pipeline-state run, its manifest's `indexes`, and its `pages-ja-{date}-{n}` and `congress-{date}-{n}` ids. Other `pages-…` and `congress-…` indexes are deleted; other names (such as `otel-…`) are ignored. Deletes go through Quickwit (`DELETE /api/v1/indexes/{id}`), which removes split files and metastore entry together, never raw blob deletes.

Without `--delete` it is a dry run listing each candidate (splits, documents, size) and the total. It takes the writer lock, so it fails with ``lock `quickwit-writer` is held by …`` during a release, and a release can't take the lock while prune holds it. It refuses when `current.json` is unreadable or empty, the serving version has no run, the rollback's indexes aren't named by its run or manifest, or a manifest's `indexes` isn't a list of `index_id`s. With `--delete` it rechecks before each delete and stops if what it keeps has changed, and stops its Quickwit node if the lock can't be renewed, so `--delete` needs `--quickwit-bin`; `--quickwit-url` is for dry runs only.

**Prod has no prune path.** Prune deletes in Azure's `qw-index`, where no on-prem identity may delete (#917), and it runs as an ingest-job execution, which prod no longer has. The retired lake versions were removed under [#245](https://github.com/tgoodyear/usnewsmap.com/issues/245), and the index account keeps one current release for the Azure spare ([ADR-0029](../design/adr/0029-onprem-primary-azure-failover.md) decisions 5 and 6). Where the ingest job exists, run prune after a release is checked, with no ingest or backfill running; it needs no scratch share. Don't roll `current.json` back by hand during prune: rollbacks don't take the lock, and prune checks what it keeps only before each delete. Dry run first:

```sh
QW=(--quickwit-bin /usr/local/bin/quickwit --quickwit-metastore azure://qw-index --quickwit-index-root azure://qw-index)
scripts/azure/start-job.sh --env <env> --job INGEST_JOB --wait-mins 120 -- prune "${QW[@]}"   # dry run: lists, deletes nothing
scripts/azure/logs.sh --env <env> --query-name prune --span 2h
```

Check `prune keeps` names the serving version `/v1/status` reports and the rollback you want, then review the `prune candidate` lines (`bytes`, `gib`) and `prune plan` (count, `total_gib`; the full report ends the console log as JSON). Then delete with the same command plus `--delete`; the log has a `deleted index` line for each, then `pruned`. Large indexes take minutes. A failed delete stops the run; rerun to continue. The index account deletes immediately; in the lake, deleted splits become blob versions removed after a day by the `qw-index/` lifecycle rule, then sit in soft delete (08 §8.1).
