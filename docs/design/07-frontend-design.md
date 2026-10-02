# 07: Frontend Design

## 7.1 Stack

| Concern | Choice | Why |
|---------|--------|-----|
| Framework | **React 19 + TypeScript**, built with **Vite** | Large hiring and contributor pool; strong typing against the OpenAPI types |
| Map | **MapLibre GL JS** (vector basemap) + **deck.gl** (`ScatterplotLayer`, `HeatmapLayer`, `GeoJsonLayer`) | WebGL rendering of thousands of animated points at 60 fps; open source, no token required |
| Basemap | Self-hosted **Protomaps PMTiles** (a US extract of OpenStreetMap) in a public-read Blob container (HTTP range requests, no tile server), with a muted "archival" dark style and a light style. The keyless OpenFreeMap service is the fallback | No per-view tile fees or third-party keys (the legacy code depended on CartoDB and Mapbox tokens) |
| Charts | **uPlot** (timeline, compare) | Tiny and fast; handles thousands of points |
| Data fetching | **TanStack Query** | Caching, deduplication, cancellation of superseded searches |
| State | URL as the source of truth (custom `useUrlState`, backed by `URLSearchParams`), plus **Zustand** for ephemeral UI | Permalinks by construction |
| Styling | CSS Modules + design tokens (CSS custom properties); light and dark themes | Small, themable, no runtime CSS-in-JS |
| UI primitives | **Radix UI** (dialogs, sliders, toggles, tooltips) | Accessible by default |
| i18n | Messages in `en` at launch, structured for more locales | Readiness for Spanish and German (many historic papers are in German) |
| Testing | Vitest + Testing Library; **Playwright** end-to-end and visual snapshots; axe-core accessibility checks in CI | |
| Hosting | **Served by the API app** from its image; no Front Door ([ADR-0010](adr/0010-site-served-by-the-api.md)) | Same origin as the API (no CORS), Entra-only deploys, site and API always on the same version, free managed TLS. No global edge; Front Door can be added later |

**Implementation notes (first slice, [`web/`](../../web/README.md)):**

- The timeline is a small SVG bar chart rather than uPlot, because it has one series until compare mode lands.
- Controls are native elements, so Radix and Zustand aren't needed yet.
- The basemap defaults to OpenFreeMap Positron until the PMTiles extract is published.
- `/status` is a separate page (lazy-loaded; the app has no router, so `main.tsx` picks it by path with `src/route.ts`) that shows the ingest pipeline's status from `GET /v1/status` and refreshes every 30 s. It also has two tables of the published pages, "Pages by state" and "Pages by language" (06 §6.3.6), sortable by any column like the search page's place table. The footer links to it. Any other path shows a not-found page, and the API sends it with a 404.
- `/privacy` is the privacy notice (lazy-loaded like `/status`, `src/privacy/PrivacyPage.tsx`). The search page, the status page and the not-found page link to it in their footers. It is the one page besides the home page that search engines index: it is in `sitemap.xml`, and the API gives its copy of `index.html` its own title, canonical link and `og:url` (`shell_for` in `crates/usnm-api/src/site.rs`).

## 7.2 Layout and wireframes

The design keeps the legacy "map-first" identity: a full-bleed map with controls layered over it. The new layout groups the controls into a **search bar**, a **time dock**, and a **side panel**.

### Desktop (≥ 1024 px)

```
┌──────────────────────────────────────────────────────────────────────────────────────┐
│ ◉ US News Map   [ "cross of gold"            ] [Phrase ▾] [1896-06-01]–[1896-12-31] ⚙ ⤴ │  ← search bar (⚙ filters, ⤴ share)
├──────────────────────────────────────────────────────────────┬───────────────────────┤
│                                                              │ Chicago, IL     612 ✕ │
│      ·   ·        ●                                          │ ───────────────────── │
│    ·        ●  ●     ◉ Chicago                ●●             │ Jul 10 1896 · Chicago │
│         ·      ●    ●   ●     ●  ●  ●●●●  ●  ●●●  NYC        │ Eagle · p1            │
│   ●  SF          ·      ●   ●   ●   ● ●●●●●●  ●●             │ "…crucify mankind upon│
│                    ·       ●   ● ●  ●  ●● ●                  │  a ▮cross of gold▮…"  │
│                         ·     ●   ●  ●                        │ [View at LoC] [PDF]   │
│  Legend: ● 1–10 ● 11–100 ● 101–1k  ◌ county-precision        │ …                     │
│  Layer: (•) Points ( ) Heat ( ) States ( ) First appearance  │ [Load more]           │
├──────────────────────────────────────────────────────────────┴───────────────────────┤
│ ▶ ❚❚  ◀ ▶  Week ▾  Speed 1× ▾   (•) Cumulative ( ) Last [3] [months▾]   Jul 12, 1896  │  ← time dock
│ ▁▁▂█▇▅▄▃▂▂▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁  hits ◉ relative ○ raw   ────────●──────────────────── │  ← timeline + scrubber
└──────────────────────────────────────────────────────────────────────────────────────┘
```

### Mobile (< 640 px)

- The search bar collapses to a single field with a filter sheet.
- The time dock becomes a bottom sheet with the scrubber, play button and interval.
- The side panel becomes a draggable bottom sheet. A **List** tab duplicates the map content as an accessible, sortable table.

### Key states

- **Empty:** example searches as cards ("Cross of Gold 1896", "Scalawag 1865–1880", "Influenza 1918") and a coverage map preview.
- **Loading:** a skeleton shimmer on the timeline, the previous results dimmed, and a cancel button.
- **Large search:** when the API answers `202` (06 §6.3.5), a status notice (`role="status"`), "Large search, still working…", stays up while the client asks again after each `Retry-After`. It stops asking when the visitor changes the search (the query's abort signal). After 150 s, a little more than the API's 2-minute limit, it stops and shows the timeout problem. A timed-out or busy search is not retried at once (`isRetryable` in `src/api/client.ts`).
- **No results:** suggestions (switch to "all words", enable OCR-tolerant, widen dates), with a link to the coverage layer.
- **Error:** the problem+json `hint` shown inline.

## 7.3 Client-side temporal engine (replaces `/update`)

The legacy app called the server on **every** animation frame. The new client gets a sparse cube once (see [06 §6.3.3](06-api-design.md#633-get-v1aggregate-response)) and computes every view locally.

**Data structure.** For P places with hits and B buckets:

- Build a **per-place prefix-sum array** `S[p][b] = Σ_{k≤b} hits[p][k]` as a flat `Uint32Array` of size `P × (B+1)`. The worst realistic case is 3,000 × 211 ≈ 633k × 4 bytes ≈ 2.5 MB. A Web Worker builds it in under 20 ms.
- **Cumulative** view at bucket `t`: `v[p] = S[p][t]`.
- **Trailing window** of `w` buckets: `v[p] = S[p][t] − S[p][t−w]`.
- **Relative** view: the same prefix sums over the baseline cube, `rel = v_hits / v_baseline`.
- **First appearance** layer: color by `first_day[p]`, and show only places where `first_day ≤ t` (animates the spread).

Every frame is **O(P)**: about 3,000 subtractions, then a single deck.gl attribute buffer update. That comfortably holds 60 fps, whereas the legacy server round-trip ran at 400 ms per frame.

**Sub-bucket playback:** if the user picks Day but the response is weekly (a long range), the UI offers to "refine": it re-queries at day granularity for a range of 4 months or less around the scrubber.

## 7.4 Visual encoding

| Layer | Encoding | Notes |
|-------|----------|-------|
| **Points** (default) | Circle **area ∝ hits**, i.e. radius ∝ √hits (perceptually honest); fill color = relative frequency on a sequential palette; hollow ring = county- or state-precision place | Replaces the legacy mean/std five-class buckets, which shifted meaning between searches |
| **Heat** | deck.gl `HeatmapLayer` weighted by hits (or relative) | Good for dense eastern regions |
| **Relative rate** (`norm=skew`) | Points colored by each place's rate against the other places in the same buckets, on a diverging scale centred on 1×; area ∝ matches expected; faded where it can't tell | [11](11-term-geographic-skew.md); replaces the colour menu with a Pages / Relative rate toggle |
| **States** | Choropleth of hits per 1,000 pages published in the window; hatched where there is no coverage | Normalized, avoids the "big city" bias |
| **First appearance** | Categorical time ramp (early = warm, late = cool); animated reveal | Designed for "Cross of Gold"-style spread stories |
| **Coverage** (overlay) | States and places with **no pages published** in the current window are shaded or hatched, with the tooltip "No digitized newspapers for this period" | A time-aware version of the legacy black-state mask |

Legends are always visible and state the unit ("pages containing the phrase"). Colors come from a color-vision-deficiency-safe palette and are validated in both light and dark themes.

## 7.5 URL state (permalinks)

```
https://usnewsmap.com/?q=%22cross+of+gold%22&mode=phrase&from=1896-06-01&to=1896-12-31
   &bucket=week&t=1896-07-12&win=cum&layer=points&norm=rel&state=&place=P00412&z=4.2&c=-92.1,39.4
```

- Only non-default values are written. Compare mode uses `q1…q4`.
- `embed=1` hides chrome, disables scroll-zoom (the legacy `disableScroll` behavior), and shows an "Open in US News Map" link.
- The **Share** dialog offers the URL, an `<iframe>` snippet, a citation (Chicago/MLA, including `index_version` and access date) and a PNG snapshot (map + timeline rendered client-side).

## 7.6 Accessibility (WCAG 2.2 AA)

- Every map view has an equivalent **List/Table** tab (place, state, hits, relative, first appearance), sortable and exportable.
- Playback is keyboard operable: `Space` plays or pauses, `←`/`→` step, `Shift+←/→` jumps 10 buckets, `Home`/`End` go to the ends. Focus is visible.
- When `prefers-reduced-motion` is set, autoplay is off and transitions are instant.
- A live region announces "Showing N places, M pages, up to July 12 1896" as the scrubber moves (throttled).
- Text contrast is ≥ 4.5:1 over the map (solid panels, not translucent overlays like the legacy header). Touch targets are ≥ 24 px.

## 7.7 Performance budget

| Metric | Budget |
|--------|--------|
| Initial JS (gzip) | ≤ 250 KB on the critical path; map and deck.gl lazy-loaded right after first paint |
| LCP (4G, mid-range phone) | ≤ 2.5 s |
| Time from search submit to first map paint (warm cache) | ≤ 300 ms |
| Playback | 60 fps at 3,000 places; ≥ 30 fps on low-end mobile |
| Memory | ≤ 150 MB of JS heap in the worst case |

## 7.8 Analytics (privacy-preserving)

As built, the site counts page views and nothing else (`src/pageview.ts`). When the app starts it works out its page (`search`, `status`, `privacy` or `not-found`) and sends one page view to `POST /v1/beacon` on its own origin (06 §6.3.7). The app has no client-side router, so every page shown is a page load and gets exactly one; searching on the home page changes the URL but not the page, and sends nothing.

- **Contents.** The page name and its fixed title (`TITLES` in `src/route.ts`). On the first page view of a page load, also `document.referrer` cut down to its origin (`internal` if it is the site itself, empty if there is none) and the landing URL's `utm_source`, `utm_medium` and `utm_campaign`, lowercased and cut to 64 characters. Later page views of the same load, if there ever are any, say `internal`. Never the path, the query string or the search; the API refuses any field beyond these.
- **Sending.** `navigator.sendBeacon` with the JSON as a string (sent as `text/plain`, so no preflight), falling back to `fetch` with `keepalive` if `sendBeacon` is missing or refuses. It never delays the page, and failures are ignored.
- **When.** Only when the page's origin is `https://usnewsmap.com` in a production build, so `npm run dev`, `vite preview` and the end-to-end runs send nothing, except the tests that load the build as that origin on purpose (below). `VITE_PAGE_VIEWS=1` at build time turns it on for any origin (to try it against a local API) and `0` turns it off. `navigator.doNotTrack === "1"` (or `window.doNotTrack`) and `navigator.globalPrivacyControl === true` turn it off in every case, and the API drops page views that arrive with `DNT: 1` or `Sec-GPC: 1`.
- **Storage.** Nothing: no cookies, no local or session storage, no IndexedDB, no Cache Storage, and no user, session or device id. An end-to-end test (`e2e/pageview.spec.ts`) loads a search and checks all of those are empty.
- **CSP.** The request is same-origin, so `connect-src 'self'` already allows it; the policy is unchanged.
- **Tests.** Unit tests cover the report's fields, the origin and tag handling, the opt-outs and the fallback (`src/pageview.test.ts`). The end-to-end tests load the build as `https://usnewsmap.com` (every request to that origin answered by the local server) and assert the exact body of each page view, that it carries no search text, and that GPC, DNT and other origins send none.

The earlier plan for this section (the Application Insights JavaScript SDK with custom events for searches, playback and sharing) is not built: the SDK can't authenticate to a component with local auth disabled. Popular queries for pre-warming, if they are ever collected, would come from the server side as daily counts per canonical query string, kept only when at least 5 requests made the same one that day, with no IP, user agent or other identifier.
