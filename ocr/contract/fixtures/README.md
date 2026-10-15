# Engine contract conformance fixtures

Inputs for `usnm-ocr-worker engines conformance` (13 §13.4.8).

- `corrupt.png`: a PNG signature and an `IHDR` header for a 100×100 image
  with a broken CRC and no image data. The worker reads its size from the
  header, so it reaches the adapter, which must answer `bad_input`.

The rendered text line, the blank page and the large page for the deadline
case are drawn by the runner (`crates/usnm-ocr-worker/src/testimg.rs`), so
they aren't checked in.
