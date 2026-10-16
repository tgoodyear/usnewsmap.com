# Untrusted OCR output fuzzing

This standalone crate is intentionally outside the production Cargo workspace.
It needs nightly and [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz); it is
local tooling, not a production service or a CI fuzzing campaign.

From the repository root:

```sh
rustup toolchain install nightly
cargo install cargo-fuzz --locked
cd fuzz
cargo +nightly fuzz build
cargo +nightly test --lib --locked
cargo +nightly fuzz run read_node_part -- -max_len=1048576 -rss_limit_mb=512 -timeout=10 -max_total_time=60
cargo +nightly fuzz run alto_check -- -max_len=1048576 -rss_limit_mb=512 -timeout=10 -max_total_time=60
cargo +nightly fuzz run decode_dead_letter -- -max_len=1048576 -rss_limit_mb=512 -timeout=10 -max_total_time=60
```

Remove `-max_total_time` for a longer local campaign. The default AddressSanitizer
and the debug and overflow checks stay enabled. Any panic, memory-budget
assertion, sanitizer failure, timeout or RSS-limit failure is a finding, not an
expected parser rejection. Replay with `cargo +nightly fuzz run TARGET artifacts/TARGET/FILE`.
Do not catch panics: malformed inputs should return errors, never crash.

## Targets and bounds

- `read_node_part`: arbitrary Parquet, and mutations inserted into a real column
  chunk while the footer stays valid. The second path (the mutations) exercises
  `check_uncompressed`'s capped Compact Thrift page-header scan, including
  forged decompression sizes, before any page is decoded. A full read that
  succeeds must respect the production 16-row bound.
- `alto_check`: the same XML checked with production limits (including ALTO
  v2, v3 and v4) and tight limits (4 KiB, depth 4, 16 elements, 4 attributes).
  Successful dimensions must be positive and finite; element counts and
  over-byte-limit rejections are asserted.
- `decode_dead_letter`: UTF-8 raw JSON and base64 JSON, v1 and all v2 causes.
  Successful values must validate; valid fixtures also round-trip through the
  queue encoding in the corpus test.
  Non-UTF-8 inputs cannot reach this `&str` API and are skipped.

All targets skip inputs above 1 MiB independently of libFuzzer's `-max_len`.
Each parser invocation asserts its additional peak live Rust-heap allocation
using `peak_alloc`: 64 MiB for Parquet and header scanning, 16 MiB for ALTO,
4 MiB for dead letters. These allow parser overhead beyond the production 4 MiB
Parquet decompression cap on purpose: they are regression budgets, not claims
that output bytes equal heap usage. The counter resets to the current baseline
per invocation, counts transient allocations and includes input copies and
returned values. Targets run single-threaded; do not call their helpers
concurrently. libFuzzer's whole-process RSS limit also bounds native
allocations (zstd's, for one), sanitizer overhead and memory leaked across
iterations. The peak assertions run after parsing; the RSS limit also guards
pathological allocations before a parser returns. A short smoke run neither
proves safety for all inputs nor covers production-sized 32 MiB ALTO files.

## Reproducible synthetic seeds

Named seeds under `corpus/` are committed. They are derived from
`usnm-overlay`'s `reads_a_node_part_strictly` and
`page_headers_are_read_without_decoding`, `usnm-alto`'s `V3` and reject fixtures,
and `usnm-ocr-contract`'s `message::tests::{sample,dead}`.
They contain no production data. Regenerate from `fuzz/`:

```sh
cargo +nightly run --example seed --locked
cargo +nightly test --lib --locked
cargo +nightly fmt --all --check
```

The generator fixes timestamps and writes valid and rejected Parquet, forged
page headers, v2, v3 and v4 XML, and raw and base64 dead letters. Git ignores
the hash-named corpus entries libFuzzer generates, artifacts, coverage and build
output; promote a useful regression to a named seed and a unit test by hand.
`Cargo.lock` here is separate from the production lockfile.
