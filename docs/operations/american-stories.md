# American Stories

Part of the [operations guide](README.md).

## American Stories' text

The release indexes American Stories' text beside LoC's (04 §4.9, 05 §5.5.4, #218) only with `USNM_AMERICAN_STORIES` (the job's `--american-stories`), off by default. The first release with it builds a full base, so turn it on for a full rebuild, after step 4 of #218 has measured the index size and rebuild time, and with an explicit go-ahead:

1. Run the writer to the end: `scripts/ja-ocr/start-quality.sh --env prod --command american-stories-write` on the one-replica job. It marks each finished year with `curated/american-stories/years/{year}.json`; start it again until all 180 years have one (its log's `american stories write finished` line counts them). A release reads only the years with a marker, and a year finished after the base reaches the base's pages only at the next full rebuild.
2. Do the [full rebuild](rebuild-and-index-storage.md#full-rebuild), with the setting added to its first provision:

   ```sh
   scripts/settings.sh --env prod --key USNM_AMERICAN_STORIES --value true
   scripts/settings.sh --env prod --key USNM_INGEST_FULL --value true          # optional: the setting forces a full base anyway
   ```

   With no year marked yet, the run fails at its start, before curation and titles-sync (`--american-stories (USNM_AMERICAN_STORIES) is set, but the curated store has no finished year of American Stories' text`). Its log has `American Stories' text` (years, parts) near the start, then `American Stories' text for the batch` per batch (pages, `text_mb`: what the release holds in memory for the batch), and `American Stories' text indexed` (documents with the text, and those with only it) before the merges.

3. Check the result: `current.json` has `american_stories: 1`; `reference/<version>/manifest.json` has `built_from.american_stories` and `build.features.american_stories`, and `american_stories.json` lists the parts read. On the site, a word from an American Stories headline finds pages, and a hit whose match is only there shows its snippet from that text.

Keep `USNM_AMERICAN_STORIES` set afterwards: weekly deltas then write the text for their pages too. Clearing it makes the next release publish a version without `american_stories`, so searches leave the text out; setting it again rebuilds in full.

To stop searching the text at once, without a release (to time cold searches without it, #251, or if it costs too much), switch the API's setting off, and on again the same way:

```sh
scripts/settings.sh --env prod --key USNM_AMERICAN_STORIES_SEARCH --value false   # on again: true, or --unset for the default
scripts/provision.sh --env prod
```

The API then searches LoC's text alone, whatever `current.json` says (05 §5.5.4): no `matched_in`, no American Stories snippets or badge, no `total.american_stories_only`. The indexes keep the text, so switching back needs no rebuild. The provision changes the API container's settings, so Container Apps starts a new replica: it loads the version, runs the startup warm-up and reports ready when it ends (about 5 minutes) or the readiness cap passes (6 minutes in prod, `USNM_READY_CAP_SECS`, 06 §6.6), and the old replica serves until then. Responses computed in one state are cached under keys of their own (`…|american_stories=off` when off), so the first switch off finds none cached: the warm-up computes what its budget allows and other searches start cold. Switching back on finds the responses persisted before (in Blob) still there. Check the result in the replica's start-up log line `American Stories' text search (USNM_AMERICAN_STORIES_SEARCH)` (`setting`, `in_version`, `searched`, `text_layout`) or in `/v1/meta` (`"american_stories"`). Browsers may keep a response they fetched in the other state for up to a day (06 §6.5).

On a version with one searched text field (`text_layout: 2`, [Full rebuild](rebuild-and-index-storage.md#full-rebuild)) the setting can't switch the text off: it shares the field with LoC's. The API logs `USNM_AMERICAN_STORIES_SEARCH=false has no effect on this version` and searches both texts. To leave the text out there, roll `current.json` back to a version with a field per text, or rebuild in full without `USNM_AMERICAN_STORIES`.

## American Stories against LoC's text

`jaocr.py american-stories --year Y [--year …] [--sample-pct 10]` (`ja-ocr/american_stories.py`, #205) compares [American Stories](https://huggingface.co/datasets/dell-research-harvard/AmericanStories) (Dell et al. 2023, CC BY 4.0), a re-OCR of Chronicling America, with LoC's text on our pages. It streams each year's tarball from Hugging Face (0.4 to 7.6 GB; nothing is written to disk), joins scans to our pages using the shared placement policy below (names read in a first pass), keeps the audit's fixed sample (`quality.sampled`), and reads LoC's text for the sampled pages of those years from the batches whose dates overlap them. For pages of titles whose first language is English it reports the join, words per page, the audit's English damage rate and function-word share for each text, pages matching a fixed term list in either text, and LoC's damage rate by the share of regions American Stories marked Illegible.

```sh
scripts/ja-ocr/start-quality.sh --env prod --command american-stories --year 1865 --year 1925
scripts/ja-ocr/quality-rows.sh --env prod --execution <execution>    # its "american stories" rows
```

It writes `audit/american-stories-<version>-<execution>.json`. It runs on the one-replica audit job, `caj-usnm-jaone-<env>` (`ja-ocr/solo.py` still keeps a second replica out, as for `jaocr.py mixed`). A failed run raises _Japanese OCR or audit job failed_ (severity 3), not the ingest alert.

### Indexing only where American Stories differs (`--diff`, #251)

`--diff` adds a measurement for indexing American Stories' text only where it differs from LoC's, to cut the second text field's cost to cold searches (#251). On the same pages it tokenizes both texts as the index does (`usnm_core::text`: hyphen rejoining, ligatures, split on what isn't a letter or digit, ASCII folding, tokens up to 40 characters), aligns them (a patience diff: tokens unique to both texts as anchors, `difflib.SequenceMatcher` with `autojunk=False` on small gaps without one), and builds each page's delta for k = 0, 2, 3 and 5: every region where American Stories' tokens differ from LoC's, with k of its tokens each side (where LoC has tokens American Stories hasn't, just the k each side), overlapping or touching windows merged. A term in American Stories' text is in LoC's or in the delta, and a phrase of n words that matches only in American Stories' text is inside one window once k is at least n - 1; the run checks both. New `american stories` tables:

- `diff_summary` (per year and k): American Stories', LoC's and the delta's tokens, the delta's share of American Stories' tokens (in total, and the median and 90th percentile per page), unique terms per page summed for each text and the delta (postings grow with them), `terms_lost` (should be 0), every page's distinct bigrams and trigrams that match only in American Stories' text and how many the delta keeps (`ngram2_loss`, `ngram3_loss`), the same for those whose words are all in LoC's text (`ngram*_edge*`: together only in American Stories'), n-grams that would match falsely if the windows ran together with no position gap (`ngram*_spurious_if_joined`), and `secs_per_1000_pages` for the diff.
- `diff_phrase` (per year, for the term list and 10 common phrases such as "new york" and "cross of gold") and `diff_phrase_group` (per year, for those two groups and the bigrams and trigrams of the run's `only_american_stories` contexts): pages matching in LoC's text, in American Stories', only in American Stories' (`only_words_all_in_loc` of those), and per k the ones the delta keeps (`kept_k*`, `loss_k*`, `edge_kept_k*`, `spurious_if_joined_k*`).
- `diff_edge_examples`: up to 10 pages a year with a phrase whose words are all in LoC's text but together only in American Stories', with loc.gov links.

```sh
scripts/ja-ocr/start-quality.sh --env prod --command american-stories --year 1865 --year 1925 --diff
```

The diff runs after the comparison and takes about 40 s per 1,000 pages of 8,000 tokens (measured on synthetic pages; the run logs its own figure in `american stories diff year`).

## American Stories' text for our pages

`jaocr.py american-stories-write [--year Y …]` (`ja-ocr/american_stories_write.py`, #218) writes American Stories' text (Dell et al. 2023, CC BY 4.0) for our pages to the curated container, for the release to index beside LoC's. It streams each year's tarball from Hugging Face twice on a fresh run (names/placement, then text), places every scan on our page as `american-stories` does, and writes `american-stories/pages/<lccn>/<year>-<nnn>.parquet`: one row per resolved page with `doc_id`, `scan_key` (JSON physical identity tuple), `source_name` (archive member), `lccn`, `date`, `text` (each article's headline, byline and text in American Stories' order, separated by blank lines), `articles` (JSON: each article's headline, byline, `start`/`end` in `text`, bounding boxes in scan pixels and the legibility of its regions), `legibility` (the page's text regions by legibility) and the scan's `width` and `height`. A page's second copy is skipped. Each year ends with a marker `american-stories/years/<year>.json` holding its counts; a rerun skips marked years, so an interrupted run resumes. A download that goes 5 minutes without data, or breaks off, fails and the year retries (reusing a frozen placement if already written; up to 3 tries, a minute apart, each logged as `american stories year failed; retrying`); a retry writes the same parts under the same names. A year counts as read only when the gzip stream reached its end marker and, when Hugging Face sends one, the byte count matches `Content-Length`, so a download that stops at a file boundary inside the tarball is retried rather than marked. All 180 archives total about 340 GB compressed (about 680 GB transferred for two fresh passes).

```sh
scripts/ja-ocr/start-quality.sh --env prod --command american-stories-write          # every year
scripts/ja-ocr/start-quality.sh --env prod --command american-stories-write --year 1865
```

It runs on the one-replica job and logs `american stories year written` per year. `jaocr.py american-stories` also lists, per term and year, up to 8 pages that match only in American Stories' text with the words around the match (`only_american_stories`), for checking those matches by hand.

### Physical scan identity and existing data (#573)

`p<page>` is a label, **not a scan identity**. The shared audit/writer key is
`(LCCN, date, edition, reel, image)` from the scan filename, ignoring its archive
directory and page label. Different images with `p1` must not be discarded as
copies. Edition and reel remain part of identity, so equal image numbers on
different reels do not alias.

Unambiguous numbered issues keep their existing joins. An issue with repeated
labels on distinct scan keys, conflicting labels on one scan, inconsistent
label/image offsets within a reel, or any missing/zero page label requires LoC's
`/item/<lccn>/<date>/ed-<edition>/?fo=json`. Its ordered `resources[0].files`
JP2 paths map **reel + image** to sequence. All labels in that issue are ignored,
including an apparently matching first `p1`; only physical keys uniquely present
in the metadata can join. Multiple representations mapping to the same verified
sequence are copies; the first archive member wins. Neither sorting image
numbers nor subtracting the minimum observed image is a safe fallback: the
archive can omit the first page, and LoC sequences need not follow image order.
Requests use the existing rate limiter (`JAOCR_API_GAP`, default 3.5 seconds).

Unavailable/ambiguous metadata and unmatched scans are **unresolved**, not
`duplicates`. They receive no guessed `doc_id`/indexed text. The writer retains
names, physical keys and reasons in `american-stories/unresolved/<year>.json`;
the audit retains the same records in its saved summary's `scans_unresolved_scans`.
Console logs include only their counts, not the unbounded inventory, so large
unresolved cohorts cannot truncate the log-based coverage summaries.
`page_from_image` now counts every resolved scan using authoritative issue
metadata, including numbered scans whose labels were ignored. Year markers
include `identity_version: 2`. The writer freezes the IDs, counts and unresolved
records in `american-stories/placements/<year>.json` **before writing parts**,
so retry/resume cannot leave stale parts after a metadata outage changes the
chosen placements. Metadata resolution is renewed only by an intentional
placement reset; unresolved cohorts must be reviewed, not interpreted as proof
of physical absence.

**No production repair or rebuild is performed by this change.** Existing
indexed `text_as` and completed curated years remain unchanged: markers still
skip existing imports. The historical 1925 cohort has 4,051 label-based
collisions, not 4,051 verified duplicate scans (#327); some indexed sequence-1
texts belong to later physical pages. Correcting the importer alone does not
repair those existing fields or establish a corrected coverage percentage.

**Coordinate any repair with the owner of #283**, while no ingest/release or
writer is reading/changing these paths. First preserve the old per-year
markers/parts and plan a scoped reimport of affected years. Remove the selected
year's completion marker, **all** its title parts (including any interrupted
old import), placement snapshot and unresolved sidecar before importing it with
the fixed writer. Do not just delete the completion marker: a smaller new import
could leave old tail parts that the release would still read. Verify the new
`identity_version`, unresolved cohort and representative physical joins before
allowing the separately approved rebuild. A metadata retry also requires this
whole-year reset, not simply removing the placement snapshot alongside existing
parts. No bulk reimport/reindex is authorized here.

The next approved **full** rebuild under #283 will take repaired curated rows
for years explicitly reimported first: recover resolvable missing sequences,
replace misassigned text at previously matched sequences, and omit unresolved
American Stories assignments (LoC text is unchanged). Without that reimport it
will read the same old rows and **will not repair #573**; an ordinary delta does
not replace existing base-page text. This change does not bump the Rust American
Stories version or trigger a full rebuild. The existing reader ignores the
additional provenance columns; the canonical `doc_id` format is unchanged.
