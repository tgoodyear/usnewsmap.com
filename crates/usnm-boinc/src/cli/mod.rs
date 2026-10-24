//! Command lines that belong to no single app. Each app (`surya`, `quality`, `qwen`, `assess`,
//! `gold`, `ndlocr`) and each group (`claim`, `handoff`, `host_trust`) keeps its own in a `cli`
//! module next to its code; `main.rs` holds only the top-level `Cli` and routes to them.
pub mod job;
