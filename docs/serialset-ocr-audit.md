# Serial Set OCR audit and pilot (#814)

## Finding

The repository's Serial Set ingestion path extracts the text layer already embedded in
GovInfo PDFs with Poppler `pdftotext -enc UTF-8`; it does not currently run OCR as part of
that path. The checked-in GovInfo fixtures are metadata and text samples, not an accuracy
benchmark: they have no page images or independently verified reference transcriptions. The
small public-source audit below establishes a first measured baseline, not a collection-wide
quality estimate. The evidence is mixed by page type and date, so it does **not** justify a
bulk re-OCR campaign.

GovInfo itself cautions that Serial Set metadata, OCR text and ALTO are intended to be useful,
but may need validation and may be incomplete or inaccurate. Its help page documents public PDF
and ALTO access and the GPO/Law Library of Congress digitization partnership. The 2021 DOI
collection description calls the text OCR-generated and searchable but gives no quality
metrics. A bounded search of indexed institutional and scholarly material for the exact
Congressional Serial Set plus OCR accuracy, evaluation, quality or reprocessing located no
collection-specific accuracy study or re-OCR evaluation. This is not proof none exists. The
Library of Congress's 2026 NDNP-Open-OCR report documents reprocessing of Chronicling America
newspapers (over 425,000 pages), not Serial Set documents; that work is related context, not
Serial Set evidence.

Sources checked (2026-10-18):

- [GovInfo, Congressional Serial Set](https://www.govinfo.gov/help/serial-set) — collection,
  formats, partnership and the qualification that OCR may not be complete or fully accurate.
- [DOI Library, U.S. Congressional Serial Set](https://www.doi.gov/library/electronic/us-congressional-serial-set)
  — describes searchable OCR text; not a quality study.
- [Library of Congress, NDNP-Open-OCR](https://blogs.loc.gov/thesignal/2026/08/yarasavage-new-ocr-reprocessing/)
  — newspaper-only reprocessing context; not a Serial Set study.
- [Neudecker et al., A Survey of OCR Evaluation Tools and Metrics](https://primaresearch.org/www/assets/papers/HIP21_CNeudecker_OcrEvalSurvey.pdf)
  — general evaluation literature, not Serial Set-specific.

## Existing material and scope

- The repo's existing `fixtures/congress/govinfo/` material contains metadata and limited text
  fixtures; it has no matched full-page images/reference transcriptions for accuracy scoring.
- `crates/usnm-ingest/src/congress/serialset.rs` currently extracts embedded PDF text with
  `pdftotext -enc UTF-8`. For the sampled packages, GovInfo ALTO identifies ABBYY FineReader
  Engine 11 for the two 1820 documents (ALTO processing date 2020-04-01) and Tesseract 5.1.0 for
  the 1990 document. The baseline was extracted from each downloaded PDF with Poppler 26.06.0 using the same
  `pdftotext -enc UTF-8 PDF -` invocation as the repository path, then split at the PDF page
  boundary. The 1990 PDF text layer is not assumed to be identical to every page's ALTO.
- Three public GovInfo PDFs were retrieved one at a time, with three-second gaps, no retries,
  and no API key; four corresponding page ALTO files were retrieved. Retrieval date, exact URLs,
  package metadata, PDF/ALTO SHA-256 values, sizes and OCR provenance are in
  [`selection.json`](../ocr/bench/serialset-pilot/selection.json). Full PDFs are not committed.
- The fixed purposive selection has four page-disjoint pages from three documents and two serial
  numbers: two prose pages, two table/layout pages, and three pages noted to show scan irregularities
  (overlapping descriptive strata, not prevalence estimates). Coverage strata are 2 prose, 2 tables,
  and 3 with visible scan irregularities; counts are generated from the selection tags. Three pages have image-checked
  references; the dense comparative table on p. 4 remains unscored and has no reference. Coverage,
  fixture checksums, and the reason for the gap are in
  [`coverage.json`](../ocr/bench/serialset-pilot/coverage.json) and
  [`outputs/annotation-gap.json`](../ocr/bench/serialset-pilot/outputs/annotation-gap.json).
- Reference transcriptions were written from the full-page images, not copied from PDF text or
  ALTO. The explicit `serialset-full-page-v1` policy preserves print capitalization, punctuation,
  historic spelling, and line-end hyphens with their line breaks; reads tables by region and then row; and omits blank
  cells and decorative leader dashes. Each of the three scored references was checked against its
  committed page image. The table reading-order convention is part of the pilot, not a
  geometry-level layout annotation schema. No third-party reference data is included.
- Source images are public-domain U.S. federal government works in the United States. Attribution,
  copyright and reference provenance are in
  [`LICENSES/govinfo/NOTICE.txt`](../ocr/bench/serialset-pilot/LICENSES/govinfo/NOTICE.txt).

## Scored comparison

The comparison uses the existing #738 `bench-score` implementation and its two rules on the same
three verified full pages. Both systems produced all three outputs. The human reference subset
has digest `a6cd647618f652b9cb1b563e8bfe6375ff4d86799bfca1e436e92884563bc2a9`; the complete
pilot file-set digest is `0f744a8fb83958fdf7eaf1d766ce9bf92e35a4621aa11fe1caa490f743580e19`.

| OCR output             | Rule           | Edits / reference chars |    CER | Missing |
| ---------------------- | -------------- | ----------------------: | -----: | ------: |
| GovInfo PDF text layer | `legacy-ws-v1` |             613 / 6,407 |  9.57% |   0 / 3 |
| GovInfo PDF text layer | `raw-v1`       |             767 / 6,431 | 11.93% |   0 / 3 |
| Local Tesseract 5.5.1  | `legacy-ws-v1` |             783 / 6,407 | 12.22% |   0 / 3 |
| Local Tesseract 5.5.1  | `raw-v1`       |             833 / 6,431 | 12.95% |   0 / 3 |

Per-page `legacy-ws-v1` results show why the aggregate is not a re-OCR decision:

| Page                               | GovInfo PDF text layer |      Tesseract 5.5.1 | Notes                                                          |
| ---------------------------------- | ---------------------: | -------------------: | -------------------------------------------------------------- |
| H. Doc. 16-42, p. 3 (1820 prose)   |     12 / 1,928 (0.62%) |   20 / 1,928 (1.04%) | Existing text layer has fewer edits.                           |
| H. Doc. 16-53, p. 5 (1820 tables)  |   455 / 1,550 (29.35%) | 755 / 1,550 (48.71%) | Both struggle with dense table layout/order; dominates errors. |
| H. Doc. 101-183, p. 3 (1990 prose) |    146 / 2,929 (4.98%) |    8 / 2,929 (0.27%) | Local Tesseract has fewer edits.                               |

The 1820 page's top table is transcribed left region then right region, and its lower table row by
row; scorer CER is character-level and reading-order-sensitive. The high p. 5 scores therefore
include layout/order mismatch as well as recognition errors, and are not a clean character-only
engine comparison. No masks or alternate layout metric were introduced. Page 4's OCR outputs are
retained for inspection but excluded from scoring because no ground truth was verified.
The four exact score reports are committed next to the pilot outputs; they record page-level edits
and denominators. `reports/binding.json` pins the input manifest, subset, scorer implementation
(including the local `usnm-ocr-quality` sources and manifest), OCR output hashes, and report hashes;
`--build` invalidates stale reports and `--verify` rejects
results that do not bind to the current references and outputs. Whitespace-normalized versus raw
scores are both retained per #738 convention.

### Runtime and resource evidence

Local Tesseract 5.5.1 used Leptonica 1.87.0, English traineddata SHA-256
`7d4322bd2a7749724879683fc3912cb542f19906c83bcc1a52132556427170b2`, `--oem 1 --psm 3`, and
full-page 200-dpi Poppler 26.06.0 JPEGs. On a CPU-only Apple M1/macOS host, one cold process per
page took 28.19 seconds for four pages (8.51 pages/minute, including process/model startup),
with p50 5.12 s, nearest-rank p95 15.01 s and max process RSS 170,033,152 bytes. The measurements
are recorded in [`tesseract-runtime.json`](../ocr/bench/serialset-pilot/tesseract-runtime.json).
They are a tiny local timing sample, not container/cgroup or production measurements, and are
not comparable to the newspaper crop rates in #738.

## Limits and next work

This purposive pilot is far too small and heterogeneous to estimate corpus-wide OCR quality or
select a winning OCR system. The aggregate source-text-layer result is lower CER than the new
local run only because this sample is dominated by a difficult 1820 table; the simple prose pages
split in both directions. Do not infer that all embedded source OCR is better, or that a corpus-wide
re-OCR is warranted. A larger stratified, page-disjoint sample and a reviewed treatment of tables,
layout/order, scan damage, omissions, and rights would be required before a production decision.

The p. 4 table page remains a human-verification gap. The transcription policy and coarse layout
labels are pilot-level, not a complete line/region annotation protocol; page damage is descriptive,
not formally severity-annotated. The literature search was scoped and non-exhaustive; the absence of
a located Serial Set-specific publication is not a first-ever claim. The existing BOINC/download
state and parent issue #229's operations were not touched; no downloader was started, no on-prem
fetch or infrastructure work was performed, and no benchmark release was published.

## Reproduce

From the repository root:

```sh
python3 scripts/ocr-bench/serialset-pilot.py --verify
python3 scripts/ocr-bench/serialset-pilot.py --score
```

`--score` invokes the repository's `usnm-ocr-survey bench-score` with both #738 rules on only
`serialset/verified-3`, stores score reports and input bindings under the pilot, rebuilds its hashes
and removes its own work directory. `--build` invalidates reports when their references, OCR outputs,
selection, or scorer implementation no longer match; run `--score` again before `--verify`. The
scorer requires a directory named `usnm-ocr-bench-<version>`, so the
runner creates that name only in `work/serialset-814/score-run` using manifest sentinel `v0.0.0`.
That local compatibility copy is deleted after scoring; `v0.0.0` is not a release, there is no
release tag/archive/index entry, and `ocr/bench/releases.json` is unchanged.
