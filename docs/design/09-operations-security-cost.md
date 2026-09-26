# 09: Operations, Security & Cost

## 9.1 Service level objectives

| SLI | SLO (30-day) | Measured by |
|-----|--------------|-------------|
| Availability of `/v1/aggregate` and `/v1/hits` (non-5xx, excluding 503 query-too-broad) | **99.0%** (lean) / 99.5% (growth) | API logs + availability tests |
| Latency of `/v1/aggregate`, uncached, typical query set | p95 ≤ **2 s** (high-frequency set ≤ 15 s on lean) | API server spans |
| Latency of `/v1/aggregate`, cached (moka or Blob) | p95 ≤ **150 ms** | API server spans |
| Data freshness (LoC batch published → searchable) | ≤ **7 days** for 95% of batches | Manifests vs discover log |
| SPA LCP | p75 ≤ **2.5 s** | App Insights browser telemetry |

The error budget for 99.0% is about 7.3 hours per month. When it is exhausted, feature deploys pause in favor of reliability work.

## 9.2 Observability

- **Traces:** OpenTelemetry from the SPA (App Insights JS) → API → search backend. The trace id is propagated with `traceparent`. Spans: `parse`, `cache`, `backend.aggregate`, `postprocess`, `encode`.
- **Metrics:** request rate, error rate and latency per endpoint; cache hit ratio (moka and Blob); backend latency; searcher CPU/memory/split-cache hits; ingest pages/s; queue depth; batches pending; index doc count by version.
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
| **Cost spike** | Check the request mix in App Insights (bot?); tighten the API token bucket; confirm `maxReplicas: 2`; confirm the Log Analytics daily cap; check for orphaned ACI Spot groups left by a failed backfill |
| **LoC source change** | If the discover job fails its schema check, pause ingestion (the site keeps serving), then update the parser |
| **Maintainer handover** | Transfer the subscription, repo, domain and DNS; rotate the OIDC federation; update the budget contacts |

## 9.4 Security

### 9.4.1 Threat model (STRIDE summary)

| Threat | Vector | Mitigation |
|--------|--------|-----------|
| **Denial of service / cost exhaustion** | Uncached, expensive queries in bulk (the realistic top risk for a public search site) | In-process + Blob caches; canonicalization; API token bucket (per client); query complexity limits; backend concurrency semaphore; 10 s timeouts; budget alerts; max replica caps |
| **Injection** | Query-language injection into the engine (the legacy Solr risk) | AST-only translation; no raw syntax pass-through; fuzzing |
| **XSS** | Snippets containing OCR'd markup; title metadata | Server-side escaping; only `<mark>` allowed; strict **CSP** (`default-src 'self'`; `img-src` includes `tile.loc.gov`, `www.loc.gov`); React's escaping by default |
| **Supply chain** | Crates, npm packages, container images | `cargo-deny`, `cargo-audit`, Dependabot, CodeQL, pinned image digests, SBOM, signed images (Notation or cosign) |
| **Secret leakage** | Keys in the repo (happened in the legacy repo) | Managed identities; GitHub secret scanning + **push protection**; no keys needed at runtime |
| **Tampering with data** | Poisoned bulk files or mirrors | Checksum verification against LoC manifests; mirrors used only after verification; source checksums recorded in the manifests; blob versioning on `curated/` |
| **Elevation** | CI credentials | OIDC federated credentials scoped to the environment and branch; no long-lived secrets; least-privilege RBAC |

### 9.4.2 Privacy

- There are **no accounts, cookies or PII**. The site is anonymous by design.
- **IP addresses:** Container Apps system logs (which may include client IPs) are retained for 30 days for abuse handling only. App Insights IP collection is masked (the default). The API rate-limiter keys are **salted hashes** held only in memory.
- **Query text** is treated as potentially sensitive (genealogy searches contain family names). It is never logged per request; only daily aggregates with k ≥ 5 are kept.
- A privacy page states all of the above, and there is nothing to consent to because no cookies are set.
- This replaces the legacy practice of storing IPs, cookies and full headers for every search, indefinitely.

### 9.4.3 Content and licensing

- Chronicling America content is freely usable. Attribute LoC/NEH and the contributing awardee on result lists and in exports.
- OpenStreetMap data (basemap): the ODbL attribution is always visible.
- GNIS and TIGER are US public domain.
- Code license: **Apache-2.0** (or MIT) for the new repo, to encourage institutional adoption. The legacy repo's license needs confirming before any legacy code is copied, but the plan is a clean-room rewrite.

## 9.5 Cost model (monthly, USD, list prices; confirm with the Azure Pricing Calculator)

**Rate assumptions (East US 2, pay-as-you-go).**
- **Container Apps Consumption:** active use is $0.000024 per vCPU-second and $0.000003 per GiB-second. The idle rate applies when a replica is warm but not processing requests. Published sources give **$0.000003–0.000008 per vCPU-second and $0.000001–0.000003 per GiB-second** for idle; the table uses the low end for "Low" and the high end for "High". The free grant each month is 180,000 vCPU-seconds, 360,000 GiB-seconds and 2M requests per subscription; it applies to active usage.
- **Blob Hot LRS:** about $0.02 per GB-month. **Cool LRS:** about $0.01 per GB-month.
- **Egress:** the first 100 GB per month is free.
- **Log Analytics:** the first 5 GB per month is free.

### Lean profile (default, [ADR-0006](adr/0006-lean-hosting-profile.md)): target under $80

| Item | Assumption | Low | Typical | High |
|------|------------|-----|---------|------|
| Static Web Apps | **Free** plan (Standard $9 if same-origin API or an SLA is wanted) | $0 | $0 | $0 |
| Container App `ca-usnm` (API + Quickwit sidecar) | 1 replica × 1.25 vCPU / 2.5 GiB, always warm, mostly idle rate. Active search time mostly within the free grant. A spike month scales to a second replica for some hours | $16 | $30 | $50 |
| Blob: search index (Hot) | 0.5–0.9 TB | $10 | $14 | $18 |
| Blob: curated Parquet (Cool) | 150–300 GB; read only during rebuilds | $2 | $2 | $3 |
| Blob: reference, response cache, tiles (Hot) + transactions | ~20–40 GB | $1 | $2 | $4 |
| Container Apps Jobs (weekly incremental ingest, stats, pre-warm) | Mostly within the free grant | $0 | $1 | $3 |
| Egress | SPA via SWA; tiles and API responses via Azure egress (first 100 GB free) | $0 | $0 | $5 |
| Log Analytics / App Insights | Daily cap keeps it within 5 GB/month free | $0 | $0 | $3 |
| Cosmos DB (document state) | Free tier: 1,000 RU/s + 25 GB (serverless ~$1–3 if the free tier is taken) | $0 | $0 | $3 |
| Azure DNS zone | 1 zone + queries | $1 | $1 | $1 |
| Container registry | GitHub Container Registry (public images) | $0 | $0 | $0 |
| **Total** | | **~$30** | **~$50** | **~$90** |

"High" is a press-spike month billed at the upper idle rates. The hard cap is `maxReplicas: 2`: even if both replicas ran at the **active** rate all month (a sustained attack, not realistic traffic), compute would be about $200. Budget alerts at $40, $60 and $75 (forecast) trigger the cost-spike runbook well before that.

**The first cost lever if search is too slow:** raise Quickwit to 2 vCPU / 4 GiB. That adds about $15–30 per month and still fits under $80 in a typical month.

### One-time backfill (ACI Spot containers, preview)

| Stage | Assumption | Estimate |
|-------|------------|----------|
| Curation workers | 8 container groups × 2 vCPU / 4 GB × ~96 h (bounded by LoC download politeness); ACI Spot at about 70% off regular ACI | ~$20–25 |
| Full index build | 1 group × 4 vCPU / 16 GB × ~24 h | ~$2–5 |
| Storage transactions + curated writes | | ~$5 |
| **Total** | | **~$25–50** |

If Spot capacity is unavailable, running the same workload on regular-priority ACI costs about $80–150, and on Azure Batch Spot about $25–50. A full re-index later (no download) costs about $5 on Spot.

### Growth profile (when a sponsor funds it)

This is the original design: Front Door Standard (edge cache and rate limiting), separate API and search apps with a 4 vCPU / 8 GiB searcher, ZRS storage, ACR and raw archive retention. It costs about **$180 (low), $345 (typical) and $695 (high)** per month. Each piece is a Bicep parameter switch, not a redesign.

### Managed-search alternative (Azure AI Search)

L1 × 1 partition costs about **$2,800 per month** (about $5,600 with 2 replicas for an SLA). The preview **Serverless Developer** tier caps an index at 1 GB, so it can't hold the corpus. AI Search is outside the lean budget; it stays documented for a well-funded sponsor.

### Cost controls

- Budget alerts at $40/$60/$75 (actual and forecast)
- `maxReplicas: 2` on the only always-on app
- Log Analytics daily cap
- Curated data kept in Cool storage; raw archives not retained
- `dev` scales to zero
- Spot compute only for offline jobs
- A tag policy (`project=usnm`, `env=`) enables cost-by-environment views

## 9.6 Backup and disaster recovery

| Asset | Protection | RPO / RTO |
|-------|-----------|-----------|
| Raw LoC archives | Not retained (lean profile); LoC is the source of record; checksums in Cosmos batch state | Re-download (days) |
| Cosmos DB document state | Continuous backup (7-day point-in-time restore); rebuildable from Parquet + the LoC batch list | RPO minutes / RTO hours |
| `curated/`, `reference/` | LRS + blob versioning + soft delete (14 d); ZRS in the growth profile; rebuildable by re-downloading from LoC on ACI Spot (~$25–50) | RPO 0 / RTO ≤ 24 h from versions, ~1 week from LoC |
| Search index | Disposable; rebuild from `curated/` | RTO ≤ 24 h full rebuild; the previous version is kept 7 days for instant rollback |
| Code, IaC, runbooks | GitHub | RPO 0 |
| Region outage | Not active/active (cost). Everything is PaaS, so there are no VM images or disks to replicate. Re-deploy to a paired region with `azd up` and **restore from GRS** if the sponsor opts in (+~$20/mo for GRS on curated/reference) | RTO 1–2 days (acceptable for this service class) |
