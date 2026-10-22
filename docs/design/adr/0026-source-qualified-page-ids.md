# ADR-0026: Source-qualified page identifiers

- **Status:** Proposed (2026-10)
- **Date:** 2026-10
- **Relates to:** ADR-0016; `docs/design/14-corpus-data-flow.md` ("Identities and observations"); #968, #962

## Context

`doc_id` was defined for Chronicling America pages: a LoC page key
(`{lccn}/{yyyy-mm-dd}/ed-{n}/seq-{n}`) with `/` replaced by `_`
(`crates/usnm-core/src/ids.rs`). The field now holds identifiers from other sources too:

- Congress pages: `SERIALSET-00033_00_00-009-0042-0000_p1` (GovInfo Serial Set),
  `unt-{metadc}_p{seq}` (UNT) and `lcg-{series}-{volume}_p{seq}` (Congress.gov)
  (`crates/usnm-ingest/src/congress.rs`).
- OCR benchmark samples, such as `overproof-1`, which Surya staging copies into `Job.doc_id`
  (`crates/usnm-boinc/src/surya/stage.rs`), and Qwen's `benchmark-{sha256}`.
- Gold bundle crops and pages, such as `pub-bln600-063` (`crates/usnm-ocr-survey/src/bundle.rs`).

Nothing in an identifier says which source it belongs to. The shared checks look only at the
characters (`[A-Za-z0-9_-]`, in `usnm-ocr-contract/src/quality.rs` and
`usnm-boinc/src/assess/mod.rs`, plus Python copies), so they accept every source's ids alike.
Code that assumes the LoC shape then misreads the others:

- `status/assemble.rs` takes the LCCN by splitting on `_`.
- The API's viewer link parses every hit with `PageKey::from_doc_id` (`usnm-api/src/routes/hits.rs`).
- Surya page units and benchmark units share one `units/<id>` directory.

`docs/design/14-corpus-data-flow.md` already proposes `loc:<doc_id>`, and only
`scripts/data_catalog.py` checks for it.

## Decision

1. **A page identifier is `<source>:<local id>`.** `<source>` matches `[a-z][a-z0-9-]{0,31}`.
   The local id is the source's own identifier, unchanged, checked by that source's rule.
   The registered sources:

   | Source      | Local id                                        | Example                                            |
   | ----------- | ----------------------------------------------- | -------------------------------------------------- |
   | `loc`       | the LoC `doc_id` (`PageKey::doc_id`)            | `loc:sn85055101_1917-04-03_ed-1_seq-3`             |
   | `serialset` | GovInfo package id and page                     | `serialset:SERIALSET-00033_00_00-009-0042-0000_p1` |
   | `unt`       | UNT page id                                     | `unt:unt-{metadc}_p{seq}`                          |
   | `lcg`       | Congress.gov page id                            | `lcg:lcg-{series}-{volume}_p{seq}`                 |
   | `bench`     | benchmark release and sample                    | `bench:usnm-ocr-bench-v1.0.0/overproof-1`          |
   | `gold`      | gold bundle manifest digest and crop or page id | `gold:{manifest sha256}/pub-bln600-063`            |

   Adding a source means adding a row here and its rule in code. A crop or rendition doesn't get
   its own page identity. It records its parent page and its coordinate convention,
   as `14-corpus-data-flow.md` says.

2. **A bare id means `loc`.** LoC `doc_id`s contain no `:`, so every stored LoC id stays
   valid as it is. The Quickwit fields, the OCR ledger, blob paths, queue messages and Cosmos
   keep their current values. Nothing is rewritten, and the search rebuild isn't affected. A
   store that holds one source only, such as the Congress index, may keep that source's bare
   ids, and its schema names the source.

3. **Qualify wherever sources can mix.** That covers BOINC staging and units (Surya, quality,
   gold, Qwen), the data catalog, the API's viewer link (which picks the link by source:
   loc.gov for `loc`, GovInfo for `serialset`, and so on) and any new shared store. In file
   and blob names, the qualified id becomes the path `<source>/<local id>` (bare LoC paths
   stay as they are).

4. **One type in `usnm-core`.** `PageId { source, local }` parses and prints this form, and
   gives a `PageKey` only for `loc`. It replaces the two Rust charset checks, and the Python
   copies are replaced by it or by a generated equivalent.

## Consequences

- LoC-only code keeps working with no change, and code that may see other sources has to
  check the source before parsing.
- `status/assemble.rs` and the viewer link stop misreading Congress ids, and Surya's two kinds
  of unit no longer share a directory.
- By design, every new source needs a row and a rule before its ids enter shared stores.
- BOINC campaigns already staged with bare benchmark ids, such as the Surya campaign
  `gold-bench-20261022`, finish as they are. New campaigns use qualified ids once the staging code does.
