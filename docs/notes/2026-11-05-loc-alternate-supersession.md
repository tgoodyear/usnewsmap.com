# Pinned supersession of bulk re-recognition by verified LoC OCR

Follow-up to [decision previews](2026-10-28-loc-alternate-decisions.md), for
[#811](https://github.com/tgoodyear/usnewsmap.com/issues/811). This adds the
scheduler half of step 5 of the [import contract](2026-10-27-loc-reuse-import-contract.md):
an explicit, independently pinned eligibility change that may suppress
**future** bulk recognition of an exact page whose selected third-party
reading still verifies. It is off by default and selects nothing on its own.

## Contract

A supersession list is JSON of at most 256 KiB and 256 entries, one per page:

```json
{
  "format": "usnm-ocr-alternate-supersession/1",
  "reason": "<why these pages need no new recognition>",
  "evidence": "<link to the review>",
  "entries": [
    {
      "doc_id": "<canonical page ID>",
      "batch": "<base batch>",
      "batch_version": 1,
      "source_image_sha256": "<decision subject scan hash>",
      "baseline_sha256": "<decision subject baseline hash>",
      "candidate": "<selected staging candidate SHA256>",
      "selection": "<id of the exact select decision record>"
    }
  ]
}
```

`usnm-ingest ocr-alternate-supersession --list list.json` verifies every
entry now and stores the exact bytes create-only at
`ocr-local/alternate-supersession/<sha256>.json`. Storing a list changes
nothing. Only setting `USNM_OCR_SEED_SUPERSESSION` to that SHA-256, through a
settings PR, turns the skip on.

With the setting, each seeding run (`ocr-sync`'s seed step and `ocr-seed`)
loads the pinned list and refuses to run if it is missing, malformed or its
bytes don't hash to the pinned name. For a **new** bulk page that passes every
existing skip, a listed entry for the same `doc_id`, base batch and batch
version is verified at most once per run: the decision history's committed
head and chain, the current selection being exactly the listed `select`
record and candidate, the candidate's retained artifacts and cleared rights,
and the paired comparison, as `ocr-alternate-preview` revalidates them. Only
then is the page left out, as `superseded_alternate`. The report's
`supersession` section counts superseded, unverified and other-copy pages.

Anything else queues the page normally and logs its page ID and reason, never
text:

- a retained baseline, rejection or revocation;
- a reselection under a different decision record;
- missing or corrupt evidence or artifacts;
- a store error while verifying;
- another batch copy or version of the page.

A damage-source unit with a superseded page is never marked finished, so its
page is queued in the first run after its selection stops verifying.

Retries of a plan's own pages, `ocr-enqueue` (hit requests and named pages),
`ocr-requeue`, and queued, leased or in-flight work are untouched. The BOINC
batch scheduler only claims messages already queued and is unchanged.

## Limits

The scheduler has no scan hash. That the decision subject's scan is the copy
the batch publishes rests on the staged registration attestation, which the
entry's batch and batch version bind but cannot recompute. Verification
rereads each listed candidate, image included, once per run when its page is
reached; the 256-entry bound limits that cost.

## Evidence

Synthetic fixtures run the real seeder against local stores:

- With no setting, a valid selection changes nothing.
- With the list pinned, only the listed copy is left out. An entry for another
  batch version is queued, as are pages whose comparison evidence no longer
  revalidates.
- After revocation the page is queued.
- The fetched source skips the same way.
- Publishing refuses an entry whose selection isn't current, and duplicate
  pages.
- Seeding refuses a missing list and bytes that don't match the pinned hash.

A read-only listing of the production curated store on 5 November 2026 found
no objects under `ocr-local/alternates/`, `ocr-local/alternate-decisions/` or
`ocr-local/alternate-supersession/`. Production's ingest settings on CT 215
don't set `USNM_OCR_SEED_SUPERSESSION`. No page is selected, so confirmed
avoidable pages remain **0**, and avoided cost remains unknown.

Turning this on still needs a real selection: authoritative same-scan
registration, independently cleared text, ALTO and layout rights, and the
[#810](https://github.com/tgoodyear/usnewsmap.com/issues/810) same-source
human-reference comparison. It then needs a maintainer settings PR that pins
a reviewed list. Bounded normal import into release and publication (#780)
remain separate.
