//! End-to-end tests of the API against the synthetic fixture corpus.

mod aggregate;
mod american_stories;
mod common;
mod congress;
mod days;
mod hits;
mod languages;
#[path = "../../../../fixtures/loc_original.rs"]
mod loc_original;
mod openapi;
#[path = "../../../../fixtures/page_language.rs"]
mod page_language;
mod page_languages;
mod reference;
mod serving;
mod snapshots;
