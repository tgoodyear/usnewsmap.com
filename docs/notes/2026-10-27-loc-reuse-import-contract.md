# LoC reuse: executable safety fixtures and the ingestion boundary

For [#811](https://github.com/tgoodyear/usnewsmap.com/issues/811), the
non-production design and safety fixtures do **not** depend on finding a
production-eligible replacement. Deferring all code work until #810 produces
one was incorrect.

## What runs now

The `loc_reuse_fixture_` tests in
[`usnm-ocr-local`](../../crates/usnm-ocr-local/src/tests.rs) use temporary local
object stores, the actual `sync::run`, import ledger, quarantine, and release
`Listing::load` / revocation reader. They make no network requests and use
synthetic worker text, not refreshed LoC text or human ground truth.

```sh
cargo test -p usnm-ocr-local --locked loc_reuse_fixture_
```

The combined fixture imports an ordinary supported synthetic worker reading,
replays the import pass, submits a conflicting later reading, and revokes the
first import by its recorded run. It verifies:

- The same page ID and selected text survive replay without another imported
  part or import record.
- A conflicting reading does not overwrite the accepted reading or bypass
  normal eligibility; it receives a separate quarantine reason record.
- A requested page with no output remains outstanding and has no replacement.
- Revocation removes the candidate from release selection, marks that page
  revoked, and retains the original baseline, imported part and import receipt
  byte-for-byte. The release can therefore retain its original LoC baseline.
- An unknown `loc` engine cannot masquerade as our worker output; malformed
  ALTO also cannot import. Both leave the original text and outstanding work.

These are **compatibility/safety fixtures**, not evidence that refreshed LoC
OCR traverses normal import and selection. Existing interrupted-import recovery
is also covered by
`a_run_stopped_before_its_deletes_is_finished_without_importing_again`.
No test constructs a fake known engine version to make a LoC candidate pass.

## Proposed integration, before any production selection

The current normal worker importer is not a generic third-party artifact
importer. `checks::ENGINE_SOURCES` accepts named local worker families;
`check_row` requires a requested page, and the worker ALTO profile is distinct
from published LoC ALTO (including its coordinate units). Broadening that
allowlist or presenting LoC as `usnm-tesseract` would discard provenance and
is not an implementation of reuse.

Use the existing import lock, create-only artifacts, conditional source
consumption and import receipts for an explicit third-party staging adapter.
The adapter must validate a bounded candidate **before** it enters selectable
`ocr-local/pages` parts:

1. Bind canonical page ID and exact source-image checksum to a checksummed
   original baseline and refreshed ALTO/text pair. Require registered
   coordinate scale/orientation, artifact-specific rights evidence, retrieval
   identity and embedded engine/version evidence, or explicit `unknown`.
   Reuse the checksum/geometry checks in
   [`loc_align.py`](../../scripts/ocr-bench/loc_align.py); its crop extraction
   policy is not a whole-page reading-order policy.
2. Keep the candidate unselected, separately addressable by immutable source
   scan and artifact hashes. Repeating that identity returns the existing
   receipt; another hash is a distinct candidate, not an update to old bytes.
   The existing worker path's quarantine-on-conflict is a safety boundary,
   **not** the required alternate-candidate import facility.
3. Keep structural acceptance separate from selection. A selection receipt
   must bind the same-source human-reference comparison from #810, format
   compatibility and explicit decision/reason. Missing coverage, unknown
   rights or failed registration leave the baseline selected. Newness and
   heuristic damage alone never select a refreshed artifact.
4. Record rejection/revocation append-only, preserving bytes, prior selection
   and import lineage. Exercise selecting each of two conflicting candidates,
   repeated staging, malformed/mismatched ALTO, revocation and baseline
   rollback through the new adapter before production acceptance.
5. Only an independently approved eligibility change may suppress **future**
   recognition of that exact page and scan. Never cancel/reassign already
   queued or leased work, shorten leases or change host assignments. An
   invalid, missing or revoked candidate cannot suppress work. Publication
   remains the separately coordinated #780 operation.

There is deliberately no new private ledger, production switch or automatic
selection in this change. The metadata catalog can observe import/selection
receipts; catalog rows alone are not normal imports or publication evidence.

## Remaining acceptance and cost

The [actual archive audit](2026-10-27-loc-package-audit.md) establishes
1,226 exact repair-ID matches in the 1,706-page sample, all outside the
current keep set. It does not establish same-scan identity, a justified
replacement or a quality improvement. Confirmed avoidable pages remain **0**;
avoided inference/download cost remains **unknown**, not 1,226 multiplied by
an assumed cost. Separate one-time archive retrieval and measured eligible
per-page work in a future cost receipt.

The adapter and its candidate-selection fixtures above can be implemented
offline now. A real bounded acceptance additionally needs artifact rights,
authoritative source registration and #810 paired evidence, followed by
normal import/selection and idempotent replay. #811 remains open for those
specific gaps; no live queue, text, source budget, release or index changed.
