//! Per-page OCR quality without transcription (#522). Everything here is a
//! pure function of its inputs, single-threaded and deterministic: two hosts
//! that run it on the same page produce the same bytes, which is what the
//! `usnm_quality` BOINC app's validator compares.
//!
//! - [`align`]: character and word alignment with a backtrace, the pairwise
//!   metrics between two readings (CER both ways, WER, word-bag Dice).
//! - [`agreement`]: two readings of a page against each other with no
//!   third reader (#962): the pair the hat takes, word-bag cover and length
//!   ratio.
//! - [`hat`]: the three-cornered hat, each reader's own error rate from the
//!   pairwise disagreements.
//! - [`page_hat`]: the page hat, version 2, solved on the passage-matched
//!   disagreement of [`agreement`] (#962).
//! - [`consensus`]: the words at least two readers agree on, and each
//!   reader's completeness and precision against them.
//! - [`findability`]: the share of the consensus content words and place
//!   names a reader's text contains.
//! - [`confusion`]: the character substitutions, insertions and deletions
//!   of a reader against the consensus.
//! - [`features`]: scan damage features from the page image, layout
//!   features from its ALTO.
//! - [`crops`]: the seeded, stratified crop selection the vision-language
//!   app reads.
//! - [`report`]: the per-page record, its JSON schema and rounding.
//! - [`qwen_pages`]: the page images a vision-language unit's crops are cut
//!   from, decoded one page at a time.

pub mod agreement;
pub mod align;
pub mod confusion;
pub mod consensus;
pub mod crops;
pub mod distance;
pub mod features;
pub mod findability;
pub mod hat;
pub mod page_hat;
pub mod qwen_pages;
pub mod report;
pub mod router;
pub mod slot;
pub mod text;
