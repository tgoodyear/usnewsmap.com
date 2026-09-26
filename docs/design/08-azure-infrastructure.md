# 08: Azure Infrastructure & Delivery

## 8.1 Resource inventory (production, lean profile)

> **No IaaS.** Every resource below is PaaS or SaaS. The inventory has no Virtual Machines, VM Scale Sets, AKS clusters or always-on Batch pools, and none may be added ([ADR-0005](adr/0005-sustainability-constraints.md)). Offline backfill and re-index work runs on **ACI Spot containers** (preview). They are container groups, not VMs, and exist only while a job runs. The documented fallback is an Azure Batch Spot pool that scales to zero ([ADR-0006](adr/0006-lean-hosting-profile.md)).

One subscription (ideally owned by a sponsoring institution), one region: **East US 2**. ACI Spot containers are available there (preview regions: East US 2, West Europe, West US), as are the higher-capacity AI Search partitions for the growth profile. Resource group `rg-usnm-prod`:

| Resource | SKU / config | Purpose |
|----------|--------------|---------|
| **Static Web App** `swa-usnm` | **Free** (custom domains `usnewsmap.com` + `www`, free managed TLS, global static distribution, PR preview environments). Standard ($9) optional: linked Container App at `/api/*`, SLA | SPA hosting. **Replaces Front Door** for the site |
| **Container Apps environment** `cae-usnm` | Consumption only | Hosts the app and jobs; managed OTel agent → App Insights |
| Container App `ca-usnm` | **One app, two containers per replica:** `api` (Rust, 0.25 vCPU / 0.5 GiB) + `quickwit` (searcher + file-backed metastore, 1.0 vCPU / 2.0 GiB, listening on localhost only). Min 1, **max 2** replicas; HTTP scaler | Public API at **`api.usnewsmap.com`** (custom domain, free managed certificate). CORS allows `https://usnewsmap.com` for GET only |
| Container Apps Jobs `job-ingest-*` | Scheduled / event-driven (KEDA Azure Queue scaler); active rate, mostly within the free grant | Weekly incremental work: discover, batch (a few new batches), titles-sync, geocode, stats, incremental index, pre-warm |
| **ACI Spot container groups** `aci-usnm-backfill-{n}` (preview) | Created on demand by the `backfill` launcher; up to 4 vCPU / 16 GB each; no public IP (not needed) | Initial backfill and full re-index; deleted when the queue drains |
| **Storage account** `stusnmdata` | StorageV2, **HNS enabled (ADLS Gen2)**, **LRS**; `qw-index/`, `reference/` and `cache/` in **Hot**; `curated/` in **Cool**; blob versioning + soft delete (14 d) on `curated/` and `reference/`; **raw archives not retained** | Data lake, index splits, persistent response cache, queues |
| **Cosmos DB account** `cosmos-usnm` | NoSQL API, **free tier** (enable at creation; one per subscription), provisioned 1,000 RU/s shared database `usnm`, `disableLocalAuth: true`, continuous backup (7-day) | Document state ([ADR-0007](adr/0007-cosmos-document-state.md)) |
| **Storage account** `stusnmtiles` | StorageV2, LRS, one container with anonymous read (non-sensitive public map data), CORS for the site | PMTiles basemap, read with HTTP range requests (kept off SWA to stay under its 250 MB app-size and 100 GB bandwidth limits) |
| **Log Analytics** `log-usnm` + **Application Insights** `appi-usnm` | Pay-as-you-go with a **daily cap** (≈150 MB/day) so ingestion stays within the free 5 GB/month; 30-day retention | Telemetry |
| **Azure DNS** zone `usnewsmap.com` | – | Apex alias to SWA, `api` CNAME to the container app, validation records |
| **Action Group** + **Budget** | – | Alerts; cost budget at $40/$60/$75 |
| *(not used in lean)* Front Door, ACR, Key Vault, AI Search | – | Growth profile only; each is a Bicep parameter switch |

Container images are published to **GitHub Container Registry** as public images, so no registry resource or pull credential is needed. If images must be private, ACR Basic (~$5/mo) is the switch.

## 8.2 Identity and access (managed identities everywhere)

| Principal | Type | Role assignments (scope) |
|-----------|------|--------------------------|
| `id-usnm-app` (user-assigned, on `ca-usnm`) | MI | `Storage Blob Data Reader` (`reference/`); `Storage Blob Data Contributor` (`qw-index/`, `cache/`) |
| `id-usnm-ingest` (on jobs and ACI Spot groups) | MI | `Storage Blob Data Contributor` (`curated/`, `reference/`, `qw-index/`); `Storage Queue Data Contributor`; **Cosmos DB Built-in Data Contributor** (database `usnm`) |
| `id-usnm-launcher` (on the `backfill` launcher job) | MI | `Contributor` on `rg-usnm-prod-spot` only (to create and delete ACI container groups); `Managed Identity Operator` on `id-usnm-ingest` |
| GitHub Actions (`usnewsmap.com` repo, `main` branch + `prod` environment) | **OIDC federated credential** on an app registration | `Contributor` on `rg-usnm-*` + `User Access Administrator` constrained by a condition to the roles above (for Bicep role assignments) |
| Maintainers | Entra users / group `grp-usnm-maintainers` | `Reader` by default; **PIM just-in-time** `Contributor` where the tenant licensing allows it |

There are no storage account keys or SAS tokens in app config: `allowSharedKeyAccess=false` on the data account. Quickwit authenticates to Azure Blob using the storage credentials it supports. S-2 confirms that managed identity works with the pinned version. If it doesn't, the fallback is a short-lived **user-delegation SAS** minted by the API container at startup and passed to Quickwit over localhost, never an account key.

## 8.3 Networking

- **Public entry points:**
  - `usnewsmap.com` → Static Web Apps (static only);
  - `api.usnewsmap.com` → Container Apps ingress, which forwards to the `api` container.
  - Quickwit binds to `127.0.0.1` inside the replica and has no ingress.
- **CORS:** `Access-Control-Allow-Origin: https://usnewsmap.com` (plus SWA preview origins in dev), GET only, no credentials. Researchers call the API directly; CORS doesn't restrict them.
- **Abuse protection without a WAF:** the per-client token bucket in the API (salted-hash keys, in memory), request timeouts, a query-complexity budget, and `maxReplicas: 2`. The replica cap turns abuse into slower responses instead of a larger bill.
- **Cosmos DB** allows Entra ID RBAC only (no keys). Its public network access is restricted with the same IP allow-list approach while a backfill runs.
- **The data storage account** uses firewall allow-lists: the Container Apps environment's outbound IPs plus trusted Azure services. ACI Spot groups have no VNet in preview, so while a backfill runs the launcher adds the groups' outbound IPs to the allow-list and removes them afterwards. Public blob access is disabled (the tiles account is the only exception).
- **Hardening path (growth profile):** Front Door Standard/Premium in front of both origins (edge cache, WAF, Private Link), a VNet-integrated environment, and private endpoints.

## 8.4 Compute sizing notes

- **`api` container:** 0.25 vCPU / 0.5 GiB. Rust handles cached responses at hundreds of requests per second on this, so the searcher is the bottleneck.
- **`quickwit` container:** 1.0 vCPU / 2.0 GiB, with ephemeral storage for the split cache. Spike S-2 measures p95 on the benchmark set at exactly this size.
  - If high-frequency terms are unacceptably slow, the first lever is **2 vCPU / 4 GiB** (+~$15–30/mo, still under $80).
  - The second lever is scaling to a second replica, since Quickwit spreads a query's splits across searchers.
  - It stays on Container Apps in every scenario; there is no VM or AKS fallback.
- **Weekly jobs:** 1 vCPU / 2 GiB. An incremental week is a few batches, which fits within the free grant.
- **Backfill on ACI Spot:**
  - The `backfill` launcher (a tiny Container Apps Job, or `azd` task) enqueues every batch and creates **N = 8** Spot container groups (2 vCPU / 4 GB each; N is bounded by LoC download politeness, not CPU).
  - Each group pulls batch messages from the Storage Queue until the queue is empty, then exits (`restartPolicy: OnFailure`), and the launcher deletes the groups.
  - **Evictions are harmless.** ACI restarts an evicted group automatically. The in-flight queue message becomes visible again after its visibility timeout, and batch processing is idempotent (deterministic output paths plus manifest checks), so a retry overwrites the same result.
  - The per-group disk limit of **50 GB** is enough, because each batch archive is streamed and processed, then discarded.
- **Full index build:** one ACI Spot group (4 vCPU / 16 GB) runs the Quickwit indexer against the curated Parquet, **one curated partition at a time**, recording completed partitions in the Cosmos `index_runs` item.
  - After an eviction it resumes at the first unfinished partition.
  - Partial partitions are discarded because splits are published only when a partition completes. The exact mechanism (per-partition commit or file-source checkpoints) is validated in S-2.
- **Fallback:** if ACI Spot is unavailable or evictions stall progress, the same image runs on regular-priority ACI (about 3× the cost, still ~$100) or on an **Azure Batch Spot pool** that scales to zero (container tasks, one per batch). Using the Batch fallback needs a time-boxed exemption from the "Not allowed resource types" policy for the `rg-usnm-prod-spot` resource group.

## 8.5 Infrastructure as code

- **Bicep** modules under `infra/` with **Azure Developer CLI (`azd`)** for environment management and a one-command `azd up`.
- Modules: `staticwebapp`, `containerapps-env`, `containerapp`, `job`, `aci-spot` (backfill groups, deployed by the launcher), `storage`, `cosmos`, `dns`, `monitoring`, `budget`, `rbac`. Growth-profile modules behind parameters: `frontdoor`, `acr`, `keyvault`, `aisearch`.
- **Parameters per environment**: `dev` (scale to zero, LRS, small sample corpus of ~1M pages), `prod`.
- Lint with `bicep lint` plus PSRule for Azure in CI; `what-if` output posted to the PR for any change under `infra/`.
- Policy: deny public blob access, require HTTPS/TLS 1.2+, require diagnostic settings, allowed locations, and a built-in **"Not allowed resource types"** assignment that blocks `Microsoft.Compute/virtualMachines`, `virtualMachineScaleSets`, `Microsoft.ContainerService/managedClusters` and `Microsoft.Batch/batchAccounts` on the project resource groups, so VMs can't creep in.

## 8.6 CI/CD (GitHub Actions)

```mermaid
flowchart LR
  PR[Pull request] --> CI1[Rust: fmt · clippy · test · deny · audit · fuzz smoke]
  PR --> CI2[Web: typecheck · lint · vitest · playwright · axe]
  PR --> CI3[Infra: bicep lint · PSRule · what-if]
  PR --> CI4[Security: CodeQL · secret scanning push protection · Dependabot]
  PR --> PREV[SWA preview env + API against dev]
  M[merge to main] --> B[build & sign images · SBOM · push GHCR]
  B --> DEV[azd deploy dev]
  DEV --> SMK[smoke + k6 short load]
  SMK --> GATE{{manual approval: prod environment}}
  GATE --> PROD[azd deploy prod · new revision 10% → 100%]
  PROD --> POST[prewarm cache · synthetic checks]
```

- **Container Apps revisions** for blue/green: a new revision receives 10% of traffic until synthetic checks pass, then 100%. Rolling back means shifting traffic to the previous revision.
- **Images:** pinned base `gcr.io/distroless/cc-debian12` (or `scratch` with a musl static build) for the API; the Quickwit image is pinned by digest and mirrored into GHCR.
- **Data pipeline deploys** are separate from app deploys. A new `index_version` is published by the `index` job and picked up by the API's refresher, not by redeploying.

## 8.7 Environments

| Env | Corpus | Scale | Cost target |
|-----|--------|-------|-------------|
| `dev` | 1M-page sample (ten diverse years) | `ca-usnm` min 0 (accepts a cold start of several seconds); SWA preview environments; ingest on ACI Spot | ≤ $10/mo |
| `prod` | Full | As in §8.1 | < $80/mo (typical ~$50) |
| *Local* | 10k-page fixture | `docker compose` (Quickwit + API + Azurite) | $0 |

## 8.8 Domain and DNS

- Move the `usnewsmap.com` registration to an account the project controls, with auto-renew and at least 2 admins. This was one of the legacy single points of failure.
- Host DNS in **Azure DNS**: apex `usnewsmap.com` as an alias record to the Static Web App, `www` → apex redirect, and `api` as a CNAME to the container app with its domain-verification TXT record. Add `CAA` records for the CAs that the SWA and Container Apps managed certificates use.
- Keep `/loc_api/*` returning **410 Gone** (an SWA route rule) with a link to the new API docs (old clients may still call it), and redirect legacy query URLs where they can be mapped.
