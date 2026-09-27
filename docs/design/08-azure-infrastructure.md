# 08: Azure Infrastructure & Delivery

## 8.1 Resource inventory (production, lean profile)

> **No IaaS.** Every resource below is PaaS or SaaS. The inventory has no Virtual Machines, VM Scale Sets, AKS clusters or always-on Batch pools, and none may be added ([ADR-0005](adr/0005-sustainability-constraints.md)). Offline backfill and re-index work runs as **Container Apps Jobs inside the VNet** (§8.4), which exist only while a job runs. ACI Spot containers (preview) remain a documented alternative, with an Azure Batch Spot pool that scales to zero as its fallback ([ADR-0006](adr/0006-lean-hosting-profile.md)).

One subscription (ideally owned by a sponsoring institution), one region: **East US 2**. ACI Spot containers are available there (preview regions: East US 2, West Europe, West US), as are the higher-capacity AI Search partitions for the growth profile. Resource group `rg-usnm-prod`:

| Resource | SKU / config | Purpose |
|----------|--------------|---------|
| **Static Web App** `swa-usnm` | **Free** (custom domains `usnewsmap.com` + `www`, free managed TLS, global static distribution, PR preview environments). Standard ($9) optional: linked Container App at `/api/*`, SLA | SPA hosting. **Replaces Front Door** for the site |
| **Virtual network** `vnet-usnm` | `10.40.0.0/24`: `snet-cae` `/27` (delegated to `Microsoft.App/environments`) and `snet-pe` `/28` (private endpoints). No charge for the VNet itself | Private path from the app and jobs to storage and Cosmos ([ADR-0008](adr/0008-private-networking.md)) |
| **Private endpoints** `pe-usnm-blob`, `pe-usnm-cosmos` | In `snet-pe`: Blob on `stusnmdata`; `Sql` on `cosmos-usnm`. Private DNS zones `privatelink.blob.core.windows.net` and `privatelink.documents.azure.com`, linked to the VNet | App → storage and jobs → storage/Cosmos traffic stays on the Microsoft backbone at private IPs |
| **Container Apps environment** `cae-usnm` | **VNet-integrated** (`snet-cae`, external ingress for the public API), set at creation because VNet integration can't be added later. **Workload-profiles environment using only the Consumption profile.** No Dedicated profile, so no management fee. This matters because a *Consumption-only* environment caps a replica at 2 vCPU / 4 GiB, while the Consumption profile in a workload-profiles environment allows up to **4 vCPU / 8 GiB** per replica, which leaves room for the sizing levers in §8.4 | Hosts the app and jobs; managed OTel agent → App Insights |
| Container App `ca-usnm` | **One app, two containers per replica:** `api` (Rust, 0.25 vCPU / 0.5 GiB) + `quickwit` (**searcher only**, opening the file-backed metastore read-only in polling mode, 1.0 vCPU / 2.0 GiB, listening on localhost only). Min 1, **max 2** replicas; HTTP scaler. See §8.4.1 for the single-writer rule | Public API at **`api.usnewsmap.com`** (custom domain, free managed certificate). CORS allows `https://usnewsmap.com` for GET only |
| Container Apps Jobs `job-ingest-*` | Scheduled (cron) with a fixed parallelism; workers claim batches from Cosmos with leases (no Storage Queue); active rate, mostly within the free grant; run inside the VNet | Weekly incremental work: discover, batch (a few new batches), titles-sync, geocode, stats, incremental index, pre-warm |
| **Container Apps Jobs** `caj-usnm-ingest-{env}`, `caj-usnm-backfill-{env}` | In `cae-usnm`, so they reach the data over the private endpoints. `ingest`: `usnm-ingest run`, 2 vCPU / 4 GiB, weekly or manual, runs the Quickwit writer. `backfill`: manual, N parallel `usnm-ingest curate` replicas at 1 vCPU / 2 GiB | Weekly increments, the initial backfill and full rebuilds (§8.4). Deployed with `ingestJobs` (`USNM_INGEST_JOBS=true`) |
| **Storage account** `stusnmdata` | StorageV2, **flat namespace (HNS off)** so that **blob versioning** and soft delete are available (neither is supported on HNS/ADLS Gen2 accounts); **LRS**; `qw-index/`, `reference/` and `cache/` in **Hot**; `curated/` in **Cool**; versioning + blob soft delete (14 d) on `curated/` and `reference/`; **raw archives not retained**. Parquet readers (DataFusion/`object_store`) and Quickwit use the Blob API and don't need HNS | Data lake, index splits, persistent response cache. **`publicNetworkAccess: Disabled`** in steady state (private endpoint only) |
| **Cosmos DB account** `cosmos-usnm` | NoSQL API, **free tier** (enable at creation; one per subscription), provisioned 1,000 RU/s shared database `usnm`, `disableLocalAuth: true`, **`publicNetworkAccess: Disabled`** in steady state, continuous backup (7-day) | Document state and the ingest work queue ([ADR-0007](adr/0007-cosmos-document-state.md)) |
| **Storage account** `stusnmtiles` | StorageV2, LRS, one container with anonymous read (non-sensitive public map data), CORS for the site | PMTiles basemap, read with HTTP range requests (kept off SWA to stay under its 250 MB app-size and 100 GB bandwidth limits) |
| **Log Analytics** `log-usnm` + **Application Insights** `appi-usnm` | Pay-as-you-go with a **daily cap** (≈150 MB/day) so ingestion stays within the free 5 GB/month; 30-day retention | Telemetry |
| **Azure DNS** zone `usnewsmap.com` | – | Apex alias to SWA, `api` CNAME to the container app, validation records |
| **Action Group** + **Budget** | – | Alerts; cost budget at $40/$60/$75 |
| Container Registry `crusnm{env}{suffix}` | **Basic** (~$5/mo); no admin user, no anonymous pull | Private images: the API, ingest and a digest-pinned copy of Quickwit. Pulled with the app and ingest identities (`AcrPull`); pushed only by CI on `main` (`AcrPush`, §8.2) |
| *(not used in lean)* Front Door, Key Vault, AI Search | – | Growth profile only; each is a Bicep parameter switch |

Container images are **private**, in ACR Basic. CI on `main` signs in to Azure with OIDC as `id-usnm-ci` (a user-assigned identity with a federated credential for this repository's `main` branch; no stored secret) and pushes the API and ingest images tagged `main` and the commit sha. It also copies Quickwit v0.9.1 in by digest, so deployments don't depend on Docker Hub. The container app and jobs pull with their managed identities. A new environment bootstraps from a placeholder until CI has pushed (`useAcr`, infra/README.md).

## 8.2 Identity and access (managed identities everywhere)

| Principal | Type | Role assignments (scope) |
|-----------|------|--------------------------|
| `id-usnm-app` (user-assigned, on `ca-usnm`) | MI | `Storage Blob Data Reader` (`reference/`, `qw-index/`); `Storage Blob Data Contributor` (`cache/`). Serving replicas **can't** write the index |
| `ca-usnm` system-assigned identity (used only by the `quickwit` sidecar) | MI | `Storage Blob Data Reader` (`qw-index/`) and nothing else |
| `id-usnm-ingest` (on the ingest jobs) | MI | `Storage Blob Data Contributor` (`curated/`, `reference/`, `qw-index/`); **Cosmos DB Built-in Data Contributor** (database `usnm`) |
| `caj-usnm-ingest` system-assigned identity (used only by the Quickwit writer the job runs) | MI | `Storage Blob Data Contributor` (`qw-index/`) and nothing else |
| `id-usnm-launcher` (on the `backfill` launcher job and the `network-guard` job) | MI | `Contributor` on `rg-usnm-prod-spot` only (to create and delete ACI container groups); `Managed Identity Operator` on `id-usnm-ingest`; custom role **`USNM Public Access Toggle`** on `stusnmdata` and `cosmos-usnm` only (`read` and `write` on the account resource, used solely to flip `publicNetworkAccess`). Azure Policy **denies** any change to `allowSharedKeyAccess` or `disableLocalAuth`, so this role can't re-enable keys |
| `id-usnm-ci` (user-assigned, federated credential for GitHub Actions on `main`) | MI | `AcrPush` on the registry only |
| GitHub Actions (`usnewsmap.com` repo, `main` branch + `prod` environment) | **OIDC federated credential** on an app registration | `Contributor` on `rg-usnm-*` + `User Access Administrator` constrained by a condition to the roles above (for Bicep role assignments) |
| Maintainers | Entra users / group `grp-usnm-maintainers` | `Reader` by default; **PIM just-in-time** `Contributor` where the tenant licensing allows it. Data-plane inspection (Cosmos queries, blob listing) runs through the `usnm-ingest admin …` CLI as an on-demand Container Apps Job inside the VNet, because the portal's Data Explorer can't reach the private endpoints from the internet |

There are no storage account keys or SAS tokens in app config: `allowSharedKeyAccess=false` on the data account.

**How the Quickwit sidecar authenticates (S-2).** From 0.9.0, when no access key is configured, Quickwit authenticates to Blob with the Azure SDK's default credential chain (`azure_identity` 0.21). In Container Apps that chain reaches the managed identity endpoint through its App Service credential. That credential asks for the **system-assigned** identity and ignores `AZURE_CLIENT_ID`, so it can't select `id-usnm-app`. The app therefore also has a system-assigned identity, used only by the sidecar and granted `Storage Blob Data Reader` on `qw-index/` alone. The `api` container keeps using `id-usnm-app`. This follows from the 0.9.1 source; it's confirmed against a real account on the first `quickwit` deployment (below). If it fails, the fallback is unchanged: a short-lived **user-delegation SAS** minted by the `api` container and passed to Quickwit over localhost, never an account key.

## 8.3 Networking

- **Public entry points:**
  - `usnewsmap.com` → Static Web Apps (static only);
  - `api.usnewsmap.com` → Container Apps ingress, which forwards to the `api` container.
  - Quickwit binds to `127.0.0.1` inside the replica and has no ingress.
- **CORS:** `Access-Control-Allow-Origin: https://usnewsmap.com` (plus SWA preview origins in dev), GET only, no credentials. Researchers call the API directly; CORS doesn't restrict them.
- **Abuse protection without a WAF:** the per-client token bucket in the API (salted-hash keys, in memory), request timeouts, a query-complexity budget, and `maxReplicas: 2`. The replica cap turns abuse into slower responses instead of a larger bill.
- **Every app/job → data path uses both a managed identity and a private endpoint** ([ADR-0008](adr/0008-private-networking.md)):

  | Caller | Target | Identity | Network path |
  |--------|--------|----------|--------------|
  | `api` container | Blob (`reference/`, `cache/`) | `id-usnm-app` | Private endpoint `pe-usnm-blob` |
  | `quickwit` sidecar | Blob (`qw-index/`, read-only) | `ca-usnm` system-assigned identity (§8.2) | Private endpoint `pe-usnm-blob` |
  | Weekly jobs, `index` job, admin CLI | Blob + Cosmos | `id-usnm-ingest` | Private endpoints `pe-usnm-blob`, `pe-usnm-cosmos` |
  | ACI Spot backfill groups | Blob + Cosmos | `id-usnm-ingest` | **Public endpoint, only during a guarded backfill window** (below) |
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
- **Hardening path (growth profile):** Front Door in front of both origins (edge cache, WAF); an Azure Monitor Private Link Scope for telemetry; and moving the backfill onto VNet-integrated compute (Container Apps Jobs, ~$150–250 per backfill instead of ~$25–50 on Spot), which removes the public-access window entirely. With **Front Door Standard**, the API app must keep **external** ingress, because internal ingress is reachable only inside the environment. Lock it to Front Door by validating the `X-Azure-FDID` header in the API, plus IP allow-list rules generated from the published `AzureFrontDoor.Backend` service-tag CIDRs and re-synced by a scheduled job (Container Apps IP restrictions take CIDRs, not service-tag names). **Front Door Premium** can instead reach an internal environment over Private Link.

## 8.4 Compute sizing notes

- **`api` container:** 0.25 vCPU / 0.5 GiB. Rust handles cached responses at hundreds of requests per second on this, so the searcher is the bottleneck.
- **`quickwit` container:** 1.0 vCPU / 2.0 GiB, with ephemeral storage for the split cache. Spike S-2 measures p95 on the benchmark set at exactly this size.
  - If high-frequency terms are unacceptably slow, the first lever is **2 vCPU / 4 GiB** for Quickwit (replica total 2.25 vCPU / 4.5 GiB, which is valid in the workload-profiles Consumption profile; +~$15–30/mo, still under $80).
  - A second replica adds **concurrency, not per-query speed**: each API talks only to the Quickwit sidecar in its own replica over localhost, so one query's splits aren't spread across replicas. Per-query latency levers are sidecar size (up to 3.75 vCPU / 7.5 GiB alongside the 0.25/0.5 API within the 4/8 replica limit) and the cache.
  - It stays on Container Apps in every scenario; there is no VM or AKS fallback.
- **Weekly jobs:** 1 vCPU / 2 GiB. An incremental week is a few batches, which fits within the free grant.
- **Backfill on Container Apps Jobs (implemented).** Spike S-1 measured the whole corpus at ~175 worker-hours of curation (bzip2-bound, 04 §4.1.1). That is small enough to run inside the VNet on the Consumption profile: `caj-usnm-backfill` starts **N = 8** replicas of `usnm-ingest curate` (1 vCPU / 2 GiB each), which claim batches from Cosmos until none remain, in about 22 hours. At list prices that is ~$15–20 of vCPU and memory time, before the monthly free grant, about the same as Spot. It needs no launcher, no public-access window and no `network-guard`. Workers stream each archive and parse it as it downloads, verifying the sha256 before committing, so they need no scratch disk. A replica that dies leaves its batch leased; another replica or the next run claims it when the lease expires. `caj-usnm-ingest` then runs `usnm-ingest run`, which finds nothing left to curate and builds the first base index.
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

S-2 checked this on Quickwit 0.9.1 with a file-backed metastore: a searcher-only node (`enabled_services: [searcher, metastore]`, `#polling_interval`) serves the writer's indexes, never writes the metastore, and serves indexes created after it started once they are warmed as above. CI repeats this on every change (the `quickwit` job). **Fallback:** a PostgreSQL metastore (Azure Database for PostgreSQL Flexible Server, Burstable B1ms, ~$13–15/month). It supports any number of Quickwit nodes and still fits under $80 (typical ~$65).

## 8.5 Infrastructure as code

- **Bicep** modules under `infra/` with **Azure Developer CLI (`azd`)** for environment management and a one-command `azd up`. The first slice is implemented in [`infra/`](../../infra/README.md). It covers the network, private endpoints, the data, tiles and Cosmos accounts, identities and RBAC, the Container Apps environment and API app, SWA, monitoring, the budget and the guardrail policies. The Quickwit sidecar is in place behind `searchBackend` (`USNM_SEARCH_BACKEND=quickwit`) and waits on the first published index. The ingest and backfill jobs are in place behind `ingestJobs` (`USNM_INGEST_JOBS=true`). DNS follows.
- Modules: `network` (VNet, subnets, private endpoints, private DNS zones), `staticwebapp`, `containerapps-env`, `containerapp`, `job`, `aci-spot` (backfill groups, deployed by the launcher), `storage`, `cosmos`, `dns`, `monitoring`, `budget`, `rbac`. Growth-profile modules behind parameters: `frontdoor`, `acr`, `keyvault`, `aisearch`.
- **Parameters per environment**: `dev` (scale to zero, LRS, small sample corpus of ~1M pages), `prod`.
- Lint with `bicep lint` plus PSRule for Azure in CI; `what-if` output posted to the PR for any change under `infra/`.
- Policy: deny public blob access, require HTTPS/TLS 1.2+, require diagnostic settings, allowed locations, and a built-in **"Not allowed resource types"** assignment that blocks `Microsoft.Compute/virtualMachines`, `virtualMachineScaleSets`, `Microsoft.ContainerService/managedClusters` and `Microsoft.Batch/batchAccounts` on the project resource groups, so VMs can't creep in. Also: **deny** `allowSharedKeyAccess != false` on storage, **deny** `disableLocalAuth != true` on Cosmos, and **audit** `publicNetworkAccess != Disabled` (the guard job remediates it outside an open window).

## 8.6 CI/CD (GitHub Actions)

```mermaid
flowchart LR
  PR[Pull request] --> CI1[Rust: fmt · clippy · test · deny · audit · fuzz smoke]
  PR --> CI2[Web: typecheck · lint · vitest · playwright · axe]
  PR --> CI3[Infra: bicep lint · PSRule · what-if]
  PR --> CI4[Security: CodeQL · secret scanning push protection · Dependabot]
  PR --> PREV[SWA preview env + API against dev]
  M[merge to main] --> B[build & sign images · SBOM · push ACR (OIDC)]
  B --> DEV[azd deploy dev]
  DEV --> SMK[smoke + k6 short load]
  SMK --> GATE{{manual approval: prod environment}}
  GATE --> PROD[azd deploy prod · new revision 10% → 100%]
  PROD --> POST[prewarm cache · synthetic checks]
```

- **Container Apps revisions** for blue/green: a new revision receives 10% of traffic until synthetic checks pass, then 100%. Rolling back means shifting traffic to the previous revision.
- **Images:** pinned base `gcr.io/distroless/cc-debian12` (or `scratch` with a musl static build) for the API; the Quickwit image is pinned by digest and copied into ACR with that digest unchanged (CI checks it).
- **Data pipeline deploys** are separate from app deploys. A new `index_version` is published by the `index` job and picked up by the API's refresher, not by redeploying.

## 8.7 Environments

| Env | Corpus | Scale | Cost target |
|-----|--------|-------|-------------|
| `dev` | 1M-page sample (ten diverse years) | `ca-usnm` min 0 (accepts a cold start of several seconds); SWA preview environments; ingest on ACI Spot | ≤ $10/mo |
| `prod` | Full | As in §8.1 | < $80/mo (typical ~$67) |
| *Local* | 10k-page fixture | `docker compose` (Quickwit + API + Azurite) | $0 |

## 8.8 Domain and DNS

- Move the `usnewsmap.com` registration to an account the project controls, with auto-renew and at least 2 admins. This was one of the legacy single points of failure.
- Host DNS in **Azure DNS**: apex `usnewsmap.com` as an alias record to the Static Web App, `www` → apex redirect, and `api` as a CNAME to the container app with its domain-verification TXT record. Add `CAA` records for the CAs that the SWA and Container Apps managed certificates use.
- Keep `/loc_api/*` returning **410 Gone** (an SWA route rule) with a link to the new API docs (old clients may still call it), and redirect legacy query URLs where they can be mapped.
