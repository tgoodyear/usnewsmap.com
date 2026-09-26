# 09: Operations, Security & Cost

## 9.1 Service level objectives

| SLI | SLO (30-day) | Measured by |
|-----|--------------|-------------|
| Availability of `/v1/aggregate` and `/v1/hits` (non-5xx, excluding 503 query-too-broad) | **99.5%** | Front Door + API logs |
| Latency of `/v1/aggregate`, uncached, typical query set | p95 ≤ **1.5 s** | API server spans |
| Latency of `/v1/aggregate`, edge cached | p95 ≤ **150 ms** | Front Door logs |
| Data freshness (LoC batch published → searchable) | ≤ **7 days** for 95% of batches | Manifests vs discover log |
| SPA LCP | p75 ≤ **2.5 s** | App Insights browser telemetry |

The error budget for 99.5% is about 3.6 hours per month. When it is exhausted, feature deploys pause in favor of reliability work.

## 9.2 Observability

- **Traces:** OpenTelemetry from the SPA (App Insights JS) → Front Door → API → search backend. The trace id is propagated with `traceparent`. Spans: `parse`, `cache`, `backend.aggregate`, `postprocess`, `encode`.
- **Metrics:** request rate, error rate and latency per endpoint; cache hit ratio (Front Door and moka); backend latency; searcher CPU/memory/split-cache hits; ingest pages/s; queue depth; batches pending; index doc count by version.
- **Logs:** structured JSON (`tracing` → OTLP). **No query text is logged at info level**. Canonical query hashes are logged instead; query text goes only into the k-anonymized daily aggregate used for pre-warming.
- **Dashboards:** an Azure Workbook "USNM Overview" with golden signals, cost to date, ingest status, and the top canonical queries (k ≥ 5).
- **Alerts (Action Group → email + optional Teams/Slack webhook):** SLO burn rate (fast 2%/1 h, slow 5%/6 h); `/readyz` failing from 2 of 3 regions; ingest job failures ≥ 3 in 24 h; queue age > 48 h; budget at 80% forecast; App Insights daily cap reached.

## 9.3 Runbooks (kept in `ops/runbooks/`)

| Runbook | Summary |
|---------|---------|
| **Full re-index** | Run `index --full --target pages-v{date}` → verify doc count equals curated rows and golden-query counts match → flip `current.json` → pre-warm → delete the old index after 7 days |
| **Rollback index** | Point `current.json` back to the previous `index_version` (kept 7 days) |
| **Rollback app** | Container Apps: shift traffic to the previous revision |
| **Search engine down** | The API serves cached results. An uncached request returns 503 with a friendly message and the SPA shows a banner. Restart the revision; if the index is corrupt, roll back the index version |
| **Cost spike** | Check the Front Door request mix (bot?), tighten the rate-limit rule, lower max replicas, raise the edge TTL |
| **LoC source change** | If the discover job fails its schema check, pause ingestion (the site keeps serving), then update the parser |
| **Maintainer handover** | Transfer the subscription, repo, domain and DNS; rotate the OIDC federation; update the budget contacts |

## 9.4 Security

### 9.4.1 Threat model (STRIDE summary)

| Threat | Vector | Mitigation |
|--------|--------|-----------|
| **Denial of service / cost exhaustion** | Uncached, expensive queries in bulk (the realistic top risk for a public search site) | Edge cache; canonicalization; Front Door rate limits (per IP, per path); API token bucket; query complexity limits; backend concurrency semaphore; 10 s timeouts; budget alerts; max replica caps |
| **Injection** | Query-language injection into the engine (the legacy Solr risk) | AST-only translation; no raw syntax pass-through; fuzzing |
| **XSS** | Snippets containing OCR'd markup; title metadata | Server-side escaping; only `<mark>` allowed; strict **CSP** (`default-src 'self'`; `img-src` includes `tile.loc.gov`, `www.loc.gov`); React's escaping by default |
| **Supply chain** | Crates, npm packages, container images | `cargo-deny`, `cargo-audit`, Dependabot, CodeQL, pinned image digests, SBOM, signed images (Notation or cosign) |
| **Secret leakage** | Keys in the repo (happened in the legacy repo) | Managed identities; GitHub secret scanning + **push protection**; no keys needed at runtime |
| **Tampering with data** | Poisoned bulk files or mirrors | Checksum verification against LoC manifests; mirrors used only after verification; raw archives immutable (**immutability policy** on `raw/`) |
| **Elevation** | CI credentials | OIDC federated credentials scoped to the environment and branch; no long-lived secrets; least-privilege RBAC |

### 9.4.2 Privacy

- There are **no accounts, cookies or PII**. The site is anonymous by design.
- **IP addresses:** Front Door access logs are retained for 30 days for abuse handling only. App Insights IP collection is masked (the default). The API rate-limiter keys are **salted hashes** held only in memory.
- **Query text** is treated as potentially sensitive (genealogy searches contain family names). It is never logged per request; only daily aggregates with k ≥ 5 are kept.
- A privacy page states all of the above, and there is nothing to consent to because no cookies are set.
- This replaces the legacy practice of storing IPs, cookies and full headers for every search, indefinitely.

### 9.4.3 Content and licensing

- Chronicling America content is freely usable. Attribute LoC/NEH and the contributing awardee on result lists and in exports.
- OpenStreetMap data (basemap): the ODbL attribution is always visible.
- GNIS and TIGER are US public domain.
- Code license: **Apache-2.0** (or MIT) for the new repo, to encourage institutional adoption. The legacy repo's license needs confirming before any legacy code is copied, but the plan is a clean-room rewrite.

## 9.5 Cost model (monthly, USD, list prices; confirm with the Azure Pricing Calculator)

### Recommended architecture (Option B: Quickwit)

| Item | Assumption | Low | Typical | High |
|------|------------|-----|---------|------|
| Front Door Standard | Base fee + ~200 GB egress + ~5M requests | $35 | $60 | $120 |
| Static Web Apps | Free (Standard if needed: $9) | $0 | $0 | $9 |
| Container Apps: API | 1 × 0.5 vCPU / 1 GiB always on; mostly idle-rate | $10 | $25 | $60 |
| Container Apps: search | 1 × 4 vCPU / 8 GiB always on, mostly idle-rate; bursts to 2–4 | $90 | $180 | $350 |
| Container Apps: jobs | Weekly incremental ingest + stats | $5 | $15 | $40 |
| Blob: curated + index + reference (Hot, ~1.3 TB) + raw (Cold, ~1–2 TB) | Incl. transactions | $30 | $45 | $80 |
| Blob: tiles | ~20 GB | $1 | $1 | $2 |
| ACR Basic, Key Vault, DNS | | $6 | $7 | $8 |
| Log Analytics / App Insights | 2–5 GB/mo ingestion with a daily cap | $0 | $10 | $25 |
| **Total** | | **~$180** | **~$345** | **~$695** |

The High column is a heavy month (a press spike with poor cache hit ratios). Rate limits and max-replica caps keep it bounded, and the budget alert fires at $400 forecast.

**One-time backfill:** jobs compute of roughly $100–300 (dominated by LoC download time, not CPU), plus a full index build of about $20–50.

### Managed alternative (Option A: Azure AI Search)

| Item | Typical |
|------|---------|
| AI Search L1 × 1 partition × 1 replica (no SLA) | ~$2,800 |
| …or × 2 replicas (read SLA) | ~$5,600 |
| Everything else (as above, minus the search container) | ~$165 |
| **Total** | **~$2,965 – $5,765** |

### Cost controls

Budget alerts at $250/$400/$500 (actual and forecast); App Insights daily cap; max replicas per app; lifecycle tiering for `raw/`; `dev` scales to zero; the index is rebuilt only on demand; a tag policy (`project=usnm`, `env=`) enables cost-by-environment views.

## 9.6 Backup and disaster recovery

| Asset | Protection | RPO / RTO |
|-------|-----------|-----------|
| `raw/` | Immutable, versioned; ZRS; LoC is also a source of truth | RPO 0 / RTO n/a |
| `curated/`, `reference/` | ZRS + blob versioning + soft delete (14 d); rebuildable from `raw/` | RPO 0 / RTO ≤ 24 h (re-curate) |
| Search index | Disposable; rebuild from `curated/` | RTO ≤ 24 h full rebuild; the previous version is kept 7 days for instant rollback |
| Code, IaC, runbooks | GitHub | RPO 0 |
| Region outage | Not active/active (cost). Re-deploy to a paired region with `azd up` and **restore from GRS** if the sponsor opts in (+~$20/mo for GRS on curated/reference) | RTO 1–2 days (acceptable for this service class) |
