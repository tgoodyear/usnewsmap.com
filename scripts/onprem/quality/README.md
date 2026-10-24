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

| Table           | One row per                    | From                                                                         |
| --------------- | ------------------------------ | ---------------------------------------------------------------------------- |
| `pages`         | page of the corpus             | `curated/audit/damage/v2/<batch>_verNN.parquet` (`load.py damage`)           |
| `damage`        | page                           | the same files: `damage_rate`, `garbage_share`, `function_share`, words      |
| `campaigns`     | source of rows                 | each loader (`v1-wave9`, `damage-v2`, `ocr-local`, a text comparison)        |
| `readers`       | text we hold of a page         | the reader names quality units use (`loc-hist`, `as`, `ours-*`, `qwen`)      |
| `unit_pages`    | page in a quality campaign     | `quality collect`'s `pages.parquet`, or the wave runner's `collected/` parts |
| `reader_scores` | page, campaign and reader      | the same files: `est_cer`, completeness, findability, the pairwise rates     |
| `text_compare`  | page, campaign and reader pair | text-only comparisons, such as LoC against American Stories over the corpus  |
| `ocr_outputs`   | published OCR run of a page    | `curated/ocr-local/pages` (`load.py ocr`)                                    |
| `staging`       | page staged in a campaign      | the feeder's page lists, with the image and ALTO each unit was staged from   |

`page_summary` is a view with one row per page that joins them: the damage
score, the newest unit result's scan features and LoC's `est_cer` there, the best of our
readers, the newest LoC-against-American-Stories comparison, our OCR runs, and whether the page
has image features yet. It's a plain view, not a materialized one: a stored copy of 23.8M
pages, refreshed, would need about twice its size free on CT231's disk, which the queue shares.

## Roles

| Role                  | May                                                                                                   |
| --------------------- | ----------------------------------------------------------------------------------------------------- |
| `quality_loader`      | Read everything; insert, update and delete every table but `text_compare` and `readers` (insert only) |
| `quality_text_writer` | Read everything; insert and update `text_compare`; insert `campaigns`                                 |
| `quality_reader`      | Read everything                                                                                       |

Each logs in only with a certificate whose CN is the role (`queue-ct.sh --issue-client --role
<name>`), and none has a password. `test_roles.sql` checks every row of this table, and CI runs it
with the loader's upserts and the ranking queries (`ci-check.sh`). Certificates go from CT231 to
the client host through the Proxmox hosts, never through a workstation, and the key stays only on
the client host (mode 0600). The loader's is in CT215 on pve-laptop, `/etc/usnm/quality-db/`.

## Loading

`load.py` runs where the files are, on-prem: CT215, which mounts the curated copy read-only at
`/srv/curated`. Campaign results come from the BOINC server first, host to host
(`scripts/boinc/onprem/quality-feed/quality-results-pull.sh`, on pve-laptop). Each load is COPY
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
- **The text-only American Stories comparison:** its job writes `text_compare` rows as
  `quality_text_writer`, `reader_a` `loc-hist`, `reader_b` `as`, under a campaign of its own.
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
