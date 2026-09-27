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
| `containerapp` | The API (`ca-usnm-{env}`): 0.25 vCPU / 0.5 GiB, external ingress, health probes, and 0–1 to 2 replicas on an HTTP scaler |
| `staticwebapp` | SWA Free for the site |
| `monitoring` | Log Analytics (30-day retention, ~150 MB/day cap) and Application Insights. An action group is created when alert emails are set |
| `budget`, `alerts` | $80 monthly budget (alerts at $40, $60, $75, plus an $80 forecast alert; needs alert emails and `USNM_BUDGET_START`) and an alert on control-plane writes to the data accounts (needs alert emails) |
| `policy-*` | Custom policies assigned to both resource groups. They deny IaaS compute, deny storage shared keys, deny Cosmos local auth, and audit public network access on the data accounts |

### What this slice runs

The API runs on its **baked synthetic fixtures** (memory backend), and its persistent response cache is set to `cache/` on the data account. That exercises the production paths the real corpus will use: the managed identity, the Blob private endpoint and private DNS, ingress, probes and scaling.

The following come in later slices:

- **Quickwit searcher sidecar:** after spike S-2 confirms how Quickwit authenticates to Blob without account keys (managed identity, or a user-delegation SAS minted by the API).
- **Ingest jobs, backfill launcher and `network-guard`:** with the ingest pipeline. Its launcher identity and the custom "public access toggle" role come with it.
- **Custom domains and DNS** (`usnewsmap.com`, `api.usnewsmap.com`): see §8.8. Managed certificates need the DNS records in place first.

## Deploy

Prerequisites:

- The [Azure Developer CLI](https://aka.ms/azd) and an account with **Owner** on the subscription. Owner is needed because the deployment creates role assignments and policy definitions.
- The API image published to GHCR. CI pushes `ghcr.io/<owner>/usnewsmap-api:main` and `:<sha>` on every merge to `main`. The first push creates the package as **private**: make it public in the package settings, or the Container App can't pull it.

```sh
azd auth login
azd env new dev
azd env set AZURE_LOCATION eastus2
azd env set USNM_ALERT_EMAILS "you@example.org"   # optional: enables alerts
azd env set USNM_BUDGET_START 2026-10-01           # optional, with emails: enables the budget; set once, never change
azd env set USNM_COSMOS_FREE_TIER false            # if another account in the subscription already uses the free tier
azd provision
```

Outputs include `API_URL`, `SITE_URL` and `TILES_URL`. Check that the deployment works:

```sh
curl "$(azd env get-value API_URL)/v1/meta"          # "synthetic": true
curl "$(azd env get-value API_URL)/readyz"
```

To roll out a specific image, run `azd env set USNM_API_IMAGE ghcr.io/<owner>/usnewsmap-api:<sha>` and then `azd provision`.

| Parameter | azd variable | Default |
|-----------|--------------|---------|
| `environmentName` | `AZURE_ENV_NAME` | — (e.g. `dev`, `prod`; `prod` keeps one warm replica, others scale to zero) |
| `location` | `AZURE_LOCATION` | `eastus2` |
| `apiImage` | `USNM_API_IMAGE` | `ghcr.io/tgoodyear/usnewsmap-api:main` |
| `cosmosFreeTier` | `USNM_COSMOS_FREE_TIER` | `true` (one free-tier account per subscription) |
| `alertEmails` | `USNM_ALERT_EMAILS` | empty (comma-separated) |
| `budgetStartDate` | `USNM_BUDGET_START` | empty. The first day of a month; the budget is created only with alert emails and this set. Azure can't change a budget's start date, so keep it fixed |
| `deployPolicies` | — | `true` (needs Resource Policy Contributor on the subscription) |

## Checks

CI runs `bicep lint` on every module and treats any warning as a failure, then `bicep build`. PSRule and `what-if` output on PRs need Azure credentials (an OIDC federated identity for this repo), so they come with the deploy pipeline.
