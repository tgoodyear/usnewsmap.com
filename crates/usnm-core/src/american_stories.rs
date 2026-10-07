//! American Stories' text (#218, 05 §5.5.4): a second OCR of a page, from
//! the American Stories dataset (Dell et al., CC BY 4.0), in the index
//! fields `text_as` (stored, for snippets) and `text_as_cg` (its
//! common-word pairs, as [`crate::common_grams`] makes them for `text`).
//! LoC's `text` and `text_cg` are unchanged. A query matches a page when it
//! matches in either text, and a page is one document, so it is counted
//! once however many texts it matches in.
//!
//! The API searches the fields only when the published version was built
//! with this [`VERSION`] (`current.json`'s `american_stories`), the same
//! way as the common-word pairs: older versions have no such fields, and
//! leaving the version out of `current.json` switches them off without a
//! rebuild.

/// Bumped whenever what `text_as` holds changes: which American Stories
/// scans a page takes, or how their text is put together. A release then
/// rebuilds every index.
pub const VERSION: u32 = 1;
