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
  (`crates/usnm-congress/src/lib.rs`).
- OCR benchmark samples, such as `overproof-1`, which Surya staging copies into `Job.doc_id`
  (`crates/usnm-boinc/src/surya/stage.rs`), and Qwen's `benchmark-{sha256}`.
- Gold bundle crops, such as `pub-bln600-063`, each with its upstream dataset in `source`
  (`bln600`) and no parent page (`crates/usnm-ocr-survey/src/bundle.rs`).

Nothing in an identifier says which source it belongs to. The shared checks look only at the
characters (`[A-Za-z0-9_-]`, in `usnm-ocr-contract/src/quality.rs` and
`usnm-boinc/src/assess/mod.rs`, plus Python copies), so they accept every source's ids alike.
Today each place that parses an id knows its source from context. These are LoC-only boundaries,
not current defects, and mixing sources would break them:

- `status/assemble/published.rs` takes the LCCN by splitting on `_`, for the main index's Japanese
  overlay, which holds LoC pages only.
- The API builds a loc.gov viewer link with `PageKey::from_doc_id` for main-index hits. Congress
  hits return their stored source link before that point (`usnm-api/src/routes/hits.rs`).
- Surya page units and benchmark units share one `units/<id>` directory, so their ids could
  collide.

`docs/design/14-corpus-data-flow.md` already proposes `loc:<doc_id>`, and only
`scripts/corpus/data_catalog.py` checks for it.

## Decision

1. **A page identifier is `<source>:<local id>`.** `<source>` matches `[a-z][a-z0-9-]{0,31}`.
   The local id is the source's own identifier, unchanged, checked by that source's rule.
   The registered sources:

   | Source      | Local id                               | Example                                            |
   | ----------- | -------------------------------------- | -------------------------------------------------- |
   | `loc`       | the LoC `doc_id` (`PageKey::doc_id`)   | `loc:sn85055101_1917-04-03_ed-1_seq-3`             |
   | `serialset` | GovInfo package id and page            | `serialset:SERIALSET-00033_00_00-009-0042-0000_p1` |
   | `unt`       | UNT page id                            | `unt:unt-{metadc}_p{seq}`                          |
   | `lcg`       | Congress.gov page id                   | `lcg:lcg-{series}-{volume}_p{seq}`                 |
   | `sample`    | upstream dataset and its own sample id | `sample:bln600/pub-bln600-063`                     |

   Adding a source means adding a row here and its rule in code.

   **An identity names a page, never a version or a container.** A benchmark release, a gold
   bundle or a manifest digest is the version of an artifact that holds pages, not part of
   their identity, so changing an unrelated bundle entry changes no page's id. The same holds
   for crops and renditions, which record their parent page and their coordinate convention
   (`14-corpus-data-flow.md`). A dataset sample with no known parent page, such as a gold crop
   or a benchmark image, is identified as `sample:<dataset>/<sample id>`. `<dataset>` is the
   upstream dataset's stable name (the gold bundle's `source`, or the benchmark's dataset, such
   as `overproof` or `reichsanzeiger`). Where the parent page is known, the crop names it
   instead.

2. **A bare id means `loc`.** LoC `doc_id`s contain no `:`, so every stored LoC id stays
   valid as it is. The Quickwit fields, the OCR ledger, blob paths, queue messages and Cosmos
   keep their current values. Nothing is rewritten, and the search rebuild isn't affected.

   A store whose bare ids aren't LoC keeps its stored values too, but nothing passes its bare
   ids to `PageId` directly. The Congress index holds GovInfo, UNT and Congress.gov pages
   together (`crates/usnm-congress/src/lib.rs`), so its read adapter resolves each
   document's source from that row's `series` and its id's prefix (`SERIALSET-`, `unt-`, `lcg-`) and qualifies the id before building a
   `PageId`.

3. **Qualify wherever sources can mix.** That covers BOINC staging and units (Surya, quality,
   gold, Qwen), the data catalog and any new shared store. Viewer links stay what each source
   provides: Congress hits keep their stored links, and `loc` keeps the loc.gov viewer. In file
   and blob names, the qualified id becomes the path `<source>/<local id>` (bare LoC paths
   stay as they are).

4. **One type in `usnm-core`.** `PageId { source, local }` parses and prints this form, and
   gives a `PageKey` only for `loc`. It replaces the two Rust charset checks, and the Python
   copies are replaced by it or by a generated equivalent.

## Consequences

- LoC-only code keeps working with no change, and code that may see other sources has to
  check the source before parsing.
- The LoC-only boundaries (`status/assemble/published.rs`, the loc.gov viewer link) check the source
  before parsing, so a mixed-source caller fails clearly instead of misreading an id. Surya's
  two kinds of unit no longer share a directory.
- By design, every new source needs a row and a rule before its ids enter shared stores.
- BOINC campaigns already staged with bare benchmark ids, such as the Surya campaign
  `gold-bench-20261022`, finish as they are. New campaigns use qualified ids once the staging code does.
