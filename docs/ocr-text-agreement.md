# Text agreement: LoC against American Stories

`usnm-ingest ocr-agreement` (#962) compares LoC's OCR text of a page with American Stories' text of the same page, for every page of an index version that has both: 15,224,177 of the 23,740,914 pages in `pages-v20261022-1`. It needs no image, makes no request to LoC and runs on the curated store alone, so it can run on a home host against the local copy of `curated`.

It is the text part of the `usnm_quality` record ([BOINC quality](boinc-quality.md)) for two readers, run over the whole corpus instead of a staged wave.

## The metric (agreement v1)

`usnm_ocr_quality::agreement::compare(loc, as)` takes LoC's text as reader `a` and American Stories' as reader `b`, and returns:

| Field | What it is |
| --- | --- |
| `pair` | `align::pair` rounded with `report::round_pair`: the same `PairMetrics` the `usnm_quality` record carries for this pair of readers. Both texts are normalised as the gold set does (NFC, a hyphen at a line's end joined, whitespace runs squashed). `cer_sym` is character edits over the mean of the two lengths, the disagreement the three-cornered hat solves on (`d_loc,as`). `cer_a` (`cer_loc` in the table) is LoC's CER with American Stories as the reference and `cer_b` (`cer_as`) the other way; `wer_*` are the same over whitespace words. `dice` is the word-bag Dice over lower-case runs of letters and digits |
| `bag_words_a`, `bag_words_b`, `shared_words` | The two word bags' sizes and their overlap (the smaller count of each word) |
| `cover_a`, `cover_b` | `shared_words` over each bag: the share of one reader's words that the other reader also has |
| `len_ratio` | American Stories' normalised characters over LoC's |
| `local_a`, `local_b` | The local alignment, each way (below): chunks, chunks placed, characters, characters placed, and the placed chunks' edits |
| `local_cer_a`, `local_cer_b`, `local_cer_sym` | Edits over placed characters: `a`'s chunks against `b`, `b`'s against `a`, and both together. This is the disagreement where both readers have the text, whatever the order |
| `matched_a`, `matched_b` | The share of each reader's characters that found a place in the other's text |

**The local alignment.** A reader's normalised text is cut into chunks of 40 whitespace words. Each chunk is placed in the other text by its anchors: words of 4 or more characters that occur at most twice in the other text. Each anchor votes for an offset, and the chunk goes where at least 2 anchors agree to within 6 words (the densest band, the first on ties). The chunk's character edits are then counted against the best-matching stretch of that place, plus 12 words either side: the smallest edit distance to any substring there. A chunk with edits above 20% of its characters is halved and each half placed on its own, down to 10 words, which handles chunks that straddle a seam between two columns the other reader has in another order. A piece with edits above 50% of its characters counts as not placed. Each piece's search costs its length times its window, so a page costs about twice its length times 64 words.

For each reader on its own, the pass adds the damage metric v2 (`usnm_core::damage::score` against the title's catalog languages plus English: language, decision, `damage_rate`, `function_share`, `garbage_share`) and a known-word rate: the share of its word tokens in the top 5,000 words of one language (`damage::score_forced`). Both readers are measured in the same language (`known_lang`): LoC's detected language, else American Stories', else the title's first catalog language.

American Stories' text is normalised with `usnm_core::text::normalize_ocr`, as the release and the quality stage normalise it; LoC's comes from the curated row, which is normalised the same way. A page where either text is over the `usnm_quality` text limit (`MAX_TEXT`, 512 KiB) gets no pair (`skipped = too_long`), as the quality stage leaves those texts out too.

Everything is a pure function of the two texts and the title's languages, single-threaded per page, with floats rounded to six decimals. The version is `usnm_ocr_quality::agreement::VERSION`. It goes up whenever the numbers for the same texts would change, including the normalisation, the rounding or the table's columns, and the tables live under the version (`text-agreement/v1/`).

## Feeding the hat

`cer_sym` here is the number the `usnm_quality` app computes for the `loc`/`as` pair, rounded the same way. So the table can stand in for that pair's disagreement on any page:

- With a third reader (our OCR in `curated/ocr-local`, 19,762 pages today, or a vision-language reading), `hat::solve(3, [(loc, as, cer_sym), (loc, ours, …), (as, ours, …)])` gives each reader's `est_cer` without recomputing the `loc`/`as` pair.
- With two readers the hat is underdetermined: `d ≈ e_loc + e_as` gives the sum of the two error rates, not either one.
- On LoC against American Stories, the whole-page `cer_sym` is mostly reading order and coverage, not misreading (below). `local_cer_sym` is the better `d` for this pair. The `usnm_quality` record solves the hat on whole-page `cer_sym` today, so its `loc`/`as` estimates carry the same inflation. Changing the record's `d` would be a new record version, outside this pass.

## What it can and can't tell

- **Agreement isn't accuracy.** Two readers that read the same smudge the same wrong way agree on it. LoC's OCR and American Stories' were made by different engines from scans of the same pages, so their errors are less correlated than two runs of one engine, but not independent: a page that is faint, torn or bled through is hard for both. A low `cer_sym` says the two texts match. It doesn't say the text is right.
- **The pair doesn't say which reader is wrong.** `cer_sym` is symmetric. The per-reader rates are the hint: the reader with the higher `damage_rate`, the lower `known_rate` or the lower `cover` is more likely the one that misread or dropped text. Those are single-reader signals with their own blind spots (names, numbers, ads and other languages are not in a top-5,000 list).
- **Reading order inflates the whole-page rate.** American Stories' text is in its layout model's reading order (headlines, bylines and articles), and LoC's is in ALTO block order. When the columns or articles come out in a different order, `cer_sym` and `wer_*` are high although both readers read the words right. The word bags and the local alignment don't see order. On the first 5,427 pages of the 1% sample, the median `cer_sym` was 0.75 while the median `dice` was 0.78, so most of the whole-page distance was order and coverage.
- **The local rate needs some agreement to start from.** A chunk is placed only by words both readers read the same, so where one reader is garbage, little places: `matched_*` is low and `local_cer_*` covers only the parts that did place. Read `local_cer_*` together with `matched_*`. A low local rate with a low matched share is not a clean page. The infix search takes the best stretch near the anchors, so it can find a slightly lower count than a full alignment would.
- **Coverage differs.** American Stories keeps what its layout model finds (articles, headlines, captions), so it can leave out parts of a page such as ads, tables or mastheads, and LoC's text can carry regions that American Stories split or dropped. `cover_loc` (how much of LoC's text American Stories also has), `cover_as` and `len_ratio` separate "one reader has half the page" from "both read it, differently".
- **Only pages with both texts.** About 36% of the corpus has no American Stories text, and those pages get no row. Pages where LoC's text is `empty` or `short` and American Stories has text get a row with LoC at zero characters (`cer_loc` 1).

## Validation plan

1. **Parity with the `usnm_quality` record.** For the 214,449 pages in validated `usnm_quality` results that have an `as` reader, the table's `cer_sym`, `cer_loc`, `cer_as`, `wer_*` and `dice` must equal the record's `readers[loc].pairs[as]` exactly, whenever both read the same texts (same curated part and American Stories part). Any difference is a bug in one of the two, or a text that changed between staging and this pass.
2. **Parity with the damage tables.** `loc_damage_rate` and `loc_function_share` must equal `audit/damage/v2`'s `damage_rate` and `function_share` for the same `doc_id` and batch version.
3. **Against the hat where it is determined.** On the 8,364 staged pages with three or more readers, compare the two-reader signals with the hat's `est_cer` for `loc` and for `as`. The checks: Spearman of `cer_sym` against `est_cer_loc + est_cer_as`, and how often the reader with the lower `known_rate` (or higher `damage_rate`) is the one the hat rates worse. That says how far the two-reader table can rank pages and readers where no third reader exists.
4. **Against ground truth.** The 49 hand-corrected Chronicling America articles (Overproof dataset 3, via HIPE-OCRepair-2026; [gold set](ocr-gold-set.md#public-ground-truth)) give true CERs for LoC's text of those articles. Where the article's page has American Stories text, align the article to each reader's page text and check that the reader with the lower true CER also has the better per-reader signals, and that `cer_sym` grows with the sum of the two true CERs. With 49 articles this is a sanity check, not a calibration.
5. **The local alignment.** The unit tests check it on synthetic pages: reordered columns with known errors, half a page missing, unrelated texts. On real pages, in a sample stratified by decade and by `cer_sym − local_cer_sym`, check by eye that the pages with the largest gap are reorderings, and that chunks left unplaced are text the other reader lacks (ads, tables, mastheads) or garbage. On the 8,364 three-reader pages, solve the hat with `local_cer_sym` as the `loc`/`as` disagreement and with the record's `cer_sym`, and compare each against the gold and against each other.

## Running it

```sh
usnm-ingest --curated <curated dir> --reference <reference dir> --state-file <file> \
  ocr-agreement --version pages-v20261022-1 --out <output dir> [--sample-pct 1] [--parallel 5]
```

- `--sample-pct` scores only the pages the damage audit's sampler picks (BLAKE2b of the `doc_id`), into `text-agreement/v1/sample-<cut>/`, so a sample never stands in for the full tables.
- Each batch version gets `<batch>_verNN.parquet` (one row per page with American Stories' text) and then `<batch>_verNN.summary.json` (fixed-bin histograms, `cer_sym` by decade, and the American Stories parts read, with their sha256). The summary marks the batch done: a rerun skips it, so a stopped run continues where it left off.
- After a run over every batch, `versions/<version>.json` lists the tables and `versions/<version>.summary.json` sums the histograms with their quantiles.
- It writes only to `--out`, never to `curated`. It logs counts and doc_ids, never text.

On-prem, it runs as a systemd unit in a container that mounts the local copy of `curated` read-only. It isn't run from a workstation.

## The table

One row per page with American Stories' text and batch version. `doc_id` repeats across batch versions for the 91,872 duplicate pages; join on `(doc_id, batch, batch_version)`, or keep the batch version the release used (`duplicates.json`).

| Column | Type | Notes |
| --- | --- | --- |
| `doc_id`, `lccn`, `batch` | string | |
| `date` | date | |
| `batch_version` | int16 | |
| `metric_version` | int32 | `agreement::VERSION` |
| `loc_text_status` | string | `ok`, `short` or `empty` |
| `loc_chars`, `loc_words`, `as_chars`, `as_words` | int32 | After normalisation |
| `loc_lang`, `loc_decision`, `as_lang`, `as_decision` | string, nullable | Damage v2's detection (MARC code; `detected`, `mixed` or `und`) |
| `loc_damage_rate`, `loc_function_share`, `loc_garbage_share`, `as_damage_rate`, `as_function_share`, `as_garbage_share` | float64, nullable | Damage v2 |
| `loc_known_rate`, `as_known_rate` | float64, nullable | Share of word tokens in `known_lang`'s top 5,000 |
| `known_lang` | string, nullable | |
| `char_edits`, `word_edits` | int32, nullable | |
| `cer_sym`, `cer_loc`, `cer_as`, `wer_loc`, `wer_as`, `dice` | float64, nullable | `PairMetrics` |
| `bag_words_loc`, `bag_words_as`, `shared_words` | int32, nullable | |
| `cover_loc`, `cover_as`, `len_ratio` | float64, nullable | |
| `local_cer_sym`, `local_cer_loc`, `local_cer_as` | float64, nullable | The local alignment's rates (null when nothing placed) |
| `matched_loc`, `matched_as` | float64, nullable | Shares of each reader's characters placed |
| `chunks_loc`, `chunks_as` | int32, nullable | 40-word chunks of each reader |
| `skipped` | string, nullable | `too_long`, with the pair columns null |
