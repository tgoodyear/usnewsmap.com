# 11: Geographic Skew of Search Terms

**Status:** Phase 1 is built and live as the map's "Relative rate" view (`norm=skew`), with the per-language baselines and the merged duplicate places from phase 2 (§11.14). Not built: the public `/v1/skew` endpoint, words as the denominator, the two-gamma prior, per-title counts, per capita rates, and a relative-rate colouring of the States layer (§11.10) · **Date:** September 2026; status checked against the code on 10 November 2026 · **Code:** `crates/usnm-core/src/skew.rs` (scoring and its tests), `crates/usnm-core/src/skew/special.rs` (numerics and known-value tests), `crates/usnm-core/examples/term_skew.rs` (the evidence below), `web/src/engine/skew.ts`, `skewModel.ts`, `skewInput.ts` and `skew.worker.ts` (the browser port), `fixtures/skew-vectors.json` (shared test vectors)

## 11.1 The question

The maintainer asked: can the map show where a term is covered disproportionately in one place compared with others? A city with several newspapers will have more hits for any term than a town with one, so raw counts mostly show where the newspapers are.

When this was written, the site showed two things (07 §7.4):

- **Circle size** was the number of pages with a match. Big places get big circles for every search.
- **"Share of pages published"** (`norm=rel`) coloured each place by `hits / pages published` in the window, scaled to the largest value on the map. This already divided by corpus volume, but §11.5.4 shows that the largest value is usually a place with a handful of pages, so almost every other place ended up in the bottom half of the colour scale. (This view was removed in October 2026; old `norm=rel` links open on Pages. See §11.14.)

This document compares the ways to answer the question, recommends one, and backs it with measurements on the published index and with simulations.

## 11.2 Available data

Per page, the index holds the title (`lccn`), the place the title is catalogued under, the date, edition, sequence and language. `/v1/aggregate` returns pages with a match per place and time bucket (the cube) plus the national series (06 §6.3.3).

Each reference snapshot also holds `baselines.json`, the pages published per place and day, built at release from the per-batch counts of every curated page, including pages whose OCR text is empty (04 §4.5, `write_snapshot` in `crates/usnm-ingest/src/release.rs`). `/v1/coverage` serves it as a place by bucket cube from memory, and the site already fetches it with every search. So the denominator for "hits per page" exists for every place, bucket and version, with no new ingest work.

Not in the snapshot are pages per title per day (the per-batch counts files have them, but the snapshot sums them per place) and population. Snapshots written since the per-language baselines (§11.14) also hold `language_baselines.json`, the same pages split by the languages their title lists; older ones don't. A Japanese query (#139) searches only the Japanese pages of titles that list Japanese, so without a `lang` filter its baseline is `lang=jpn`: those titles' pages, including the Japanese pages LoC shipped without text, which the release adds to the baselines once we've OCR'd them (04 §4.8). Likewise an English query (#237): without a `lang` filter, when its words are English by `usnm_core::query_language`'s word lists, its baseline is the pages of titles that list English, while its hits stay unfiltered (06 §6.3.3). On a version with page languages (05 §5.5.8), as the published `pages-v20261026-1` is, these baselines count each page by its own languages instead of its title's (§11.14).

The published version at the time of writing, `pages-v20260929-4`:

|              |                                            |
| ------------ | ------------------------------------------ |
| Pages        | 6,557,925                                  |
| Places       | 398, in 42 states and territories          |
| Titles       | 1,217 (195 places have exactly one)        |
| Batches      | 882                                        |
| No pages yet | HI, ME, MT, NV, NM, ND, RI, SD, VT, PR, VI |

At 13:30 UTC on 30 September 2026, `/v1/status` reported the backfill had curated 66.0% of LoC's 2,997 batches: 15,788,248 pages, of which 15,766,980 have usable text. The 0.13% without text are in the denominator but can't match. That makes no measurable difference overall, though a place whose pages were mostly blank would read as under-represented.

The backfill has since reached LoC's full page count ([corpus coverage note](../notes/2026-10-05-corpus-coverage.md)). On 10 November 2026 the published `pages-v20261026-1` had 1,610 places in 53 states and territories, every one in the "No pages yet" row included. The measurements below are on the September version.

## 11.3 Candidate measures

| Measure                                                                           | Denominator                             | Available now                                             | Main problem                                                                                              |
| --------------------------------------------------------------------------------- | --------------------------------------- | --------------------------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| Raw hits                                                                          | none                                    | yes                                                       | Tracks corpus volume (§11.5.2)                                                                            |
| Hits per newspaper title                                                          | titles active in the place and window   | no (titles per place are known only for the whole corpus) | Titles differ by thousands of times in digitized pages (§11.5.3)                                          |
| Hits per page ("share of pages")                                                  | pages published in the place and window | yes                                                       | Tiny places take the extremes; mixes years with different national rates (§11.5.4, §11.5.5)               |
| Hits per capita                                                                   | decennial population, interpolated      | no                                                        | Measures digitization as much as interest; population data is hard to get for small places (§11.3.1)      |
| Significance only (z-score, log-likelihood G²)                                    | expected hits                           | yes                                                       | Ranks by sample size, so the biggest places come first again (§11.5.6)                                    |
| **Lift against other places, standardized by time, with shrinkage** (recommended) | expected hits from pages per bucket     | yes                                                       | Needs a dispersion correction for reprinted items (§11.5.7); language mix is still a confounder (§11.5.9) |

### 11.3.1 Per capita rates

Per capita answers a different question ("how much did residents read about this?"), and in this corpus it would mostly measure how much of each place LoC has digitized. The top 10 places hold 60.7% of all pages, and pages per title range from 4 at the 10th percentile of places to 8,728 at the 90th (§11.5.1). A city with one digitized title and a town with ten would get per-capita rates that reflect the digitization program, not the press.

Sourcing is also expensive. The Census Bureau's historical series of city populations covers only the 100 largest urban places in each census from 1790 to 1990 (Gibson 1998). The median place in the index has 1,764 pages and is usually a small town that isn't in that series. County totals exist for every decennial census through IPUMS NHGIS, but a newspaper's readership doesn't follow its town's or county's boundaries, county boundaries changed over the period, and places would still need matching to historical counties. We recommend deferring per capita until there is a county layer (§11.10, phase 3).

## 11.4 Recommendation

Score each place by how much more or less often its pages matched than the other places' pages in the same time buckets, pull scores with little evidence behind them toward the typical place, and show the result on a diverging scale around 1.

### 11.4.1 The score

For a search with national hits `H_b` and national pages `N_b` in bucket `b` (both in the aggregate response), and a place with `P_pb` pages (the coverage cube) and `O_pb` hits (the aggregate cube) in bucket `b`:

1. **Expected hits, against the others, standardized by time.** `E_p = sum over b of P_pb * (H_b - O_pb) / (N_b - P_pb)`. This is indirect standardization, as in a standardized mortality ratio: the place's own pages, each weighted by how often the other places' pages matched in that month or year. It accounts for the number of newspapers, how much of each was digitized, and when. Leaving the place out of its own reference changes the result most for the biggest places: Washington has 30% of all pages. Buckets where the place published every page have no reference and are left out of both `O_p` and `E_p`. In a bucket where the others have no hits but the place has some, the others are credited with half a hit (a continuity correction, as in a Jeffreys prior); a reference rate of 0 would give the place's hits no expected count at all. In that case the direction is clear but the size of the lift is set by the correction, and the colour scale stops at 8 times (§11.6).
2. **Lift.** `O_p / E_p`. 1 means "as often as the others", 2 means twice as often.
3. **Dispersion.** Hits are pages, and a newspaper can print the same item in issue after issue, so counts vary more than independent pages would. `phi` is measured per search. Each place's cells are merged into calendar years (calendar months for windows under three years), then consecutive cells into chunks that each expect at least 5 hits at the place's own lift. The place's dispersion is the Pearson chi-square of its chunks, with binomial variance, per degree of freedom. `phi` is the median over places with at least 3 chunks, and at least 1. The fixed calendar resolution makes `phi` the same whichever of day, month or year buckets the search uses (§11.5.7). Week buckets are the exception: a week that crosses a month or year boundary is counted in the period of its first day, because the cells only carry weekly totals, so `phi` for a weekly search can differ slightly from the same search by day. A place whose interest rises and falls over the window also adds to `phi`, so it errs toward wider intervals. When no place expects enough hits (small searches), `phi` is 4, about the middle of the measured values (2.3 to 11.6, §11.5.7). Dividing `O_p` and `E_p` by `phi` treats a place as having `1/phi` as much independent evidence.
4. **Shrinkage (empirical Bayes).** The lift gets a gamma prior with mean `mu` and shape `alpha`, both fitted per search by maximizing the negative binomial marginal likelihood of all places' counts. `mu` is fitted because the typical place need not match the page-weighted reference: for yellow fever it is 0.85, for Klondike 1.40. The posterior for each place is `Gamma(a + O_p/phi, rate a/mu + E_p/phi)`, where `a` is `alpha` widened by the uncertainty in the fitted `mu`: with `v` the sampling variance of `ln mu` (the inverse of its Fisher information, a sum over places of `alpha m/(alpha + m)` with `m` the place's mean count over `phi`), `1/a = 1/alpha + v (1 + 1/alpha)`. Without that widening, a search with few hits where the places don't differ fits `alpha` at its bound and puts every place at the fitted `mu` with a very narrow interval, so a `mu` of 0.87 from 17 hits flagged every place as clearly below the others (the unit test `few_hits_and_no_difference_flags_nothing`). For the seven searches `a` is 0.7% to 1.8% below `alpha`, except cross of gold (8.66 against 10.13). This is the Poisson-gamma model used in disease mapping (Clayton and Kaldor 1987); DuMouchel (1999) uses a mixture of two gammas for the same purpose, which is a possible refinement (§11.10).
5. **What is shown.** The posterior median as the place's estimate (the colour scale is logarithmic, and the median of a skewed posterior sits better on it than the mean); the 90% central credible interval for "clearly above 1", "clearly below 1" or "can't tell"; and lists of places sorted by the interval's lower bound (most clearly over-represented) and upper bound (most clearly under-represented). Ranking by a lower credible bound follows DuMouchel's EB05. The posterior probability that the lift is above 1 is available for tooltips.

A village with one page and one hit has a raw lift near 20, but its estimate stays near the prior and its interval includes 1 (§11.5.6), so it is drawn as "can't tell". How far a place moves depends on `alpha`: when places differ a lot (boll weevil, `alpha` 0.52) the prior is wide and a single hit moves the estimate further. That is why the interval, not a page-count cut-off, decides what is drawn prominently.

**States** use the same steps with the state as the unit: each state's cells are summed per bucket and compared with the other states' pages in that bucket, so a term common across a whole state isn't part of its own reference. They use the larger of the place-level `phi` and their own, measured the same way on the state cells, and their own `alpha` and `mu` fitted across states. A state's hits vary more than a place's: on the seven searches the states' own `phi` was 2 to 4 times the places' (7.6 against 3.08 for yellow fever, 41.4 against 11.62 for baking powder), and the places' value made state intervals too narrow. A state's score is dominated by its biggest cities, and the District of Columbia, with 30% of pages, is compared with everything else.

### 11.4.2 Comparison set

The comparison is with the other **published** places in the same buckets, within the search's `state` filter. With `state=GA,SC`, a place is compared with the other Georgia and South Carolina places, and the legend states how many places and states that is. The view is off when fewer than 10 places have pages in the window: with fewer, the typical place and the spread between places come from a handful of points, and the rates mostly compare a few newspapers ("guerra civil" in Spanish papers, 1935–1940, has 8 places with pages and one paper with 71% of the matches). The end-to-end tests build the site with `VITE_SKEW_MIN_PLACES=5`, since their fixture corpus has 6 places. With `lccn` or `front` filters the baselines are not exact (the API returns `baseline: null`), so the score is unavailable. With a `lang` filter the baselines count only the pages of titles that list one of the languages, so the score compares those pages with each other place's pages in the same languages (§11.14). A version published before per-language counts has none, and the score is unavailable there too.

### 11.4.3 Implementation

**In the browser.** It already has every input: the aggregate's cube and national series, and the coverage cube. The fit is a two-parameter search over at most a few thousand places, and the rest is arithmetic per place. Computing it there needs no new endpoint, costs the search backend nothing, and leaves the aggregate's cache format and its role in the search log (ADR-0012) untouched.

`usnm_core::skew` is the reference implementation: `place_counts`, `dispersion_for`, `Prior::fit`, `Prior::score`, `score_all`, and `score_search` and `score_groups` for a whole search and its states, with the tests in §11.5.8. The web version is a port of it (`web/src/engine/skew.ts`, about 450 lines of TypeScript including the gamma functions). `fixtures/skew-vectors.json` holds inputs and the scores the Rust gives them, with the tolerances both sides allow. `cargo test` (`crates/usnm-core/tests/skew_vectors.rs`, which rewrites the file when `UPDATE_SKEW_VECTORS` is set) and the web unit tests (`web/src/engine/skew.test.ts`) both check it, so the two can't drift (§11.14).

A public `/v1/skew` endpoint for researchers is not built (phase 2). It would reuse the aggregate's cache key, with these details to handle: the cache holds serialized bytes, so the handler must parse the aggregate body; on a cache miss it would run a search that the search log doesn't count, so ADR-0012 would have to name it; a coarsened aggregate changes the bucket; the model needs a version in the cache key so a change to the scoring isn't hidden by cached responses; and scoring should run off the async workers.

### 11.4.4 Fallbacks

- If `alpha` reaches its upper bound (no detectable difference between places), every estimate is close to the prior mean and the map says "can't tell" everywhere. That is the correct answer when places don't differ (the null simulation in §11.5.8). That answer depends on the scoring shape allowing for the uncertainty in the fitted mean (§11.4.1 step 4). Otherwise a mean fitted from few hits that lands away from 1 flags every place.
- If the coverage cube can't be fetched, the site keeps the raw view, as the relative view does today.
- The older "share of pages" view, which coloured by the maximum (§11.5.4), was removed in October 2026 (§11.14).

## 11.5 Evidence

### 11.5.1 Method

The evidence comes from read-only requests to the public API, pinned to `v=pages-v20260929-4`, one at a time with pauses. The script below makes 16 requests; the runs behind this document also made two retries, a few `/v1/hits` requests (two of them saved for §11.5.9) and `/v1/meta` and `/v1/status` requests (the status response saved for §11.2). Each aggregate response records the backend time it took when computed: 16 to 22 seconds for the three example searches, which were served from the cache, and 3.3 to 6.9 seconds for the other four. Two of those four timed out (503) on the first try and succeeded on one retry.

```sh
V=pages-v20260929-4; API=https://usnewsmap.com/v1; D=/tmp/termskew; mkdir -p $D
curl -s "$API/places?v=$V" -o $D/places.json
curl -s "$API/coverage?bucket=year&from=1751-05-09&to=1963-12-31&v=$V" -o $D/cov_year.json
while read -r name qs; do
  curl -s "$API/aggregate?$qs&v=$V" -o $D/agg_$name.json
  curl -s "https://usnewsmap.com$(jq -r .cube.baseline_ref $D/agg_$name.json)" -o $D/cov_$name.json
  sleep 10
done <<'EOF'
yellow_fever q=%22yellow+fever%22&bucket=month
free_silver q=%22free+silver%22&bucket=month
cross_of_gold q=%22cross+of+gold%22&from=1896-06-01&to=1896-12-31&bucket=week
boll_weevil q=%22boll+weevil%22&from=1895-01-01&to=1930-12-31&bucket=year
klondike q=klondike&from=1896-01-01&to=1901-12-31&bucket=month
mormon q=mormon&from=1850-01-01&to=1930-12-31&bucket=year
baking_powder q=%22baking+powder%22&from=1880-01-01&to=1920-12-31&bucket=year
EOF
cargo run -q --release -p usnm-core --example term_skew -- --dir $D \
  --names yellow_fever,free_silver,cross_of_gold,boll_weevil,klondike,mormon,baking_powder
```

A request pinned to an older version is redirected once a newer one is published, so rerunning this later measures the newer version. The full output is in §11.13, which points to [the note that holds it](../notes/2026-10-01-term-skew-full-output.md). The corpus figures:

```
places 398  pages 6557925  median pages/place 1764  p90 18728  max 1990131  top 10 places hold 60.7% of pages
pages per title, by place: p10 4  median 805  p90 8728  min 1  max 67594  (p90/p10 = 2182x)
spearman(pages, titles) across places = 0.437  places with one title 195
states and territories with pages 42
years with pages 198  of which with pages from fewer than 5 states 60  first year with 5+ states 1776
```

The harness also checks that the inputs agree: in every bucket, the national hits equal the sum of the places' hits, the national pages equal the sum of the coverage cells, and no hits fall in a cell without pages.

### 11.5.2 Raw counts and corpus volume

Spearman correlation between a place's hits and its pages in the window, over every place with pages, and how many of the 10 places with the most hits are also among the 10 with the most pages:

| Search            | Window                         | Places with pages | Spearman(hits, pages) | Top 10 by hits that are top 10 by pages |
| ----------------- | ------------------------------ | ----------------- | --------------------- | --------------------------------------- |
| `"yellow fever"`  | whole corpus, by month         | 398               | 0.825                 | 7                                       |
| `"free silver"`   | whole corpus, by month         | 398               | 0.736                 | 7                                       |
| `"cross of gold"` | June to December 1896, by week | 58                | 0.597                 | 9                                       |
| `"boll weevil"`   | 1895 to 1930, by year          | 262               | 0.759                 | 4                                       |
| `klondike`        | 1896 to 1901, by month         | 163               | 0.883                 | 8                                       |
| `mormon`          | 1850 to 1930, by year          | 336               | 0.862                 | 6                                       |
| `"baking powder"` | 1880 to 1920, by year          | 282               | 0.916                 | 8                                       |

For most searches the raw map is largely a map of where the pages are. New York and Washington are in the top 5 by hits for all seven searches.

### 11.5.3 Titles as a denominator

Across places, pages per title range from 4 (10th percentile) to 8,728 (90th percentile), and the rank correlation between a place's pages and its titles is only 0.437. How much of a title LoC has digitized varies far more than how many titles a place has. Rank correlation of hits with pages and with titles over all places with pages, and the share of the variance in log hits explained by each over places with hits:

| Search        | Spearman with pages | Spearman with titles | R² with pages | R² with titles |
| ------------- | ------------------- | -------------------- | ------------- | -------------- |
| yellow fever  | 0.825               | 0.381                | 0.561         | 0.217          |
| free silver   | 0.736               | 0.341                | 0.409         | 0.126          |
| cross of gold | 0.597               | 0.555                | 0.660         | 0.394          |
| boll weevil   | 0.759               | 0.165                | 0.326         | 0.059          |
| klondike      | 0.883               | 0.156                | 0.779         | 0.030          |
| mormon        | 0.862               | 0.359                | 0.640         | 0.146          |
| baking powder | 0.916               | 0.302                | 0.767         | 0.085          |

Two caveats weaken the titles column: titles are counted over the whole corpus, not the window, and 195 of the 398 places have exactly one title, so the title count varies little. The two whole-corpus searches (yellow fever, free silver) avoid the first caveat and show the same gap. Pages track hits much more closely, and they are the unit hits are counted in.

### 11.5.4 Share of pages and small places

The site's relative view colours by `rel / max(rel)`. The place with the largest share, and how many places with hits land in the top half of the colour scale:

| Search        | Largest share | Place                | Its pages | Its hits | Places in top half |
| ------------- | ------------- | -------------------- | --------- | -------- | ------------------ |
| yellow fever  | 1.000         | Little Rock Ark., AR | 1         | 1        | 2 of 265           |
| free silver   | 0.500         | Laurel, DE           | 8         | 4        | 6 of 204           |
| cross of gold | 0.250         | Laurel, DE           | 4         | 1        | 1 of 32            |
| boll weevil   | 0.125         | Columbia, SC         | 8         | 1        | 1 of 164           |
| klondike      | 0.750         | Batesville, AR       | 4         | 3        | 6 of 120           |
| mormon        | 0.545         | Mariposa, CA         | 209       | 114      | 2 of 261           |
| baking powder | 0.263         | Bessemer, CO         | 392       | 103      | 5 of 209           |

Of the 10 places with the largest share of pages, 4 to 10 (depending on the search) have fewer pages than the median place in the window.

### 11.5.5 Time standardization and the leave-one-out reference

The alternative to §11.4.1 step 1 is `E = total hits * place pages / total pages`, which ignores when the place's pages were printed. With `phi` unchanged and the prior refitted, the number of places whose flag (above, below, can't tell) changes, and the largest gap between the two expectations:

| Search        | Flag changes | Largest gap                                       |
| ------------- | ------------ | ------------------------------------------------- |
| yellow fever  | 162 of 398   | Miami, FL: 18.0 expected with time, 280.8 without |
| free silver   | 165 of 398   | Chicago, IL: 9.8 with, 280.1 without              |
| cross of gold | 1 of 58      | (one half-year window)                            |
| boll weevil   | 22 of 262    | Deland, FL: 7.0 with, 28.2 without                |
| klondike      | 17 of 163    | Milford, DE: 5.1 with, 18.6 without               |
| mormon        | 76 of 336    | Mariposa, CA: 19.5 with, 2.7 without              |
| baking powder | 44 of 282    | Hardy, AR: 17.3 with, 4.8 without                 |

Over long windows a term's national rate moves a lot, and places are digitized for different decades. Chicago's pages mostly fall outside the years when "free silver" was frequent, so a plain share of pages would expect 29 times as many hits there (280.1 against 9.8) and understate Chicago's lift by the same factor.

Leaving each place out of its own reference changes fewer flags (0 or 1 per search) but moves the biggest places: Washington's raw lift for boll weevil is 0.360 against a national rate that includes Washington and 0.284 against the others; for yellow fever, 1.093 and 1.116. The unit tests `compares_each_place_with_the_others_in_the_same_buckets` and `a_big_place_is_not_compared_with_itself` check both effects on small examples.

The bucket unit still matters a little, because it sets how finely time is standardized: scoring the three monthly searches by year instead changes the flags of 5 places (yellow fever), 3 (free silver) and 2 (Klondike), with `phi` identical.

### 11.5.6 Shrinkage and ranking

Yellow fever, whole corpus, by month (`phi` 3.08 from 144 places, `alpha` 2.321, `mu` 0.846, scored with shape 2.305):

Top 5 by raw hits:

| Place               | Pages     | Hits   | Expected | Raw lift | Estimate | 90% interval |
| ------------------- | --------- | ------ | -------- | -------- | -------- | ------------ |
| New-York, NY        | 699,264   | 15,124 | 13,236.0 | 1.14     | 1.14     | 1.12 to 1.17 |
| Washington, DC      | 1,990,131 | 12,658 | 11,337.8 | 1.12     | 1.12     | 1.09 to 1.15 |
| Wilmington, DE      | 192,859   | 3,413  | 3,373.4  | 1.01     | 1.01     | 0.96 to 1.06 |
| Birmingham, AL      | 164,346   | 3,316  | 1,207.4  | 2.75     | 2.73     | 2.60 to 2.87 |
| Washington City, DC | 118,302   | 3,114  | 3,751.7  | 0.83     | 0.83     | 0.79 to 0.87 |

Top 5 by raw lift:

| Place                | Pages | Hits | Expected | Raw lift | Estimate | 90% interval |
| -------------------- | ----- | ---- | -------- | -------- | -------- | ------------ |
| Little Rock Ark., AR | 1     | 1    | 0.0      | 21.68    | 0.84     | 0.23 to 2.09 |
| Langston City, OK    | 4     | 1    | 0.1      | 19.97    | 0.84     | 0.23 to 2.09 |
| Littleton, NC        | 4     | 1    | 0.1      | 15.81    | 0.84     | 0.23 to 2.09 |
| Hydaburg, AK         | 56    | 1    | 0.1      | 9.10     | 0.83     | 0.23 to 2.08 |
| Stamford, CT         | 8     | 1    | 0.1      | 7.25     | 0.83     | 0.23 to 2.07 |

Top 8 by the lower bound of the interval:

| Place          | Pages   | Hits  | Expected | Raw lift | Estimate | 90% interval |
| -------------- | ------- | ----- | -------- | -------- | -------- | ------------ |
| Mobile, AL     | 1,351   | 336   | 47.3     | 7.10     | 6.14     | 5.23 to 7.15 |
| Key West, FL   | 51,153  | 204   | 38.9     | 5.25     | 4.45     | 3.62 to 5.39 |
| Montgomery, AL | 135,189 | 2,058 | 646.8    | 3.18     | 3.15     | 2.95 to 3.35 |
| Birmingham, AL | 164,346 | 3,316 | 1,207.4  | 2.75     | 2.73     | 2.60 to 2.87 |
| Huntsville, AL | 2,796   | 201   | 73.7     | 2.73     | 2.52     | 2.05 to 3.06 |
| Monticello, AR | 4,283   | 132   | 48.3     | 2.73     | 2.43     | 1.89 to 3.08 |
| Marysville, CA | 3,999   | 298   | 155.4    | 1.92     | 1.86     | 1.57 to 2.18 |
| Batesville, AR | 18,005  | 265   | 136.6    | 1.94     | 1.87     | 1.56 to 2.22 |

The one-hit places fall back to the prior (about 0.84) with intervals that include 1, so the map draws them as "can't tell". The places the lower bound ranks first have tens to thousands of hits each. Places with many pages still appear when their lift is large (Montgomery, Birmingham), because the ranking is by how clearly a place differs. Without the zero-reference correction (§11.4.1 step 1), Philadelphia ranks third, with 41 hits against 2.0 expected, because some of its hits fall in months when no other place's pages matched and get no expected count.

Klondike, 1896 to 1901, by month (`phi` 3.94, `alpha` 1.229, `mu` 1.401, scored with shape 1.207), top 8 by lower bound:

| Place              | Pages | Hits  | Expected | Estimate | 90% interval   |
| ------------------ | ----- | ----- | -------- | -------- | -------------- |
| Skagway, AK        | 1,900 | 1,109 | 46.0     | 22.52    | 20.39 to 24.80 |
| Douglas City, AK   | 668   | 249   | 22.4     | 9.79     | 7.92 to 11.95  |
| Fort Wrangel, AK   | 476   | 209   | 31.0     | 6.17     | 4.89 to 7.66   |
| Skaguay Alaska, AK | 54    | 36    | 4.8      | 4.82     | 2.73 to 7.76   |
| Arizona City, AZ   | 1,188 | 141   | 48.7     | 2.78     | 2.09 to 3.60   |
| Mineral Park, AZ   | 1,561 | 168   | 70.1     | 2.33     | 1.80 to 2.96   |
| Salisbury, CT      | 1,297 | 147   | 64.3     | 2.22     | 1.68 to 2.87   |
| Elbert, CO         | 463   | 66    | 26.3     | 2.34     | 1.54 to 3.38   |

By raw hits the Klondike top 4 are San Francisco, Washington, New York and Los Angeles; Skagway is fifth. For boll weevil (1895 to 1930) the top 8 by lower bound are Montgomery and Birmingham, Alabama, and six Arkansas places (Magnolia, AR: 144 hits where 12.9 were expected), while Washington (1,867 hits, lift 0.28) and New York (527 hits, lift 0.28) are third and fifth by raw hits.

How many of each ranking's top 10 are among the 10 places with the most pages:

| Search        | Raw hits | Share of pages | Estimate | Lower bound |
| ------------- | -------- | -------------- | -------- | ----------- |
| yellow fever  | 7        | 0              | 2        | 2           |
| free silver   | 7        | 0              | 1        | 2           |
| cross of gold | 9        | 0              | 5        | 5           |
| boll weevil   | 4        | 1              | 2        | 2           |
| klondike      | 8        | 0              | 0        | 1           |
| mormon        | 6        | 0              | 0        | 0           |
| baking powder | 8        | 0              | 0        | 0           |

Share of pages removes the big places from the top, but only by putting the one-page places there. The lower bound removes both. For "cross of gold" (263 hits in half a year) only one place's interval excludes 1 (Washington, DC: 30 hits where 49.4 were expected, 0.49 to 0.99). Only the larger places have enough hits to say anything, and not enough to say it clearly.

Ranking by significance alone (a z-score) brings the big places back: in the unit test `ranks_by_size_of_skew_not_by_size_of_place`, 10 big places at 1.15 times the reference rate fill the z-score top 10, while the lower bound's top 10 are the 10 mid-size places at 3 times.

### 11.5.7 Reprinted items (over-dispersion)

`phi` per search, and the places flagged above and below 1 with and without it:

| Search        | phi (places measured) | Above / below with phi = 1 | With the measured phi |
| ------------- | --------------------- | -------------------------- | --------------------- |
| yellow fever  | 3.08 (144)            | 42 / 138                   | 23 / 91               |
| free silver   | 4.65 (94)             | 48 / 70                    | 27 / 32               |
| cross of gold | 2.33 (5)              | 4 / 6                      | 0 / 1                 |
| boll weevil   | 4.85 (76)             | 41 / 130                   | 34 / 104              |
| klondike      | 3.94 (67)             | 38 / 44                    | 28 / 28               |
| mormon        | 3.29 (153)            | 67 / 125                   | 53 / 86               |
| baking powder | 11.62 (155)           | 102 / 88                   | 63 / 33               |

Every search is over-dispersed. We expect repeated advertisements to be part of it for "baking powder", the highest, but these counts can't separate reprinting from a place's own changes in interest over the window. Either way, treating pages as independent would overstate the evidence. Measured on the search's own buckets, `phi` depends on the bucket unit (for yellow fever, 1.30 by month and 2.29 by year). Measuring on calendar years removes that: the same three searches by year give identical `phi`, and the unit test `dispersion_for_ignores_the_bucket_unit` checks it.

In the simulations (§11.5.8), `phi` comes out at 3.78 with 240 fine buckets and 4.04 with 20 coarse ones for items that run 4 pages at a time with interest drifting 30% over the window. When no place differs from the others but items run 6 pages at a time, the plain Poisson model flags 124 of 300 places and the corrected model flags none (`phi` measured 5.52).

### 11.5.8 Simulations (unit tests)

`cargo test -p usnm-core skew` runs these, with a fixed-seed generator. The first three test the model on data it assumes; the next three on a prior of the wrong shape, only 42 units, and reprinted items.

| Test                                                                        | Setup                                                                                          | Result                                                                                                                                                      |
| --------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `no_difference_between_places_means_no_skew`                                | 400 places, heavy-tailed exposure (median 20 expected hits), every lift 1                      | `alpha` about 31,000, `mu` 0.998; 0 places flagged; estimates between 0.996 and 1.003, while the highest raw lift is 4.4 at a place with 0.46 expected hits |
| `recovers_the_spread_between_places_and_calibrates_intervals`               | 20 runs of 400 places, lifts from `Gamma(4, 4)`                                                | Fitted `alpha` 3.4 to 5.3 (median 3.9, true 4); 90% intervals contain the true lift for 90.4% of 8,000 places                                               |
| `one_page_one_hit_does_not_top_the_map`                                     | a village (1 hit, 0.01 expected) and a city (2,000 hits, 1,000 expected)                       | Village: raw lift 100, estimate 1.39, interval 0.72 to 2.38; city: estimate 2.00, interval 1.92 to 2.07; only the city is flagged                           |
| `hits_only_here_still_have_an_expected_count`                               | the term appears only in one place (5 hits on 100 pages; 10,000 other pages with none)         | Both places get a finite expected count; the first is flagged above 1 and the other below                                                                   |
| `stays_calibrated_when_the_prior_is_wrong`                                  | lifts not gamma: 85% log-normal with mean 1.3, 15% at 0.05 (like a paper in another language)  | Coverage 88.5%; fitted `mu` 1.02 to 1.17 (the true mean lift is 1.11)                                                                                       |
| `intervals_with_few_units`                                                  | 42 units, as in the states layer, 200 runs                                                     | Coverage 89.8%                                                                                                                                              |
| `reprinting_with_real_differences_stays_calibrated_once_dispersion_is_used` | `Gamma(4, 4)` lifts, items in runs of 6, 10 runs of 300 places                                 | Coverage 53.9% with `phi = 1`, 89.6% with `phi = 6`                                                                                                         |
| `dispersion_measures_reprinting_whatever_the_bucket`                        | 300 places; runs of 4 and 30% drift in interest, 240 or 20 buckets                             | `phi` 1.00 with independent pages; 3.78 fine, 4.04 coarse                                                                                                   |
| `dispersion_for_ignores_the_bucket_unit`                                    | the same data by month and by year                                                             | identical `phi`                                                                                                                                             |
| `reprinting_alone_is_not_skew_once_dispersion_is_used`                      | no differences, runs of 6                                                                      | `phi` 5.52; 124 flagged with `phi = 1`, 0 with it                                                                                                           |
| `few_hits_and_no_difference_flags_nothing`                                  | 12 places at the reference rate with 17 hits; `mu` fitted 0.87 by chance, `alpha` at its bound | none flagged (all 12 without the mean's uncertainty, §11.4.1 step 4)                                                                                        |
| `ranks_by_size_of_skew_not_by_size_of_place`                                | 10 big places at 1.15x, 10 mid-size at 3x, 280 at 1x                                           | z-score top 10: all 10 big places; lower-bound top 10: all 10 at 3x                                                                                         |

The special functions are checked against known values: `ln_gamma`; the regularized incomplete gamma function at chi-square 95th percentiles for 1 to 1,000 degrees of freedom; its inverse by round trip for shapes from 0.01 to 9,999; the Wilson and Hilferty approximation used above shape 10,000 against the asymptotic median of large gammas; and the normal quantile.

### 11.5.9 Other findings

- **Language.** San Diego, CA has 0 hits for "yellow fever" where 130.6 were expected, and it is among the five most under-represented places for five of the seven searches. Its one title is the _Süd California Deutsche Zeitung_, and Pittsburg, PA, among the five most under-represented for three searches, has the _Amerikanski Srbobran_ (`/v1/hits`). By their names these are German- and Serbian-language papers (the snapshot's title records list languages; we did not check them). English search terms rarely match such pages, so these places read as under-represented for any English term. Phase 1 names the languages of a place's papers wherever any of them is not English (tooltip, lists, table and CSV) and scores every place the same way; phase 2 adds per-language baselines. Many titles list English as well as another language (the maintainer reports that LoC lists the _Amerikanski Srbobran_ as Serbian and English), so a rule that only marked places whose titles are all non-English would miss the cases it was meant for.
- **Duplicate places.** In the measured version "Washington, DC" and "Washington City, DC", "Skagway, AK" and "Skaguay Alaska, AK", "Little Rock, AR" and "Little Rock Ark., AR" were separate places, each scored on its own pages, so the scores were correct but split. The catalog now merges them (PR #190; `catalog/overrides/place-aliases.json` holds the Skaguay alias), and `/v1/places` for `pages-v20261026-1` lists one Washington, one Skagway and one Little Rock.
- **Thin early years.** 60 of the 198 years with pages have pages from fewer than 5 states. For searches in those years "the others" are a handful of places.

## 11.6 Web

- A toggle, **"Pages"** and **"Relative rate"** (`norm=skew`), replacing the colour menu. A third button, "Median date" (`norm=when`, #127), was added later (`web/src/components/MeasureToggle.tsx`). The older "Share of pages published" (`norm=rel`) was removed; its permalinks open on Pages (§11.14).
- **Colour:** the estimate on a diverging, colour-blind-safe scale in log2, centred on 1, clamped at 1/8 and 8, with legend ticks at 1/8, 1/4, 1/2, 1, 2, 4 and 8 times. Places whose interval includes 1 are drawn at reduced opacity. County- and state-precision places, drawn as hollow rings today, get the colour on the ring. The heat layer is not offered in this view.
- **Which places, and how big:** every place with pages in the window, including those with no hits (the under-represented list is mostly places with 0 hits). Circle area follows expected hits, the amount of evidence behind each colour, with a visible minimum size so a place with pages but almost no expected hits (a few pages, or pages only from months when the term was absent elsewhere) is still drawn, as "can't tell" unless its interval excludes 1. Circles change size when switching into this view; the alternative, area by hits, would hide the places with 0 hits that fill the under-represented list.
- **Tooltip and table:** "Mobile, AL: 336 pages matched where 47 were expected from its 1,351 pages. About 6.1× the rate of the other places (5.2 to 7.2×)." The place table gains Expected and Relative rate columns and the 90% range, and the panel gets two short lists, most clearly above and most clearly below, sorted by the interval bounds and limited to places whose interval excludes 1. The UI does not use the word "significant".
- **Legend:** the title, the colour bar and its ticks. The explanation, "Matches per page compared with the other 397 places with pages in this window, in 42 states, over the same months. 1× is the same rate.", and what faded circles and circle area mean, are behind the legend's info button, which opens on a tap and closes with Escape or a tap elsewhere. The Pages and Median date legends work the same way.
- **Playback:** at load, build an expected-hits cube (each coverage cell's pages times the other places' rate in its bucket) and prefix-sum it like the page sums, so each frame is O(places). `alpha`, `mu` and `phi` stay fixed at the full-window values so colours don't jump because of a refit; for short trailing windows the intervals are then only a guide, since the spread between places can differ from the full window's.
- **States layer:** the same score per state. The States map layer (#599) colours states by hits per 1,000 pages and is offered under Pages only, so this view shows the state scores as a table under the place table.
- **Export:** a separate per-place CSV for the window (`place_id,name,state,pages,hits,expected,estimate,lower,upper,languages`, the last empty or a label such as "Papers in Serbian and English"), because the existing export (06 §6.3.2) is one row per place and bucket. `hits` counts the pages that matched in buckets with other places to compare with, as the score does.

## 11.7 Performance and cost

- **Search backend:** no new queries. The inputs are the aggregate and coverage responses the site already requests.
- **CPU:** scoring the seven searches (the fit, dispersion and every place's interval) took 0.8 to 20 ms each in the Rust release build on an Apple silicon machine; the whole-corpus monthly searches are the slowest. A synthetic worst case, 3,000 places (about the full corpus, 06 §6.1) with no difference between them so every posterior shape is large, took 48 ms. In the browser port, run in Node 22 (which uses V8, Chrome's JavaScript engine) on the same machine, fitting a search takes 3 to 31 ms for the seven searches (yellow fever is the slowest) and runs in a web worker; scoring a playback frame takes 0.3 to 2.3 ms. Synthetic cases with 3,000 places: 197 ms to fit 2,552 months at about the corpus's density (511,000 cells), with 10.5 ms per frame; 103 ms for 240 months with 40% of cells filled (297,000 cells), with 8.9 ms per frame.
- **Memory:** running totals of observed hits, expected hits and pages, stored once per coverage cell rather than per place and bucket (each frame looks them up by binary search): about 28 bytes a cell, 1.7 MB for yellow fever and 14 MB for the 511,000-cell synthetic case. A dense places × buckets layout would have taken 24 MB and about 184 MB.
- **Azure cost:** none. No new resources, storage, endpoint or ingest steps.

## 11.8 The backfill

Scores are relative to what is published. A permalink or citation records the index version its numbers came from, but once a newer version is published a request for the old one is redirected to the new one (06 §6.5), so revisiting the link shows the new version's scores. They can differ: when a release adds places, as the end of the backfill added Maine, Montana, New Mexico and the other states that had no pages in September 2026, "the others" change, and so do the reference rates behind every expected count. The legend states what the comparison covered and which version it came from, and the citation planned in 07 §7.5 would include it too (not built, §11.14), so a reader can tell when numbers were measured on an earlier version. No recomputation job is needed: baselines are rebuilt with every release (04 §4.4).

## 11.9 Search log and privacy

Nothing changes. The score is computed in the browser from responses the site already fetches, so no new request carries the query and nothing new is logged (ADR-0012, 09 §9.4).

## 11.10 Limits and plan

**Limits that remain after phase 1:**

- **Language** (§11.5.9). Without a language filter, places with mostly non-English pages read as under-represented for English terms; the view only names the languages of their papers. Since #237 an English query is compared with English-language titles' pages instead (§11.14), so this limit is left to queries whose language the word lists can't tell. Under a language filter the baselines count only those languages' pages, so a German search compares German pages with German pages. On a version without page languages, a title in several languages is in every language it lists, so `lang=eng` compares a New York paper's English and German pages as one stream. A version with page languages (05 §5.5.8) counts each page by its own languages.
- **The reference rate is treated as exact.** In buckets where the other places have few pages or few hits, it isn't, and a place can get a large lift from a handful of hits. The zero-reference correction (§11.4.1) handles the extreme case; the legend's place count helps; a later version could add the reference's own uncertainty.
- **OCR quality** varies by title and era. A title with poor OCR matches fewer pages for every term and will read as under-represented. A words-per-page denominator (the curated corpus has `word_count`) would partly correct it, and needs a change to the snapshot.
- **Pages differ in size.** 04 §4.3.1 estimates that later eight-column broadsheet pages carry two to five times the text of an 1879 small-town page, so they are more likely to contain any term. The same fix as above would help; it isn't measured here.
- **Place is the title's place of publication.** A paper's readers and news sources lie beyond its town. The score measures where the pages were printed.
- **`phi` is one number per search** and errs wide; a place with one heavily repeated advertisement can still be flagged. Phase 3's per-title check addresses it.
- **Many places at once.** With a few hundred places and 90% intervals, the lists will include some places that don't really differ, more so when `alpha` is small. The lists sort by the interval bounds, so the clearest cases come first; the intervals are a guide to what to show prominently, not a hypothesis test.
- **Plug-in prior.** The intervals ignore uncertainty in `alpha`, `mu` and `phi`. With 42 units (states) the simulation still covered 89.8%.

**Phases:**

1. **Built (§11.14).** `usnm_core::skew`; the browser port with shared test vectors; the `norm=skew` view with its lists, legend and states layer; naming the languages of places whose papers aren't all in English; the per-place CSV. No ingest or infrastructure change; the API's one change is a `languages` list on each place in `/v1/places`.
2. **Language, places and API.** Built: baselines per place, language and day in the snapshot, so `lang` filters keep exact baselines and the view can compare German pages with German pages (§11.14); duplicate places merged through the catalog overrides (§11.5.9). Not built, and optional: words instead of pages as the denominator; a two-gamma prior (DuMouchel 1999) if the single gamma fits poorly; the public `/v1/skew` endpoint (§11.4.3).
3. **Optional research features (not built).** Per-title counts ("titles that printed it" out of titles publishing), a terms aggregation on `lccn` that costs one more engine call per search, as a check that reprints don't inflate. Per capita at county level from IPUMS NHGIS, if a county layer is added.

## 11.11 References

- Clayton, D. and Kaldor, J. (1987). Empirical Bayes estimates of age-standardized relative risks for use in disease mapping. _Biometrics_ 43(3): 671 to 681.
- DuMouchel, W. (1999). Bayesian data mining in large frequency tables, with an application to the FDA spontaneous reporting system. _The American Statistician_ 53(3): 177 to 190.
- Dunning, T. (1993). Accurate methods for the statistics of surprise and coincidence. _Computational Linguistics_ 19(1): 61 to 74. (The G² statistic in §11.3.)
- Gibson, C. (1998). _Population of the 100 Largest Cities and Other Urban Places in the United States: 1790 to 1990._ U.S. Census Bureau, Population Division Working Paper No. 27.
- IPUMS NHGIS, National Historical Geographic Information System, <https://www.nhgis.org/>.
- Wilson, E. B. and Hilferty, M. M. (1931). The distribution of chi-square. _Proceedings of the National Academy of Sciences_ 17(12): 684 to 688.
- Acklam, P. J. An algorithm for computing the inverse normal cumulative distribution function (the rational approximation in `normal_quantile`).

## 11.12 Review notes

The design was reviewed before phase 1 was built (PR #62): an internal review, an adversarial reviewer who re-implemented the model and reproduced the first version's flag counts, and three Copilot reviews. Building phase 1 (PR #65) raised more. Each point was resolved in the model or the text, and the sections above describe the result:

- `phi` measured on calendar years, not the search's buckets, so flags don't change with the bucket menu (§11.4.1 step 3, §11.5.7); its median averages the middle pair; a window of exactly three years is measured by year.
- Each place left out of its own reference rate, with a continuity correction where the others have no hits (§11.4.1 step 1, §11.5.5).
- The prior mean fitted, not fixed at 1, and the scoring shape widened by the uncertainty in that mean (§11.4.1 step 4).
- Colour by posterior median; prominence decided by the interval; every place with pages drawn, sized by expected hits, with a visible minimum (§11.6).
- States scored as units against the other states, with the larger of their own `phi` and the places' (§11.4.1, "States").
- Faster and more accurate `gamma_quantile`, and running totals per coverage cell instead of dense prefix sums (§11.7).
- Simulations beyond the model's own assumptions: a misspecified prior, 42 units, and reprinting combined with real differences (§11.5.8).
- Papers in other languages named, not handled differently (§11.5.9, §11.14).
- The `/v1/skew` endpoint moved to phase 2 with its cache and logging details listed (§11.4.3).

The full review tables are in this file's git history and the two PRs.

## 11.13 Appendix: full output

The full output of the seven searches is in the note [Term skew: full output for seven searches](../notes/2026-10-01-term-skew-full-output.md).

## 11.14 Phase 1 as built

- **Labels.** The toggle reads "Pages", "Relative rate" and "Median date". "Pages" is the word the site already uses for raw counts (summary, tooltip, table, legend). "Relative rate" is short enough for a segmented toggle on a phone, which "Compared with other places" is not. The legend line says what it is compared with, and an info button explains the shrinkage and the faded circles.
- **The older share of pages.** Removed in October 2026. Phase 1 kept it for `norm=rel` permalinks, with a third button shown only while it was selected; now those links open on Pages (`web/src/state/url.ts`). The place table keeps its "Share of pages published" column.
- **Languages.** `/v1/places` lists the languages of each place's titles. Every place is fitted, scored and drawn the same way. Where any of a place's titles isn't in English, its tooltip, list entry, table row and CSV row say so ("Papers in Serbian and English"), so a reader can see why it may read low for an English term. In the lists, an info button beside the label says how many of the place's titles are in each language and what share of its titles that is (from `language_titles` in `/v1/places`; a title in several languages counts in each). It explains that these are the languages the titles are catalogued in across the collection, not the language of the matched pages, and points to the language filter for a like-with-like comparison.
- **Per-language baselines** (issue 86). The release writes `language_baselines.json` next to `baselines.json`: the same pages per place and day, split into one set per distinct list of languages a title has (`["eng"]`, `["eng","ger"]`, …). A page is in one set, so for `lang=eng,ger` the API sums every set that has English or German and no page is counted twice. A title in English and German is in both `lang=eng` and `lang=ger` but once in their union. `/v1/aggregate` returns `series.baseline`, `total.baseline_pages` and `cube.baseline_ref` for that scope, and `/v1/coverage` takes `lang` (06 §6.3.3). The site then scores the relative rate the same way as without a filter, shows the place table's "Share of pages published" column and offers the CSV. It leaves out the "Papers in X and English" line, since the baseline already compares like with like. A language's places can be few: German reaches 2 of the 6 places in the fixtures, and below the minimum (10 places with pages on the site, 5 in the end-to-end build) the view says so. Titles that list no language have no set, as no filter matches them. A version with page languages (05 §5.5.8) also writes `page_language_baselines.json`, the same pages split by each page's own languages, and the API sums those sets instead, so a German search under `lang=ger` is compared with pages written in German.
- **The query's language** (#237). Four of the five places most clearly below 1× for "Korea", 1945 to 1956, had papers in other languages (Yiddish, French, Croatian, and one in Spanish, Italian and English): an English word can't match "Corée" or "Koreja". Without a `lang` filter, the API now compares a query whose words are English with the pages of titles that list English, using the per-language baselines above, and says so in the aggregate's `baseline` (`{"languages": ["eng"], "why": "query_language"}`). The rule (06 §6.3.3, `crates/usnm-core/src/query_language.rs`) is that each side of any `OR` has a word at least as frequent in English as in each of the corpus's other Latin-script languages (wordfreq, folded as the index folds), and its other words are English words no such language uses more than 10 times as often, or digits. A query it can't place keeps every page, as before. The hits are not filtered: `lang=eng` makes cold searches about 40% slower and prunes nothing, so an English search's matches still include the few in titles catalogued in other languages, mostly bilingual papers, and the relative rate's info button says so. On a version without page languages, mixed places are compared on their English titles' pages, so a bilingual paper's other-language matches count against its English pages only, and a place whose papers are all in other languages drops out of the view. The examples that set a language keep it. Since the rebuild that records page language (#283, 05 §5.5.8), the published version's language baselines count each page by its own languages, so the query-language baseline is the pages written in English, and a mixed place is compared on its English pages.
- **Where it runs.** The fit runs in a web worker (`web/src/engine/skew.worker.ts`), falling back to the main thread where workers aren't available; frames are scored on the main thread from prefix sums (§11.7). Each new search shows pages with a notice while its fit runs. A refit of the same search (same index version, query and buckets) keeps the previous colours until it is ready, and switching to Pages and back doesn't refit. When the coverage cube is missing or doesn't match the search's buckets, the map shows pages with a notice. A layer=heat permalink shows points in this view. With fewer than 10 places with pages between the search's dates (5 in the end-to-end build, §11.4.2) it says so and shows pages.
- **Shared vectors.** Ten generated cases (lone publishers, the zero-reference correction, reprints and drift by month, half a year by week, few hits by day with the fallback `phi`, no difference with `alpha` at its bound, both sides of the three-year boundary including from 29 February, places left out of the fit, states with their own `phi`) and two recorded searches (cross of gold and Klondike, reduced to counts), each with playback frames for places and states, plus the special functions. Tolerances (relative unless noted): counts 1e-12, `phi` 1e-10, `1/alpha`, `mu` and the shape 1e-5, estimates and bounds 1e-5, P(above 1) 1e-6 absolute, special functions 1e-12. The largest differences measured between the port and the Rust were about 1e-7 for place bounds, 1e-6 for one Klondike state's lower bound (33 states, a flatter fit) and 6e-8 for `1/alpha`; `phi` and the counts were identical.
- **Not built yet:** relative-rate colours on the States map layer (state scores are a table; the layer shows hits per 1,000 pages under Pages), and a citation in the share button (it copies the link; the legend names the index version).
