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
awk -F'\t' 'NR > 1 && $2 == "tesseract/5.5.0 frak2021" && (
    ($7 == "worst" && ($5 == 1890 || $5 == 1900)) ||
    (($7 == "moderate" || $7 == "low") && $5 >= 1870 && $5 <= 1910)
  ) { print $1 }' "$T" | sort -u > /var/tmp/revoke-doc-ids.txt
wc -l /var/tmp/revoke-doc-ids.txt
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
