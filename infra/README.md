# Infrastructure (Bicep + azd)

The lean hosting profile from [design doc 08](../docs/design/08-azure-infrastructure.md). It uses only PaaS: there are no VMs, scale sets, AKS clusters or Batch accounts, and an assigned policy denies them.

## What this deploys

`main.bicep` runs at subscription scope. It creates two resource groups: `rg-usnm-{env}` for the platform and an empty `rg-usnm-{env}-spot` for the backfill launcher's ACI Spot groups. Inside `rg-usnm-{env}` it creates:

| Module | Resources |
|--------|-----------|
| `network` | VNet `10.40.0.0/24`: `snet-cae` (/27, delegated to Container Apps) and `snet-pe` (/28). Private DNS zones for Blob and Cosmos, linked to the VNet |
| `storage` | Data lake account: flat namespace, **no shared keys**, **public network access disabled**, versioning and 14-day soft delete, containers `curated`, `reference`, `cache`, `qw-index`. Lifecycle rules expire old response-cache entries and prune blob versions. Write and delete logs go to Log Analytics |
| `tiles` | Public basemap account (anonymous read of the `tiles` container, CORS for the site, no keys) |
| `cosmos` | Cosmos DB for NoSQL. It uses the free tier with 7-day continuous backup, or serverless with periodic backup; **local auth disabled**, **public network access disabled**, and database `usnm` holding `titles`, `batches`, `issues`, `index_runs` and `ops` |
| `private-endpoints` | `pe-usnm-blob` and `pe-usnm-cosmos` in `snet-pe`, registered in the private DNS zones |
| `identities`, `rbac` | `id-usnm-app`: Blob Data **Reader** on `reference` and `qw-index`, Blob Data **Contributor** on `cache`. `id-usnm-ingest`: Blob Data Contributor on `curated`, `reference` and `qw-index`, plus Cosmos Built-in Data Contributor on `usnm` |
| `containerapps-env` | VNet-integrated, workload-profiles environment that uses only the Consumption profile (no management fee) |
| `containerapp` | The API (`ca-usnm-{env}`): 0.25 vCPU / 0.5 GiB, external ingress, health probes, and 0–1 to 2 replicas on an HTTP scaler. With `searchBackend: quickwit`, also a read-only Quickwit 0.9.1 sidecar (1 vCPU / 2 GiB, localhost only) and a system-assigned identity with Blob Data **Reader** on `qw-index` only |
| `staticwebapp` | SWA Free for the site. Content is uploaded by the `deploy web` workflow (below) |
| `ingestjobs` (with `ingestJobs: true`) | `caj-usnm-ingest-{env}` (`usnm-ingest run`, weekly or manual) and `caj-usnm-backfill-{env}` (N parallel `curate` workers, manual), both in the VNet with `id-usnm-ingest`. The ingest job's system-assigned identity, used by its Quickwit writer, gets Blob Data Contributor on `qw-index` only |
| `monitoring` | Log Analytics (30-day retention, ~150 MB/day cap) and Application Insights. An action group is created when alert emails are set |
| `budget`, `alerts` | $80 monthly budget (alerts at $40, $60, $75, plus an $80 forecast alert; needs alert emails and `USNM_BUDGET_START`) and an alert on control-plane writes to the data accounts (needs alert emails) |
| `policy-*` | Custom policies assigned to both resource groups. They deny IaaS compute, deny storage shared keys, deny Cosmos local auth, and audit public network access on the data accounts |

### What this slice runs

By default (`USNM_SEARCH_BACKEND=fixtures`) the API runs on its **baked synthetic fixtures** (memory backend), and its persistent response cache is set to `cache/` on the data account. That exercises the production paths the real corpus will use: the managed identity, the Blob private endpoint and private DNS, ingress, probes and scaling.

`USNM_SEARCH_BACKEND=quickwit` switches to the production search path (08 §8.4.1):

- The API reads `current.json` and reference snapshots from `reference/`.
- It queries a **Quickwit searcher sidecar** on `127.0.0.1:7280`.
- The sidecar opens the file-backed metastore in `qw-index/` read-only, polling every 30 s. Before a new version goes live, the API looks up each index that version lists, so the searcher loads indexes created after it started.
- **Quickwit authenticates with the app's system-assigned identity.** Quickwit 0.9 uses the Azure SDK's default credential chain, which in Container Apps can only use the system-assigned identity. That identity has Blob Data Reader on `qw-index` and nothing else.

Switch only after the ingest jobs have published indexes and a `current.json`. Until then the API has nothing to serve and keeps retrying its first load. On the first deployment with `quickwit`, the identity's role assignment is created after the app, so the sidecar and API may restart a few times until the assignment propagates (a few minutes). This deployment is also where Quickwit's managed-identity auth is first confirmed against a real account (spike S-2). If it fails, the fallback is a user-delegation SAS minted by the API (08 §8.2).

The following come in later slices:

- **`titles-sync` and `geocode`**, which write the catalog the ingest job needs.
- **Custom domains and DNS** (`usnewsmap.com`, `api.usnewsmap.com`): see §8.8. Managed certificates need the DNS records in place first.

## Deploy

Prerequisites:

- The [Azure Developer CLI](https://aka.ms/azd) and an account with **Owner** on the subscription. Owner is needed because the deployment creates role assignments and policy definitions.
- The images come from the private registry the deployment creates. The first `azd provision` deploys everything except the API, which has no image to pull until CI has pushed one (see [Container registry](#container-registry)).

```sh
azd auth login
azd env new dev
azd env set AZURE_LOCATION eastus2
azd env set USNM_ALERT_EMAILS "you@example.org"   # optional: enables alerts
azd env set USNM_BUDGET_START 2026-10-01           # optional, with emails: enables the budget; set once, never change
azd env set USNM_COSMOS_FREE_TIER false            # if another account in the subscription already uses the free tier
azd provision
```

Then set up the [container registry](#container-registry), which deploys the API. Outputs include `API_URL`, `SITE_URL` and `TILES_URL`. Check that the deployment works:

```sh
curl "$(azd env get-value API_URL)/v1/meta"          # "synthetic": true
curl "$(azd env get-value API_URL)/readyz"
```

To roll out a specific build, run `azd env set USNM_IMAGE_TAG <commit sha>` (default `main`) and then `azd provision`.

| Parameter | azd variable | Default |
|-----------|--------------|---------|
| `environmentName` | `AZURE_ENV_NAME` | — (e.g. `dev`, `prod`; `prod` keeps one warm replica, others scale to zero) |
| `location` | `AZURE_LOCATION` | `eastus2` |
| `apiImage` | `USNM_API_IMAGE` | empty. A public image to run while `useAcr` is off; empty skips the API until then |
| `useAcr` | `USNM_USE_ACR` | `false`. Turn on once CI has pushed to the registry |
| `imageTag` | `USNM_IMAGE_TAG` | `main` (or a commit sha) |
| `searchBackend` | `USNM_SEARCH_BACKEND` | `fixtures`, or `quickwit` once indexes are published |
| `cosmosFreeTier` | `USNM_COSMOS_FREE_TIER` | `true` (one free-tier account per subscription) |
| `alertEmails` | `USNM_ALERT_EMAILS` | empty (comma-separated) |
| `budgetStartDate` | `USNM_BUDGET_START` | empty. The first day of a month; the budget is created only with alert emails and this set. Azure can't change a budget's start date, so keep it fixed |
| `deployPolicies` | — | `true` (needs Resource Policy Contributor on the subscription) |

## Web app

The `deploy web` workflow (`.github/workflows/deploy-web.yml`) builds `web/` and uploads it to the Static Web App on every push to `main` that touches `web/`, and on demand. It needs one secret and one variable on the GitHub repository:

```sh
gh secret set AZURE_STATIC_WEB_APPS_API_TOKEN --body "$(az staticwebapp secrets list \
  -n swa-usnm-$(azd env get-value AZURE_ENV_NAME) -g "$(azd env get-value AZURE_RESOURCE_GROUP)" \
  --query properties.apiKey -o tsv)"
gh variable set USNM_API_URL --body "$(azd env get-value API_URL)"
gh workflow run "deploy web"
```

The API allows the site's `*.azurestaticapps.net` origin (and `allowedOrigins`) for CORS.

## Container registry

Images are private, in ACR Basic (`crusnm{env}…`). CI pushes to it from `main`, signing in with OIDC as `id-usnm-ci-{env}`; no secret is stored anywhere. One-time setup after the first `azd provision` (these are repository **variables**, not secrets):

```sh
gh variable set USNM_ACR_LOGIN_SERVER --body "$(azd env get-value ACR_LOGIN_SERVER)"
gh variable set USNM_CI_CLIENT_ID     --body "$(azd env get-value CI_CLIENT_ID)"
gh variable set AZURE_TENANT_ID       --body "$(azd env get-value AZURE_TENANT_ID)"
gh variable set AZURE_SUBSCRIPTION_ID --body "$(azd env get-value AZURE_SUBSCRIPTION_ID)"
gh workflow run ci --ref main          # pushes the API and ingest images and copies Quickwit in
azd env set USNM_USE_ACR true
azd provision                          # the API (and sidecar) now pull from the registry
```

Once the API runs from the registry, the old GHCR packages can be made private again or deleted.

## Ingest jobs

1. Set up the [container registry](#container-registry) (the ingest image is only published there).
2. Deploy the jobs: `azd env set USNM_INGEST_JOBS true`, then `azd provision`. Optionally set a weekly schedule with `azd env set USNM_INGEST_CRON "17 3 * * 1"` (UTC) and `USNM_BACKFILL_WORKERS` (default 8).
3. Put the catalog in `reference/catalog/titles.json` and `places.json` (from `titles-sync` and `geocode`, once they exist).
4. Backfill, then publish the first version:
   ```sh
   RG=$(azd env get-value AZURE_RESOURCE_GROUP)
   # Queue every batch from LoC's listing and curate it with 8 workers (~22 h).
   az containerapp job start -n "$(azd env get-value BACKFILL_JOB)" -g "$RG"
   # When that has finished: curate any leftovers, build the base index, publish.
   az containerapp job start -n "$(azd env get-value INGEST_JOB)" -g "$RG"
   ```
   Watch with `az containerapp job execution list -n <job> -g "$RG" -o table` and the job logs in Log Analytics.
5. Switch the API to the published indexes: `azd env set USNM_SEARCH_BACKEND quickwit`, then `azd provision`.

The storage and Cosmos accounts stay private throughout: the jobs run inside the VNet.

## Checks

CI runs `bicep lint` on every module and treats any warning as a failure, then `bicep build`. PSRule and `what-if` output on PRs need Azure credentials (an OIDC federated identity for this repo), so they come with the deploy pipeline.
