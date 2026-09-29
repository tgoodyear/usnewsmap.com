# 08: Azure Infrastructure & Delivery

## 8.1 Resource inventory (production, lean profile)

> **No IaaS.** Every resource below is PaaS or SaaS. The inventory has no Virtual Machines, VM Scale Sets, AKS clusters or always-on Batch pools, and none may be added ([ADR-0005](adr/0005-sustainability-constraints.md)). Offline backfill and re-index work runs as **Container Apps Jobs inside the VNet** (§8.4), which exist only while a job runs. ACI Spot containers (preview) remain a documented alternative, with an Azure Batch Spot pool that scales to zero as its fallback ([ADR-0006](adr/0006-lean-hosting-profile.md)).

One subscription (ideally owned by a sponsoring institution), one region: **East US 2**. ACI Spot containers are available there (preview regions: East US 2, West Europe, West US), as are the higher-capacity AI Search partitions for the growth profile. Resource group `rg-usnm-prod`:

| Resource | SKU / config | Purpose |
|----------|--------------|---------|
| **Virtual network** `vnet-usnm` | `10.40.0.0/24`: `snet-cae` `/27` (delegated to `Microsoft.App/environments`) and `snet-pe` `/28` (private endpoints). No charge for the VNet itself | Private path from the app and jobs to storage and Cosmos ([ADR-0008](adr/0008-private-networking.md)) |
| **Private endpoints** `pe-usnm-blob`, `pe-usnm-cosmos` | In `snet-pe`: Blob on `stusnmdata`; `Sql` on `cosmos-usnm`. Private DNS zones `privatelink.blob.core.windows.net` and `privatelink.documents.azure.com`, linked to the VNet | App → storage and jobs → storage/Cosmos traffic stays on the Microsoft backbone at private IPs |
| **Container Apps environment** `cae-usnm` | **VNet-integrated** (`snet-cae`, external ingress for the public API), set at creation because VNet integration can't be added later. **Workload-profiles environment using only the Consumption profile.** No Dedicated profile, so no management fee. This matters because a *Consumption-only* environment caps a replica at 2 vCPU / 4 GiB, while the Consumption profile in a workload-profiles environment allows up to **4 vCPU / 8 GiB** per replica, which leaves room for the sizing levers in §8.4 | Hosts the app and jobs. Its managed OpenTelemetry agent is not used: it can't authenticate with Entra, and Application Insights accepts nothing else (§8.1.2) |
| Container App `ca-usnm` | **One app, two containers per replica:** `api` (Rust, 0.25 vCPU / 0.5 GiB) + `quickwit` (**searcher only**, opening the file-backed metastore read-only in polling mode, 1.0 vCPU / 2.0 GiB, listening on localhost only). Min 1, **max 2** replicas; HTTP scaler. See §8.4.1 for the single-writer rule | Public API at **`api.usnewsmap.com`** (custom domain, free managed certificate). CORS allows `https://usnewsmap.com` for GET only |
| Container Apps Jobs `job-ingest-*` | Scheduled (cron) with a fixed parallelism; workers claim batches from Cosmos with leases (no Storage Queue); active rate, mostly within the free grant; run inside the VNet | Weekly incremental work: discover, batch (a few new batches), titles-sync, geocode, stats, incremental index, pre-warm |
| **Container Apps Jobs** `caj-usnm-ingest-{env}`, `caj-usnm-backfill-{env}` | In `cae-usnm`, so they reach the data over the private endpoints. `ingest`: `usnm-ingest run`, 4 vCPU / 8 GiB (the first prod release ran out of memory at 4 GiB with Quickwit's default 2 GiB indexing heap and ingest queue; both are now capped at 1 GiB), weekly or manual, runs the Quickwit writer. `backfill`: manual, N parallel `usnm-ingest curate` replicas at 1 vCPU / 2 GiB | Weekly increments, the initial backfill and full rebuilds (§8.4). Deployed with `ingestJobs` (`USNM_INGEST_JOBS=true`) |
| **Storage account** `stusnmdata` | StorageV2, **flat namespace (HNS off)** so that **blob versioning** and soft delete are available (neither is supported on HNS/ADLS Gen2 accounts); **LRS**; `qw-index/`, `reference/` and `cache/` in **Hot**; `curated/` in **Cool**; versioning + blob soft delete (14 d) on `curated/` and `reference/`; **raw archives not retained**. Parquet readers (DataFusion/`object_store`) and Quickwit use the Blob API and don't need HNS | Data lake, index splits, persistent response cache. **`publicNetworkAccess: Disabled`** in steady state (private endpoint only) |
| **Cosmos DB account** `cosmos-usnm` | NoSQL API, **free tier** (enable at creation; one per subscription), provisioned 1,000 RU/s shared database `usnm`, `disableLocalAuth: true`, **`publicNetworkAccess: Disabled`** in steady state, continuous backup (7-day) | Document state and the ingest work queue ([ADR-0007](adr/0007-cosmos-document-state.md)) |
| **Storage account** `stusnmtiles` | StorageV2, LRS, one container with anonymous read (non-sensitive public map data), CORS for the site | PMTiles basemap, read with HTTP range requests (kept out of the API image; range requests go straight to Blob) |
| **Log Analytics** `log-usnm` + **Application Insights** `appi-usnm` | Pay-as-you-go with a **daily cap** (≈150 MB/day) so ingestion stays within the free 5 GB/month; 30-day retention; local (key) auth disabled on both (ADR-0009) | Telemetry |
| **Azure DNS** zone `usnewsmap.com` | – | Apex A record to the environment's static IP; `www` and `api` CNAMEs to the container app; `asuid` validation records |
| **Action Group** + **Budget** | – | Alerts; cost budget at $40/$60/$75 |
| Container Registry `crusnm{env}{suffix}` | **Basic** (~$5/mo); no admin user, no anonymous pull | Private images: the API, ingest and a digest-pinned copy of Quickwit. Pulled with the app and ingest identities (`AcrPull`); pushed only by CI on `main` (`AcrPush`, §8.2) |
| *(not used in lean)* Front Door, Key Vault, AI Search | – | Growth profile only; each is a Bicep parameter switch |

Container images are **private**, in ACR Basic. CI on `main` signs in to Azure with OIDC as `id-usnm-ci-{env}` (a user-assigned identity whose federated credential trusts only this repository's GitHub Environment `{env}`, which only `main` may use; no stored secret) and pushes the API and ingest images tagged `main` and the commit sha. It also copies Quickwit v0.9.1 in by digest, so deployments don't depend on Docker Hub. The container app and jobs pull with their managed identities. A new environment has no API until CI has pushed; `scripts/bootstrap.sh` sequences that (§8.9).

### 8.1.1 Resource logs and maintenance windows

**Every resource with resource logs sends them to `log-usnm`** through a diagnostic setting, all declared in one module, [`infra/modules/diagnostics.bicep`](../../infra/modules/diagnostics.bicep). An audit policy (§8.5) flags any resource of those types that lacks one.

| Resource | Categories |
|---|---|
| Log Analytics, Container Registry, VNet; storage queue, table and file services | All (`allLogs`) |
| Container Apps environment | `ContainerAppConsoleLogs`, `ContainerAppSystemLogs`. The environment's `appLogsConfiguration` is `azure-monitor`, so app and job logs arrive through this setting rather than the workspace's shared key |
| Cosmos DB | `ControlPlaneRequests`, `PartitionKeyStatistics` |
| Blob service (data and tiles accounts) | `StorageWrite`, `StorageDelete` |

- **Per-request categories are left out** because they would consume the ≈150 MB/day cap: blob reads (public tile fetches, and Quickwit's split reads on every search), Cosmos `DataPlaneRequests` and the per-query statistics (every ingest write), and Container Apps HTTP logs (every API request with its query string, which holds the search text; the API reports its requests itself, without paths or query strings, §8.1.2). Writes, deletes and control-plane changes are the audit trail.
- **Also left out:**
  - Application Insights is workspace-based, so its telemetry is already in `log-usnm`. Its resource logs would store every row twice.
  - Metrics-only resources (DNS zones, private endpoints, Container Apps and jobs) have no logs. Azure Monitor keeps their platform metrics for 93 days at no cost.
- **Retention** is the workspace's 30 days. Tables can go as low as 4 days, but 31 days are included in the ingestion price, so a 14-day limit would save nothing.
- **Bicep writes the settings, not a `deployIfNotExists` policy.** Every resource here is created by the template, and nothing is created at runtime. The template can choose categories per resource, while the built-in policy initiatives enable whole category groups (`allLogs`/`audit`), which include the per-request logs above. Remediation would also need a policy identity with role-assignment rights, and it lags creation by minutes. The policy is therefore audit-only: it catches drift without writing anything.

### 8.1.2 Logs, telemetry and alerts: ingest jobs and API

The ingest and backfill jobs are short-lived and leave nothing on disk, so everything needed to diagnose a run goes out while it runs.

- **Console logs (Log Analytics).** `usnm-ingest` writes JSON lines to stdout, which arrive in `ContainerAppConsoleLogs`. The Quickwit writer's own output is read by the pipeline: warnings, errors and its readiness lines are forwarded as `target: quickwit` lines, and its last 50 lines go into the error if it fails to start. While a release builds an index it logs `release progress` every 30 s and after each batch: documents sent and expected, MB sent, documents per second, 429/503 retries, free disk of the work directory, and the memory of the pipeline and of the Quickwit writer. Each curated batch logs pages, parts, archive size and seconds taken. Every failed command ends with one `command failed` line. Saved queries are in `ops/queries/`; `scripts/logs.sh <env> <query> [timespan]` runs one with the operator's Entra sign-in.
- **Traces and metrics (Application Insights).** The jobs get `APPLICATIONINSIGHTS_CONNECTION_STRING` and export OpenTelemetry spans (`release`, one `curate` per batch) and metrics (`ingest.docs_sent`, `ingest.bytes_sent`, `ingest.retries`, `curate.batches`, `curate.pages`, `curate.duration_seconds`, `release.docs_expected`, `work_disk_free_bytes`) directly, as `id-usnm-ingest` with an Entra token for `https://monitor.azure.com/` (Monitoring Metrics Publisher on the component). The connection string only names the ingestion endpoint; with local auth disabled it authenticates nothing. The exporters are flushed before the process exits.
- **Alerts** (`infra/modules/ingest-alerts.bicep`, log search alerts on the workspace, severity 2, email through the action group; deployed with the jobs when alert emails are set):
  - *Ingest or backfill job failed*: a `command failed` line, or a platform event for a replica that was killed or exited non-zero, in the last 15 minutes (checked every 5 minutes).
  - *Release stalled*: a release logged `building index` in the last day (the job's replica timeout), hasn't finished, and has logged no `release progress` for 10 minutes (every 15 minutes).
  - *Backfill stalled*: backfill workers logged both at the start and at the end of the last hour but none curated a batch in it, and LoC isn't rate limiting (every 15 minutes). Workers are silent while curating, so a silent hang is caught only when the 24 h replica timeout fails the job.

The API (`usnm-api`) uses the same setup, shared in `crates/usnm-telemetry`, under the cloud role `usnm-api`:

- **Requests.** Every request except the `/healthz` and `/readyz` probes becomes one Application Insights request (`AppRequests`): name `<method> <route template>` (for example `GET /v1/aggregate`; `(site)` for the web app's files, `(no route)` for an unknown API path), result code, duration, and success, which is false only for a 5xx. `usnm.index_version` is the version the response came from (for search responses) or the one serving when it finished. The same request writes one console line (`message: request`, with `method`, `route`, `status`, `ms`), so Log Analytics has it without Application Insights. Neither records the path, the query string or the client address (09 §9.4.2). Warnings and errors logged while a request is handled also go to `AppTraces`, linked to the request.
- **Metrics** (`AppMetrics`): `api.requests` (by `route` and `status_class`, probes included), `api.request_duration_seconds` (by `route`), `api.backend_duration_seconds` (search backend time for aggregate and hits, by `endpoint` and `outcome`), `api.cache_lookups` (by `layer`: `memory` or `blob`, and `result`: `hit` or `miss`), `api.rejected_queries` (by `reason`: `syntax`, `bad_parameter`, `unsupported`, `too_broad`, `rate_limited`), `api.reference_reloads` (by `outcome`: `published` or `failed`), `api.prewarm_duration_seconds` (one cache warm-up run, by `trigger`: `startup` or `publish`), `api.prewarm_queries` (by `outcome`: `ok`, `timeout`, `error` or `skipped` when the budget ran out) and `api.index_version` (1, with the serving version in `index_version`). Warm-up queries are not counted in the request, backend or cache-lookup metrics.
- **Cache warm-up** (06 §6.5). Each run logs one `warm-up finished` line with `trigger`, `version`, `queries`, `ok`, `timed_out`, `failed`, `skipped` and `ms`. A warm-up query that takes 1 s or more logs `slow warm-up query` (endpoint, example id, outcome, `ms`) and a failed one logs `warm-up query failed`; neither includes the query string. A start that reaches the readiness cap logs `warm-up still running at the readiness cap; reporting ready`.
- **Identity.** The app gets `APPLICATIONINSIGHTS_CONNECTION_STRING` and exports as `id-usnm-app` (selected by `AZURE_CLIENT_ID`), which has Monitoring Metrics Publisher on the component.
- **Queries:** `scripts/logs.sh <env> api-requests` (requests, 4xx, 5xx, 5xx rate and p50/p95 by route) and `api-errors` (5xx by route and status, and the API's warning and error lines).
- **Alerts** (`infra/modules/api-alerts.bicep`, deployed with the API when alert emails are set):
  - *API server errors* (severity 2): at least 5 responses with a 5xx in 10 minutes that are also more than 2% of requests (checked every 5 minutes).
  - *Slow aggregate searches* (severity 3): p95 of `/v1/aggregate` over 3 s in 15 minutes, with at least 20 requests and at least 3 of them over 3 s. A cold search takes 2–7 s and a cached one about 0.15 s, so one or two cold searches don't trigger it; a busy period right after a new index version (when every search is cold) can.
  - *Site or API unavailable* (severity 1): a standard availability test `GET https://{domain}/` (the site and the API are one app; `/readyz` is already probed by Container Apps), every 15 minutes (`USNM_AVAILABILITY_FREQUENCY`: 300, 600 or 900 s) from East US, North Central US and West US, expecting 200 and a certificate valid for 7 more days, with retries. The alert fires when 2 of the 3 locations fail. Each test exists only once its hostname's certificate is bound. The results are written by the availability service, not sent by a client, so the component's `DisableLocalAuth` doesn't apply to them; Microsoft's list of features that don't work with Entra-only ingestion doesn't include availability tests. The first deployment should confirm rows in `AppAvailabilityResults`. Each run from one location costs $0.0005, so both tests at 5 minutes cost about $26 a month (09 §9.5).

**Workbooks** (`infra/modules/workbooks.bicep`, definitions in `infra/workbooks/`). Two Azure Monitor workbooks over the same workspace, listed under `appi-usnm-{env}` → Workbooks (and in the resource group as Azure Workbook resources). They have no charge of their own; opening one runs its queries. Times are UTC.

- *usnewsmap pipeline* (deployed with the jobs; default range 48 h). Tiles: batches curated, curation failures, LoC rate-limit pauses, download retries, Quickwit pushbacks and releases in the range, and the last published version (pages, full or delta) next to the version the API reports serving. Charts: batches per hour (curated, failed, throttled); Quickwit pushbacks per 10 minutes by status; curation time and archive size per batch, from the `curated` line's `secs` and `archive_mb`. Tables: the releases of the last 30 days from `building index` to `released` (type, batches, documents, pages, minutes, and those that failed or stopped without a result); warnings and errors by batch; job failures (`command failed` lines, and non-zero exits, backoff and deadline events from `ContainerAppSystemLogs`). A release picker (latest selected) charts one release's `release progress` lines: documents sent and expected, documents per second with the 429 and 503 retries so far, free disk of the work directory, and the memory of the pipeline and the Quickwit writer.
- *usnewsmap API* (deployed with the API; default range 24 h). Tiles: requests, 5xx rate, p95 latency of the API routes (the site's files and unknown paths left out). Charts and tables: requests by route and by status class; p50, p95 and max by route, and p95 by API route over time; search backend time by endpoint and outcome; response cache hit rate by layer; rejected queries by reason; availability test success by test and location; cache warm-up runs; reference-data loads (new versions published, failed reloads, failed initial loads). Backend time is shown as mean and max: `AppMetrics` keeps a histogram's sum, count, min and max per export interval, not its buckets, so it has no percentiles.

**Maintenance windows: none apply to this stack.**

- Customer-scheduled maintenance (Maintenance Configurations) covers VMs and dedicated hosts, Azure SQL, and some network gateways. None of Cosmos DB, Storage, Container Registry, Static Web Apps, Log Analytics or DNS offers one.
- Container Apps has *planned maintenance* for an environment, but only for apps on Dedicated workload profiles. This environment is Consumption-only. The feature is also billed on the Dedicated Plan Management meter (about $0.10/hour, ≈ $73/month), which alone is most of the $80 budget.
- If the environment ever moves to a Dedicated profile, add `Microsoft.App/managedEnvironments/maintenanceConfigurations` (`default`), with `startHourUtc: 7` and `durationHours: 8` on the chosen weekday. The start hour is UTC only: 7 is 3 am Eastern in summer (EDT) and 2 am in winter (EST).

## 8.2 Identity and access (managed identities everywhere)

| Principal | Type | Role assignments (scope) |
|-----------|------|--------------------------|
| `id-usnm-app` (user-assigned, on `ca-usnm`) | MI | `Storage Blob Data Reader` (`reference/`, `qw-index/`); `Storage Blob Data Contributor` (`cache/`); **Cosmos DB Built-in Data Reader** (containers `batches`, `index_runs` and `ops` only, for the status page); `Monitoring Metrics Publisher` (`appi-usnm`, to send the API's requests, traces and metrics). Serving replicas **can't** write the index or the pipeline state |
| `ca-usnm` system-assigned identity (used only by the `quickwit` sidecar) | MI | `Storage Blob Data Reader` (`qw-index/`) and nothing else |
| `id-usnm-ingest` (on the ingest jobs) | MI | `Storage Blob Data Contributor` (`curated/`, `reference/`, `qw-index/`); **Cosmos DB Built-in Data Contributor** (database `usnm`); `Monitoring Metrics Publisher` (`appi-usnm`, to send the jobs' traces and metrics) |
| `caj-usnm-ingest` system-assigned identity (used only by the Quickwit writer the job runs) | MI | `Storage Blob Data Contributor` (`qw-index/`) and nothing else |
| `id-usnm-launcher` (on the `backfill` launcher job and the `network-guard` job) | MI | `Contributor` on `rg-usnm-prod-spot` only (to create and delete ACI container groups); `Managed Identity Operator` on `id-usnm-ingest`; custom role **`USNM Public Access Toggle`** on `stusnmdata` and `cosmos-usnm` only (`read` and `write` on the account resource, used solely to flip `publicNetworkAccess`). Azure Policy **denies** any change to `allowSharedKeyAccess` or `disableLocalAuth`, so this role can't re-enable keys |
| `id-usnm-ci-{env}` (user-assigned, federated credential for the GitHub Environment `{env}`, restricted to `main`) | MI | `AcrPush` on its registry, and a custom role, "usnm deployer", on its resource group (`infra/modules/deployer.bicep`) that can only roll Container Apps onto new images (the API image also carries the site). Azure rejects role assignments scoped to a Container App itself, and `az containerapp update` also needs join rights on the app's environment, hence the group scope; the group holds only this environment. It also needs assign rights on the app's identity, granted as Managed Identity Operator on `id-usnm-app-{env}` alone, so CI can't attach the ingest identity (and its data access) to the app |
| Infrastructure changes (the deployment stack, via `scripts/bootstrap.sh` or `scripts/provision.sh`) | The operator's own sign-in | `Owner` on the subscription (role assignments, policy definitions). CI never provisions, so no identity with role-assignment rights exists outside a person's session |
| Maintainers | Entra users / group `grp-usnm-maintainers` | `Reader` by default; **PIM just-in-time** `Contributor` where the tenant licensing allows it. Data-plane inspection (Cosmos queries, blob listing) runs through the `usnm-ingest admin …` CLI as an on-demand Container Apps Job inside the VNet, because the portal's Data Explorer can't reach the private endpoints from the internet |

**No shared keys anywhere ([ADR-0009](adr/0009-entra-identity-only.md)).** Every caller is an Entra identity authorized by RBAC. Local (key) auth is off on every service that has the switch, and policy denies turning it back on: storage (`allowSharedKeyAccess: false`, both accounts), Cosmos (`disableLocalAuth`), Log Analytics (`features.disableLocalAuth`), Application Insights (`DisableLocalAuth`) and the registry (no admin user, no anonymous pull). The Container Apps environment sends logs through a diagnostic setting, not the workspace key. `scripts/ci/no-shared-keys.sh` fails CI on any key pattern. The one open exception is the Static Web App's deployment token (ADR-0009).

**How the Quickwit sidecar authenticates (S-2).** From 0.9.0, when no access key is configured, Quickwit authenticates to Blob with the Azure SDK's default credential chain (`azure_identity` 0.21). In Container Apps that chain reaches the managed identity endpoint through its App Service credential. That credential asks for the **system-assigned** identity and ignores `AZURE_CLIENT_ID`, so it can't select `id-usnm-app`. The app therefore also has a system-assigned identity, used only by the sidecar and granted `Storage Blob Data Reader` on `qw-index/` alone. The `api` container keeps using `id-usnm-app`. This follows from the 0.9.1 source; it's confirmed against a real account on the first `quickwit` deployment (below). If it fails, the fallback is a localhost proxy in the `api` container that adds `id-usnm-app`'s bearer token to Quickwit's Blob requests. SAS of any kind, user-delegation included, is ruled out by ADR-0009.

## 8.3 Networking

- **Public entry points:**
  - `usnewsmap.com`, `www.usnewsmap.com` and `api.usnewsmap.com` → Container Apps ingress, which forwards to the `api` container. It serves `/v1` and the built site ([ADR-0010](adr/0010-site-served-by-the-api.md)).
  - Quickwit binds to `127.0.0.1` inside the replica and has no ingress.
- **CORS:** the site calls the API on its own origin, so it needs none. The API still allows the domain, `www`, the app's own hostname and any `allowedOrigins`, GET only, no credentials. Researchers call the API directly; CORS doesn't restrict them.
- **Abuse protection without a WAF:** the per-client token bucket in the API (salted-hash keys, in memory), request timeouts, a query-complexity budget, and `maxReplicas: 2`. The replica cap turns abuse into slower responses instead of a larger bill.
- **Every app/job → data path uses both a managed identity and a private endpoint** ([ADR-0008](adr/0008-private-networking.md)):

  | Caller | Target | Identity | Network path |
  |--------|--------|----------|--------------|
  | `api` container | Blob (`reference/`, `cache/`) | `id-usnm-app` | Private endpoint `pe-usnm-blob` |
  | `api` container (status page, read-only) | Cosmos | `id-usnm-app` | Private endpoint `pe-usnm-cosmos`, resolved through the same `privatelink.documents.azure.com` zone the ingest jobs use (same environment and subnet) |
  | `quickwit` sidecar | Blob (`qw-index/`, read-only) | `ca-usnm` system-assigned identity (§8.2) | Private endpoint `pe-usnm-blob` |
  | Weekly jobs, `index` job, admin CLI | Blob + Cosmos | `id-usnm-ingest` | Private endpoints `pe-usnm-blob`, `pe-usnm-cosmos` |
  | Backfill job (`caj-usnm-backfill`) | Blob + Cosmos | `id-usnm-ingest` | Private endpoints `pe-usnm-blob`, `pe-usnm-cosmos` |
| ACI Spot backfill groups (alternative, not built) | Blob + Cosmos | `id-usnm-ingest` | **Public endpoint, only during a guarded backfill window** (below) |
  | Browser | Tiles account (`stusnmtiles`) | Anonymous (public map data) | Public by design; no private data |
  | Browser | API | None (anonymous public read API) | Public ingress `api.usnewsmap.com` |

- **Authentication is identity-only everywhere:** `allowSharedKeyAccess=false` (no account keys, no service or account SAS) on `stusnmdata`, and `disableLocalAuth=true` (no Cosmos keys). Azure Policy **denies** turning either back on. Access is least-privilege RBAC on managed identities, and diagnostic settings record data-plane access.
- **Steady state is private:** `publicNetworkAccess: Disabled` on `stusnmdata` and `cosmos-usnm`. The only way in is through `pe-usnm-blob` and `pe-usnm-cosmos` from the VNet, resolved by the linked private DNS zones.
- **Guarded public-access window for Spot backfills (only if the ACI Spot alternative is used).** The default backfill runs as Container Apps Jobs inside the VNet and needs no window. ACI Spot groups can't join a VNet and have no stable egress IP, so for the duration of a Spot backfill or full re-index only:
  1. The launcher records an `ops/public-access-window` item in Cosmos with the reason, an owner and an **expiry** (default 7 days).
  2. It sets `publicNetworkAccess: Enabled` on both accounts, with no IP rules (the Spot IPs are unknowable). Access still requires Entra tokens, and keys stay disabled by policy.
  3. When no unclaimed batches remain and the index run is complete, it sets `publicNetworkAccess: Disabled` again and closes the window item.
  4. A scheduled **`network-guard`** job runs hourly inside the VNet. It disables public access on any account that has it enabled without an open, unexpired window, so a crashed launcher can't leave the data services exposed.
  5. An Azure Monitor alert fires whenever `publicNetworkAccess` changes, and the change is recorded in the Activity Log.
- **Not private (accepted):**
  - the tiles account, which holds public map data only;
  - telemetry ingestion to Azure Monitor (an Azure Monitor Private Link Scope is part of the growth profile);
  - image pulls from the registry's public endpoint, which requires an Entra ID token (a private endpoint needs ACR Premium, ~$50/mo); images are code, not data;
  - LoC downloads.
- Anonymous public blob access is disabled on the data account (the tiles account is the only exception).
- **Hardening path (growth profile):** Front Door in front of both origins (edge cache, WAF); and an Azure Monitor Private Link Scope for telemetry. (The backfill already runs inside the VNet as Container Apps Jobs, §8.4, so the public-access window exists only if the Spot alternative is used.) With **Front Door Standard**, the API app must keep **external** ingress, because internal ingress is reachable only inside the environment. Lock it to Front Door by validating the `X-Azure-FDID` header in the API, plus IP allow-list rules generated from the published `AzureFrontDoor.Backend` service-tag CIDRs and re-synced by a scheduled job (Container Apps IP restrictions take CIDRs, not service-tag names). **Front Door Premium** can instead reach an internal environment over Private Link.

## 8.4 Compute sizing notes

- **`api` container:** 0.25 vCPU / 0.5 GiB. Rust handles cached responses at hundreds of requests per second on this, so the searcher is the bottleneck.
- **`quickwit` container:** 1.0 vCPU / 2.0 GiB, with ephemeral storage for the split cache. Spike S-2 measures p95 on the benchmark set at exactly this size.
  - If high-frequency terms are unacceptably slow, the first lever is **2 vCPU / 4 GiB** for Quickwit (replica total 2.25 vCPU / 4.5 GiB, which is valid in the workload-profiles Consumption profile; +~$15–30/mo, still under $80).
  - A second replica adds **concurrency, not per-query speed**: each API talks only to the Quickwit sidecar in its own replica over localhost, so one query's splits aren't spread across replicas. Per-query latency levers are sidecar size (up to 3.75 vCPU / 7.5 GiB alongside the 0.25/0.5 API within the 4/8 replica limit) and the cache.
  - It stays on Container Apps in every scenario; there is no VM or AKS fallback.
- **Weekly jobs:** 1 vCPU / 2 GiB. An incremental week is a few batches, which fits within the free grant.
- **Backfill on Container Apps Jobs (implemented).** Spike S-1 measured the whole corpus at ~175 worker-hours of curation (bzip2-bound, 04 §4.1.1). That is small enough to run inside the VNet on the Consumption profile: `caj-usnm-backfill` starts **N = 4** replicas of `usnm-ingest curate` (1 vCPU / 2 GiB each), which claim batches from Cosmos until none remain. LoC's bulk limit (10 requests per 10 minutes per IP), not CPU, sets the pace: downloads are spaced 75 s apart across all workers (04 §4.4), so the corpus takes about 2.5 days, and each 24 h execution is started again until the queue is empty. At list prices that is ~$20–30 of vCPU and memory time, before the monthly free grant; workers are billed while they wait for a slot. It needs no launcher, no public-access window and no `network-guard`. Workers stream each archive and parse it as it downloads, verifying the sha256 before committing, so they need no scratch disk. A replica that dies leaves its batch leased; another replica or the next run claims it when the lease expires. `caj-usnm-ingest` then runs `usnm-ingest run`, which finds nothing left to curate and builds the first base index.
- **Alternative: backfill on ACI Spot** (not built):
  - The `backfill` launcher (a tiny Container Apps Job inside the VNet) marks every batch `queued` in Cosmos, opens the public-access window (§8.3), and creates **N = 8** Spot container groups (2 vCPU / 4 GB each; N is bounded by LoC download politeness, not CPU).
  - Each group repeatedly claims the next `queued` batch from Cosmos with a conditional lease patch until none remain, then exits (`restartPolicy: OnFailure`). The launcher then deletes the groups and closes the public-access window.
  - **Evictions are harmless.** Microsoft's ACI Spot documentation states that evicted container groups "are automatically restarted without requiring any action from the customer." As insurance against preview behavior changes, the launcher also runs a **reconcile loop** every 5 minutes while a backfill is active: any group that is missing, stopped or failed is redeployed, including the singleton full-index group, until no unclaimed batches remain and the index run is complete. A batch claimed by an evicted worker becomes claimable again when its lease expires (default 30 minutes). Batch processing is idempotent (new attempt path plus a Cosmos commit), so a retry never corrupts committed data.
  - The per-group disk limit of **50 GB** is enough, because each batch archive is streamed and processed, then discarded.
- **Full index build:** one ACI Spot group (4 vCPU / 16 GB) runs the Quickwit indexer against the curated Parquet, **one curated partition at a time**, recording completed partitions in the Cosmos `index_runs` item.
  - After an eviction it resumes at the first unfinished partition.
  - Partial partitions are discarded because splits are published only when a partition completes. The exact mechanism (per-partition commit or file-source checkpoints) is validated in S-2.
- **Fallback:** if ACI Spot is unavailable or evictions stall progress, the same image runs on regular-priority ACI (about 3× the cost, still ~$100) or on an **Azure Batch Spot pool** that scales to zero (container tasks, one per batch). Using the Batch fallback needs a time-boxed exemption from the "Not allowed resource types" policy for the `rg-usnm-prod-spot` resource group.

### 8.4.1 Quickwit metastore: one writer, many readers

Quickwit's **file-backed metastore** (a JSON file per index on Blob) doesn't support concurrent writers. Scaling a node that *writes* it would risk corrupting metadata. The design therefore separates readers from the writer:

| Process | Quickwit roles | Metastore access | Count |
|---------|----------------|------------------|-------|
| `quickwit` sidecar in each `ca-usnm` replica | `searcher` (plus the metastore service pointed at the file-backed URI with `#polling_interval=30s`) | **Read-only.** It picks up newly published splits by polling. No indexer or janitor roles run here, and the identity has only Blob *Reader* on `qw-index/` | 1–2 |
| `index` job (weekly incremental: Container Apps Job) | `indexer` + `janitor`, embedded in the job container | **Sole writer.** It only ever writes a **new delta index** (`pages-delta-{date}-{n}`), never an index that a published version already lists | ≤ 1, enforced by a Cosmos `ops/quickwit-writer` lease (ETag compare-and-swap, 2-hour TTL, renewed while running) |
| Compaction / full rebuild (`caj-usnm-ingest` with `--full`) | `indexer` + `janitor` | Sole writer of a **new base index** (`pages-base-{date}`) | 1 |

**Index versions are immutable sets of sealed indexes.** An `index_version` names an explicit list of Quickwit indexes in `current.json`: one base plus zero or more deltas, e.g. `["pages-base-20261001", "pages-delta-20261008-1", "pages-delta-20261015-1"]`.

- **Sealing:** once an index appears in a published version, it's sealed. No job writes to it again; replacement or deletion only happens by building a new base.
- **Explicit index lists:** the API passes exactly the listed index ids to Quickwit (multi-index search with the same doc mapping). No query touches a new delta until a new `current.json` lists it. A given `v` therefore always returns identical results, and rolling back to the previous `current.json` restores the exact previous set.
- **Warming new indexes (S-2):** a read-only searcher reads the metastore's index list only at startup. Polling then picks up new splits of the indexes it knows, but a search naming an index created later fails with "could not find indexes". Looking the index up by id (`GET /api/v1/indexes/{id}`) loads it from Blob and adds it to the polled set. The API therefore looks up every index a new `current.json` lists before it swaps in that version (`SearchBackend::prepare`). If any lookup fails, it keeps serving the previous version and retries on the next poll. No sidecar restart is needed.
- **Weekly increments** add new pages as a new delta, then publish a new version listing base + all deltas.
- **Replacements** (new batch versions, OCR reprocessing) and **title-metadata changes** can't be applied to sealed indexes. They're recorded as pending in Cosmos and applied by the next **compaction**: a full rebuild into a new base by the ingest job (a few dollars), which also clears the deltas. Compaction runs monthly, or sooner once there are 8 deltas or when pending replacements are flagged urgent. Until then the previous text and place assignments keep serving, consistently with their reference snapshot.
- **Garbage collection:** the janitor deletes an index only after no version within the 7-day rollback window lists it.
- **Azure AI Search (growth profile)** can't query several indexes in one request. That backend publishes only on compaction (a new index per version), and weekly increments wait for it.

S-2 checked this on Quickwit 0.9.1 with a file-backed metastore: a searcher-only node (`enabled_services: [searcher, metastore]`, `#polling_interval`) serves the writer's indexes, never writes the metastore, and serves indexes created after it started once they are warmed as above. CI repeats this on every change (the `quickwit` job). **Fallback:** a PostgreSQL metastore (Azure Database for PostgreSQL Flexible Server, Burstable B1ms, ~$13–15/month). It supports any number of Quickwit nodes, but puts a typical month at ~$86, over $80.

## 8.5 Infrastructure as code

- **Bicep** modules under `infra/`, deployed as **one deployment stack per environment** (`usnm-<env>`, subscription scope; [ADR-0011](adr/0011-deployment-stacks.md)). A resource removed from the template is deleted on the next deployment (`actionOnUnmanage: deleteResources`), and a deny assignment blocks deletes of any managed resource outside the stack (`denyDelete`). Settings live in `.azure/<env>/.env` and reach the template through `infra/main.bicepparam`. `scripts/bootstrap.sh` stands an environment up, `scripts/provision.sh` redeploys it, and `scripts/teardown.sh` removes it. The first slice is implemented in [`infra/`](../../infra/README.md). It covers the network, private endpoints, the data, tiles and Cosmos accounts, identities and RBAC, the Container Apps environment and the API app (which serves the site), monitoring, the budget and the guardrail policies. The Quickwit sidecar is in place behind `searchBackend` (`USNM_SEARCH_BACKEND=quickwit`) and waits on the first published index. The ingest and backfill jobs are in place behind `ingestJobs` (`USNM_INGEST_JOBS=true`). DNS follows.
- Modules: `network` (VNet, subnets, private endpoints, private DNS zones), `containerapps-env`, `containerapp`, `job`, `aci-spot` (backfill groups, deployed by the launcher), `storage`, `cosmos`, `dns`, `monitoring`, `budget`, `rbac`. Growth-profile modules behind parameters: `frontdoor`, `acr`, `keyvault`, `aisearch`.
- **Parameters per environment**: `dev` (scale to zero, LRS, small sample corpus of ~1M pages), `prod`.
- Lint with `bicep lint` plus PSRule for Azure in CI. No `what-if` preview: deployment stacks don't support it yet ([ADR-0011](adr/0011-deployment-stacks.md)).
- Policy: deny public blob access, require HTTPS/TLS 1.2+, require diagnostic settings (**as built:** `usnm-audit-diagnostic-settings`, `auditIfNotExists` on every resource type with resource logs, §8.1.1), allowed locations, and a built-in **"Not allowed resource types"** assignment that blocks `Microsoft.Compute/virtualMachines`, `virtualMachineScaleSets`, `Microsoft.ContainerService/managedClusters` and `Microsoft.Batch/batchAccounts` on the project resource groups, so VMs can't creep in. Also: **deny** `allowSharedKeyAccess != false` on storage, **deny** `disableLocalAuth != true` on Cosmos, and **audit** `publicNetworkAccess != Disabled` (the guard job remediates it outside an open window).

## 8.6 CI/CD (GitHub Actions)

```mermaid
flowchart LR
  PR[Pull request] --> CI1[Rust: fmt · clippy · test · deny · audit · fuzz smoke]
  PR --> CI2[Web: typecheck · lint · vitest · playwright · axe]
  PR --> CI3[Infra: bicep lint · PSRule]
  PR --> CI4[Security: CodeQL · secret scanning push protection · Dependabot]
  PR --> PREV[preview revision of the app against dev]
  M[merge to main] --> B[build & sign images · SBOM · push ACR (OIDC)]
  B --> DEV[roll dev onto the images]
  DEV --> SMK[smoke + k6 short load]
  SMK --> GATE{{manual approval: prod environment}}
  GATE --> PROD[roll prod · new revision 10% → 100%]
  PROD --> POST[prewarm cache · synthetic checks]
```

- **As built:** `ci` runs every check on each PR and push. On `main`, after all of them pass, `publish` runs once per environment listed in `USNM_DEPLOY_ENVIRONMENTS`: it pushes the images to that environment's registry and rolls its API app, which carries the site, onto the commit. Each environment is a GitHub Environment restricted to `main`, with its own variables and OIDC trust (§8.9). Only the commit `main` currently points at deploys, so a slow run for an older commit can't overwrite a newer one. The diagram above is the fuller target (preview environments, staged traffic, approvals).
- **Container Apps revisions** for blue/green: a new revision receives 10% of traffic until synthetic checks pass, then 100%. Rolling back means shifting traffic to the previous revision.
- **Images:** pinned base `gcr.io/distroless/cc-debian12` (or `scratch` with a musl static build) for the API; the Quickwit image is pinned by digest and copied into ACR with that digest unchanged (CI checks it).
- **Data pipeline deploys** are separate from app deploys. A new `index_version` is published by the `index` job and picked up by the API's refresher, not by redeploying.

## 8.7 Environments

| Env | Corpus | Scale | Cost target |
|-----|--------|-------|-------------|
| `dev` | 1M-page sample (ten diverse years) | `ca-usnm` min 0 (accepts a cold start of several seconds); ingest on ACI Spot | ≤ $10/mo |
| `prod` | Full | As in §8.1 | < $80/mo (typical ~$72) |
| *Local* | 10k-page fixture | `docker compose` (Quickwit + API + Azurite) | $0 |

## 8.8 Domain and DNS

- Move the `usnewsmap.com` registration to an account the project controls, with auto-renew and at least 2 admins. This was one of the legacy single points of failure.
- Host DNS in **Azure DNS**. All three names point at the one container app, which serves the site and the API ([ADR-0010](adr/0010-site-served-by-the-api.md)):
  - the apex `usnewsmap.com` is an A record to the environment's static IP (an apex can't be a CNAME);
  - `www` and `api` are CNAMEs to the app;
  - each name has an `asuid` TXT record, which Container Apps checks before binding it;
  - `CAA` records allow only DigiCert, the managed certificates' CA.

  **As built:** the zone is `infra/modules/dns.bicep`. Once the delegation is live, `scripts/bootstrap.sh` binds each name to the app with a free managed certificate: the apex by HTTP validation, `www` and `api` by CNAME. It records each certificate so Bicep keeps the binding (§8.9).
- The legacy `/loc_api` endpoints aren't kept: the old site has been offline for years.

## 8.9 Portability: redeploying into another subscription or tenant

The whole system can be stood up from this repository in **any** Azure subscription, in any tenant, with one command, and the corpus rebuilt from public sources. That is a design requirement: no single account, subscription or person is load-bearing (the legacy site's failure mode).

**Command:** `scripts/bootstrap.sh <env> [--subscription ID] [--location REGION] [--domain NAME] [--alert-email ADDR] [--ingest]`. It is idempotent: re-running it brings an existing environment up to date. It:

1. checks the sign-ins (`az`, `gh`) and registers the resource providers the templates use;
2. writes the environment's settings (`.azure/<env>/.env`), including:
   - the GitHub repository (and its immutable-ID OIDC form, looked up from the API);
   - whether the subscription's one Cosmos DB free-tier slot is still free;
3. deploys the stack `usnm-<env>` (everything but the API, which has no image yet);
4. creates the **GitHub Environment** of the same name, restricted to the `main` branch, writes that environment's variables, and adds it to the repository variable `USNM_DEPLOY_ENVIRONMENTS`;
5. runs `ci` on `main` (every time, so the registry holds the current `main`), which publishes the images, and waits for that run;
6. switches to the registry (`USNM_USE_ACR=true`) and deploys the stack again, which deploys the API;
7. once the registrar delegates the domain to the zone, binds the apex, `www` and `api` to the app with managed certificates (re-run it after delegating), and removes the Static Web App that used to serve the site;
8. checks the API's `/readyz` and the site.

Then, for a full corpus, start the backfill job and let the weekly ingest job publish (04 §4.4). A **new environment starts with no data**: the system of record is LoC's public data, and everything in Blob and Cosmos is derived from it by a deterministic pipeline. Rebuilding takes ~4.5 h of title sync (LoC's 20-requests/minute API limit) and ~2.5 days of curation (LoC's bulk limit of 10 downloads per 10 minutes per IP; 4 paced workers, the backfill job restarted each 24 h), for about $20–30. Hand-curated state belongs in git, never only in an environment: the place-coordinate overrides (`catalog/overrides/places.json`) ship in the ingest image.

**What is per environment, and what is shared:**

| Thing | Scope | Notes |
|-------|-------|-------|
| The deployment stack `usnm-{env}`: resource groups `rg-usnm-{env}` and everything in them (registry, identities, storage, Cosmos, VNet, apps, jobs, DNS zone) | Per environment | Globally unique names carry a suffix derived from the subscription and environment, so the same environment name works in another subscription |
| CI identity `id-usnm-ci-{env}` | Per environment | Trusts only jobs in the GitHub Environment `{env}` of the configured repository, which only `main` may use. Rights: AcrPush on its registry, and the "usnm deployer" custom role on its resource group (roll out API images, which also carry the site), and Managed Identity Operator on the API's identity only. No secret is stored or read anywhere |
| GitHub Environment `{env}` | Per environment | Its variables are written by bootstrap. `publish` (in `ci`) runs once per listed environment |
| Guard-rail policy definitions (`usnm-deny-*`, `usnm-audit-*`) | Per subscription | Their own deployment stack, `usnm-guardrails`, shared by every environment in the subscription; the assignments are per resource group, in each environment's stack. Needs Owner (or Resource Policy Contributor) on the subscription |
| Cosmos DB free tier | One account per subscription | Bootstrap turns it off for a second environment in the same subscription (~$1–3/month serverless instead) |
| The domain | One environment | Only the environment whose zone the registrar delegates to serves `usnewsmap.com`; others use their app's `*.azurecontainerapps.io` name |

**Prerequisites in the target:** an account with **Owner** on the subscription (role assignments, policy definitions). No tenant-level objects are created (no app registrations; all identities are managed identities), so no Entra admin is needed. On GitHub: admin on the repository (environments and variables). A **fork** works the same way: bootstrap reads the fork's name and ids, and the federated credentials trust that repository only.

**Moving production** to another subscription (or tenant):
1. Run `scripts/bootstrap.sh <env> --subscription NEW --domain usnewsmap.com --ingest`. For an environment whose local settings already name another subscription, add `--move`. Without it, bootstrap refuses to rebind an environment to a different subscription. With it, bootstrap resets the settings that describe the old subscription: the registry switch-over, the domain certificates, and the Cosmos DB free-tier check. Use a new environment name if the old one should keep deploying from CI meanwhile: the GitHub Environment `<env>` points at whichever subscription bootstrap wired last.
2. Let the new environment rebuild the corpus, and check it on its app's `*.azurecontainerapps.io` name.
3. Delegate the domain to the new zone's name servers (`scripts/settings.sh <env> NAME_SERVERS`), then **re-run bootstrap**. Once it sees the delegation, it binds the apex, `www` and `api` to the new app and issues their certificates. HTTPS on the custom names needs this step.
4. The old environment keeps serving until the delegation changes; afterwards, delete it with `scripts/teardown.sh <env> --subscription OLD`. That removes only its Azure resources; the GitHub Environment and the settings belong to the new copy.

