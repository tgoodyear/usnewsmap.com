# The title catalog and releases

Part of the [operations guide](README.md).

## The title catalog

The catalog (`reference/catalog/titles.json`, `places.json`) is built by `titles-sync`, which `run` calls before every release. A batch is released only once its titles are in the catalog. LoC rate limits title records, often well under its published 20 a minute; titles-sync then sends nothing for 65 minutes and resumes more slowly, until 8 h into the run (`--titles-max-runtime-secs`), and the next run continues from where it stopped. `/v1/status` shows `titles.pipeline.awaiting_sync` (titles still to fetch) and `batches_waiting_for_titles`. Place corrections live in git and ship in the ingest image; the next run applies them (04 §4.6):

- **A misspelled or renamed town** that should share a place with the town's other titles: add `{"added": "2026-10-06", "from": "Skaguay", "reason": "old spelling", "ref": 190, "source": "…", "state": "AK", "to": "Skagway"}` to `catalog/overrides/place-aliases.json`. Every override entry carries its audit trail: `added` (the date), `reason`, `source` (what shows the change is right: a URL or a precise citation) and `ref` (the issue or PR), plus `updated` and `history` when it changes later (`catalog/overrides/README.md`). `from` is the city as LoC spells it (spelling variants such as "St."/"Saint" or a trailing state abbreviation already match); `to` is the town's name, which becomes the place's name. Keep the entries sorted by state, then `from`, and each entry's keys in alphabetical order; `cargo test -p usnm-ingest places` checks both, and refuses an alias whose `to` is another alias's `from`. Only merge towns that are one place on the map; a town that was separate (East Providence, Winston before 1913) keeps its own place.
- **Wrong coordinates or name**: add `{"added": "…", "city": "Lusk", "lat": 42.7625, "lon": -104.4522, "reason": "…", "ref": 123, "source": "…", "state": "WY"}` to `catalog/overrides/places.json`, sorted by state, then city, keys in alphabetical order, with `"name"` to change the place's name and `"precision"` (`city`, `county` or `state`) if needed. `source` says where the coordinates come from, `reason` why the override exists. Take the coordinates from the gazetteer (`catalog/gazetteer/us-places.csv`) or another public source, not from LoC, unless the gazetteer lacks the town and one of the place's own LoC records has its point (New Echota, #223). The `geocode` log lists places where LoC's point and the gazetteer's are more than 25 km apart and LoC's was kept (`furthest` in its warning, `disagreement_examples` in the report): start there. A LoC point more than 100 km from the gazetteer's town of that name (when the name isn't ambiguous there) is replaced by the gazetteer's already (`gazetteer_over_loc_places`); check that list too, since the gazetteer can have the name for a different town than the paper's. It also lists places at the median of LoC points more than 25 km apart (`loc_apart_examples`, with the two furthest points and their LCCNs): one of those records likely has another town's point. Each place in `catalog/places.json` records the rule its coordinates came from (`coordinates_from`) and the other names its titles use with the rule that merged each (`variants`).
- **A title whose record lists several places** (a paper that moved): add `{"added": "…", "city": "Chicago", "lccn": "sn84024055", "reason": "…", "ref": 190, "source": "…", "state": "IL"}` to `catalog/overrides/title-places.json`, sorted by `lccn`, keys in alphabetical order.
- **Check the effect first**: run `curl -s https://api.usnewsmap.com/v1/places > /tmp/places.json`, then `cargo run -p usnm-ingest --example place-report -- --places /tmp/places.json`; the report lists the merges, renamed places and places that move.

A corrected point or name ships with the next release, delta or full. A change that moves a published title to another place (most aliases, a title place, new merging rules) waits for the next full release: incremental releases keep published titles where they were published, and log `N published titles' place changes wait for the next full release`. Run a [full rebuild](rebuild-and-index-storage.md#full-rebuild) on the E4 profile to apply them; the weekly run never starts one for this, since on the Consumption profile a full release runs out of memory (#172). To refresh the gazetteer for a newer Census year, run `scripts/build-gazetteer.py --year YYYY` and commit `catalog/gazetteer/us-places.csv`.

## Backfill and the first version

Backfill, then publish the first version:

```sh
RG=$(scripts/settings.sh --env prod --key AZURE_RESOURCE_GROUP)
# Queue every batch from LoC's listing and curate it. Downloads are paced to
# LoC's limit (10 bulk requests per 10 minutes per IP), one every 75 s across
# all workers, so this takes about 2.5 days. Each worker stops claiming 22 h
# after it starts and exits 0 once its current batch is done (by about
# 22 h 45 min). Start the job again when an execution has ended, until the
# queue is empty (it resumes where it left off).
az containerapp job start -n "$(scripts/settings.sh --env prod --key BACKFILL_JOB)" -g "$RG"
# When that has finished: curate any leftovers, build the base index, publish.
az containerapp job start -n "$(scripts/settings.sh --env prod --key INGEST_JOB)" -g "$RG"
```

Watch with `az containerapp job execution list -n <job> -g "$RG" -o table` and the saved log queries:

```sh
scripts/logs.sh --list                                                  # the saved queries (ops/queries/)
scripts/logs.sh --env prod --query-name release-progress --span 6h      # docs sent, rate, retries, free disk, memory, merges
scripts/logs.sh --env prod --query-name index-layout --span 2d          # splits, docs and size of each index a release published
scripts/logs.sh --env prod --query-name curation-throughput --span 1d   # batches and pages per hour
scripts/logs.sh --env prod --query-name curate-replicas --span 6h       # each worker's last line, batch and stage
scripts/logs.sh --env prod --query-name errors-by-batch --span 1d
scripts/logs.sh --env prod --query-name job-executions --span 2d
```

The workspace id is the stack output `LOG_ANALYTICS_WORKSPACE_ID`; the queries run with your own sign-in (Log Analytics Reader or more). Traces (`release`, `curate`) and metrics are in the environment's Application Insights component (`appi-usnm-prod` in the pinned prod, `usnm-core-appi-<env>` otherwise). With alert emails set, a failed job, a stalled release, a stalled backfill or a backfill replica silent for 15 minutes sends an email (08 §8.1.2). So does a full Log Analytics daily cap (08 §8.1.1): until the cap resets nothing is logged, so a job that looks stalled may still be working; check `az containerapp job execution list` before stopping it. The same views appear as charts in the "usnm pipeline (<env>)" workbook, under that component → Workbooks ("usnewsmap pipeline" before the Phase 2 provision).

The last lines of each worker say why it stopped: `max runtime reached; claiming no more batches` (with the number of batches still queued) when the execution should be started again. `curation finished` on its own means the worker found nothing more it could claim. A batch that failed in this execution is queued again but skipped by that worker, so check `scripts/logs.sh --env prod --query-name errors-by-batch --span 1d` and start the job once more if it lists batches that weren't curated later.

## Switch the API to the published indexes

Once a version is published (`current.json` exists in `reference/`): `scripts/settings.sh --env <env> --key USNM_SEARCH_BACKEND --value quickwit`, then `scripts/provision.sh --env <env>`. Until then the API would have nothing to serve ([`infra/README.md`](../../infra/README.md#what-this-slice-runs)).

## Weekly updates

Poll LoC weekly for new batches: `scripts/settings.sh --env <env> --key USNM_INGEST_CRON --value "17 3 * * 1"` (Mondays 03:17 UTC), then `scripts/provision.sh --env <env>` while no ingest execution is running. The schedule is committed in `infra/env/<env>.json` ([Settings](environments-and-deploys.md#settings)). Each run fetches LoC's listing once, curates and releases what is new, and exits 0 when there is nothing (08 §8.4). It stops curating 6 h in and releases what it has; if its log says `max runtime reached; claiming no more batches` with batches still queued, start the backfill job to curate the rest. A scheduled job can still be started by hand.

A delta's writer works on the replica's own disk (about 13 GiB free at the start), with no scratch share (#308). That holds a weekly delta: the delta of 10 October 2026 (5,647 pages) used about 0.1 GB, and the full rebuilds of 6 to 9 October (23.7M pages each) peaked at 11 to 14 GB on the share (sampled every 30 s). The writer's disk use grows with a delta's size only up to its caps (a 4 GiB write-ahead log, a 4 GiB split cache and one merge of about 30,000 pages at a time, up to about 9 GB with American Stories' text, 08 §8.4), and a weekly delta reaches none of them. The tightest case is a large delta after a backfill, hundreds of thousands of pages: if the writer reports a full disk, the release fails and publishes nothing (`current.json` stays where it was). Then turn the share on for one run (`USNM_INGEST_SCRATCH_GIB=128`, provision, start the job), and clear it afterwards as in [Full rebuild](rebuild-and-index-storage.md#full-rebuild). `disk_free_mb` in the `release progress` lines shows how close a delta came.

## Check a release's splits

Before it publishes, a release waits for the writer's merges: `release merges` lines (every 30 s: step `settle` then `finalize`, splits, merges running and queued, free disk), then `merges settled; closing the index for its final merges`, then `merged; the index is closed to further writes` with the new index's split count. It then logs one `index layout` line per index of the new version and records the same under `indexes` in `reference/<version>/manifest.json`. `scripts/logs.sh --env prod --query-name index-layout --span 2d` lists them. A merged index has at most `pages / 30,000` (rounded down) `+ 7` splits (`split_num_docs_target` in `infra/quickwit/pages-index.yaml`), and the release fails rather than publish more. It also fails if the merges don't finish within `USNM_MERGE_TIMEOUT_SECS`, if the writer reports a full disk or a failed merge, or if the writer exits. The timeout is a setting (default 14400, 4 hours): `scripts/settings.sh --env <env> --key USNM_MERGE_TIMEOUT_SECS --value <secs>`, then `scripts/provision.sh --env <env>`. It is budgeted under the ingest job's 48-hour replica timeout in `infra/modules/ingestjobs.bicep` (the backfill and Japanese OCR jobs keep 24 hours). A writer the kernel killed for memory shows as `the Quickwit writer was killed by signal 9 (SIGKILL): it ran out of memory (…)` in the `command failed` line; `quickwit_rss_mb` in `release progress` is its memory (08 §8.4).
