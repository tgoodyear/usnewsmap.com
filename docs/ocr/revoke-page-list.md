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

The release finds a host's pages in the provenance files under
`ocr-local/provenance/<stamp>.jsonl`, create-only. They are exports of
`quality.boinc_page_provenance` in the quality database (`scripts/onprem/quality/provenance.sql`),
which is built from the BOINC database while a page's workunit and result rows still exist;
`db_purge` removes them 30 days after the workunit is done. The release stops if a revocation
names hosts and there are no provenance files.

- A reading counts for a host only when its status is `resolved`. Readings whose workunit is
  gone (`no_wu`) have no host and never match; a snapshot holding only such rows is valid and
  matches nothing.
- A job that ran in two workunits on different hosts is `ambiguous_duplicate_wu`. It matches
  only with `"ambiguous_hosts": true`, then for either candidate host. Use that when
  withdrawing a failing host's work and over-matching a few pages is acceptable.
- `ocr_local.json` records `hosts`, `ambiguous_hosts` and `host_pages` for the revocation.

Export the table and publish it. Each file is a full snapshot; of two lines for one page a
resolved one is never replaced by a less complete one, so older files stay valid.

```sh
stamp=$(date -u +%Y%m%dT%H%M%SZ)
psql -d usnm_quality -At -c "SELECT row_to_json(t) FROM (
  SELECT doc_id, job_id, wu_id, canonical_result_id, hostid, app_version_id, platform,
         family, resolved_via, resolved_at, status, image12, image_digest, candidate_host_ids
  FROM quality.boinc_page_provenance ORDER BY doc_id) t" > provenance.jsonl
az storage blob upload --auth-mode login --account-name "$CURATED_ACCOUNT" \
  --container-name curated --name "ocr-local/provenance/$stamp.jsonl" \
  --file provenance.jsonl --overwrite false
```
