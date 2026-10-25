# ADR-0014: One searched text field for LoC's and American Stories' text

- **Status:** Accepted (built behind a setting; the full rebuild that turns it on waits for #283's measurements)
- **Date:** 2026-10
- **Amends:** 05 §5.5.4 (American Stories' text in fields of its own)
- **Amended by:** the [October 2026 amendment](#amendment-october-2026-locs-original-text-beside-our-ocrs-925) below (a third text in layout 3)

## Context

Since American Stories' text (#218), the main index has four positional text fields: LoC's `text` and its common-word pairs `text_cg`, and American Stories' `text_as` and `text_as_cg`. Each term of a query is looked up in both texts' fields, so a cold search warms two term dictionaries and two posting lists per word in every split. On 9 October, on production, switching American Stories' fields off halved a cold search's backend time (mean 15.8 s against 32.3 s) and took the median from 21.4 s to 13.1 s (#251). The patched Quickwit (ADR-0013) cut cold searches to a 3.5 to 3.8 s median, but common-word phrases over the whole range still took 14 to 45 s, and their cost is per field.

Two more facts shaped the design. Quickwit 0.9.1's `remove_long` filter in our `usnm_text` tokenizer drops tokens of 255 bytes or more, not the 40 characters `usnm_core` assumes (#286). And the aggregate's `total.american_stories_only` is a count of `(both texts) AND NOT (LoC's text alone) AND (American Stories' alone)`, which needs LoC's text indexed on its own.

## Decision

- **A version can search both texts in one field** (`text_layout: 2` in `current.json`, 05 §5.5.7). `text_all` holds LoC's words, 32 positions no query can match, then American Stories' words; `text_all_cg` holds each text's common-word pairs the same way. `text` and `text_as` are stored for snippets and `matched_in`, not indexed. A query looks each word up once.
- **The release writes the tokens**, one per word as the version's analyzer folds them, with `_` where a word is dropped and in the gap. The fields use Quickwit's `whitespace` tokenizer, as the pairs field already does. A gap of dropped filler words would cost at least 255 bytes a position, and a shorter filler word would be indexed and reachable by a prefix query.
- **`total.american_stories_only` is not counted on such a version.** Hits keep `matched_in` and `snippet_source`, which the API works out from the stored texts.
- **`USNM_AMERICAN_STORIES_SEARCH=false` has no effect on such a version**: the API logs a warning and searches both texts.
- **The API serves both layouts**, and refuses to load a layout it doesn't know. **Only a full base changes the layout** (`--single-text-field`, `USNM_SINGLE_TEXT_FIELD`, off by default); a delta follows the published version, as for the analyzer (#168, #278) and the decades (#123).

## Alternatives

| Option                                                          | Why not                                                                                                                                                          |
| --------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Index only where American Stories differs from LoC              | Measured on a 10% sample: the lossless delta (2 words of context) keeps 80 to 91% of American Stories' tokens, so it saves 5 to 20% of its postings (#251, #253) |
| `text_all` analyzed by `usnm_text`, gap of dropped filler words | At least 255 bytes per gap position (about 8 KB a page with both texts); a shorter filler is indexed and a 5-letter prefix query reaches it                      |
| Quickwit's `concatenate` field or an array of the two texts     | Tantivy leaves a gap of one position between values, so a NEAR search could span the texts                                                                       |
| Keep `text` indexed for the American Stories-only count         | Two texts' postings in the index again, and the count request reads them on every computed aggregate                                                             |
| A field of the words only American Stories has, for the count   | Exact for words and `AND` only; wrong for phrases, NEAR, prefixes and `NOT`                                                                                      |

## Consequences

- **Fewer lookups per search**: one term dictionary and posting list per word instead of two; three lookups for `"cross of gold"` instead of six; one request fewer per computed aggregate. The posting lists are longer than LoC's alone, so the expected cold-search cost is near, not at, the "American Stories off" timing. To be measured on the dev 1% set before the rebuild (#283).
- **The index should be no larger**: the same words with positions in one dictionary, and OCR runs of 41 to 254 bytes become `_` instead of a term each.
- **The indexed words are `usnm_core`'s**, the ones the query parser, the memory backend and the snippets use, rather than Quickwit's analysis of the raw text. Quickwit no longer analyzes queries a second time on these fields.
- **The site loses the page-count note** about pages only American Stories' text matches; each hit keeps its badge.
- **American Stories' text can't be switched off at query time** on such a version. Rolling `current.json` back to a version with a field per text, or a full rebuild without `--american-stories`, switches it off. A release without `--american-stories` on such a version builds a full base.
- **CI checks both layouts**: the parity tests load the fixture indexes a second time with the new mapping and run every query, filter and bucket against the reference on both.

## Amendment, October 2026: LoC's original text beside our OCR's (#925)

**Context.** With `--ocr-local` (12 §12.5 Step 9, #296) rule v1 gives a page our OCR's text in place of LoC's where it prefers it, and LoC's text is then in no field. In `pages-v20261022-1` that is 19,762 pages (19,374 of them frak2021) of 23.7 million. Where our text has a line of the next column inside a sentence, a phrase that LoC's text has whole finds nothing on that page (#925). Fixing the reading order of our OCR's own text is separate work that stays open on #925. Its to-do starts with comparing the block order in our ALTO with LoC's.

**Decision.** Keep both texts (decided 24 October 2026, #925, its option 1).

- **Layout 3** (`text_layout: 3`, `TextLayout::SingleWithLoc`) is layout 2 with a third text. On a page our OCR replaced LoC's text on, `text` is ours and `text_all` holds ours, then American Stories' if the page has it, then LoC's original text, each after a gap of 32 positions, with the pairs made per text. `text_loc` stores LoC's text, not indexed. A page is one document, so it counts once. The mapping gains `text_loc` in layout 3 only.
- **Which pages.** Those rule v1 replaced (`local_text` in the release) whose LoC text has a word the index keeps. The pages our Japanese OCR's Latin text replaced (#203) are not among them: that text is used where it has more words than LoC's, and LoC's text there stays out. The tally, `ocr_local.json` and `ocr_local_pages.bin` are unchanged.
- **Layout 2 is untouched.** Its documents, its mapping and its deltas are as they were, and the API still serves it. A delta follows the published layout, so a version published in layout 2 takes layout-2 deltas until the next full rebuild. `--single-text-field` means the latest one-field layout, so the next full rebuild with it builds layout 3. The API refuses a layout it doesn't know, so the API that knows layout 3 has to be serving before a version in it is published.
- **Hits say which text matched.** `matched_in` can list `loc_original` beside `loc` (the page's own text, ours on these pages) and `american_stories`, and `snippet_source` can be `loc_original`, when the query marks nothing in the page's own text or American Stories'. The site marks a page found only in LoC's original text with a "Library of Congress OCR" badge.
- **Only a full rebuild adds it.** Nothing in a published version changes.

**Alternatives.**

| Option                                                      | Why not                                                                                                                                                        |
| ----------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Edit layout 2 to carry the third text                       | Published layout-2 indexes and their deltas would no longer match what the code writes, and an API from before the change would read a layout it can't explain |
| Fix the reading order first and replace only agreeing pages | Waits on the reading-order fix, which is still open; until it lands, phrases stay broken on the replaced pages (#925's option 2)                               |

**Consequences.**

- **About 19.8 thousand pages of 23.7 million** gain a third text, so the index grows by LoC's words and stored text for those pages only.
- **Every hit that LoC's text gave comes back**, and so do matches on its garbled words, as before the replacement.
- **A search that matches only LoC's text** lists the page with the snippet from that text, so a reader can tell it from a match in ours.
- **`total.american_stories_only` stays absent** on one-field versions, as for layout 2.
- **CI checks layout 3** on the fixtures loaded a third time with `text_loc` on some pages, against the reference on every query, and the pipeline tests build it with `--ocr-local`.
