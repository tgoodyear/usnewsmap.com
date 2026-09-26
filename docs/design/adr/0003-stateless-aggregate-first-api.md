# ADR-0003: Stateless, aggregate-first, cacheable API

- **Status:** Accepted
- **Date:** 2026-09

## Context

The legacy API returned a **random sample of 500 hits** per request, kept per-user state in MongoDB, and was called on **every playback frame** (every 400 ms). Maps were sampled, scaling was poor, and there was a documented race condition.

## Decision

- One `GET /v1/aggregate` returns **complete** counts as a sparse *place × bucket* cube, together with the national series and the first appearance per place.
- The **browser** computes cumulative, trailing-window, relative and first-appearance views from prefix sums in O(places) per frame.
- All reads are **GET with canonical URLs**, immutable per `index_version`, and cached in the browser, in process and in a persistent Blob cache (plus Front Door in the growth profile).
- There are no sessions, cookies or per-user server state.

## Alternatives

- *Keep server-side playback state*: rejected (latency, cost, fragility).
- *Return raw hits and aggregate in the browser*: rejected, because millions of hits can't be shipped to the client.
- *GraphQL*: rejected, because POST-by-default and flexible shapes defeat CDN caching, and the query surface is small.

## Consequences

- One search is one request, and popular searches are served from the edge.
- The response size is bounded by automatic bucket selection (≤ ~400k cells in the worst case, ~1 MB compressed).
- Finer-grained playback over long ranges needs a "refine" re-query (acceptable).
