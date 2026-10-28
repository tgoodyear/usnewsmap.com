# Explicit third-party OCR staging

Follow-through for [#811](https://github.com/tgoodyear/usnewsmap.com/issues/811)
and the [import contract](2026-10-27-loc-reuse-import-contract.md).
The worker allowlist is unchanged. LoC output is not relabeled as a known local
engine, and staging is not selection, queue suppression or publication.

## Executable path

`usnm-ingest ocr-alternate-stage --input DIRECTORY` stages exactly one whole-page
reading using the existing curated `ObjectStore` and renewed `ocr-sync` lock.
The directory holds `binding.json` and six exact artifacts:
`source-image`, `baseline.txt`, `source.alto.xml`, `source.txt`, `registration`,
and `rights`. Global store/state arguments are the same as other ingest commands.
Input reads are bounded even if a file grows after its size check.

The binding schema is
[`alternate::Binding`](../../crates/usnm-ocr-local/src/alternate.rs). It includes
the canonical page ID and all six SHA256 hashes, artifact retrieval URL/time,
engine and version evidence, separately classified text/ALTO/layout rights,
explicit non-pixel DPI and the reading policy
`loc-whole-page-alto-document-order-v1`.
An undisclosed engine or version must be the literal `unknown`, not an invented
worker identity.

`registration` is a checksummed JSON attestation with `doc_id`,
`source_image_sha256`, `baseline_sha256`, `orientation: "upright"` and nonempty
`evidence`. It must bind this exact scan, page and baseline. `rights` is a
checksummed JSON record with `alto_sha256`, `text_sha256`, component fields
`text`, `alto`, `layout` (each `unknown`, `cleared` or `restricted`) and nonempty
`evidence`. These must agree with the binding. **These are retained attestations,
not independent proof of rights or same-scan identity.** Unknown/restricted
rights can be staged privately, never implicitly selected or redistributed.

The adapter checks actual JPEG/PNG image dimensions, source/artifact hashes,
bounded safe ALTO, one explicit measurement unit and page, registered dimensions,
and positive in-frame coordinates on every text block, line and word.
Non-pixel units require explicit registered DPI. It rejects ambiguous units,
missing geometry and coordinates the worker parser would normally repair or
clamp. It does not guess rotation or registration from a title/year/page ID.

The separately supplied text must agree with the existing ALTO parser's
document-order reading after whitespace collapse. This is a conservative
compatibility policy, **not** a new assertion that XML order is the correct
whole-page human reading order. Original text/ALTO bytes remain unchanged.
Different reading-order policies need their own adapter/version.

## Commit and replay

Artifacts live only under
`ocr-local/alternates/<doc_id>/<candidate>/`, with `receipt.json` committed last.
The candidate is the SHA256 of the typed binding serialization, including the
exact evidence and retrieval metadata. Exact replay, including after an
interrupted prefix, verifies existing bytes and returns the same receipt.
A different binding is a distinct candidate; an unexpected object at the same
identity fails explicitly instead of overwriting it.

This writes no `ocr-local/pages`, successful worker-import outcome, requested-work
row, queue message, baseline replacement or release pointer. Staged originals
are not consumed. The existing release `Listing` does not see staged candidates.
Logs contain page IDs, candidate hashes and errors, never OCR text.

## Offline evidence and remaining work

The crate tests use synthetic images, text and attestations in temporary stores.
They exercise byte-exact replay after an interrupted prefix, conflicting
readings, retained baseline/ALTO/text, unknown engine/rights, held import lock,
immutable object conflicts, bad checksums, mismatched registration, malformed
ALTO, missing/duplicate units, mismatched dimensions, missing/out-of-frame
geometry and text disagreement. They also run the actual release reader to
confirm candidates remain absent from selection and requested-work ledgers.

```sh
cargo test -p usnm-ocr-local --locked alternate_stage
```

An append-only rejection/revocation/selection interface and explicit release
integration remain required; this change deliberately cannot select anything.
Real #811 acceptance still needs independently checked artifact rights,
authoritative source registration, #810's paired human-reference evidence and
one bounded normal import/selection/replay trace. Existing production queue and
publication approval gates remain unchanged. No live import, LoC request,
policy change or production switch was performed.
