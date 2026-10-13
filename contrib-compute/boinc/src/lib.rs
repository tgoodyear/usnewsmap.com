//! BOINC project glue for the usnewsmap OCR contribution route.
//!
//! BOINC's validator and assimilator plug-in API is C++. Its `script_validator` and
//! `script_assimilator` daemons run an executable from the project's `bin/` directory, so
//! these pieces are plain Rust programs and need no C++.

pub mod assimilate;
pub mod cli;
pub mod fetch;
pub mod manifest;
pub mod result;
pub mod runner;
pub mod similarity;
pub mod workgen;

/// Name of the BOINC app. Part of the interface the host scripts rely on.
pub const APP_NAME: &str = "usnm_ocr";
