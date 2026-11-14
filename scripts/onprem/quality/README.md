# OCR-quality results in PostgreSQL

`usnm_quality` holds every quality signal we have about a page, keyed by `doc_id`, so pages
can be ranked for OCR and for the image pass with one query (#962). Before it, the results
were per-campaign Parquet and JSONL files on the BOINC share and the damage audit's files in
curated. It lives on the queue container's PostgreSQL 17 server (CT231 `usnm-queue`, home-infra
`scripts/lab/queue-ct.sh --quality-db`), beside `usnm_queue` and separate from it: its own
database, roles and `pg_hba` line, the same CA, TLS 1.3 and client certificates only
([ADR-0025](../../../docs/design/adr/0025-onprem-database-client-certificates.md)).

## Schema

`setup.sql` creates the schema `quality` and the roles. Run it as `postgres` in the container;
it's safe to run again.

```sh
psql -v ON_ERROR_STOP=1 -d usnm_quality -f setup.sql
```

| Table                    | One row per                    | From                                                                                                                      |
| ------------------------ | ------------------------------ | ------------------------------------------------------------------------------------------------------------------------- |
| `pages`                  | page of the corpus             | `curated/audit/damage/v2/<batch>_verNN.parquet` (`load.py damage`)                                                        |
| `damage`                 | page                           | the same files: `damage_rate`, `garbage_share`, `function_share`, words                                                   |
| `campaigns`              | source of rows                 | each loader (`v1-wave9`, `damage-v2`, `ocr-local`, a text comparison)                                                     |
| `readers`                | text we hold of a page         | the reader names quality units use (`loc-hist`, `as`, `ours-*`, `qwen`)                                                   |
| `unit_pages`             | page in a quality campaign     | `quality collect`'s `pages.parquet`, or the wave runner's `collected/` parts                                              |
| `reader_scores`          | page, campaign and reader      | the same files: `est_cer`, completeness, findability, the pairwise rates                                                  |
| `text_compare`           | page, campaign and reader pair | text-only comparisons, such as LoC against American Stories over the corpus                                               |
| `ocr_outputs`            | published OCR run of a page    | `curated/ocr-local/pages` (`load.py ocr`)                                                                                 |
| `excluded_estimates`     | campaign whose hat is wrong    | campaigns left out of ranking's estimates until rescored (none now; see #1079)                                            |
| `staging`                | page staged in a campaign      | the feeder's page lists, with the image and ALTO each unit was staged from                                                |
| `loc_alto`               | page and source batch          | LoC's own word confidence (ALTO `WC`) from the bulk OCR archives: mean, median, share under 0.5 (`load.py locwc`)         |
| `loc_software`           | batch version                  | the software that made LoC's OCR, from each bulk OCR archive's first ALTO header (`load.py locsw`)                        |
| `boinc_page_provenance`  | published BOINC OCR page       | which BOINC host, workunit and app version read it, from the BOINC database (`provenance.sql`, #1956)                     |
| `boinc_page_holds`       | held reading of a page         | a reading the release leaves out, by engine tag or node, added by hand as postgres (`holds.sql`, #1956)                   |
| `excluded_qwen_readings` | excluded Qwen unit or crop     | a `usnm_qwen` unit, or one crop of it, kept out of the quality tables until re-read (`excluded_qwen_readings.sql`, #1919) |

`boinc_page_provenance` is filled from the BOINC database, not by `load.py`: run
`boinc_provenance.py` on exports of the workunit, result, host and app version rows, then
`provenance_load.sql` as `postgres` or `quality_loader`. It has to run before `db_purge` removes
those rows; the docstring of `boinc_provenance.py` has the queries. A host with an empty or
unknown CPU vendor keeps its host id and gets a NULL `family`. The release reads the table at
release time, on the replica, as `quality_reader` (`USNM_QUALITY_DSN_FILE`;
docs/ocr/revoke-page-list.md); nothing is exported to the lake.

`provenance-daily.sh` (with `usnm-quality-provenance.{service,timer}`) is the daily run in the batch
container: it skips the readings already `resolved`, resolves the new and unresolved pages
(`boinc_provenance.py --skip`) from a read-only BOINC export that `BOINC_EXPORT` in
`/etc/usnm/quality-provenance.env` produces, and upserts them. Install it with the files in
`/opt/usnm-quality-feed/`, the units in `/etc/systemd/system/`, then
`systemctl enable --now usnm-quality-provenance.timer`. It must run before `db_purge` starts
removing the OCR workunits (about 24 November 2026); once #1959 is deployed and verified the
timer's `OnCalendar` can become weekly. Pages with no workunit row left are `host_unknown`: they
have no host to hold and are accepted unless their engine image is held.

`boinc_page_holds` (`holds.sql`) holds single readings of single pages out of the index: a
`doc_id` with the stored engine tag or the `node`, a `reason`, `created_at` and `created_by`.
Rows are added and removed as `postgres` on the primary; the release reads them with the
provenance. The odd page goes here; node, run, time and page-list revocations stay files in
`curated/ocr-local/revoked/`.

`excluded_qwen_readings` (`excluded_qwen_readings.sql`) keeps `usnm_qwen` readings out of the
quality tables until their unit is re-read. A row has the Qwen `job_id`, the workunit `wu_id`, the
page `doc_id` when known (a bundle has a whole-unit row for each of its pages), a `crop_id` (empty for the whole unit; in a bundle the id is
`<doc_id>:<crop>` as in the unit's `readings.json`), a `reason`, `created_at`, `created_by` and
`released_at`. It is added as `postgres` on the primary and released by setting `released_at`;
rows are never deleted (a trigger refuses a delete and any change but the release). A live row is
one with `released_at` null, and a unit or crop has one live row at most.

- `quality collect` reads the live rows as `quality_reader` (`--qwen-exclusions-dsn-file`, default
  `USNM_QUALITY_DSN_FILE`) and builds no `vlm` reading from an excluded unit or crop: its crops and
  every other reader's rows stay. The exclusions are part of the row cache's key, so adding or
  releasing one re-collects the unit. Without a DSN file nothing is excluded, so run it with one.
- `reader_scores_effective` is `reader_scores` without the vlm readers' rows of a page that has a
  live exclusion with a `doc_id` (reader_scores has no Qwen job, so the page is the join).
  `quality_gain.py` reads it. `page_signals` reads only LoC's and our readers' rows, which the
  exclusions never touch, so it still reads `reader_scores`.
- Readers of `crops.parquet` and `pages.parquet` files made before a unit was excluded
  (`build-quality-assessment-db.py --excluded-qwen`) clear the excluded readings from an export of
  the live rows' `job_id`, `doc_id` and `crop_id`.
- To release, as `postgres`: `UPDATE quality.excluded_qwen_readings SET released_at = now() WHERE
job_id = '<64 hex>' AND crop_id IS NULL AND released_at IS NULL;`, then collect again.

Unit results carry the page hat's version (#1078): `hat_version` 2 is solved on the
passage-matched disagreement, and `est_cer_v1` and the other `_v1` columns keep the unit's
own version 1 values; a table without `hat_version` loads as version 1. When a page is in
several campaigns, `page_signals` takes its version 2 row first, and the hat estimates of a
campaign in `excluded_estimates` stay out.

`page_signals` is a view with one row per page that joins them: the damage score, the newest
unit result's scan features and LoC's `est_cer` there, the best of our readers, the newest
LoC-against-American-Stories comparison (its whole-page `cer_sym`, and agreement v1's
`local_cer_sym` and `matched_loc`), our OCR runs, and whether the page has image features
yet. `page_summary` is its stored copy, which the ranking queries read;
`quality.refresh_summary()` rebuilds it (plainly the first time, then concurrently), and only
the loader may call it. CT231's disk grew to 96 GiB for it (24 October 2026). A change to
`page_signals`' columns needs `DROP MATERIALIZED VIEW quality.page_summary` before `setup.sql`.

## Roles

| Role                  | May                                                                                                                                                               |
| --------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `quality_loader`      | Read everything; insert, update and delete every table but `text_compare`, `readers` (insert only) and `loc_software` (insert and update); refresh `page_summary` |
| `quality_text_writer` | Read everything; insert and update `text_compare`; insert `campaigns`                                                                                             |
| `quality_reader`      | Read everything                                                                                                                                                   |

Each logs in only with a certificate whose CN is the role (`queue-ct.sh --issue-client --role
<name>`), and none has a password. `test_roles.sql` checks every row of this table, and CI runs it
with the loader's upserts and the ranking queries (`ci-check.sh`). Certificates go from CT231 to
the client host through the Proxmox hosts, never through a workstation, and the key stays only on
the client host (mode 0600). The loader's is in CT215 on pve-laptop, `/etc/usnm/quality-db/`.

## Loading

`load.py` runs where the files are, on-prem: CT215, which mounts the curated copy read-only at
`/srv/curated`. Campaign results come from the BOINC server first, host to host
(`boinc/scripts/onprem/quality-feed/quality-results-pull.sh`, on pve-laptop). Each load is COPY
into a temporary table and then `INSERT ... ON CONFLICT`, so a rerun replaces what it loaded.

```sh
DSN="host=usnm-queue.tjg.fyi dbname=usnm_quality user=quality_loader sslmode=verify-full \
  sslcert=/etc/usnm/quality-db/client.crt sslkey=/etc/usnm/quality-db/client.key \
  sslrootcert=/etc/usnm/quality-db/ca.pem"
load.py results --dsn "$DSN" --campaign v1-wave9 --engine sha256:... --file <part>.parquet ...
load.py damage --dsn "$DSN" --curated /srv/curated
load.py ocr --dsn "$DSN" --curated /srv/curated
load.py staging --dsn "$DSN" --campaign v1-wave10 --docs done.txt --image-source keep-set:scanserve --alto-source empty
```

Where each source comes in:

- **`usnm_quality` units:** `quality-collect` (or the runner's own collection) writes Parquet on
  the share; `quality-results-pull.sh` and `load.py results` load it.
- **Damage summaries:** `load.py damage` reads the per-batch damage files from the curated copy.
- **The text-only American Stories comparison:** `usnm-ingest ocr-agreement` (#1062) writes
  one Parquet file per batch version under `text-agreement/v1/`. `load.py agreement`, as
  `quality_text_writer`, loads each finished one (its `.summary.json` is there) into
  `text_compare` as `loc-hist` against `as`, campaign `text-agreement-v1`.
  `usnm-quality-agree-load.timer` runs it hourly in CT215, which mounts that tree read-only
  at `/srv/text-agreement`.
- **LoC's word confidence:** `usnm-ocr-worker loc-alto-stats` in CT211 reads each bulk OCR
  archive in `/tank/locbulk/ocr` into one Parquet file in `tank/locstats`, which CT215 mounts
  read-only at `/srv/locstats`; `load.py locwc` (hourly, `usnm-quality-locwc-load.timer`) loads
  them into `loc_alto`. Ranking averages the damage score with `1 - wc_mean`.
- **LoC's OCR software:** `usnm-ocr-worker loc-software`, run with
  `--archives /tank/locbulk/ocr` and `--out /tank/locstats/software/loc-software.parquet`, reads
  the first page's ALTO header of each archive (about a minute for all 3,011) into one row per
  batch version. The same timer then loads it with `load.py locsw` into `loc_software` when the
  file changes. Software is recorded per batch: `software` keeps every `processingSoftware` in
  document order with its own version, and `engine_family`, `engine_version`, `wrapper` and
  `vendor` come from the rules in `crates/usnm-ocr-worker/src/loc_software.rs`. `page_summary`
  doesn't carry them, because a new column there means rebuilding the 23.8M-row copy; join on
  `(batch, batch_version)`:

  ```sql
  SELECT s.engine_family, s.vendor, count(*) AS pages
  FROM quality.page_summary p JOIN quality.loc_software s USING (batch, batch_version)
  GROUP BY 1, 2 ORDER BY 3 DESC;
  ```

  A few batches mix engines across pages (Apex batches with both PrimeOCR and FineReader
  pages, such as `vi_White_ver01`), and their row names only the first page's engine. The
  page-level `loc_alto.software` shows those pages.

- **OCR readers:** `load.py ocr` records each published `ocr-local` run; their scores against
  the other readers arrive with the next quality unit of the page.

## Ranking

`rank.sql` (as `quality_reader`) holds the queries that order pages:

1. **OCR priority:** pages we haven't OCR'd, by how bad LoC's text is, from the best signal the
   page has (the hat's `est_cer`, else half LoC's disagreement with American Stories, else the
   damage score).
2. **The image pass:** pages with no image features yet, worst damage first, round-robin over
   (title, decade) strata.
3. **Titles:** pages, mean damage and the share badly damaged per title, with OCR and image
   coverage.
