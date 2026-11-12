# BOINC dashboard feed

A Cloudflare Worker that serves the figures of the public [OCR dashboard](https://ocr.usnewsmap.com/) as a JSON feed. A [cloud-boinc](https://github.com/tgoodyear/cloud-boinc) project site can show a project's own figures as cards from such a feed, and it shows these on its jobs page (`work.php`). `GET /feed.json` returns the document; any other path is a 404 and any other method a 405.

It reads two public endpoints and nothing else:

- `https://ocr.usnewsmap.com/api/pipeline.json`: the corpus data-flow stages.
- `https://ocr.usnewsmap.com/api/view.json`: the fetch queue, volunteer activity and throughput.

It has no bindings and no secrets, sends nothing but a plain `GET` to those two addresses, and logs only a reason when one fails (never an address or a body). It changes nothing on the dashboard or anywhere else.

## What the feed shows

Six cards, each linking to the full dashboard. Every figure is a number the dashboard shows, or a plain sum its own code defines, under the label the dashboard uses. A label that is too long for the feed's limit of 60 characters is shortened; a trailing ", last 14 days" moves under the figure.

| Card             | Figures                                                                                                                                                                                                                                                                                              |
| ---------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Jobs in flight   | BOINC result replicas, unsent and assigned, in total and for `usnm_ocr`, `usnm_qwen`, `usnm_quality` and `usnm_gold` (pipeline stages `boinc_*`).                                                                                                                                                    |
| Pipeline         | The OCR pages' journey in order: the `ocr-bulk` queue, pages at BOINC or awaiting collection, then, for the last 14 days, validated workunits, collected pages, imports and rejected parts (stages `queued`, `allocated`, `validated`, `collected`, `imported`, `rejected`).                         |
| What's next      | Pages still to fetch (`fetch.queue_summary.pages_left`), split into those for OCR and for the quality checks as the dashboard does from `fetch.queue`, and the days the rest takes at the last day's pace (pages left divided by `fetch.last_day.pages`, rounded up).                                |
| What's been done | Page scans fetched so far (`fetch.total.pages`), pages searchable with our OCR (stage `indexed`) and quality and Qwen pages loaded in the last 14 days (stage `quality_loaded`).                                                                                                                     |
| Volunteers       | Volunteers active in the last 3 hours, jobs finished in the last 24 hours and the jobs by kind, from `boinc_week`, summed the way the overview page sums them.                                                                                                                                       |
| Throughput       | Scans fetched, OCR pages imported and pages scored in the last 24 hours, the 7-day average of OCR pages imported a day, results back and valid results in the last full hour, and two daily bar charts (pages fetched and OCR pages imported a day) from `throughput.steps[].days` and `days_start`. |

The two last-full-hour figures (`throughput.results_last_hour` and `valid_last_hour`) are in the JSON but the dashboard's page does not show them; their meaning is documented in `ThroughputView` in `crates/usnm-ocr-dash/src/throughput.rs`.

### What is unknown

A figure the dashboard would not show is sent as `null`, which the project site shows as "unknown", never as 0:

- a pipeline stage that is not observed, has no count, is marked historical, or has a stale or missing freshness entry (the dashboard's `summary_stage` rule);
- a 14-day total the catalog did not have every hour of (the dashboard's `fortnight` rule);
- the fetch counts, including the pages-fetched-a-day chart, while the dashboard is warming up or cannot read the fetcher's records, and the queue figures when the queue is not configured or reports an error;
- the volunteer figures when the BOINC week is stale or missing;
- a day of a daily series, and the 24-hour figures, when not every hour of it was counted.

When one of the two endpoints cannot be read, its figures are unknown and the cards built from the other stay. When neither can be read the Worker answers 502.

Each card's `updated` is the dashboard's own observation time for its oldest figure, so the project site's "stale" badge reflects the real age of the data. A source with no observation time is left out of that, so it cannot hide the times the others have. A card whose figures have no observation time has no `updated`.

## Caching

The project site fetches the feed about once a minute for each data center. The Worker keeps its answer for 60 seconds (in the isolate, in the Cache API where it works, and with `Cache-Control: public, max-age=60`), shares one round of upstream requests between requests that arrive together, and asks the edge cache to keep each upstream answer for 60 seconds. A Cloudflare location therefore asks the dashboard at most about once a minute per endpoint, however often the feed is requested. Each upstream request has a 2-second limit, inside the project site's 3 seconds for the whole exchange, and a 4 MiB limit counted in bytes as the body is read. A cache that cannot be read counts as empty. The answer is a few kilobytes against the feed's limit of 64.

## Deploy

The Worker is one file with no dependencies and no build step. With Wrangler:

```sh
cd edge/boinc-feed
npx wrangler deploy
```

`wrangler.jsonc` enables the `workers.dev` name and nothing else. With the API instead, upload `worker.mjs` as a module Worker: `PUT /accounts/<account>/workers/scripts/<name>` with a multipart form holding a `metadata` part (`{"main_module": "worker.mjs", "compatibility_date": "2026-10-01"}`, no bindings) and the module, then `POST /accounts/<account>/workers/scripts/<name>/subdomain` with `{"enabled": true, "previews_enabled": false}`.

The feed's address is then `https://<name>.<account subdomain>.workers.dev/feed.json`. The project site needs a feed on a hostname other than its own, and `workers.dev` is one.

## Configuring a cloud-boinc site

Set `dashboard_feed_url` in the site's config to the feed's address. With cloud-boinc's `cboinc` tool, which changes only that option:

```sh
cboinc config-set --url https://<project site> --token-file <admin token file> \
  --tag dashboard_feed_url --value https://<name>.<account subdomain>.workers.dev/feed.json
```

`work.php` shows the cards within a minute or two (its own page cache). If the address is wrong or the document is refused, the page shows a note that the figures are not available and the rest of the page is unaffected. The feed's rules are in cloud-boinc's `cloud/docs/dashboard-feed.md`.

## Tests

```sh
node --test edge/boinc-feed/worker.test.mjs
```

They run offline on Node's own test runner. `fixtures/` holds two small documents shaped like the dashboard's, with made-up values on a fixed day and only the fields the Worker reads and a few beside them; `fixtures/make-fixtures.mjs --out <directory>` writes them again. The test file carries a validator for every rule of the feed schema (limits, types, unknown fields, times, size), and checks that each document the Worker builds passes it: from the fixtures, with each source missing or stale, and with odd input. The fetching and caching run against stand-ins for `fetch` and the Cache API: one round of upstream requests per minute, requests arriving together, a cache that fails, a slow or oversized upstream and the 404 and 405 answers.
