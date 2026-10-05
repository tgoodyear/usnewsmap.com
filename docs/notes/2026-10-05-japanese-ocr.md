# Japanese pages: OCR of the pages LoC ships without text

**Status:** engine chosen, job built and running; `pages-v20261005-1` is the first version built with the Japanese index's fold (`ja_fold` 1); its page count and the live search are not yet captured · **Date:** October 2026 · **Measured on:** 23 reference crops from 1942–45 papers (engine accuracy); `dlc_ballston_ver01` and Rocky Shimpo (the gap) · **Code:** `ja-ocr/` (`jaocr.py`, `alto.py`, `audit.py`), `scripts/ja-ocr/` (`sample.py`, `crops.py`, `run.py`, `score.py`), `Dockerfile.ja-ocr`, `crates/usnm-ingest/src/ocr_ja.rs`, `crates/usnm-core/src/ja.rs`; [04 §4.8](../design/04-data-sources-and-ingestion.md), [06 §6.4](../design/06-api-design.md), [07 §7.10](../design/07-frontend-design.md) · **Issues:** #128, #135, #139, #145, #149, #151, #152, #157

## The question

LoC's OCR has no Japanese script. The Japanese pages of the Japanese-language titles, the internment-camp papers and the Japanese-American papers catalogued "In English and Japanese", come from LoC either with no text at all (`[NO TEXT AVAILABLE FOR THIS PAGE]` on loc.gov; an empty ALTO and no `ocr.txt` in the bulk archives) or as garbled Latin characters. Either way no search can find them. LoC serves every page image through IIIF at full scan resolution, so the question was whether we could read them ourselves well enough to search, at a cost the project can carry.

The size of the gap (#135, `jaocr.py audit`): LoC's per-title "Pages (Full Text)" count on loc.gov includes pages without text, while our `title_pages.json` counts only pages with an `ocr.txt`. Rocky Shimpo has 1,012 pages on loc.gov and 256 with text. In `dlc_ballston_ver01` alone, 2,433 of its 5,791 pages have an `ocr.xml` and no `ocr.txt`.

## What we did

**Choosing an engine** (#128, `scripts/ja-ocr/`). `sample.py` walks the titles loc.gov lists as Japanese, spreads issues across each title's years and keeps the pages whose LoC OCR is empty or mostly not words; `crops.py` cuts one region from each of a couple of dozen sampled pages (24 by default), split between the typeset papers (Rocky Shimpo, Colorado Times) and the camp papers (mostly hand-lettered mimeograph), for hand transcription; `run.py` runs each engine; `score.py` scores them. Two measures: character error rate, which also counts reading-order differences (vertical pages have plenty), and bigram F1, the overlap of adjacent character pairs, which ignores order beyond pairs and so measures what search sees: whether the words are there. Old character forms are folded to modern ones on both sides first (戰 → 戦), since engines differ on that and searchers type modern forms.

**The job** (`ja-ocr/jaocr.py`, Container Apps Job `caj-usnm-jaocr-{env}`, started by hand). It lists target pages from two sources: the pages LoC's bulk archives ship with an `ocr.xml` and no `ocr.txt` (these never reached the curated store, because curation reads only `ocr.txt`), and the pages in the published version's Parquet whose text is empty, short, or `ok` but under 35% word-like tokens. Per issue it claims a create-only blob, asks loc.gov for the issue's page images once, downloads each target page's full-resolution JPEG, runs NDLOCR-Lite on them together, writes each page's ALTO and then the issue's Parquet part. It paces itself to loc.gov's limits (one API request per 3.5 s, one image per second) divided by the replica count. A page is done once it has both; the job is started again until every target is done. The target file's name carries the rules' version (`targets-v2.jsonl`), so changing the rules lists them again.

**ALTO** (#151, #152, `ja-ocr/alto.py`). LoC's `ocr.xml` for these pages is an empty ALTO recorded in `inch1200` at 300 dpi, and that page size is our full-size IIIF image's pixels × 4 to within a few pixels. So the job writes ALTO v3 in the same units, one `TextBlock` per NDLOCR-Lite block and one `TextLine` per line with its text and confidence, in NDLOCR-Lite's reading order. NDLOCR-Lite measures lines, not characters, so there are no character boxes. About 90 KB a page. The release doesn't read it; it exists so the output can stand in for the ALTO LoC ships.

**Indexing** (#139, #145). Every release reads the overlay: each part under `ocr-ja/pages/`, the newest row per page, only pages of catalogued titles in the version. It builds a separate index, `pages-ja-{stamp}-{n}`, from scratch each time. Japanese has no spaces, so each character is a token and a word is a phrase of its characters; old forms fold to modern, small kana to large, compatibility ideographs and halfwidth katakana to their standard forms (`usnm_core::ja`), and `current.json` names the fold version so the API can check its own folding matches. A Japanese query searches that index and nothing else, so a page with garbled LoC text in the main index and our OCR in the Japanese index is never counted twice; a version without the index answers a Japanese query `422`. Pages curation never had are added to the baselines at release. An incremental release with no new batches still publishes when the overlay's parts differ from the published version's, so new OCR goes live at the next run without waiting for new LoC batches (#145).

**Visible on the status page** (#149, #157): "Japanese pages read: X of Y", the pace, how many are searchable, when the job last reported. It sits in its own "OCR experiments" section because the OCR isn't a pipeline step.

## What we measured

Engine accuracy on 23 reference crops from 1942–45 papers, typeset and hand-lettered (04 §4.8; details in #128):

| Engine | Character pairs found (bigram F1) | Crops where it scored highest |
|--------|-----------------------------------|-------------------------------|
| NDLOCR-Lite (National Diet Library, CC BY 4.0, CPU only) | 77% | beat Azure on 19 of 23 |
| Azure AI Document Intelligence Read | 61% | |
| Tesseract `jpn_vert` | 26% | |

Scale, from the design documents: about 11k Japanese pages in the overlay, indexed in about a minute (04 §4.8).

What the numbers don't show: 23 crops is a small sample, chosen to cover both kinds of printing; the per-page scores on mimeograph are well below the average; and bigram F1 says whether the words are there, not whether a snippet reads well.

## What is left

- **The live count.** The `pages-v20261003-1` capture answered the three Japanese searches `422`. `pages-v20261005-1` is the first version with `ja_fold`; its capture should answer them `200`, and `/v1/status` gives `published.ja.pages`. Neither is recorded yet.
- **Progress to completion.** The job's `ocr-ja.json` report (targets, done, eta) is the record of how far the OCR has got; when it finishes, note the page and issue counts and the wall time per page on the job's CPU.
- **The audit over every title** (`jaocr.py audit --all-languages`, about 4.5 hours at loc.gov's pace) would say whether the pages-without-text problem is only Japanese. The default run covers the non-English titles.
- **LoC's own reprocessing.** LoC announced NDNP-Open-OCR reprocessing (04 §4.1). If it ever covers Japanese script, our overlay becomes a fallback; the batch-version path in 04 §4.7 is how its text would arrive.
- **Mimeograph accuracy.** The camp papers are where NDLOCR-Lite scores lowest, and they are the pages with the most historical interest. No engine tried does well on them; a larger reference set for those pages would show whether anything is worth trying.
