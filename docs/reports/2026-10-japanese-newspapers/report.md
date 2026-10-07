---
title: "Japanese-language newspapers in Chronicling America"
subtitle: "The missing text, our OCR of it, and what other collections hold"
date: "October 2026"
---

# Summary

- **LoC has no searchable Japanese text for these papers.** The Japanese pages of the WWII camp newspapers and of Denver's *Rocky Shimpo* and *Colorado Times* arrive from the Library of Congress with no text, or with garbled Latin characters. LoC's files show that the original OCR (ABBYY 8) ran in Japanese on these pages and returned no words (116 of 124 sampled pages; the other 8 were run as English). About 9,330 pages across 23 titles have no text.
- **We read them ourselves.** NDLOCR-Lite, the National Diet Library's open OCR engine, scored best of the engines we tested. Our job has read 11,018 of the 11,058 target pages (the targets also include pages whose LoC text is empty, short or garbled, which is why there are more than 9,330). The first 10,414 become searchable on usnewsmap.com with the index version now being built.
- **For most of these pages we found no Japanese text anywhere else, but the sites likeliest to hold the camp papers could not be searched yet.** We checked other collections that hold these papers:
  - *Rocky Shimpo*: Hoji Shinbun (Hoover Institution) has Japanese OCR for April 1943 to April 1944. Colorado Historic Newspapers has Japanese OCR for 1943 and part of 1944; where its run ends is not yet checked. LoC's run, and so ours, covers June 1944 to December 1945, so ours is new unless CHNC's run goes past June 1944.
  - *Colorado Times*: Colorado Historic Newspapers already has Japanese OCR, including 1945, so ours is a second reading, useful for comparison.
  - *Camp newspapers*: no Japanese OCR in any collection we could check. The sites most likely to have them (Densho, Utah Digital Newspapers, Wyoming Newspapers and others in the table) could not be searched automatically and are still to be checked by hand.

# The gap at LoC

Chronicling America lists 27 titles as Japanese. They are mostly the newspapers of the WWII incarceration camps (*Minidoka Irrigator*, *Manzanar Free Press*, the Japanese edition of the *Heart Mountain Sentinel*, *Poston Chronicle*, *Gila News-Courier*, *Topaz Times*, *Granada Pioneer*, *Tulean Dispatch*, *Rohwer Outpost*, *Newell Star* and others) plus two Denver papers, *Rocky Shimpo* and *Colorado Times*. The page images are fine: LoC serves them at full scan resolution through IIIF (all but 14 pages, which have no image at LoC). The text is missing:

- **In the bulk OCR archives**, a Japanese page has an empty ALTO `ocr.xml` and no `ocr.txt`. In one batch, `dlc_ballston_ver01`, 2,433 of 5,791 pages are like that.
- **On loc.gov**, the page viewer shows "NO TEXT AVAILABLE FOR THIS PAGE", and full-text searches across Chronicling America for 年, 日本 and 真珠湾 return no pages.
- **LoC's files record what was run.** The empty ALTO keeps its processing settings. All 124 pages we sampled record `Engine:Abbyy8`: 116 with `Lang:ja` and `Word Count:0`; the other 8 were run as English, which is where the garbled Latin text comes from. The Japanese OCR was run and read nothing.
- **The gap dates from 2014.** Four of the six batches with these papers were ingested in December 2014 and the other two in June 2026. All six were rebuilt in June 2026 and still have no Japanese text. NDNP's technical guidelines have listed Japanese as a supported full-text language since at least the 2016–18 edition.

Comparing LoC's per-title page counts, which include pages without text, with the pages that have text gives the size of the gap: about 9,330 Japanese pages across 23 titles. Every other non-English language we checked has its text (German, Spanish, French, Polish, Yiddish, Russian, Serbian, Czech, Hebrew).

![Pages with no OCR text at LoC (dark) against all pages LoC lists for the title (light), for the 11 of 23 Japanese titles with the largest gaps. Per-title audit, 4 October 2026 (#135).](figures/missing-text.svg)

# What other collections hold

We searched other collections for Japanese OCR of the same papers. Where a site allowed it, the test was to find the title, search it for a common word (日本, "Japan") and for exact lines from LoC's pages, and compare the dates covered.

| Collection | Holds | Japanese OCR | What it means for our OCR |
|---|---|---|---|
| Hoji Shinbun Digital Collection (Hoover Institution) | *Rocky Shimpo* 1943-04-12 to 1944-04-12 (101 issues, 404 pages); camp magazines and directories; not *Colorado Times*, not the camp newspapers | Yes, for what it holds | No overlap: LoC's *Rocky Shimpo* starts 1944-06-02 |
| Colorado Historic Newspapers (CHNC) | *Rocky Shimpo* 1943–1944; *Colorado Times* 1918–1969 | Yes (日本: 620 results in *Rocky Shimpo*, 72,008 in *Colorado Times*) | *Colorado Times* 1945 is already OCR'd there; CHNC's last *Rocky Shimpo* date is not yet confirmed |
| National Diet Library Digital Collections | One *Minidoka Irrigator* issue, English | No | None |
| Internet Archive | *Rohwer Outpost*, 115 items, 1942–44 | No (English OCR only) | None |
| HathiTrust | No records for these titles | n/a | None |
| Densho Digital Repository | Camp newspapers, all ten camps | Unknown (not searchable automatically) | To check by hand |
| Utah Digital Newspapers | *Topaz Times*, about 470 issues | Unknown (not searchable automatically) | To check by hand |
| Wyoming Newspapers | *Heart Mountain Sentinel*, reportedly the Japanese edition too | Unknown (not searchable automatically) | To check by hand |
| Arizona Memory Project / ASU, CDNC / Calisphere, CRL | Possibly *Poston*, *Gila*, *Tule Lake*, *Manzanar* (unconfirmed) | Unknown (not searchable automatically) | To check by hand |

![*Rocky Shimpo* and *Colorado Times*: which collection holds which dates, and whether it has Japanese text. CHNC's *Rocky Shimpo* dates come from search results by year and are not confirmed. CHNC's *Colorado Times* runs 1918–1969; the axis starts at 1943.](figures/coverage-timeline.svg)

By paper:

- **Rocky Shimpo** is split in time. Hoji holds April 1943 to April 1944 with Japanese OCR, and CHNC holds 1943 and at least part of 1944; LoC holds June 1944 to December 1945 with no Japanese text. Hoji's run ends 1944-04-12 and LoC's starts 1944-06-02. Of the collections we checked, our OCR of LoC's 756 pages without text is the only Japanese text for those 19 months, unless CHNC's run extends past June 1944.
- **Colorado Times** is the one paper where our text duplicates existing work: CHNC has Japanese OCR across 1918–1969. An exact line from the 1945-08-14 issue (皆様お馴染の三光樓) found nothing on CHNC, probably because the two OCR readings differ, which makes the pair useful for comparing quality.
- **The camp newspapers** are not in Hoji, which instead holds camp literary magazines and directories (*Dotō* and *Gyōen* from Tule Lake, an Amache directory) that LoC doesn't have. We found no Japanese OCR of the camp newspapers anywhere we could search. The sites likeliest to have it could not be checked automatically.

# Our OCR

**Choosing an engine.** We cut 23 reference regions from 124 sampled Japanese pages of 17 titles (1942–45), split between the typeset Denver papers and the camp papers (mostly hand-lettered mimeograph), and scored each engine by bigram F1, the overlap of adjacent character pairs between the engine's text and the reference, which measures whether the words are there, as search needs. NDLOCR-Lite (CC BY 4.0, runs on CPU) scored highest overall, 77% against Azure's 61%, and beat Azure Document Intelligence on 19 of the 23 crops. We also tried LoC's reprocessing pipeline, NDNP-Open-OCR (commit `cbe0ca5`). It is set up for English only, so it read every page as English. With Japanese Tesseract models it reached 24%, and with NDLOCR-Lite swapped in as its engine (our local prototype, not proposed upstream) 67%. The pipeline runs on full pages, and scoring only the text inside each crop costs it some points against the crop-only scores (#128).

![Bigram F1 (overlap of adjacent character pairs with the reference) on the 23 reference crops, typeset and camp papers. NDNP-Open-OCR rows ran on full pages and keep only text inside each crop, which lowers their scores somewhat. The prototype is ours, not LoC's.](figures/engines.svg)

**The job.** For each issue with target pages, the job downloads the full-resolution images from loc.gov, runs NDLOCR-Lite, and writes the text twice: as ALTO in the same units as LoC's empty files, so it could stand in for them, and as one row per page for our index. It paces itself to loc.gov's limits. Targets are the pages LoC ships without text plus pages whose LoC text is empty, short or garbled: 11,058 pages in 3,421 issues.

![From a LoC page image to a search result.](figures/pipeline.svg)

**Where it stands (6 October 2026).**

- **Read:** 11,018 of 11,058 target pages.
- **Not read:** the other 40 pages. 14 of them have no image at LoC. The job writes an issue only when every page in it is read, so pages that share an issue with them are held back too.
- **Search:** the index version being built now includes the 10,414 pages read before it started; the other 604 follow with the next release. A search in Japanese script searches only these pages, with old character forms matched to modern ones (戰 finds 戦).

# Caveats

- **The accuracy figures are agreement with a machine reference.** The reference transcriptions were machine-made and one was checked against its scan. The ranking between engines is better established than any one percentage. A Japanese reader should review a sample of the crops and of NDLOCR-Lite's output.
- **Camp papers are harder for every engine.** They are mostly hand-lettered mimeograph, and NDLOCR-Lite scores 72% on them against 83% on typeset pages.
- **Other collections were checked only by searching their public websites,** and the coverage table reflects what those sites showed on 4 and 5 October 2026. Absence of a search hit is not proof of absence.

# Open questions

1. **CHNC's last *Rocky Shimpo* issue.** If it is before June 1944, our *Rocky Shimpo* text is new throughout.
2. **Densho, Utah Digital Newspapers, Wyoming Newspapers and the other sites in the table,** checked by hand: search 日本 within each camp title.
3. **A quality comparison with CHNC** on the same *Colorado Times* 1945 pages.
4. **A Japanese reader's review** of the reference crops and a sample of our OCR.

# Sources

- Progress on 6 October 2026: the usnewsmap.com status API (`https://api.usnewsmap.com/v1/status`, section `ocr_ja`: 11,018 of 11,058 pages read). The 10,414 pages in the index being built are those the job had written when the build read the OCR output, at 6:21 AM ET on 6 October, as recorded in [#187](https://github.com/tgoodyear/usnewsmap.com/pull/187).
- usnewsmap.com issues [#128](https://github.com/tgoodyear/usnewsmap.com/issues/128) (OCR), [#135](https://github.com/tgoodyear/usnewsmap.com/issues/135) (the gap and other collections) and the technical note *Japanese pages: OCR of the pages LoC ships without text* (`docs/notes/2026-10-05-japanese-ocr.md`).
- Library of Congress, [Japanese-American Internment Camp Newspapers](https://www.loc.gov/collections/japanese-american-internment-camp-newspapers/about-this-collection/); NDNP Technical Guidelines [2025–27](https://loc.gov/ndnp/guidelines/NDNP_202527TechNotes.pdf) and [2016–18](https://loc.gov/ndnp/guidelines/archive/NDNP_201618TechNotes.pdf); [NDNP-Open-OCR](https://github.com/LibraryOfCongress/ndnp-open-ocr).
- [Hoji Shinbun Digital Collection](https://hojishinbun.hoover.org/), Hoover Institution; [Colorado Historic Newspapers](https://www.coloradohistoricnewspapers.org/).
- [NDLOCR-Lite](https://github.com/ndl-lab/ndlocr-lite), National Diet Library.
