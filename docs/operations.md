# Operations

Running the ingest pipeline in Azure: the backfill, releases, full rebuilds, and how to watch them. The pipeline is two Container Apps jobs, `caj-usnm-ingest-{env}` (`usnm-ingest run`: new LoC batches, the title catalog and a release) and `caj-usnm-backfill-{env}` (parallel `curate` workers for the initial corpus). Both run inside the VNet as `id-usnm-ingest`; the storage and Cosmos accounts stay private throughout. How they are deployed is in [`infra/README.md`](../infra/README.md); the design is in [08](design/08-azure-infrastructure.md) and [04](design/04-data-sources-and-ingestion.md).

Every command below uses your own `az` sign-in. CI can only roll the API onto new images, so starting jobs and changing settings is always a person's action.

## Deploy the jobs

`scripts/bootstrap.sh <env> --ingest` deploys both jobs (or `scripts/settings.sh <env> USNM_INGEST_JOBS true` and `scripts/provision.sh <env>` on an environment already on the registry). `USNM_BACKFILL_WORKERS` sets the backfill's workers (default 4), and `USNM_BACKFILL_CRON` an optional schedule for it (a UTC cron such as `"0 9 2-4 10 *"`; empty is manual), which every deployment keeps. The ingest job's Quickwit writer works on an NFS share, `USNM_INGEST_SCRATCH_GIB` in size (default 128; 0 removes it, 08 §8.4). The weekly schedule comes after the first release ([Weekly updates](#weekly-updates)).

## The title catalog

The catalog (`reference/catalog/titles.json`, `places.json`) is built by `titles-sync`, which `run` calls before every release. A batch is released only once its titles are in the catalog. LoC rate limits title records, often well under its published 20 a minute; titles-sync then sends nothing for 65 minutes and resumes more slowly, until 8 h into the run (`--titles-max-runtime-secs`), and the next run continues from where it stopped. `/v1/status` shows `titles.pipeline.awaiting_sync` (titles still to fetch) and `batches_waiting_for_titles`. Place corrections live in git and ship in the ingest image; the next run applies them (04 §4.6):

- **A misspelled or renamed town** that should share a place with the town's other titles: add `{"from": "Skaguay", "note": "old spelling", "state": "AK", "to": "Skagway"}` to `catalog/overrides/place-aliases.json`. `from` is the city as LoC spells it (spelling variants such as "St."/"Saint" or a trailing state abbreviation already match); `to` is the town's name, which becomes the place's name. Keep the entries sorted by state, then `from`, and each entry's keys in alphabetical order; `cargo test -p usnm-ingest places` checks both, and refuses an alias whose `to` is another alias's `from`. Only merge towns that are one place on the map; a town that was separate (East Providence, Winston before 1913) keeps its own place.
- **Wrong coordinates or name**: add `{"city": "Lusk", "lat": 42.7625, "lon": -104.4522, "state": "WY"}` to `catalog/overrides/places.json`, with `"name"` to change the place's name and `"precision"` (`city`, `county` or `state`) if needed. Take the coordinates from the gazetteer (`catalog/gazetteer/us-places.csv`) or another public source, not from LoC. The `geocode` log lists places where LoC's point and the gazetteer's are more than 25 km apart and LoC's was kept (`furthest` in its warning, `disagreement_examples` in the report): start there. A LoC point more than 100 km from the gazetteer's town of that name (when the name isn't ambiguous there) is replaced by the gazetteer's already (`gazetteer_over_loc_places`); check that list too, since the gazetteer can have the name for a different town than the paper's.
- **A title whose record lists several places** (a paper that moved): add `{"city": "Chicago", "lccn": "sn84024055", "note": "…", "state": "IL"}` to `catalog/overrides/title-places.json`, sorted by `lccn`, keys in alphabetical order.
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

Before it publishes, a release waits for the writer's merges: `release merges` lines (every 30 s: step `settle` then `finalize`, splits, merges running and queued, free disk), then `merges settled; closing the index for its final merges`, then `merged; the index is closed to further writes` with the new index's split count. It then logs one `index layout` line per index of the new version and records the same under `indexes` in `reference/<version>/manifest.json`. `scripts/logs.sh prod index-layout 2d` lists them. A merged index has at most `pages / 60,000` (rounded down) `+ 7` splits (`split_num_docs_target` in `infra/quickwit/pages-index.yaml`); the release fails rather than publish more, or if merges don't finish within `USNM_MERGE_TIMEOUT_SECS` (default 14400, 4 hours; a setting, `scripts/settings.sh <env> USNM_MERGE_TIMEOUT_SECS <secs>` then `scripts/provision.sh <env>`, budgeted under the ingest job's 48-hour replica timeout (the backfill and Japanese OCR jobs keep 24 hours) in `infra/modules/ingestjobs.bicep`), or if the writer reports a full disk or a failed merge, or exits. A writer the kernel killed for memory shows as `the Quickwit writer was killed by signal 9 (SIGKILL): it ran out of memory (…)` in the `command failed` line; `quickwit_rss_mb` in `release progress` is its memory (08 §8.4).

## Full rebuild

Rebuild the indexes as one merged base (one-off, or a compaction). A release also builds a full base by itself, without `USNM_INGEST_FULL`, when the published version was built with another search backend or another version of the common-word pairs (05 §5.5.3); the steps below force one. It is also how merged places and other place changes of published titles reach the site (04 §4.6). Indexes built before the merge settings (September 2026) keep their many small splits: their splits are past any maturation period, so nothing merges them in place, and they are sealed. A full release builds a new base from every curated batch, merged, and publishes a version with no deltas; the previous version stays in Blob for rollback (`current.json`). It takes the writer lock like any release, so it never runs alongside another writer (08 §8.4.1). Run it when no backfill or ingest execution is running and the backfill has finished:

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

**Why the E4 profile.** The first full rebuild with the common-word pairs (#154) was killed when the Quickwit writer ran out of memory at the Consumption profile's limit (7.5 GiB with the scratch share's init container), 6.5 h in, at 26.5% (#172). On the E4 profile the ingest container gets 3.0 vCPU and 25 GiB, and the writer a 6 GiB indexing heap, a 120 s commit timeout and a 4 GiB ingest queue (`USNM_WRITER_HEAP`, `USNM_WRITER_COMMIT_SECS`, `USNM_WRITER_QUEUE`, set by `infra/modules/ingestjobs.bicep`), so it writes splits at the 60,000-page target instead of cutting small ones every 30 s and merging them (#172). The `writer tuning` line at the start of the release shows them. A full run also takes longer than the October 2026 rebuild (about 230 to 460 pages a second instead of 750), which is why the ingest job's replica timeout is 48 h (#173). Benchmark afterwards with `scripts/bench-cold-searches.py`. The commit timeout was 600 s at first: the ingest queue's write-ahead log is only truncated when a commit is published, so it filled in about 4 minutes and Quickwit refused pages ("no shards available") for the rest of each 10 minutes, and the October 6 rebuild was stopped at 15% after averaging 132 pages/s. At 120 s the queue holds about 2 minutes of pages.

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

`jaocr.py mixed` (`ja-ocr/mixed.py`) measures two gaps the Japanese OCR leaves. The OCR job reads a page only when LoC's text for it is missing, empty, short or under 35% word-like; a Japanese query searches only our OCR, any other query only LoC's text.

- Japanese on pages LoC read as words: a page with an English column and a Japanese one, read in English, can pass the 35% test, and its Japanese column is then garbled Latin. For every page with LoC text of a title that lists Japanese, in the batches that hold such titles, it counts pages by word-like share (bands from under 0.35 to 0.9 and over); the other titles in those batches, same scanning and OCR but English only, are the control.
- English on the pages we read: NDLOCR-Lite reads some English (mastheads, short lines), but only into the Japanese index. For every page of our OCR it counts the tokens that are among English's 5,000 most frequent words (3 letters or more).

```sh
scripts/ja-ocr/start-quality.sh prod mixed          # prints the execution's name
scripts/ja-ocr/quality-rows.sh prod <execution>     # its "mixed pages" rows, once it has finished
```

It writes `audit/mixed-pages-<version>-<execution>.json` and a CSV per table in the curated container (named by execution, so two runs never write the same files): `loc-text-bands` (Japanese titles against the control), `loc-text-by-title`, `loc-text-samples` (15 page ids per band, with their loc.gov links, for looking at the scans), `our-ocr-english` (by why LoC's text wasn't used), `our-ocr-by-title` and `our-ocr-samples`. One replica of the execution does the work, renewing a lock; the other waits for its finished marker (`mixed pages written by another replica`) and takes over if the lock goes stale, and a retried replica of a finished execution exits at once. `our-ocr-english` and `our-ocr-by-title` also give the median Latin share of our OCR's letters and the share of pages over half Latin.

## American Stories against LoC's text

`jaocr.py american-stories --year Y [--year …] [--sample-pct 10]` (`ja-ocr/american_stories.py`, #205) compares [American Stories](https://huggingface.co/datasets/dell-research-harvard/AmericanStories) (Dell et al. 2023, CC BY 4.0), a re-OCR of Chronicling America, with LoC's text on our pages. It streams each year's tarball from Hugging Face (0.4 to 7.6 GB; nothing is written to disk), joins scans to our pages (a scan named `<date>_p<page>_<lccn>_…` is our `<lccn>_<date>_ed-<n>_seq-<page>`), keeps the audit's fixed sample (`quality.sampled`), and reads LoC's text for the sampled pages of those years from the batches whose dates overlap them. For pages of titles whose first language is English it reports the join, words per page, the audit's English damage rate and function-word share for each text, pages matching a fixed term list in either text, and LoC's damage rate by the share of regions American Stories marked Illegible.

```sh
scripts/ja-ocr/start-quality.sh prod american-stories --year 1865 --year 1925
scripts/ja-ocr/quality-rows.sh prod <execution>    # its "american stories" rows
```

It writes `audit/american-stories-<version>-<execution>.json`. One replica of the execution works (`ja-ocr/solo.py`, as `jaocr.py mixed` does).

## Search log

The API keeps every search from the site, with only its filters, page count and UTC day, in the `searches` container ([ADR-0012](design/adr/0012-anonymous-search-log.md), 06 §6.8). Nothing expires `searches/days/` and `searches/import/`; staged batches are deleted after 7 days and stay in soft delete for 14 more. To read it:

```sh
scripts/settings.sh prod USNM_SEARCH_LOG_READERS "$(az ad signed-in-user show --query id -o tsv)"
scripts/provision.sh prod
scripts/searches.sh prod 30      # searches per day, top queries, queries that found nothing
```

The data account is reachable only through its private endpoint, so run `searches.sh` from a network that reaches it. The first day file appears an hour after the first full UTC day with the log deployed.
