# American Stories

Part of the [operations guide](README.md). The `jaocr.py` commands here run on the Japanese OCR job's image through `ocr/ja-ocr/tools/start-quality.sh`, which needs the Azure jobs that prod doesn't have ([Japanese OCR](japanese-ocr.md)).

## American Stories' text

The release indexes American Stories' text beside LoC's (04 §4.9, 05 §5.5.4, #218) when `USNM_AMERICAN_STORIES` is set (the job's `--american-stories`), off by default and on in prod. Prod's text was written and indexed under #218; in prod's one-field layout it shares `text_all` with LoC's ([Full rebuild](rebuild-and-index-storage.md#full-rebuild)). The first release with the setting builds a full base. To turn it on in an environment:

1. Run the writer to the end ([below](#american-stories-text-for-our-pages)). It marks each finished year with `curated/american-stories/years/{year}.json`; start it again until all 180 years have one (its log's `american stories write finished` line counts them). A release reads only the years with a marker, and a year finished after the base reaches the base's pages at the next full rebuild.
2. Do a [full rebuild](rebuild-and-index-storage.md#full-rebuild) with `USNM_AMERICAN_STORIES=true` (it forces a full base anyway). With no year marked yet, the run fails at its start, before curation and titles-sync (`--american-stories (USNM_AMERICAN_STORIES) is set, but the curated store has no finished year of American Stories' text`). Its log has `American Stories' text` (years, parts) near the start, then `American Stories' text for the batch` per batch (pages, `text_mb`: what the release holds in memory for the batch), and `American Stories' text indexed` (documents with the text, and those with only it) before the merges.
3. Check the result: `current.json` has `american_stories: 1`; `reference/<version>/manifest.json` has `built_from.american_stories` and `build.features.american_stories`, and `american_stories.json` lists the parts read. On the site, a word from an American Stories headline finds pages, and a hit whose match is only there shows its snippet from that text.

Keep `USNM_AMERICAN_STORIES` set afterwards: deltas then write the text for their pages too. Clearing it makes the next release publish a version without `american_stories`, so searches leave the text out; setting it again rebuilds in full.

**Searching without it.** On a version with a field per text, `USNM_AMERICAN_STORIES_SEARCH=false` and a provision make the API search LoC's text alone, whatever `current.json` says (05 §5.5.4): no `matched_in`, no American Stories snippets or badge, no `total.american_stories_only`. The indexes keep the text, so switching back needs no rebuild. Responses computed in each state are cached under keys of their own (`…|american_stories=off` when off), so the first switch off finds none cached. Check the API's start-up line `American Stories' text search (USNM_AMERICAN_STORIES_SEARCH)` (`setting`, `in_version`, `searched`, `text_layout`) or `/v1/meta` (`"american_stories"`). On a one-field version (`text_layout` 2 or 3, as prod's) the setting can't switch the text off: the API logs `USNM_AMERICAN_STORIES_SEARCH=false has no effect on this version` and searches both. To leave the text out there, roll `current.json` back to a version with a field per text, or rebuild without `USNM_AMERICAN_STORIES`.

## American Stories against LoC's text

`jaocr.py american-stories --year Y [--year …] [--sample-pct 10]` (`ocr/ja-ocr/american_stories.py`, #205) compares [American Stories](https://huggingface.co/datasets/dell-research-harvard/AmericanStories) (Dell et al. 2023, CC BY 4.0), a re-OCR of Chronicling America, with LoC's text on our pages. It streams each year's tarball from Hugging Face (0.4 to 7.6 GB; nothing is written to disk), joins scans to our pages using the shared placement policy below (names read in a first pass), keeps the audit's fixed sample (`quality.sampled`), and reads LoC's text for the sampled pages of those years from the batches whose dates overlap them. For pages of titles whose first language is English it reports the join, words per page, the audit's English damage rate and function-word share for each text, pages matching a fixed term list in either text, and LoC's damage rate by the share of regions American Stories marked Illegible.

```sh
ocr/ja-ocr/tools/start-quality.sh --env prod --command american-stories --year 1865 --year 1925
ocr/ja-ocr/tools/quality-rows.sh --env prod --execution <execution>    # its "american stories" rows
```

It writes `audit/american-stories-<version>-<execution>.json`. It runs on the one-replica audit job, `caj-usnm-jaone-<env>` (`ocr/ja-ocr/solo.py` keeps a second replica out, as for `jaocr.py mixed`).

### Indexing only where American Stories differs (`--diff`, #251)

`--diff` adds a measurement for indexing American Stories' text only where it differs from LoC's, to cut the second text field's cost to cold searches (#251). On the same pages it tokenizes both texts as the index does (`usnm_core::text`: hyphen rejoining, ligatures, split on what isn't a letter or digit, ASCII folding, tokens up to 40 characters), aligns them (a patience diff: tokens unique to both texts as anchors, `difflib.SequenceMatcher` with `autojunk=False` on small gaps without one), and builds each page's delta for k = 0, 2, 3 and 5: every region where American Stories' tokens differ from LoC's, with k of its tokens each side (where LoC has tokens American Stories hasn't, just the k each side), overlapping or touching windows merged. A term in American Stories' text is in LoC's or in the delta, and a phrase of n words that matches only in American Stories' text is inside one window once k is at least n - 1; the run checks both. New `american stories` tables:

- `diff_summary` (per year and k): American Stories', LoC's and the delta's tokens, the delta's share of American Stories' tokens (in total, and the median and 90th percentile per page), unique terms per page summed for each text and the delta (postings grow with them), `terms_lost` (should be 0), every page's distinct bigrams and trigrams that match only in American Stories' text and how many the delta keeps (`ngram2_loss`, `ngram3_loss`), the same for those whose words are all in LoC's text (`ngram*_edge*`: together only in American Stories'), n-grams that would match falsely if the windows ran together with no position gap (`ngram*_spurious_if_joined`), and `secs_per_1000_pages` for the diff.
- `diff_phrase` (per year, for the term list and 10 common phrases such as "new york" and "cross of gold") and `diff_phrase_group` (per year, for those two groups and the bigrams and trigrams of the run's `only_american_stories` contexts): pages matching in LoC's text, in American Stories', only in American Stories' (`only_words_all_in_loc` of those), and per k the ones the delta keeps (`kept_k*`, `loss_k*`, `edge_kept_k*`, `spurious_if_joined_k*`).
- `diff_edge_examples`: up to 10 pages a year with a phrase whose words are all in LoC's text but together only in American Stories', with loc.gov links.

```sh
ocr/ja-ocr/tools/start-quality.sh --env prod --command american-stories --year 1865 --year 1925 --diff
```

The diff runs after the comparison and takes about 40 s per 1,000 pages of 8,000 tokens (measured on synthetic pages; the run logs its own figure in `american stories diff year`).

## American Stories' text for our pages

`jaocr.py american-stories-write [--year Y …]` (`ocr/ja-ocr/american_stories_write.py`, #218) writes American Stories' text (Dell et al. 2023, CC BY 4.0) for our pages to the curated container, for the release to index beside LoC's. It streams each year's tarball from Hugging Face twice on a fresh run (names/placement, then text), places every scan on our page as `american-stories` does, and writes `american-stories/pages/<lccn>/<year>-<nnn>.parquet`: one row per resolved page with `doc_id`, `scan_key` (JSON physical identity tuple), `source_name` (archive member), `lccn`, `date`, `text` (each article's headline, byline and text in American Stories' order, separated by blank lines), `articles` (JSON: each article's headline, byline, `start`/`end` in `text`, bounding boxes in scan pixels and the legibility of its regions), `legibility` (the page's text regions by legibility) and the scan's `width` and `height`. A page's second copy is skipped. Each year ends with a marker `american-stories/years/<year>.json` holding its counts; a rerun skips marked years, so an interrupted run resumes. A download that goes 5 minutes without data, or breaks off, fails and the year retries (reusing a frozen placement if already written; up to 3 tries, a minute apart, each logged as `american stories year failed; retrying`); a retry writes the same parts under the same names. A year counts as read only when the gzip stream reached its end marker and, when Hugging Face sends one, the byte count matches `Content-Length`, so a download that stops at a file boundary inside the tarball is retried rather than marked. All 180 archives total about 340 GB compressed (about 680 GB transferred for two fresh passes).

```sh
ocr/ja-ocr/tools/start-quality.sh --env prod --command american-stories-write          # every year
ocr/ja-ocr/tools/start-quality.sh --env prod --command american-stories-write --year 1865
```

It runs on the one-replica job and logs `american stories year written` per year. `jaocr.py american-stories` also lists, per term and year, up to 8 pages that match only in American Stories' text with the words around the match (`only_american_stories`), for checking those matches by hand.

### Physical scan identity and existing data (#573)

`p<page>` is a label, not a scan identity. The audit and the writer key a scan by `(LCCN, date, edition, reel, image)` from its filename, ignoring its archive directory and page label, so different images labelled `p1` are never discarded as copies, and equal image numbers on different reels don't alias.

Unambiguous numbered issues keep their joins. An issue with repeated labels on distinct scan keys, conflicting labels on one scan, inconsistent label-to-image offsets within a reel, or any missing or zero page label needs LoC's `/item/<lccn>/<date>/ed-<edition>/?fo=json`: its ordered `resources[0].files` JP2 paths map reel and image to sequence. All labels in that issue are then ignored, and only physical keys uniquely present in the metadata can join. Several representations mapping to the same verified sequence are copies; the first archive member wins. Neither sorting image numbers nor subtracting the smallest image is a safe fallback: the archive can omit the first page, and LoC sequences need not follow image order. Requests use the rate limiter (`JAOCR_API_GAP`, default 3.5 seconds).

Unavailable or ambiguous metadata and unmatched scans are unresolved, not duplicates: they get no guessed `doc_id` or indexed text. The writer keeps names, physical keys and reasons in `american-stories/unresolved/<year>.json`; the audit keeps the same records in its summary's `scans_unresolved_scans`. Console logs carry only their counts. Year markers include `identity_version: 2`. The writer freezes the ids, counts and unresolved records in `american-stories/placements/<year>.json` before writing parts, so a retry can't leave stale parts after a metadata outage changes the placements. Treat an unresolved cohort as something to review, not as proof a page is missing.

**Existing data is unchanged.** Indexed text and completed curated years written before #573 stay as they were: markers still skip them. The 1925 cohort has 4,051 label-based collisions, not 4,051 verified duplicate scans (#327); some indexed sequence-1 texts belong to later physical pages. A repair needs a scoped reimport of the affected years, coordinated with the maintainer while no release or writer reads or changes these paths, and then a full rebuild:

1. Keep the old per-year markers and parts.
2. For each selected year, remove its completion marker, all its title parts (including any interrupted old import), its placement snapshot and its unresolved sidecar, then import it with the fixed writer. Removing only the marker isn't enough: a smaller new import could leave old tail parts that the release would still read. A metadata retry needs the same whole-year reset.
3. Check the new `identity_version`, the unresolved cohort and representative physical joins before the separately approved rebuild.

That rebuild then recovers resolvable missing sequences, replaces misassigned text at previously matched sequences and leaves out unresolved assignments (LoC's text is unchanged). Without the reimport it reads the same old rows and doesn't repair #573; a delta doesn't replace base-page text. This procedure authorizes no bulk reimport or reindex.
