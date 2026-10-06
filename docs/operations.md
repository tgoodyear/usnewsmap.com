# Operations

Running the ingest pipeline in Azure: the backfill, releases, full rebuilds, and how to watch them. The pipeline is two Container Apps jobs, `caj-usnm-ingest-{env}` (`usnm-ingest run`: new LoC batches, the title catalog and a release) and `caj-usnm-backfill-{env}` (parallel `curate` workers for the initial corpus). Both run inside the VNet as `id-usnm-ingest`; the storage and Cosmos accounts stay private throughout. How they are deployed is in [`infra/README.md`](../infra/README.md); the design is in [08](design/08-azure-infrastructure.md) and [04](design/04-data-sources-and-ingestion.md).

Every command below uses your own `az` sign-in. CI can only roll the API onto new images, so starting jobs and changing settings is always a person's action.

## Deploy the jobs

`scripts/bootstrap.sh <env> --ingest` deploys both jobs (or `scripts/settings.sh <env> USNM_INGEST_JOBS true` and `scripts/provision.sh <env>` on an environment already on the registry). `USNM_BACKFILL_WORKERS` sets the backfill's workers (default 4), and `USNM_BACKFILL_CRON` an optional schedule for it (a UTC cron such as `"0 9 2-4 10 *"`; empty is manual), which every deployment keeps. The ingest job's Quickwit writer works on an NFS share, `USNM_INGEST_SCRATCH_GIB` in size (default 128; 0 removes it, 08 §8.4). The weekly schedule comes after the first release ([Weekly updates](#weekly-updates)).

## The title catalog

The catalog (`reference/catalog/titles.json`, `places.json`) is built by `titles-sync`, which `run` calls before every release. A batch is released only once its titles are in the catalog. LoC rate limits title records, often well under its published 20 a minute; titles-sync then sends nothing for 65 minutes and resumes more slowly, until 8 h into the run (`--titles-max-runtime-secs`), and the next run continues from where it stopped. `/v1/status` shows `titles.pipeline.awaiting_sync` (titles still to fetch) and `batches_waiting_for_titles`. Coordinate corrections live in git, in `catalog/overrides/places.json` (`[{city, state, lat, lon}]`), and ship in the ingest image; the next run applies them.

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

Before it publishes, a release waits for the writer's merges: `release merges` lines (every 30 s: step `settle` then `finalize`, splits, merges running and queued, free disk), then `merges settled; closing the index for its final merges`, then `merged; the index is closed to further writes` with the new index's split count. It then logs one `index layout` line per index of the new version and records the same under `indexes` in `reference/<version>/manifest.json`. `scripts/logs.sh prod index-layout 2d` lists them. A merged index has at most `pages / 60,000` (rounded down) `+ 7` splits (`split_num_docs_target` in `infra/quickwit/pages-index.yaml`); the release fails rather than publish more, or if merges don't finish within `USNM_MERGE_TIMEOUT_SECS` (default 14400, 4 hours; a setting, `scripts/settings.sh <env> USNM_MERGE_TIMEOUT_SECS <secs>` then `scripts/provision.sh <env>`, budgeted under the ingest job's 48-hour replica timeout (the backfill and Japanese OCR jobs keep 24 hours) in `infra/modules/ingestjobs.bicep`), or if the writer reports a full disk or a failed merge, or exits. A writer the kernel killed for memory shows as `the Quickwit writer was killed by signal 9 (SIGKILL): it ran out of memory (…)` in the `command failed` line; `quickwit_rss_mb` in `release progress` is its memory (08 §8.4).

## Full rebuild

Rebuild the indexes as one merged base (one-off, or a compaction). A release also builds a full base by itself, without `USNM_INGEST_FULL`, when the published version was built with another search backend or another version of the common-word pairs (05 §5.5.3); the steps below force one. Indexes built before the merge settings (September 2026) keep their many small splits: their splits are past any maturation period, so nothing merges them in place, and they are sealed. A full release builds a new base from every curated batch, merged, and publishes a version with no deltas; the previous version stays in Blob for rollback (`current.json`). It takes the writer lock like any release, so it never runs alongside another writer (08 §8.4.1). Run it when no backfill or ingest execution is running and the backfill has finished:

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
- `release merges`: step `settle`, then `finalize`; `splits` falls by 9 every minute or two after ingest ends. About 1.7 h for 23.8M pages in October 2026; with the 60,000-page splits and the common-word pairs, expect about 2.7 h (#156), and at most `USNM_MERGE_TIMEOUT_SECS`.
- `merged; the index is closed to further writes`, `index layout` (about 400 splits for 23.8M pages at the 60,000-page target) and `released`. About 11 h from the start when the catalog was complete, in the October 2026 rebuild before #154. With the pairs, the 2026-10-05 rebuild sent only 230 to 460 pages a second, so plan for up to about 29 h of sending and about 41 h in all (up to 8 h of titles-sync, sending, up to 4 h of merges), inside the ingest job's 48 h replica timeout (#172, #173).

Then turn the full rebuild off, and the E4 profile with it, in two provisions: the job has to leave the profile before the profile can go.

```sh
scripts/settings.sh prod USNM_INGEST_FULL ""
scripts/settings.sh prod USNM_INGEST_ON_DEDICATED ""
scripts/provision.sh prod
scripts/settings.sh prod USNM_DEDICATED_PROFILE ""
scripts/provision.sh prod
```

Don't leave `USNM_INGEST_FULL` set: every run, scheduled ones included, would rebuild the whole corpus. Don't leave `USNM_DEDICATED_PROFILE` set either: while the environment has the E4 profile it pays the Dedicated plan management fee every hour (about $0.10 an hour in East US 2 in October 2026), whether or not a node runs. An E4 node itself (4 vCPU, 32 GiB) costs about $0.39 an hour while the job runs.

**Why the E4 profile.** The first full rebuild with the common-word pairs (#154) was killed when the Quickwit writer ran out of memory at the Consumption profile's limit (7.5 GiB with the scratch share's init container), 6.5 h in, at 26.5% (#172). On the E4 profile the ingest container gets 3.0 vCPU and 25 GiB. A full run also takes longer than the October 2026 rebuild (about 230 to 460 pages a second instead of 750), which is why the ingest job's replica timeout is 48 h (#173). Benchmark afterwards with `scripts/bench-cold-searches.py`.

## Search log

The API keeps every search from the site, with only its filters, page count and UTC day, in the `searches` container ([ADR-0012](design/adr/0012-anonymous-search-log.md), 06 §6.8). Nothing expires `searches/days/` and `searches/import/`; staged batches are deleted after 7 days and stay in soft delete for 14 more. To read it:

```sh
scripts/settings.sh prod USNM_SEARCH_LOG_READERS "$(az ad signed-in-user show --query id -o tsv)"
scripts/provision.sh prod
scripts/searches.sh prod 30      # searches per day, top queries, queries that found nothing
```

The data account is reachable only through its private endpoint, so run `searches.sh` from a network that reaches it. The first day file appears an hour after the first full UTC day with the log deployed.
