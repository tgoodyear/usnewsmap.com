# Phrases with common words, and the index layout around them

**Status:** shipped; first full rebuild with it is `pages-v20261005-1` (`common_grams` 1), not yet measured on the live site · **Date:** October 2026 · **Measured on:** 90,745 real pages from 11 LoC batches (cost and gain, cold on 2 CPUs); the capture of `pages-v20261003-1` in `ops/version-snapshots/` (the before) · **Code:** `crates/usnm-core/src/common_grams.rs`, `crates/usnm-ingest` (release), `infra/quickwit/pages-index.yaml`; [05 §5.5.1](../design/05-search-and-storage.md), [05 §5.5.3](../design/05-search-and-storage.md), [06 §6.5](../design/06-api-design.md) · **Issues:** #154, #156, #158, #160

## The question

One search cost four to five times the rest. In the cold-search benchmark (06 §6.5) the phrase `"cross of gold"` took 65 s alone on a warm searcher. Over 1896 alone: the phrase 41 s, `cross gold` within 2 words 9 s, the phrase `"yellow fever"` 8 s and `gold` on its own 4 s. A phrase reads the positions of every word in it, and `of` is on almost every page many times over, so a phrase holding a common word reads a large part of the whole index's positions. These are the home page's own example searches ("Cross of Gold, 1896"), so visitors hit them first.

The capture of the live version before the rebuild shows the same thing. `scripts/compare-versions.py capture` asked `pages-v20261003-1` the ten benchmark searches, the home page's 50 examples and three Japanese ones on 5 October 2026. The slowest, by the backend's own timing (which is the time when the result was computed, possibly earlier than the capture if a cache answered):

| Search | Hits | Backend time |
|--------|------|--------------|
| bench: `"cross of gold"` 1896 | 1,592 | 57.6 s |
| example: standard-oil | 354,285 | 50.8 s |
| example: cross-of-gold (June–December 1896) | 1,583 | 44.8 s |
| example: civil-service-reform | 58,720 | 32.3 s |
| example: new-deal | 104,511 | 19.6 s |
| bench: `"yellow jack"` 1878 | 536 | 16.7 s |
| bench: `gold` near `silver` | 1,408,756 | 16.0 s |
| most other examples | | 4–15 s |

Only the "cross of gold" searches hold a word on the common-word list. The other slow phrases (`standard oil`, `civil service reform`, `new deal`) don't, so the pairs don't explain them; see *What is left*.

## What we did

**Common-word pairs** (#154, 05 §5.5.3). Each page is indexed a second time in a field `text_cg` where each common word is joined to the word after it: "Upon a cross of gold" becomes `upon_a a_cross cross of_gold gold`. The field has the same positions as `text`, one token per word the analyzer sees (and a placeholder where it drops an over-long OCR run). An exact phrase with a common word that isn't last is searched there, mapped word for word (`cross of_gold gold`): it matches at a position exactly when the phrase's words are at that position in `text`, because each common word's token also checks the word after it. Phrases that end in a common word, phrases with none, and phrases with slop stay in `text`. The query still requires each non-common phrase word in `text` so the snippets on `text` get their highlights. Unit tests check the equivalence for every phrase of up to four words from a small vocabulary, and the Quickwit parity tests compare counts, cubes, hits and their order with the reference backend on the fixtures.

The list, `usnm_core::common_grams::WORDS`, is 89 words: the function words on about half of all pages or more, measured over a week of 1896, plus `mr`. Frequent content words (`new`, `time`) are left out. Changing the list changes the index, so it bumps `common_grams::VERSION`.

**Rollout.** `current.json` names the `common_grams` version its indexes were built with. The API searches `text_cg` only when that matches its own, so a new API on old indexes keeps phrases in `text`. A release whose published version has another version (or none) builds a full base, so no version mixes the two. That full base is `pages-v20261005-1`.

**Two changes carried by the same rebuild.**

- **Splits of 60,000 pages** (#156). The pairs make a page about 37 KB of index instead of 23, so 100,000-page splits would have grown from about 2.6 GB to 4.2 GB, and the writer holds a split in memory while it uploads it. At 60,000 pages a merged split is about 2.4 GB and at most 4.4 GB again. The Japanese index gets the same target, because the release's merge checks read the main template for every index.
- **128 KB docstore blocks** (#160). A cold page of hits decompresses one docstore block per page returned; at Quickwit's 1 MB default that is about 40 pages decompressed for each one shown. At 128 KB (about 5 pages) cold hits were about 3× faster on the 90,745 pages, for an index about 8% larger.

**A finding on the way** (#158, 05 §5.5.1). Quickwit's `datetime` field stores and returns 1890s dates correctly, but every timestamp range on it (`start_timestamp`/`end_timestamp`, `date:[…]`, an Elasticsearch `range`) matches no page before April 1972, with no error: Quickwit rewrites the range with bounds in nanoseconds, which it only reads for 1972 to 2242. Buckets and date filters use the integer fields (`day`, `ym`, `year`) and the API never sends timestamps.

## What we measured

**Setup** (from #154, where the measurements were reported): 90,745 real Chronicling America OCR pages from 11 random LoC batches (2.6 GB of text, about one production split), indexed with Quickwit 0.9.1 once without `text_cg` and once with it; the searcher pinned to 2 CPUs with its result cache off; each search run cold, with the OS cache dropped first; medians of 3 runs; bytes read from disk and wall time per search. The commands themselves aren't in the repository, so the PR description is the record of the procedure, and rerunning it means setting it up again from that description.

| Phrase | Read from disk, `text` → `text_cg` | Time |
|--------|-----------------------------------|------|
| cross of gold | 17.3 MB → 0.6 MB (31× less) | 3.0× faster |
| secretary of war | 17.5 → 0.8 MB (22×) | 3.2× |
| united states of america | 17.8 → 1.1 MB (16×) | 2.3× |
| in the city of new york | 52.6 → 4.6 MB (11×) | 3.8× |
| board of trade | 17.5 → 0.8 MB (22×) | 2.7× |
| the president | 21.4 → 0.8 MB (28×) | 6.5× |
| for sale | 4.1 → 0.9 MB (4×) | 2.1× |

Hit counts were identical for every phrase. Index size 4.8 GB against 3.0 GB (1.6×); indexing 273 s against 143 s (1.9×). Over the corpus that is about +0.35 TB on Blob Hot, about +$7 a month (05 §5.5.3).

**Docstore blocks** (#160, same pages, Quickwit 0.9.1): cold (OS cache dropped), a page of 50 hits, median of 5:

| Query | 1 MB blocks | 128 KB blocks |
|-------|-------------|---------------|
| `text:gold` | 67 ms | 18 ms |
| `text:silver AND text:bryan` | 57 ms | 19 ms |
| `text:fever` | 51 ms | 19 ms |
| Index size | 2.6 GB | 2.8 GB (+8%) |

What the numbers don't show: both were measured on 11 batches on local disk, where times are mostly CPU, not on the job's searcher against Blob, where bytes read dominate. The 11–31× drop in bytes is the better guide to production; the times are a prediction. The live before/after is the next capture.

## What is left

- **Capture `pages-v20261005-1`** at a quiet hour with `scripts/compare-versions.py capture`, keep it under `ops/version-snapshots/`, and `diff` it against the `pages-v20261003-1` capture. Expected: the same hits for every search (the equivalence proof says so; the diff flags any search whose hits move by more than 1%), and the two "cross of gold" searches several times faster. Record the result in a follow-up note.
- **The other slow phrases.** `standard oil`, `civil service reform` and `new deal` have no word on the list, yet took 20–50 s on `pages-v20261003-1`. Whether that is the positions of frequent content words or the number of matches isn't known; it isn't the docstore, because a capture asks only `/v1/aggregate`, whose engine requests ask for no hits (`max_hits: 0` in `usnm_search::quickwit`), so a capture's timings never touch it. The 60,000-page splits will have moved these searches too, so measure before explaining.
- **The docstore change on the live site.** For the same reason, no capture can show the 128 KB blocks' effect. Measuring it means timing `/v1/hits` cold on both versions, which `scripts/compare-versions.py` doesn't do; adding a hits page per search to the capture is the simplest way.
- **The list is from one week of 1896.** Whether the commonest words of 1850 or 1950 differ enough to matter hasn't been measured; the list is versioned, so changing it is a full rebuild.
