# Web app

The US News Map single-page app ([design doc 07](../docs/design/07-frontend-design.md)). It uses React 19, TypeScript, Vite, MapLibre GL, deck.gl and TanStack Query. The API app serves it from its image, on the same origin as `/v1` ([ADR-0010](../docs/design/adr/0010-site-served-by-the-api.md)).

## Develop

Start the API on the synthetic fixtures, then the dev server, which proxies `/v1` to the API:

```sh
cargo run -p usnm-api            # from the repo root; serves :8080
cd web && npm ci && npm run dev  # http://localhost:5173
```

| Script                                | What                                                                                                                                                                                                                                                                                   |
| ------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `npm run lint` / `typecheck` / `test` | ESLint, `tsc` (TypeScript 7), Vitest unit tests (engine, URL state, dates, snippets)                                                                                                                                                                                                   |
| `npm run format` / `format:check`     | Prettier, from the repo root: this app and the repo's Markdown ([development guide](../docs/development.md#checks))                                                                                                                                                                    |
| `npm run build`                       | Production build into `dist/`. The API image also runs `node scripts/precompress.mjs dist` for brotli and gzip copies                                                                                                                                                                  |
| `npm run e2e`                         | Playwright end-to-end tests with axe accessibility checks. By default against `vite preview` and a running API. With `PW_BASE_URL` (as CI does), against an API serving the build (`USNM_SITE_DIR=web/dist cargo run -p usnm-api`). Set `PW_CHROMIUM_PATH` to use an existing Chromium |

TypeScript 7 ships no compiler API yet, so it is installed side by side with TypeScript 6, as [the TypeScript team recommends](https://devblogs.microsoft.com/typescript/announcing-typescript-7-0/): `@typescript/native` is TypeScript 7 and provides `tsc`, and `typescript` is an alias for `@typescript/typescript6`, which typescript-eslint imports. Drop the alias once typescript-eslint supports TypeScript 7's API (expected with 7.1).

Build-time settings:

| Variable             | Default                 | Meaning                                                                                                                                                              |
| -------------------- | ----------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `VITE_API_BASE`      | same origin             | API origin. Production leaves it empty: the API serves the site                                                                                                      |
| `VITE_BASEMAP_STYLE` | OpenFreeMap Positron    | MapLibre style URL. `none` gives a plain background (offline and tests). The self-hosted PMTiles style replaces the default once the tiles are published             |
| `VITE_PAGE_VIEWS`    | unset                   | `1` sends page views from any origin (to try `POST /v1/beacon` against a local API), `0` never. Unset, only a production build on `https://usnewsmap.com` sends them |
| `USNM_API_ORIGIN`    | `http://127.0.0.1:8080` | API the dev and preview servers proxy `/v1` to                                                                                                                       |

## How it works

- **The URL is the state** (`src/state/url.ts`). Searches use `/search/<encoded-query>` (for example, `/search/whigs` or `/search/%22cross%20of%20gold%22`). Optional dates, states, newspaper languages (`lang`, [07 §7.9](../docs/design/07-frontend-design.md#79-language-filter)), scrubber position, window, layer, measure, selected place and the order of its pages (`sort`), tab and viewport remain query parameters. Only non-default values are written, and every parameter is validated when the URL is parsed. The API permanently redirects old `/?q=...` and `/index.html?q=...` links, preserving filters. Dot-only queries stay in the legacy format because browsers normalize dot path segments.
- **One aggregate request per search.** `src/engine/cube.ts` builds per-place prefix sums from the sparse cube, so each playback frame, cumulative or trailing window, is a subtraction per place. The place table's share of pages published and the relative rate use the coverage cube named by `baseline_ref`, aligned to the same places.
- **Relative rate** (`norm=skew`, [doc 11](../docs/design/11-term-geographic-skew.md)). `src/engine/skew.ts` ports the Rust scoring (`usnm_core::skew`); `src/engine/skewModel.ts` fits a search once, in a web worker (`skew.worker.ts`), and scores each playback frame from prefix sums. `src/engine/skew.test.ts` checks the port against `fixtures/skew-vectors.json`, which `cargo test -p usnm-core --test skew_vectors` checks (and rewrites with `UPDATE_SKEW_VECTORS=1`). The share-of-pages measure (`norm=rel`) was removed; old links open on Pages.
- **Version pinning.** Every request carries the `v` from `/v1/meta`, so a whole session reads one published snapshot.
- **Large searches.** A search the API is still computing comes back as `202 Accepted`. The client asks again after `Retry-After` for up to 150 s and shows "Large search, still working…" meanwhile, or how many searches are ahead while the API has it queued; changing the search stops the asking, though the API still finishes the computation (`src/api/client.ts`). The end-to-end tests get a slow search from the fixture API's `USNM_FIXTURE_SLOW_TERM` switch, set in `scripts/ci/start-api-fixtures.sh`.
- **First and last mention.** The summary line names the search's earliest matching page and the date of its latest (`total.first` and `total.last` from `/v1/aggregate`). Each date opens that page's place with its pages oldest or newest first, so the page is at the top of the list, with its snippet and Library of Congress link: one OCR misread can make a misleading first mention, and this is how to check it. The place table has first appearance and last seen, and the relative-rate CSV has both (`first_seen`, `last_seen`).
- **Snippets** are split into text and `<mark>` segments and rendered as text nodes. No API HTML is ever injected.
- **Accessibility.** A Table tab mirrors the map. Playback works from the keyboard: `Space`, `←`/`→` (`Shift` for 10), `Home`/`End`. The current date is announced in a live region. Reduced motion is honored. Without WebGL2, the table is shown instead of the map.
- **Page views** (`src/pageview.ts`, 07 §7.8). One per page load, sent with `navigator.sendBeacon` to `/v1/beacon`: the page name and title, and on the first page the referrer's origin and the landing URL's `utm_*` tags. Never the search or other query parameters. Do Not Track and Global Privacy Control turn it off, and nothing is stored in the browser. The privacy page (`/privacy`) describes it for visitors.
- **Basemap startup.** The entry bundle warms the tiles, sprites and label glyphs of the initial viewport (the permalink's, or the default) before the lazy map stack loads (up to 40 requests; skipped with `VITE_BASEMAP_STYLE=none`). TileJSON resolves the provider's current tile URLs at runtime. Dots fade in after the basemap's first loaded idle, or after a two-second fallback if the provider is slow or blocked; reduced motion disables the fade. Requests use the browser HTTP cache, not persistent application storage.
- **Performance.** The map stack is lazy-loaded; the critical-path JS is about 99 KB gzip (budget 250 KB).

## States layer

Choose **States** under Map layer (or share `layer=states`). Modern borders
apply even to historical territories. The boundaries are the Census Bureau's
public-domain ones, prepared by a script; see
[the asset source and preparation](src/assets/README.md). Alaska, Hawaii and
the territories stay at their true positions.

`engine/states.ts` builds state prefix sums from both sparse cubes once per
search, using the same cumulative or trailing window as points. Denominators
include **all coverage places**, even those without matches, to avoid inflated
rates. Missing or unknown states appear as Unassigned. The API's 5,000-place cap
triggers an incomplete-numerator warning; the client cannot recover omitted
matches. Real-query cap frequency has not been surveyed. No API changes or
extra aggregate or coverage requests are needed.

First-pass decisions for #452:

- Page-weighted matches per 1,000 pages published, not raw counts or per-title
  rates. Both raw counts remain in the tooltip, sortable table and CSV.
- Minimum denominator: one page, with no suppression. The legend warns about
  noisy small counts. No pages means unknown: solid grey on the map, hatched
  grey in the legend. Pages without matches use light neutral.
- Five quantiles of positive state rates in the **current window**, with
  zero a separate neutral class. Ties stay together and can leave empty
  classes, which are labelled as such. Breaks change with scrubbing, and the
  legend changes with them. There is no toggle between raw counts and rates.
- Desktop clicks add a state filter; existing search filters remove it.
  Touch taps pin details with Filter and Dismiss buttons. The keyboard-accessible
  table offers the same filter action and a CSV download.
- States is unavailable for measures other than Pages, searches filtered by
  newspaper, Congress, and versions whose coverage baseline is unavailable. A
  shared link that asks for it in one of those cases draws points instead.
  The layer selector stays hidden in the Table tab and without WebGL; a
  States layer already selected keeps its accessible table there.
- Geometry is reused; only fill colours change. A MapLibre GeoJSON source
  promotes state codes to feature IDs and sets colours through feature state,
  below the first symbol layer. Direct States entry defers the deck overlay
  until another layer needs it. This replaces #452's proposed deck
  `GeoJsonLayer`, whose cold luma pipeline serialized costly link-status
  queries. Points, Heat and other measures retain deck. States does not prefetch
  geometry or change deck's basemap-readiness gate.
- If the initial basemap style fails, a plain local style enables the native
  fills, and an initial request that never settles is bounded by the existing
  two-second fallback. Errors in the tiles or fonts of a style already parsed
  neither replace it nor discard the state geometry. Unavailable coverage
  never draws no-pages fills.
- Escape dismisses hover or pinned details regardless of focus. Small positive
  rates keep significant digits; legend precision increases to distinguish
  unequal thresholds.
- Performance gates and hardware limits follow. Functional tests do not
  establish performance budgets.

### States performance measurements

Run against the synthetic API serving the production web build, not production:

```sh
# 5,000 places, 120 buckets, 600,000 cells in each cube; 2,000 timed frames.
USNM_STATES_PERF=1 npm test -- src/engine/states.performance.test.ts

# PW_BASE_URL points at the local synthetic API serving web/dist.
# USNM_STATES_GPU=metal explicitly selects native Apple GPU rendering on macOS.
USNM_STATES_PERF=1 USNM_STATES_GPU=metal PW_BASE_URL=http://127.0.0.1:18092 \
  npm run e2e -- e2e/states.performance.spec.ts --project=desktop --workers=1

# Diagnostic only: executable shader variants discourage driver cache reuse.
USNM_STATES_PERF=1 USNM_STATES_GPU=metal USNM_STATES_SHADER_COLD=1 \
  PW_BASE_URL=http://127.0.0.1:18092 npm run e2e -- \
  e2e/states.performance.spec.ts --project=desktop --workers=1

# Isolate the current built app's shaders from React, deck, map and geometry:
node scripts/probe-states-shaders.mjs --base-url http://127.0.0.1:18092 --metal
# Control: enqueue all programs before inspection, poll KHR completion.
node scripts/probe-states-shaders.mjs --base-url http://127.0.0.1:18092 --metal --parallel
```

After 200 warm-up frames, the scrub gate requires **every measured frame ≤2 ms**
and reports p95. It times totals, quantiles and all fill-colour lookups, excluding
preparation, React and deck.gl draw. On **Apple M1 (macOS arm64, Node 25.8.2)**,
the isolated 2026-10-15 run measured **16.679 ms** preparation and, over 2,000
frames, **0.014 ms** median, **0.023 ms** p95 and **0.100 ms** maximum.
A concurrent run had **0.080 ms** p95 but a **12.780 ms** maximum. The former
p95-only gate would miss that failure. Passing does not guarantee future frames.

The browser gate requires **≤300 ms** in five fresh processes **per entry path**:
selecting States on a ready basemap, or opening `layer=states` without basemap
prewarming. Chrome shader disk and program caches are disabled. Timing starts at
the asset's Resource Timing `responseEnd` and ends after native features render
plus two animation frames. `usnm-states-first-paint` is a **presentation proxy**,
not a GPU fence or compositor timestamp.

Separate CDP screencast frame-swap timestamps bound asset-to-composited-frame
time. An Illinois-centre pixel must match one of five legend colours blended
over the fixture background, within rounding tolerance. A merely non-background
pixel, unrelated UI or CPU callback cannot pass. Capture and delivery overhead
is included; physical display scanout is not measured.

Tests use fixture data and a plain local basemap, excluding real tiles and
network conditions. Selection verifies no earlier geometry request; direct URLs
already select States. `usnm-states-parsed` and `usnm-states-first-draw` separate
JSON decoding and CPU render completion. The proxy waits for a visible map
canvas; native fills are outside the separately gated deck canvas.

#### Native MapLibre follow-up (2026-10-16)

On an **Apple M1 (macOS arm64, Chromium 153.0.8010.12, ANGLE Metal)**, with
unique executable vertex-shader variants, the intervals from asset arrival
were:

| Entry path        | Native draw + two-frame proxy (ms) | Composited state-colour frame (ms) |
| ----------------- | ---------------------------------- | ---------------------------------- |
| Select States     | 143.6, 143.0, 151.2, 133.9, 152.6  | 133.4, 129.3, 144.3, 123.8, 145.0  |
| Direct States URL | 236.8, 251.5, 248.7, 255.8, 241.6  | 244.4, 240.6, 237.9, 263.4, 248.4  |

All ten proof pixels were `[189,212,222,255]`, matching the blended legend ramp.
Selection recorded three new vertex variants; direct entry recorded five,
including map startup. Points mode fetched no geometry. The 300 ms pass applies
only to this hardware, backend and fixture, excluding other laptops, real
basemap latency, software rendering and fully cold drivers. Shared OS and
driver caches were not cleared.

After reducing sparse-remap and per-frame allocations (below), five isolated
Node processes measured 2,000-frame maxima of **0.0602, 0.0558, 0.0574, 0.0998,
0.0519 ms**, preparation **10.0–10.3 ms** and p95 **0.0162–0.0170 ms**.
Five processes concurrent with the browser suite had three failures:
**6.1104, 3.7456, 5.9017 ms** maxima. Only the 3.7456 ms frame overlapped
recorded GC (**3.7077 ms**); the other causes are unproven. Failed samples remain
failures. Isolated runs meet the arithmetic budget on this host, without
guaranteeing wall time under contention or GC. The strict assertion remains;
neither budget is a production measurement.

#### Historical deck-renderer measurements and discarded alternatives

On the same Apple M1 with **Chromium 153.0.8010.12, ANGLE Metal native GPU**,
the initial run measured **489.4, 45.6, 49.4, 50.1, 44.9 ms**: the first cold
draw **failed** the 300 ms target. A repeat process, with driver caches retained,
measured **64.2, 39.6, 48.1, 47.7, 48.4 ms**, passing the gate.
Default headless Chromium used **SwiftShader software Vulkan**, not the Apple
GPU, and measured **2,165.6, 1,677.3, 1,586.5, 1,511.1, 1,485.3 ms**, failing.
These historical runs did **not** establish the cold first-paint target;
success with a warm cache did not waive the failures on a cold driver and on
the software backend.

On 2026-10-16, the process-isolated native-Metal gate measured **60.8, 45.5,
52.3, 58.4, 51.3 ms**. The corresponding CPU first-draw intervals were **43.4,
40.3, 48.9, 42.8, 47.4 ms**, and JSON decoding completed **4.3–10.4 ms** after
the asset arrived. An independent five-process run measured **46.8–84.6 ms**
for the paint proxy. Disabling Chrome caches did **not** reproduce the original
489.4 ms failure: the persistent Apple GPU driver cache remains outside the
test's control. These results are browser-cold and possibly driver-warm,
**not a cold-driver pass or an optimization claim**. Clearing the shared
machine and OS caches, or rebooting, was not attempted. A capture on a cold
driver on representative hardware, including GPU or compositor completion if
strict actual presentation is required, remained a release blocker for that
renderer. The first failed run did not capture the new intermediate marks, so
attributing its excess time to shader compilation or to the two animation
frames would be guesswork. After the initial basemap readiness gate was added,
the visibility-aware five-process native-Metal run measured **52.9, 52.8,
54.7, 81.4, 67.7 ms**. The same limit applies, since the driver cache is
uncontrolled; no cold pass is claimed.

The optional shader diagnostic wraps all vertex shader
entry points with a random, executable **1–2 × 10⁻⁸ clip-space offset**. This
subpixel perturbation discourages reusing exactly the same compiled program,
unlike changing a comment. It checks that at least three new native programs
were instrumented (the earlier deck diagnostic required its two programs).
It is **test-only**, not a production shader or a simulated mid-range laptop.
It cannot reset the driver or compiler infrastructure, or prove a cache miss
at every driver layer. The separate frame-colour proof measures compositor
output.

On the same M1, the first five-variant run of the original renderer measured
**547.1, 280.1, 270.8, 266.5, 273.8 ms** (CPU first draw **530.3 ms** in
the first sample), reproducing a cold miss without clearing shared OS caches.
A later matched original-renderer run measured **289.4, 252.6, 260.7, 269.9,
276.0 ms**. An experimental simpler line-segment border shader measured
**273.2–280.6 ms**, which did not clearly beat the matched original, so that
renderer change was discarded. The initial state of the compiler and the
global cache still confounds the comparison. No optimization or resolved cold
budget is claimed. In a separate diagnostic on the software renderer, a CPU
draw of **73.2 ms** against a proxy of **1,398.5 ms** located most of the
delay after CPU submission; on native Metal, a CPU draw of **63.0 ms** against
a proxy of **77.7 ms** did not reproduce the original native cold miss.
Synchronous shader program-status checks in that native sample cost
**8.8 + 8.5 ms**. These observations do not carry over to the uninstrumented
original 489.4 ms sample.

#### Cold-link root cause and minimal reproduction (2026-10-16)

The shader diagnostic also times WebGL compile, link and status calls,
records the parameter enums and captures caller stacks for stalls of 1 ms or
more. On the same M1, Chromium 153 and native Metal, a run measured **307.0,
285.0, 262.5, 257.5, 291.7 ms** (the first sample **fails**). Synchronous
WebGL calls after the asset arrived accounted for **227.3–255.6 ms**; the two
dominant calls were `getProgramParameter(LINK_STATUS = 0x8b82)`, at
**133–152 ms** for the polygon fill and **94–104 ms** for the borders. The
stack enters luma.gl 9.4.2's
`WEBGLSharedRenderPipeline._linkShaders → _getLinkStatus` while the two deck
Models are constructed. That places the measured delay before the CPU draw,
whether or not the basemap is visible. A later run's five proxies passed at
**271.4–284.5 ms**, which does not resolve the earlier cold failures.

`probe-states-shaders.mjs` captures the States shader programs the app
generates once, then links unique executable variants in five fresh browser
processes on a blank page, with no React, deck, basemap, state geometry or
drawing. It reports the host's CPU, backend and version, verifies that the
links succeeded and writes no files. Serial immediate status queries took
**226.6, 255.5, 243.7, 238.5, 244.8 ms**. The control, which enqueues both
programs and polls KHR completion without blocking, took **151.5, 145.3,
146.7, 143.3, 157.3 ms**, with no measured blocking in the final status
checks. This is a **scheduling bottleneck in the dependency's pipeline, not
proof of a fundamental hardware lower bound** or an improvement implemented in
the app.

The device supports `KHR_parallel_shader_compile`, but luma.gl 9.4.2 disables
its asynchronous-compilation feature by default. Enabling that device flag on
its own was tested: it moves the **133 + 95 ms** stalls to
`getProgramParameter(ACTIVE_ATTRIBUTES = 0x8b89)`, in the synchronous
shader-layout introspection during render-pipeline construction. Five proxies
still measured **263.7–298.6 ms** after an earlier compiler warm-up. That
experimental flag was discarded: it does not remove the serialization, and a
warm-cache pass says nothing about cold behavior.

A separate flat, unlit specialization of the polygon shader was also tested
and discarded: its initial proxy was **454.8 ms**, including **447.1 ms** to
CPU draw. None of these discarded alternatives changes production. A safe fix
in the dependency would need to defer **both** the link-status checks and the
shader-layout introspection until the program completes asynchronously, while
preserving layer readiness, picking and error handling. Overriding GL
queries, enabling an experimental flag, or prefetching or delaying the asset
would not be that fix. The raw parallel control is a minimal reproducer for
pursuing that change in the dependency. At that stage the app's 300 ms
requirement remained unmet, not waived. The native renderer kept above avoids
those deck Models rather than overriding luma's private behavior. With the
native build, the raw three-program serial control measured
**133.0–139.2 ms** and the enqueue-all-then-poll control **76.7–80.6 ms**.
These blank-page controls are not measurements of the app's paint.

The latest 5,000-place arithmetic gate (2,000 frames) measured preparation at
**19.009 ms**, a p95 of **0.023875 ms** and a maximum of **0.493375 ms**. This
validates the scoped arithmetic budget on this host, not the shader and draw
budget or a guarantee about every frame on other hardware. A later run
alongside the MapView unit suite measured a p95 of **0.028125 ms** but a
maximum of **4.663916 ms**. That run passed the old gate, which checked only
the percentile, but does **not** meet a strict 2 ms requirement on every
measured frame. The new maximum assertion keeps such outliers as failures
rather than hiding them. The first isolated run with the strict maximum
assertion **failed**, as it should: preparation **16.134 ms**, p95
**0.024541 ms**, maximum **5.113209 ms**. No CPU, GC or scheduler cause was
recorded for that outlier, so it can neither be attributed to garbage
collection nor dismissed as unrelated.

The scrub investigation's follow-up reports the GC entries that overlap any
over-budget frame, without subtracting them from the measured wall time. It
also reduces allocation pressure. State preparation remaps sparse place
indices during prefix accumulation instead of allocating two temporary
600,000-entry arrays. Per-window prefix scratch buffers are reused privately,
and the returned rows stay independent. Quantile collection no longer creates
one small array per state, and colour lookup no longer allocates a closure
per row. The unit tests check that input cubes and prior-window snapshots are
preserved. After these changes, one run measured preparation at **12.598 ms**,
a p95 of **0.017500 ms** and a maximum of **3.064042 ms**, which overlapped a
recorded GC pause of **3.038375 ms**. That run's strict maximum gate
**failed**; the GC pause is real observed wall time, not a reason to waive
it. Earlier instrumented outliers of **5.693417 ms** and **4.423750 ms**
likewise overlapped recorded GC pauses of **5.605417 ms** and **4.363625 ms**.
These are observed runs, not a controlled benchmark that proves any
improvement in maximum latency.

The public metadata read on 2026-10-15 (`pages-v20261010-2`) listed **1,617
places**, none without a state code, and the boundary asset has every code,
PR and VI included. The published catalog is below `MAX_PLACES=5,000`, so no
query over those known places can exceed the cap today. This is not a survey
of real queries or of orphaned place IDs in the index. The truncation
warning, which is tested, stays for future growth.

Not yet built: compare mode, the first-appearance layer, the coverage overlay, embed mode, export beyond the available CSV downloads, and the share dialog beyond copying the link.

## Search engines and link previews

- `index.html` has the canonical link (the home page), Open Graph and Twitter card tags, and JSON-LD for the site and its publisher. `src/seo.test.ts` checks them, and `e2e/seo.spec.ts` checks them in a browser along with what the API sends crawlers.
- The root element holds a short description of the site, for crawlers and previews that don't run JavaScript. React replaces it on its first render.
- `public/og-image.png` remains the homepage's share image. Search links get a 1200×630 Chromium screenshot of the actual linked page, preserving map viewport, filters, timeline position, layer, legend, and attribution. The API injects Open Graph and Twitter metadata before JavaScript runs and serves the PNG at `/share-image.png`. Captures wait for search data, tiles, and the overlay; unlike interactive views, they do not replace a slow initial basemap request with the offline background after two seconds. A failed basemap is rejected, never cached as a complete screenshot. Image URLs carry a render revision to avoid reusing earlier degraded previews. If rendering takes more than two seconds, fails, or both render slots are occupied, the endpoint serves the existing static homepage image with `Cache-Control: no-store` and logs the fallback without the search query. Slow captures continue within their existing 40-second limit and populate the screenshot cache on success; fallback images never enter that cache. Up to 128 successful images are cached for an hour, with two renders at once. Dataset/privacy pages use lightweight summary cards with the bundled SIL Open Font License Noto Sans font.
- The API image bundles Node and Chromium. Locally, run `npm ci` and `node node_modules/playwright-core/cli.js install chromium` in `web`, build the app, then serve it through the API with `USNM_SITE_DIR=web/dist`. Screenshot navigation uses loopback and the port in `USNM_BIND` (default 8080), allowing only local app requests and OpenFreeMap assets. `USNM_SHARE_SCRIPT` overrides the renderer location; `PW_CHROMIUM_PATH` selects an existing browser. Vite alone does not serve dynamic sharing metadata or images.
- `public/robots.txt` tells crawlers not to fetch `/v1/`. The Vite build generates `dist/sitemap.xml` with the home page, `/privacy`, `/datasets`, `/credits` and deduplicated base queries from `src/examples.json`. Add curated examples there to seed more searches; the API reads the same list to allow indexing. Filter variants and uncurated searches remain `noindex`. Curated base searches include a count and up to ten earliest results from the city with the most hits (dates, newspaper titles, snippets and source links) in their initial HTML, with their own escaped title, canonical link and `og:url`. A visually hidden (but in-DOM) "Sample results" section keeps these available after the app loads. These previews share the API's versioned caches, computation queue, concurrency and timeout limits; an unavailable preview returns a retryable error rather than an indexable empty shell. The homepage's initial HTML links to every curated base search, and example cards are real links. Plain exact-phrase searches get readable lowercase paths (`/search/cross-of-gold`, quotes dropped, hyphens for spaces; search is case-insensitive and a hyphen is a word break); searches with operators, wildcards, other punctuation or a non-default mode keep the percent-encoded form, and old `%22` URLs redirect to the readable one. Interactive map results still require JavaScript. `/privacy`, `/datasets` and `/credits` also get their own metadata, and `/status` redirects to `/datasets`.
- `public/<key>.txt` is the public IndexNow ownership key. After a production main deploy, CI fetches the live generated `sitemap.xml` and compares its URL set with the last successful submission, persisted in an Actions cache per environment. **Only a changed URL set or ownership key triggers submission** of every listed URL, including all curated searches, in batches of at most 10,000 (`scripts/ci/indexnow.sh`). It verifies the live key and hostname first; PRs, preview deployments and unchanged sitemaps never submit. There is no source-tree sitemap or second URL list to maintain. Failed submissions warn without failing the deploy and do not advance the saved fingerprint, so a later deploy retries. If the cache is missing or evicted, the next production deploy submits once to establish its baseline.
