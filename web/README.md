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

- **The URL is the state** (`src/state/url.ts`). Query, dates, states, newspaper languages (`lang`, [07 §7.9](../docs/design/07-frontend-design.md#79-language-filter)), scrubber position, window, layer, measure, selected place and the order of its pages (`sort`), tab and viewport are all in the URL. Only non-default values are written, and every parameter is validated when the URL is parsed.
- **One aggregate request per search.** `src/engine/cube.ts` builds per-place prefix sums from the sparse cube, so each playback frame, cumulative or trailing window, is a subtraction per place. The place table's share of pages published and the relative rate use the coverage cube named by `baseline_ref`, aligned to the same places.
- **Relative rate** (`norm=skew`, [doc 11](../docs/design/11-term-geographic-skew.md)). `src/engine/skew.ts` ports the Rust scoring (`usnm_core::skew`); `src/engine/skewModel.ts` fits a search once, in a web worker (`skew.worker.ts`), and scores each playback frame from prefix sums. `src/engine/skew.test.ts` checks the port against `fixtures/skew-vectors.json`, which `cargo test -p usnm-core --test skew_vectors` checks (and rewrites with `UPDATE_SKEW_VECTORS=1`). The share-of-pages measure (`norm=rel`) was removed; old links open on Pages.
- **Version pinning.** Every request carries the `v` from `/v1/meta`, so a whole session reads one published snapshot.
- **Large searches.** A search the API is still computing comes back as `202 Accepted`. The client asks again after `Retry-After` for up to 150 s and shows "Large search, still working…" meanwhile, or how many searches are ahead while the API has it queued; changing the search stops the asking, though the API still finishes the computation (`src/api/client.ts`). The end-to-end tests get a slow search from the fixture API's `USNM_FIXTURE_SLOW_TERM` switch, set in `scripts/ci/start-api-fixtures.sh`.
- **First and last mention.** The summary line names the search's earliest matching page and the date of its latest (`total.first` and `total.last` from `/v1/aggregate`). Each date opens that page's place with its pages oldest or newest first, so the page is at the top of the list, with its snippet and Library of Congress link: one OCR misread can make a misleading first mention, and this is how to check it. The place table has first appearance and last seen, and the relative-rate CSV has both (`first_seen`, `last_seen`).
- **Snippets** are split into text and `<mark>` segments and rendered as text nodes. No API HTML is ever injected.
- **Accessibility.** A Table tab mirrors the map. Playback works from the keyboard: `Space`, `←`/`→` (`Shift` for 10), `Home`/`End`. The current date is announced in a live region. Reduced motion is honored. Without WebGL2, the table is shown instead of the map.
- **Page views** (`src/pageview.ts`, 07 §7.8). One per page load, sent with `navigator.sendBeacon` to `/v1/beacon`: the page name and title, and on the first page the referrer's origin and the landing URL's `utm_*` tags. Never the search or other query parameters. Do Not Track and Global Privacy Control turn it off, and nothing is stored in the browser. The privacy page (`/privacy`) describes it for visitors.
- **Basemap startup.** The entry bundle warms the initial permalink/default viewport’s tiles, sprites and label glyphs before the lazy map stack loads (up to 40 requests; skipped with `VITE_BASEMAP_STYLE=none`). TileJSON resolves the provider’s current tile URLs at runtime. Dots fade in after the basemap’s first loaded idle, or after a two-second fallback if the provider is slow/blocked; reduced motion disables the fade. Requests use the browser HTTP cache, not persistent application storage.
- **Performance.** The map stack is lazy-loaded; the critical-path JS is about 99 KB gzip (budget 250 KB).

## States layer

Choose **States** under Map layer (or share `layer=states`). Modern borders
apply even to historical territories. Census public-domain boundaries are
prepared reproducibly; see [the asset source and preparation](src/assets/README.md).
Alaska, Hawaii and territories remain at their true positions.

`engine/states.ts` reduces both sparse cubes to state prefix sums once per
search, then uses the same cumulative/trailing window as points. Crucially,
the denominator includes **all coverage places**, including places with no
matches; aligning coverage only to matching places would inflate state rates.
Unknown/missing place states go to Unassigned, never disappear from the
table. If the API caps matching places (5,000), a visible warning marks
incomplete numerators; the missing tail cannot be assigned or recovered in
the client. No real-query cap survey has been performed. No API changes or
extra aggregate/coverage requests are introduced.

First-pass decisions for #452:

- Page-weighted matches per 1,000 pages published, not raw counts or per-title
  rates. Both raw counts remain in the tooltip, sortable table and CSV.
- Minimum denominator: one page. No arbitrary suppression threshold; the
  legend warns that small counts are noisy. Zero pages is grey/unknown
  (not rate zero), while pages with no matches are light neutral. The legend's
  no-pages swatch is additionally hatched; map no-pages fills are solid grey.
- Five quantiles of positive state rates in the **current window**, with
  zero a separate neutral class. Ties remain together and can leave empty
  classes, explicitly labelled. Breaks change with scrubbing; the legend
  changes with them. No raw/rate toggle.
- Desktop click adds a state filter; existing search filters remove it.
  Touch taps pin details with explicit Filter and Dismiss buttons, so the
  initial tap does not accidentally filter. The states table is the keyboard
  path, with the same filter action and a CSV download.
- States is unavailable for non-Pages measures, newspaper-filtered searches,
  Congress, or unavailable coverage baselines. Unsupported shared links draw
  points instead. The layer selector stays hidden in Table/no-WebGL contexts;
  an already-selected States layer retains its accessible table there.
- Geometry is reused across frames; only fill attributes change.
  Unavailable coverage never draws no-pages fills. Escape dismisses hover
  or pinned state details regardless of keyboard focus. Small positive rates
  retain significant digits, and legend precision increases when needed to
  distinguish unequal thresholds.
  Opt-in reproducible performance gates and the measured hardware/backend
  limitations are described below. Functional test success is not proof
  of the performance budgets.

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
```

The scrub gate asserts **p95 ≤2 ms**, after 200 warm-up frames, including totals,
quantiles and all fill-colour lookups. Preparation is reported separately; this
does not measure the rest of React or deck.gl's draw. On **Apple M1 / macOS arm64,
Node 25.8.2**, the isolated 2026-10-15 run measured preparation **16.679 ms**,
median **0.014 ms**, p95 **0.023 ms**, maximum **0.100 ms** across 2,000 frames.
A concurrent test run had p95 **0.080 ms** but a **12.780 ms** maximum outlier.
This is a percentile gate, **not a guarantee that every frame is ≤2 ms**.

The browser gate asserts a maximum **300 ms** over five fresh browser processes
with Chrome's shader disk and program caches disabled. It
measures asset Resource Timing `responseEnd` to the first deck draw plus two
animation frames, allowing a browser paint opportunity. The mark
`usnm-states-first-paint` is a **presentation proxy**, not a GPU-fence or
compositor timestamp. It uses fixture data and a local plain basemap with the
map loaded before selecting States; it is not a real-data basemap benchmark.
The test checks that no geometry resource was requested before States selection.
`usnm-states-parsed` and `usnm-states-first-draw` additionally separate JSON
decoding and CPU draw submission from the subsequent paint opportunity. Neither
draw submission nor two animation frames proves GPU completion.
The paint proxy waits until the basemap-gated overlay has nonzero opacity;
an invisible deck draw is not counted as visible paint. The benchmark waits
for the initial basemap/overlay fade to finish before selecting States.

On the same Apple M1 with **Chromium 153.0.8010.12, ANGLE Metal native GPU**,
the initial run measured **489.4, 45.6, 49.4, 50.1, 44.9 ms**: the first cold
draw **failed** the 300 ms target. A repeat process, with driver caches retained,
measured **64.2, 39.6, 48.1, 47.7, 48.4 ms**, passing the gate.
Default headless Chromium used **SwiftShader software Vulkan**, not the Apple
GPU, and measured **2,165.6, 1,677.3, 1,586.5, 1,511.1, 1,485.3 ms**, failing.
These runs do **not** establish the cold first-paint target on a representative
mid-range laptop. That acceptance remains outstanding; warm-cache success
must not conceal the cold/backend failures.

On 2026-10-16, the process-isolated native-Metal gate measured **60.8, 45.5,
52.3, 58.4, 51.3 ms**. The corresponding CPU first-draw intervals were **43.4,
40.3, 48.9, 42.8, 47.4 ms**, and JSON decoding completed **4.3–10.4 ms** after
the asset arrived. An independent five-process run measured **46.8–84.6 ms**
for the paint proxy. Disabling Chrome caches did **not** reproduce the original
489.4 ms failure: the persistent Apple GPU-driver cache remains uncontrolled.
These are browser-cold, potentially driver-warm results, **not a cold-driver
pass or an optimization claim**. Clearing shared machine/OS caches or rebooting
was deliberately not attempted. A driver-cold capture on representative
hardware, including GPU/compositor completion if strict actual presentation is
required, remains a release blocker. The first failed run did not capture the
new intermediate marks, so attributing its excess time to shader compilation
or to the two animation frames would be speculation.
After integrating the initial basemap readiness gate, the visibility-aware
five-process native-Metal run measured **52.9, 52.8, 54.7, 81.4, 67.7 ms**.
The same uncontrolled-driver-cache limitation applies; no cold pass is claimed.

The optional shader diagnostic wraps the polygon and border vertex shader
entry points with a random, executable **1–2 × 10⁻⁸ clip-space offset**. This
subpixel perturbation discourages reusing exactly the same compiled program,
unlike changing a comment. It asserts that both shaders were instrumented.
It is **test-only**, not a production shader or a simulated mid-range laptop.
It cannot reset driver/compiler infrastructure, prove a cache miss at every
driver layer, or measure actual compositor presentation.

On the same M1, the first five-variant run of the original renderer measured
**547.1, 280.1, 270.8, 266.5, 273.8 ms** (CPU first draw **530.3 ms** in
the first sample), reproducing a cold miss without clearing shared OS caches.
A later matched original-renderer run measured **289.4, 252.6, 260.7, 269.9,
276.0 ms**. An experimental simpler line-segment border shader measured
**273.2–280.6 ms**, but did not convincingly beat the matched original;
that renderer change was discarded. The initial compiler/global-cache state
still confounds the comparison. No optimization or resolved cold budget is
claimed. In a separate software-renderer diagnostic, CPU draw **73.2 ms**
versus proxy **1,398.5 ms** located most delay after CPU submission; native
Metal CPU **63.0 ms** versus proxy **77.7 ms** did not reproduce the original
native cold miss. Synchronous shader program-status checks in that native
sample cost **8.8 + 8.5 ms**. These observations must not be extrapolated to
the uninstrumented original 489.4 ms sample.

Read-only public metadata on 2026-10-15 (`pages-v20261010-2`) listed **1,617
places**, none missing state codes; all codes are represented by the boundary
asset, including PR and VI. The published catalog is below `MAX_PLACES=5,000`,
so no query over those known places can exceed it today. This is not a survey
of real queries or orphaned index place IDs. The tested truncation warning
remains necessary for future growth.

Not yet built: compare mode, the first-appearance layer, the coverage overlay, embed mode, export beyond the available CSV downloads, and the share dialog beyond copying the link.

## Search engines and link previews

- `index.html` has the canonical link (the home page), Open Graph and Twitter card tags, and JSON-LD for the site and its publisher. `src/seo.test.ts` checks them, and `e2e/seo.spec.ts` checks them in a browser along with what the API sends crawlers.
- The root element holds a short description of the site, for crawlers and previews that don't run JavaScript. React replaces it on its first render.
- `public/og-image.png` is the share image, a 1200×630 screenshot of the "Cross of Gold, 1896" example on the live site.
- `public/robots.txt` tells crawlers not to fetch `/v1/`, and `public/sitemap.xml` lists the home page and `/privacy`. Search permalinks and `/datasets` (the dataset details, which change often) are left out, since the API marks them `noindex`; it redirects the page's old path, `/status`, to `/datasets`. The API serves `/privacy` with its own title, canonical link and `og:url`.
- `public/<key>.txt` is the IndexNow key. After each production deploy, CI submits the sitemap's URLs to IndexNow (`scripts/ci/indexnow.sh`). A failed submission is logged as a warning and never fails the deploy.
