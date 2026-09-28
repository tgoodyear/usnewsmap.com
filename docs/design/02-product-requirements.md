# 02: Product Requirements

## 2.1 Vision

> Type a word or phrase and see where and when America's newspapers printed it: every page, on a map, over time. Then click through to the page.

The revived site should be **accurate enough for scholarship**, **simple enough for a classroom or a genealogist**, and **cheap and boring enough to keep running for a decade**.

## 2.2 Personas (derived from the press coverage)

| Persona | Evidence | Primary goals | Key needs |
|---------|----------|---------------|-----------|
| **Historian / DH researcher** | NEH prize; JAH review; Saunt's "Cross of Gold" study; "miscegenation"/"scalawag" examples | Measure how a term, story or idea spread; compare regions; support claims with numbers | Full, unsampled counts; normalization by corpus size; proximity and fuzzy search for OCR noise; export; citable permalinks; API |
| **Public-history reader / journalist** | Washington Post "viral memes" piece; Reddit r/history launch | Explore, be surprised, share | Instant results, attractive animation, one-click sharing, mobile, embed |
| **Librarian / state newspaper project** | Library of Virginia UncommonWealth | Show off and promote digitized holdings; teach patrons | Coverage view per state and title; embed in LibGuides; accurate links to LoC |
| **Genealogist** | RICIGS genealogy society article | Find which local papers mention a surname or place and when, then read the page | Filters by state, title and date; list view sorted by date; keyword-in-context snippets; direct page links |
| **Teacher / student** | Public-history coverage; the "epidemics / political discourse" examples | Classroom exercises ("track 'influenza' in 1918") | Preset example searches, guided tour, stable URLs, accessibility |

## 2.3 Key user journeys

1. **Spread of a story (headline journey).** Search `"cross of gold"` for 1896-06-01 to 1896-12-31 and choose **Week** playback. Watch Chicago light up, then the East Coast, then the West Coast. Switch to the **First appearance** layer to see a color gradient by date of first mention. Share the URL.
2. **Coinage and adoption of a word.** Compare `miscegenation` with `amalgamation` over 1860–1870, normalized. Look at the timeline, then the map by region.
3. **Epidemic path.** Search `yellow fever` OR `yellow jack` over 1878 by month. Use the trailing 3-month window. Export CSV.
4. **Genealogy lookup.** Search a surname such as `"McWhorter"` with state = GA, 1880–1910. Switch to list view, read the snippets, and open the LoC page with the term highlighted.
5. **Librarian embed.** Build a view and copy the `<iframe>` embed code (scroll-zoom disabled, compact controls).

## 2.4 Feature inventory

Priority: **P0** = must ship at relaunch (legacy parity plus accuracy); **P1** = shortly after relaunch; **P2** = later.

### 2.4.1 Legacy parity

| ID | Feature | Legacy | New behavior | Pri |
|----|---------|--------|--------------|-----|
| F-01 | Keyword/phrase search with date range | Phrase only, 1789–1925 | Covers the whole corpus (1756–1963 as available) | P0 |
| F-02 | Map of hits by place | Sample of 500, city markers, 5 color classes | **All matching pages**, aggregated per place; graduated circles with a legend; quantile or log scaling | P0 |
| F-03 | Place drill-down list | Date-sorted list with PDF and viewer links | Paginated, date-sorted, **with KWIC snippets**; title name; page number; links to the LoC viewer (terms highlighted) and the PDF | P0 |
| F-04 | Playback over time | Server round-trip per frame | **Runs entirely in the browser** from one aggregate response; smooth at 60 fps | P0 |
| F-05 | Cumulative vs trailing N-year window | Yes | Yes, with N in years, months or weeks | P0 |
| F-06 | Step interval Year/Month/Week/Day, ◀ ▶ step, spacebar | Yes | Yes, plus keyboard shortcuts and a speed control | P0 |
| F-07 | Timeline of relative frequency | Yearly, hard-coded denominators | Year/month buckets with **data-driven baselines**; toggle between raw and relative | P0 |
| F-08 | No-data mask | Static list of states | **Time-aware coverage layer**: which states and titles have pages in the current window | P0 |
| F-09 | Embed mode | `?disableScroll` | `?embed=1` with compact UI and a copyable iframe snippet | P1 |
| F-10 | About/credits | Info panel | About page crediting LoC/NEH, GTRI/eHistory legacy, and data licenses | P0 |

### 2.4.2 New capabilities

| ID | Feature | Rationale | Pri |
|----|---------|-----------|-----|
| F-20 | **Search modes**: exact phrase, all words, any word, proximity (`NEAR/n`), exclude terms | Legacy had these commented out; LoC offers the same operators; researchers need them | P0 |
| F-21 | **OCR-tolerant matching**: fuzzy term (edit distance 1–2) as an explicit option | 19th-century OCR is noisy; the legacy "Fuzzy" mode was disabled | P1 |
| F-22 | **Shareable permalinks**: all state in the URL (query, filters, time, layer, viewport) | Journalists, teachers and citations | P0 |
| F-23 | **Multi-term comparison** (up to 4 series), with a color per series on the timeline and map | "miscegenation vs amalgamation" journeys | P1 |
| F-24 | **First appearance layer**: color each place by the date of first match | Main feature for studying spread (the "Cross of Gold" study) | P1 |
| F-25 | **Filters**: state, title (LCCN), language, front pages only | Genealogists and librarians; matches LoC facets | P0 (state, title), P1 (others) |
| F-26 | **Map modes**: graduated points, heatmap, state choropleth (normalized per 1,000 pages) | Different questions need different views | P1 |
| F-27 | **Accessible list and table view** of the same aggregates | WCAG 2.2 AA; screen readers; mobile | P0 |
| F-28 | **Export**: CSV of aggregates (place × bucket), GeoJSON of places, CSV of hits (capped) | Research reproducibility | P1 |
| F-29 | **Public read-only API** with OpenAPI docs and fair-use limits | Researchers; replaces scraping | P1 |
| F-30 | **Example searches / guided tour** (Cross of Gold, scalawag, 1918 influenza) | Onboarding; classroom use | P1 |
| F-31 | **Title and place pages** ("Macon Telegraph, 1826–1909: coverage chart") | SEO, librarians | P2 |
| F-32 | **Historical boundaries** (state and territory borders by year) | Correct context for the 1800s | P2 |
| F-33 | **Article-level results** (using the American Stories segmentation) | Finer granularity and better snippets | P2 |
| F-34 | **Reprint / "viral text" clusters**: near-duplicate article detection | Directly supports the virality narrative | P2 (research) |
| F-35 | **Semantic search** ("stories about railroad strikes") via embeddings | Discovery beyond exact words | P2 (optional; cost-gated) |

### 2.4.3 Explicitly out of scope

- User accounts, saved searches or annotations at relaunch (permalinks cover sharing). Could come later with Entra External ID and Cosmos DB serverless.
- Hosting page images or PDFs. We link to LoC, which is canonical, free and IIIF-enabled.
- Re-OCR of pages. We use LoC's improved OCR (NDNP-Open-OCR) as it is released.

## 2.5 Non-functional requirements

| Category | Requirement |
|----------|-------------|
| **Accuracy** | Map and timeline counts are exact page counts for the query (no sampling). The UI states that counts are *pages containing the match*, not the number of occurrences. |
| **Latency** (p95, warm) | Aggregate query ≤ **2 s** for typical queries (≤ 1M hits); very common terms may take up to **15 s** uncached on the lean profile (≤ 4 s on the growth profile); cached responses ≤ **150 ms**. Place drill-down ≤ **1 s**. Playback frame ≤ **16 ms** client-side. |
| **Throughput** | Normal load 1–5 req/s; handle **press spikes of 100+ req/s** mostly from cache: popular and example searches are pre-warmed into the in-process and Blob caches; static assets are precompressed and cached by browsers for a year (hashed names). |
| **Availability** | 99.0% monthly for the read path on the lean profile (99.5% on the growth profile). Planned maintenance served by a static "read-only / degraded" page. |
| **Cost** | Steady state **under $80 per month** all-in (typical ~$72 including private networking and the registry); one-time backfill ~$15–20 on Container Apps Jobs; dev under $10; hard budget alerts and replica caps. |
| **Operability** | No VMs. All infrastructure as code. One-command deploy. Full re-index from the data lake with no manual steps. Runbooks in the repo. |
| **Portability / succession** | The whole system can be handed to a new owner (a university library, for example) by transferring the subscription and repo. |
| **Security & privacy** | Every app/job → storage/Cosmos path uses a **managed identity** and a **private endpoint** (keys disabled by policy; public access only during a guarded Spot-backfill window). No accounts, cookies or other PII collected by the application. The application never logs client IPs, and App Insights masks them. **Only exception:** if platform HTTP access logs are enabled for abuse investigation (off by default on the lean profile; Front Door logs in the growth profile), they contain raw IPs, are retained for **at most 30 days**, and have **query strings stripped at ingestion**, so search text is never stored per request. TLS everywhere. Least-privilege managed identities. Dependency and secret scanning. |
| **Accessibility** | WCAG 2.2 AA. Everything on the map is also available as a table. Keyboard-operable playback. Honors `prefers-reduced-motion`. |
| **Browser support** | Evergreen browsers; mobile Safari/Chrome; WebGL2 with a graceful non-WebGL fallback (table view). |
| **Data freshness** | New LoC batches and OCR reprocessing picked up within **7 days**, automatically. |
| **Attribution** | Every page result credits Chronicling America (LoC/NEH) and the contributing institution where known. |

## 2.6 Success metrics

- Median searches per session ≥ 3; share-link creation rate; embed count.
- Adoption by researchers: API keys issued, citations, course syllabi.
- p95 latency and error rate within SLO; monthly cost within budget.
- Zero unplanned outages longer than 24 h in the first year.
