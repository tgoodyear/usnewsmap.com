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
  The 300 ms post-download first-paint and 2 ms / 5,000-place scrub targets
  have **not been measured on a mid-range laptop**; passing functional tests
  must not be read as those performance measurements.

Not yet built: compare mode, the first-appearance layer, the coverage overlay, embed mode, export beyond the available CSV downloads, and the share dialog beyond copying the link.

## Search engines and link previews

- `index.html` has the canonical link (the home page), Open Graph and Twitter card tags, and JSON-LD for the site and its publisher. `src/seo.test.ts` checks them, and `e2e/seo.spec.ts` checks them in a browser along with what the API sends crawlers.
- The root element holds a short description of the site, for crawlers and previews that don't run JavaScript. React replaces it on its first render.
- `public/og-image.png` is the share image, a 1200×630 screenshot of the "Cross of Gold, 1896" example on the live site.
- `public/robots.txt` tells crawlers not to fetch `/v1/`, and `public/sitemap.xml` lists the home page and `/privacy`. Search permalinks and `/datasets` (the dataset details, which change often) are left out, since the API marks them `noindex`; it redirects the page's old path, `/status`, to `/datasets`. The API serves `/privacy` with its own title, canonical link and `og:url`.
- `public/<key>.txt` is the IndexNow key. After each production deploy, CI submits the sitemap's URLs to IndexNow (`scripts/ci/indexnow.sh`). A failed submission is logged as a warning and never fails the deploy.
