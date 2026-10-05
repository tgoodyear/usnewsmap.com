# Corpus coverage: what is searchable, what is counted, what is missing

**Status:** the backfill reached LoC's full page count by `pages-v20261003-1`; the gaps below are open · **Date:** October 2026 · **Measured on:** `/v1/status` on 30 September 2026; the `pages-v20260929-4` snapshot (the corpus table in 11 §11.2); the capture of `pages-v20261003-1` · **Code:** `/v1/coverage` (`crates/usnm-api`), `write_snapshot` in `crates/usnm-ingest/src/release.rs` (`baselines.json`, `language_baselines.json`, `title_pages.json`, `duplicates.json`), `ja-ocr/audit.py`, the status page (`web/src/status/`); [04 §4.1](../design/04-data-sources-and-ingestion.md), [04 §4.7](../design/04-data-sources-and-ingestion.md), [06 §6.3](../design/06-api-design.md), [07 §7.4](../design/07-frontend-design.md) · **Issues:** #135, #159

## The question

"Coverage" means two things here, and both come from the same numbers.

1. **How much of Chronicling America is searchable**, and what in it can't be found: pages LoC ships without text, pages shipped twice, titles catalogued under the wrong language.
2. **The coverage layer**: telling "no mention" from "no digitized newspaper". A place with no circle might have had nothing to say or might have had no paper that year. The legacy site showed a static black mask over states with no data; the requirement (02, F-08) is a time-aware layer, and the relative-rate measure (11) needs the same denominator: pages published per place and time bucket.

## What we did

**The denominator.** Every release writes `baselines.json` into the version's reference snapshot: pages published per place and day, summed from each batch's `counts.json` over every curated page, including pages whose OCR text is empty. Snapshots since the per-language work also hold `language_baselines.json`, the same pages split by the languages their title lists, and `title_pages.json`, pages per title. `/v1/coverage` serves the baselines as a place-by-bucket cube from memory, filtered by `state` and `lang`, cached a day. Every `/v1/aggregate` response names the matching coverage cube in `cube.baseline_ref`, and the site fetches it with every search; the warm-up before a publish requests each example's coverage cube too (06 §6.5). Under an `lccn` or `front` filter the baseline is `null`, because baselines are kept per place, day and title language only, and `lang` on a version from before the per-language baselines answers `422`.

**What counts as a page.** The rules decide what the denominator means:

- A page LoC ships with no `ocr.txt` never reaches the curated store, so it isn't in the baselines until our own OCR adds it (the Japanese pages, [the OCR note](2026-10-05-japanese-ocr.md)). A page with an `ocr.txt` that is empty, short or garbled is in the denominator but can't match.
- LoC ships some issues in two batch archives (`wa_duwamish_ver01` and `wa_lacamas_ver01` both carry 20 pages of `sn87093109` from 1875). The release keeps one copy of each such page, the same copy every time (text before no text, then the batch whose name sorts first), lists copies an earlier index already holds in `duplicates.json` and hides them from every search; the page counts once (04 §4.7).
- LoC's listing counts aren't exact: `ncu_cane_ver01` held 11,100 pages in 2,509 issues where the listing said 10,550 in 2,365 (04 §4.1.2), so completeness is checked by the archive's sha256 and our own counts, never by the listing.
- Language codes come from LoC's title records through a MARC mapping. Adahooniłigii (sn92024097) lists its languages as navaho, navajo and english; neither name had a code, so both passed through and the pages-by-language table counted the title's 1,295 pages twice. Both map to `nav` now, and a title's codes are kept once each (#159).

**Finding what is missing** (#135, `jaocr.py audit`). LoC's per-title "Pages (Full Text)" on loc.gov counts pages with or without text; `title_pages.json` counts pages with an `ocr.txt`. The audit compares the two for every non-English title (about 400 requests at loc.gov's pace, about 25 minutes) or every title (`--all-languages`, about 4,700, about 4.5 hours), writes a CSV sorted by the gap to the curated store, and flags titles whose batches aren't all published yet, since their gap isn't (only) missing text.

**The status page** shows the result: "Searchable now: X of Y pages", the steps of the pipeline, pages by state and language, and the OCR's progress.

## What we measured

The backfill, against LoC's listing of 2,997 batches, 23.7M pages and 2.47 TB of archives (04 §4.1.1):

| When | Where measured | Batches | Pages | Notes |
|------|----------------|---------|-------|-------|
| 29 Sep 2026, `pages-v20260929-4` | the snapshot (11 §11.2) | 882 | 6,557,925 | 398 places in 42 states and territories; 1,217 titles, 195 places with exactly one; no pages yet from HI, ME, MT, NV, NM, ND, RI, SD, VT, PR, VI |
| 30 Sep 2026 13:30 UTC | `/v1/status` | 66.0% curated | 15,788,248 curated, 15,766,980 with usable text | 0.13% without text: in the denominator, can't match |
| 5 Oct 2026, `pages-v20261003-1` | the capture, `yellow fever` 1751–1963 | | 23,722,885 in the baseline | effectively LoC's listed total; 1,632 places had a `railroad` hit |

What limited the pace was LoC's download limit, not CPU: 10 bulk requests per 10 minutes per IP. The first production backfill ran 8 workers at about 140 downloads an hour, was tolerated for about 90 minutes, then refused almost everything for over an hour. Workers now take download slots 75 s apart from one shared Cosmos item, and a `429` pauses all of them for an hour: about 48 batches an hour, about 2.5 days for the corpus with 4 workers (04 §4.4).

The gap, where measured: Rocky Shimpo, 1,012 pages on loc.gov and 256 with text; `dlc_ballston_ver01`, 2,433 of 5,791 pages with an `ocr.xml` and no `ocr.txt`.

What the numbers don't show: the 23.7M baseline says every listed batch is curated, not that every page in every batch is; the per-batch sha256 and counts are the check for that, and the duplicates hidden per version are on the status page.

## What is left

- **A full accounting of pages without text.** The audit's `--all-languages` run gives the gap for every title, not only the non-English ones. Its result, per language and per state, belongs in a follow-up note, with how much of it the Japanese OCR closes.
- **The eleven states and territories with no pages** at `pages-v20260929-4`. Whether they have pages in `pages-v20261003-1` and after is a `/v1/status` reading away; which of them LoC has no digitized titles for at all is a catalog question the status page could answer.
- **Baselines per title per day** aren't in the snapshot (the per-batch counts have them; the snapshot sums per place), which is why the relative rate can't be shown under a newspaper filter (11 §11.2). Title and place pages with a coverage chart (02, F-31) would want them.
- **Geocoding cross-check.** Places come from LoC's own coordinates with overrides; the GNIS/Census cross-check in `geocode` is still to come (04 §4.4), so a place on the wrong spot is a coverage error the layer can't see.
