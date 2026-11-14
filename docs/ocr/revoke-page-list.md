# Revoking our OCR text for a list of pages

A revocation file can name pages directly. Use it when the pages to give back
to LoC's text are picked by a comparison (for example by decade and damage
band) and no node, engine, run or time window selects them. Pages whose rows
are all withdrawn keep LoC's text at the next full index rebuild; published
indexes keep ours until then. Nothing is revoked until the file is in the
curated store, and nothing is rebuilt by writing it.

## The file

`ocr-local/revoked/<stamp>.json`, create-only, in the curated store:

```json
{
  "reason": "worse than LoC's text in these decade and damage cells (#968)",
  "reference": "#835",
  "at": "2026-11-13T00:00:00Z",
  "source": "usnm-wf1415-replaced-v20261026-1.tsv; rows where decade and band are in the revoke cells",
  "engine": "tesseract/5.5.0 frak2021",
  "doc_ids": ["sn84026749_1894-05-01_ed-1_seq-1", "..."]
}
```

- `doc_ids` lists doc ids; `doc_hashes` lists the same pages as 16 lowercase
  hex digits of `usnm_state::ocr_local::doc_hash`, the keys of
  `ocr_local_pages.bin`. Either or both may be given.
- `at` and `source` are required with a page list. `reason` and `reference`
  are always required. The release copies them, with the page count and the
  engine, into the version's `ocr_local.json` under `revocations`.
- `engine` is optional: with it, only that engine's readings of the listed
  pages are withdrawn, so another engine's reading of the same page can still
  win. Give the full tag or the short key (`<family>/<version> <profile>`).
- An empty page list is refused, so a filter that selects nothing cannot
  widen the revocation to every page of the engine.
- A file the release cannot read stops the release.

## Producing the list from the decoded table

The table `usnm-wf1415-replaced-v20261026-1.tsv` holds one row per page
release `pages-v20261026-1` replaced, with columns `doc_id`, `engine`,
`batch_index`, `lang`, `decade`, `loc_damage` and `band`. It lives in `/var/tmp`
in the analysis CT; only read it.

Check the values the cells use before filtering:

```sh
T=/var/tmp/usnm-wf1415-replaced-v20261026-1.tsv
head -1 "$T"
cut -f5 "$T" | sort | uniq -c
cut -f7 "$T" | sort | uniq -c
```

Select the cells to revoke. This example is the provisional list on #835
(worst: 1890 and 1900; moderate and low: 1870 to 1910); replace it with the
final rule from #968:

```sh
awk -F'\t' 'NR > 1 && $2 == "tesseract/5.5.0 frak2021" &&
  (($7 == "worst" && ($5 == 1890 || $5 == 1900)) ||
   (($7 == "moderate" || $7 == "low") && $5 >= 1870 && $5 <= 1910)) {
    print $1
  }' "$T" | sort -u > /var/tmp/revoke-doc-ids.txt
wc -l /var/tmp/revoke-doc-ids.txt   # must not be 0
```

Compare the count with the cell counts in the comparison before going on.
Then build the file:

```sh
jq -n --rawfile ids /var/tmp/revoke-doc-ids.txt \
  --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  '{reason: "worse than LoC in these decade and damage cells (#968)",
    reference: "#835", at: $at,
    source: "usnm-wf1415-replaced-v20261026-1.tsv, cells per #968",
    engine: "tesseract/5.5.0 frak2021",
    doc_ids: ($ids | split("\n") | map(select(length > 0)))}' \
  > revoke.json
```

Upload it create-only with Entra login (no SAS, no keys), so it never
overwrites an earlier file:

```sh
az storage blob upload --auth-mode login --account-name "$CURATED_ACCOUNT" \
  --container-name curated --name "ocr-local/revoked/$(date -u +%Y%m%dT%H%M%SZ).json" \
  --file revoke.json --overwrite false
```

The next release run with `--ocr-local` reads every file under
`ocr-local/revoked/`. Its estimate of replaced pages and `ocr_local.json`
show the effect: `pages.kept.revoked` counts the pages given back.

## Holding a BOINC host's pages

A revocation can name BOINC host ids with `hosts`, so the readings one host made can be
given back to LoC (or checked) without a page list. Only that host's reading goes: another
reading of the same page (a retry on another host, a local worker) stays in the race,
because a reading is matched by its page and its `node` (`boinc-<12 hex of the job id>`). It combines with the other
selectors: with `engine`, only that host's readings by that engine; with `doc_ids`,
only the listed pages that host read. Copy `engine` as the stored `ocr_engine` value in
full (`models`, `route` and `contract` included): it is matched exactly, or by its short key.

```json
{
  "reason": "host 12 failed its spot check on image 247c131a5994 (#1863)",
  "reference": "#1855",
  "engine": "tesseract/5.5.0 profile=eng image=sha256:247c131a5994 models=sha256:75ba1066e1a4 route=boinc@r1 contract=0",
  "hosts": [12]
}
```

The release finds a host's pages in `quality.boinc_page_provenance` in the quality database
(`scripts/onprem/quality/provenance.sql`), which is built from the BOINC database while a page's
workunit and result rows still exist; `db_purge` removes them 30 days after the workunit is
done. It reads the table when it loads the revocations, on the replica (CT 236), as the
read-only `quality_reader` role, from the DSN file named by `USNM_QUALITY_DSN_FILE`
(`/etc/usnm/quality-reader/dsn` on the host that has the role's certificate). Nothing is
exported to the lake. The release stops if a revocation names hosts and the variable is
unset, the database can't be reached, or the table has no rows.

- A reading counts for a host only when its status is `resolved`. Readings whose workunit is
  gone are `host_unknown`: they have no host to hold, so they are accepted unless their
  engine image is held (an `engine` or page hold). A table holding only such rows is valid and
  matches nothing. A host whose CPU vendor is empty or unknown stays `resolved` with a null
  `family`.
- A job that ran in two workunits on different hosts is `ambiguous_duplicate_wu`, and
  `candidate_host_ids` keeps every host behind it. Such a reading is held when any candidate
  host is listed, since it may be that host's.
- `ocr_local.json` records `hosts` and `host_pages` for the revocation.

## Holding a single page's reading

For the odd page, add a row to `quality.boinc_page_holds` (`scripts/onprem/quality/holds.sql`)
instead of writing a file. The release reads the table with the provenance, so the hold needs
no upload. A row names the page (`doc_id`) and the reading to give back: the stored
`ocr_engine` tag in full (or its short key), the `node`, or both; `reason`, `created_at` (now)
and `created_by` are recorded. A row naming neither would take every reading of the page, so
the table refuses it. Another reading of the page keeps its place.

Add and remove rows as `postgres` on the primary (CT 231); the replica has them a second later.
The roles that read have `SELECT` only. Adding a row twice leaves one.

```sh
psql -v ON_ERROR_STOP=1 -d usnm_quality <<'SQL'
INSERT INTO quality.boinc_page_holds (doc_id, engine, reason, created_by)
VALUES ('sn88084068_1917-11-01_ed-1_seq-1',
        'tesseract/5.5.0 profile=frak2021 image=sha256:9c8eac0485d5 models=sha256:153518ad562f route=boinc@r1 contract=0',
        'image dropped from gold trust 17 Oct (#1956)', 'tgoodyear')
ON CONFLICT DO NOTHING;
SQL
```

`ocr_local.json` lists the holds as revocations whose path is `quality.boinc_page_holds`, one
for each reason, engine, node and author, with their pages and the latest `created_at` as `at`.
To give a reading back, `DELETE` its row; published indexes keep ours until the next full
rebuild. The other selectors (node, run, time window, a page list from a comparison) stay
files, as above.

## Where the release reads it

Releases run in the `usnm-release` container on pve-big, on the same LAN as the replica. The
replica admits a role only over TLS with its client certificate (`hostssl usnm_quality all
samenet cert`), so the container needs the `quality_reader` certificate, key and CA and a DSN
file naming them, then `USNM_QUALITY_DSN_FILE` set in the release environment. The DSN file
is one line (`host=… dbname=usnm_quality user=quality_reader sslmode=verify-full sslcert=…
sslkey=… sslrootcert=…`) or a `postgres://` URL as the queue uses. A release with `--ocr-local` fails closed on the holds: with the variable unset, the DSN file
missing, a connection, TLS or query error, or no rows in `quality.boinc_page_holds`, it exits
non-zero while it plans, before it writes anything (31,786 `frak2021` revocation rows live
there as of #1956, so a missing credential would otherwise ship those pages). It also stops at
a revocation that names hosts if the provenance can't be read.
`usnm-boinc quality texts-export` (the quality hand-off job) reads the same variable.

### Checking the count: `--expect-revoked N`

After the revocations are applied to the batches the release indexes, it compares the pages
revoked with N and logs both (`pages revoked (kept.revoked)`, with `expected` and `actual`).
Below N it exits non-zero before anything is published. The count is the total
`kept.revoked` in `ocr_local.json`: the pages kept on LoC's text because a revocation file or a
hold row took our reading, and no other reading of the page survived. It is the figure the
planning output shows, and it includes the file revocations as well as the holds. It counts
the batches the release indexes, so give it on a full rebuild; a delta sees only its new
batches. The count stays under the number of rows when a held page has another reading or
isn't in the batches indexed, so take N from a previous full rebuild's `ocr_local.json`, not
from the row count. The full rebuild after #1956 uses `--expect-revoked 31786`.

On-prem, `scripts/onprem/release.sh --step release` and `--step finish` need one of the two:
`--expect-revoked N` (passed to `usnm-ingest release`), or `--no-db-holds`.

### Running without the holds: `--no-db-holds`

A release meant to run without database holds (a rehearsal, a fixture, a rollback build) says
so with `--no-db-holds`; nothing else makes the release skip them. It logs a warning, reads
no database and applies no hold rows. A non-zero `--expect-revoked` is an error with it, and
both flags need `--ocr-local`. The Azure ingest job (`infra/modules/ingestjobs.bicep`) has no route to
the replica, so it passes `--no-db-holds`.

On `usnm-release` (installed 14 November 2026) they are `/etc/usnm/quality-reader/{ca.pem,client.crt,client.key,dsn}` (root:usnm, key mode 0640) and `/etc/usnm/release.env`, which `release.sh` runs get through `systemd-run -p EnvironmentFile=/etc/usnm/release.env` (see the rebuild runbook). The certificate comes from `queue-ct.sh --issue-client --role quality_reader` on pve and is valid 825 days (renew before 2029-02). The container stays stopped between releases.
