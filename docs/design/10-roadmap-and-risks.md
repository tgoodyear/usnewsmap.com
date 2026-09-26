# 10: Roadmap, Risks & Open Questions

## 10.1 Phased delivery

```mermaid
gantt
  dateFormat  YYYY-MM-DD
  title US News Map revival: indicative plan (1–2 engineers)
  section Phase 0 · Validate
  S-1 corpus sizing & source access      :p0a, 2026-10-05, 10d
  S-2 search engine bake-off (1M pages)  :p0b, after p0a, 15d
  S-3 geocoding QA                       :p0c, 2026-10-05, 10d
  Decision gate (ADR-0001 final)         :milestone, after p0b, 0d
  section Phase 1 · MVP (parity + accuracy)
  Infra (Bicep, azd, CI/CD)              :p1a, after p0b, 10d
  Ingest pipeline + curated lake         :p1b, after p0b, 20d
  Rust API (aggregate, hits, meta)       :p1c, after p0b, 20d
  SPA (search, map, playback, list)      :p1d, after p0b, 25d
  Full backfill + index                  :p1e, after p1b, 10d
  Beta (invite historians, librarians)   :milestone, after p1e, 0d
  section Phase 2 · Public relaunch
  Compare, first-appearance, filters     :p2a, after p1e, 15d
  Export, embed, public API docs         :p2b, after p1e, 10d
  Accessibility audit, perf, load test   :p2c, after p2a, 10d
  Relaunch + press                       :milestone, after p2c, 0d
  section Phase 3 · Research features
  Historical boundaries                  :p3a, after p2c, 15d
  Article-level (American Stories)       :p3b, after p2c, 30d
  Reprint / viral-text clusters          :p3c, after p3b, 30d
  Semantic search (optional, cost-gated) :p3d, after p3b, 20d
```

### Phase exit criteria

| Phase | Exit criteria |
|-------|---------------|
| **0 Validate** | Corpus size known within ±15%; search backend chosen from S-2 data; geocoding ≥ 98% of titles at city precision, with the rest at county precision; bulk-download rate plan agreed with LoC's published limits |
| **1 MVP** | F-01…F-08, F-20, F-22, F-25 (state/title), F-27 shipped; counts equal a brute-force scan on the golden set; p95 targets met; cost ≤ $500 in staging projections |
| **2 Relaunch** | F-09, F-21, F-23, F-24, F-26, F-28, F-29, F-30 shipped; WCAG 2.2 AA audit passed; runbooks rehearsed (full re-index, rollback) |
| **3 Research** | Each feature has its own proposal and cost check before build |

## 10.2 Validation spikes

| ID | Question | Method | Output |
|----|----------|--------|--------|
| **S-1** | How big is the corpus really (pages, text bytes, languages)? Which bulk endpoints and formats exist, and what are the rate limits? | Download 20 batches across eras; measure; read the Datasets portal docs; email `ndnptech@loc.gov` about bulk etiquette | Sizing sheet; updated [04](04-data-sources-and-ingestion.md) |
| **S-2** | Quickwit vs Azure AI Search on our query shape | 1M-page sample in both; benchmark set ([05 §5.8](05-search-and-storage.md#58-benchmark-query-set-spike-s-2)); verify pre-1970 timestamps, fuzzy, slop, snippets, delete tasks, managed-identity Blob auth in Quickwit, and split-cache behavior on Container Apps | Benchmark report; final [ADR-0001](adr/0001-search-engine.md) |
| **S-3** | Geocoding quality from LoC metadata + GNIS | Resolve all titles; diff against legacy `town_ref.csv`; manually review outliers | `overrides/places.csv`; precision stats |
| **S-4** | Playback performance on low-end devices | Synthetic worst-case cube (3,000 × 211) on a low-end Android device and an older iPhone | Adjust the budget or thin the cube |

## 10.3 Risk register

| # | Risk | Likelihood | Impact | Mitigation | Owner |
|---|------|-----------|--------|-----------|-------|
| R-1 | **Sustainability again**: the maintainer leaves and the bills lapse | Med | High | ≤ $500/mo; IaC; runbooks; institutional sponsor owns the subscription and domain; ≥ 2 admins; handover runbook ([ADR-0005](adr/0005-sustainability-constraints.md)) | Owner |
| R-2 | Quickwit fails S-2 (latency on high-frequency terms, pre-1970 dates, cache limits) | Med | Med | Integer bucket fields; Dedicated workload profile; fallback to Elastic on Azure or AI Search via the `SearchBackend` trait | Eng |
| R-3 | AI Search chosen, but budget can't sustain ~$3–6k/mo | Med (if chosen) | High | Only choose A with committed multi-year funding; otherwise B | Owner |
| R-4 | LoC changes bulk formats or endpoints again (as in 2025) | Med | Med | The curated lake decouples the site from the source, so the site keeps serving; a parser schema check in `discover`; follow the NDNP news feed | Eng |
| R-5 | Bulk download throttled; backfill slow | Med | Low | Start early; respect limits; validated mirror as accelerator; the backfill is one-time | Eng |
| R-6 | Quickwit project cadence slows after the acquisition | Low–Med | Med | Apache-2.0; pin versions; Tantivy maintained independently; backend abstraction | Eng |
| R-7 | Abuse or bot traffic causes cost spikes | Med | Med | Edge cache; rate limits; replica caps; budget alerts; Front Door Premium bot protection if needed | Eng |
| R-8 | OCR quality misleads users (false negatives) | High | Med | OCR-tolerant mode; "pages containing" wording; improved NDNP-Open-OCR re-ingestion; methodology page | Product |
| R-9 | Geographic misattribution (titles that moved; county-level fallbacks) | Med | Low | Precision flags; date-ranged places; overrides reviewed in PRs | Eng |
| R-10 | Rust maintainer pool is thin | Low–Med | Med | Small, well-tested API; OpenAPI contract; documented rewrite path ([ADR-0004](adr/0004-rust-api.md)) | Owner |
| R-11 | Legacy secrets in the public legacy repo are still valid | Unknown | Med | **Revoke the Google Places key and Mapbox token now**; history rewrite optional (the keys are already exposed, so revocation is what matters) | Owner |

## 10.4 Open questions for the owner

1. **Funding and sponsor.** Is there an institution (UGA Libraries / eHistory, GTRI, a state newspaper project, LoC Labs) willing to own the subscription? This decides Option A vs B and the handover story.
2. **Search backend preference.** Is a fully managed service (AI Search, about $3–6k/mo) required on policy grounds, or is the Quickwit PaaS-container design (about $0.2–0.4k/mo) acceptable if S-2 passes?
3. **Branding and credits.** Keep the "US News Map" name and credit the original GTRI and eHistory team and Prof. Saunt? Contact them for endorsement and redirect permissions?
4. **Domain.** Confirm control of `usnewsmap.com` (the registrar account), and whether `usnewsmap.net` (used for legacy Solr hosts) is still held.
5. **Legacy repo.** Archive `tgoodyear/usnewsmap` with a README pointing to the new project, after revoking the exposed keys.
6. **Scope of the public API.** Anonymous-only with fair-use limits, or issue keys to researchers for higher limits?
7. **Analytics.** Is App Insights custom-event analytics enough, or is a privacy-friendly product analytics tool wanted?

## 10.5 Definition of done for relaunch

- [ ] All P0 features live; counts verified against brute force on the golden set
- [ ] SLOs met for 14 consecutive days in beta
- [ ] Cost ≤ $500 per month for 2 consecutive months (actual)
- [ ] Runbooks rehearsed: full re-index, index rollback, app rollback, handover dry run
- [ ] Accessibility audit (WCAG 2.2 AA) passed
- [ ] Legacy keys revoked; secret scanning and push protection on
- [ ] About, methodology, privacy and API docs pages published
