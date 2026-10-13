# ocr-sync and BOINC results

A note on `ocr-sync` and results from the BOINC route. Nothing in `ocr-sync` has been changed for
it. Read at `usnewsmap.com` commit `d9d009e` (2026-10-13): `crates/usnm-ingest/src/ocr_local/`
(`checks.rs`, `sync.rs`), `crates/usnm-core/src/ocr_engine.rs`,
`docs/design/13-ocr-engines.md` section 13.8, and `crates/usnm-ocr-worker/README.md`.

## Why

The BOINC route runs a native Tesseract (a static binary with the traineddata as files, no
container). `ocr-sync` refuses `image=local`, and the worker's engine tag assumes a pinned
container image. The question is what a BOINC row must carry to pass, and whether `ocr-sync`
has to change.

## How ocr-sync decides today

`checks.rs`, per part (a part is one row for one page):

1. Name and size. `pages/<lccn>_<YYYY-MM-DD>_ed-<n>.<node>.<stamp>.parquet`; the node is 1 to
   32 letters, digits and hyphens; the stamp is `YYYYMMDDTHHMMSSffffffZ`; the part is at most
   1 MiB. The ALTO is `alto/<doc_id>.<node>.<stamp>.xml`, at most 32 MiB.
2. The strict reader (`read_node_part`): exactly one row; its issue equals the name's.
3. Asked for: the page is outstanding in the ledger, and the row's `batch` and `loc_text`
   equal the queued batch and reason.
4. Text: at most 512 KiB, no control characters but newline and tab, and `text_sha256`,
   `text_chars` and `text_status` equal what the text gives.
5. Provenance: `ocr_source` is in `ENGINE_SOURCES` (`usnm-tesseract`, `usnm-kraken`,
   `usnm-doctr`); `ocr_engine` is at most 512 characters, has no control character and has no
   field equal to `image=local`; `loc_text` is `bulk` or `user`; the batch is in the published
   version; the title is catalogued and not Japanese; `image_url` starts with
   `https://tile.loc.gov/`.
6. The ALTO passes `usnm-alto`'s bounded reader.

Check 7 (the runs record) is not built. `ocr-sync` does not compare the tag's digests to
`engines.lock.json` or `models.lock`: `engine_tag_ok` looks only at length, control characters
and `image=local`. (The refusal is therefore a refusal of one literal field, not a check that
the image is pinned.)

## How BOINC results are tagged

The assimilator (`boinc/src/assimilate.rs`) writes, per page:

| Part field                       | Value                                                                                                                                | Source                                               |
| -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------- |
| node (in the file names)         | `boinc`                                                                                                                              | constant `assimilate::NODE`                          |
| stamp                            | the server time at assimilation, with microseconds set from the result id and the page's index so each page of a result gets its own | `assimilate::stamp`                                  |
| `ocr_source`                     | `usnm-tesseract`                                                                                                                     | on the allowed list; no change                       |
| `ocr_engine`                     | `tesseract/5.5.0 profile=frak2021 image=sha256:<12 hex> models=sha256:<12 hex> route=minimal@r1 contract=0`                          | see below                                            |
| `batch`, `loc_text`, `image_url` | copied from the workunit's manifest, never from the volunteer's result                                                               | manifest written by the staging step from the ledger |
| `text`, hash, chars, status      | from the validated canonical result                                                                                                  | `result.json`                                        |
| ALTO                             | from the validated result                                                                                                            | see "Gaps"                                           |

The tag's digests:

- `image` is the first 12 hex digits of the SHA-256 of the app version's signed bundle
  (the manifest of the `usnm_ocr` app version's files: the `ocr-run` driver, the static
  `tesseract`, and the wrapper job file). It is not a container image digest; there is no
  image. It is the same kind of value: a hash of everything that ran the engine.
- `models` is the SHA-256 of sorted lines `<name> <sha256>` over the traineddata files, as
  13 section 13.8.1 defines it. `result.json` carries each file's SHA-256, so the assimilator
  can compute it, and the validator already rejects a result whose hashes are not the pinned
  ones.
- `image=local` is never written. A BOINC row says what ran, with a hash that identifies it.

The BOINC-specific provenance has no column in the overlay row. It goes in the ALTO
`Processing` description (13 section 13.8.2 says the description carries the provenance): the
workunit name, the result id, the validation state (`replicated`, `single_trusted`), and the
app version. BOINC host and user ids stay in the BOINC database and join through the workunit
name and result id. No email, no IP.

A page whose only result is `single_untrusted_fallback` is not written at all. The decision
sits in the assimilator, so `ocr-sync` needs no new rule for it.

## What passes today without a change

With the table above, a BOINC part passes checks 1, 2, 4 and 5, because the tag has no
`image=local` and the source is allowed. Checks 3 and 6 depend on the staging step and the app
(next section).

## Gaps (BOINC side, to build)

0. The staged-page manifest (`boinc/src/manifest.rs`) carries page id, size, MD5, language and
   profile only. It needs `batch`, `loc_text` and `image_url` too, and the assimilator must read
   them from the workunit's manifest. Not built; the page id also has to be the doc id
   (`lccn_date_ed-N_seq-N`), which `assimilate::sync_names` already requires.
1. The ledger. A BOINC workunit may carry only pages that are outstanding in the ledger, with
   their batch and reason. The staging step must read them from the queue's ledger, not from
   the damage tables, and the same page must not also be on an `ocr-bulk` message that a node
   will take. Not built.
2. ALTO. `ocr-sync` requires an ALTO per part. The worker gets its ALTO from the engine
   adapter (13 section 13.8.2: pixel units in the frame of the full IIIF image, the
   `sourceImageInformation` file name, `LANG` from the page, a description). The BOINC app today runs the
   `tesseract` command and returns text. It needs the same ALTO writer, or the same
   conversion, and `result.json` must carry it. UNVERIFIED: ALTO size for a 36-megapixel
   page. Four pages may exceed the 8 MiB output cap in `usnm_ocr_out`; measure and raise
   `max_nbytes`.
3. The text of the static build must match the container build (open question 5 in
   design.md), or the validator's thresholds must change.

## What would change in ocr-sync (a separate change to the main code, not made here)

None of these is needed for the first pass; each makes the arrangement honest.

1. Pin the digests. Add a `native` kind to `engines.lock.json` (13 section 13.7) with the
   bundle digest and the models digest, and make check 5 compare the tag's `image` and
   `models` to the lock. Today it compares neither, so a wrong digest passes. With the check
   in, the BOINC bundle digest must be in the lock before its rows pass, which is the control
   we want when an app version changes.
2. Tie the node to the digest kind. A node named `boinc` may carry only a `native` digest; a
   container node only an image digest. This stops a container node from claiming a bundle
   and the reverse.
3. Check 7. When the runs record exists, BOINC rows get one: workunit, result id, validation
   state and credit, in the same schema as the worker's, with `node: boinc`.
4. A quarantine code per reason so BOINC rejects are countable on the dashboard (the codes
   already exist: `alto_missing`, `engine_tag`, `not_requested`, and so on).

## Not part of this

- The write side: the identity that may write `pages/*.boinc.*` and `alto/*.boinc.*` in
  `ocr-out` is a BOINC deployment matter (design.md section 4.3).
- Replacing the worker for any route.

## Open points for the maintainer

1. Is a bundle digest in the `image=` field acceptable, or should the tag grammar get a
   distinct field (for example `bundle=sha256:...`)? A new field changes `OcrEngineTag` and
   the parser, so it is more work and more honest.
2. Should BOINC rows in `ocr_local_pages.bin` use the same short key (`tesseract/5.5.0
frak2021`) as the worker's? The short key hides the image, so yes unless the quality of
   the two differs enough to show.
