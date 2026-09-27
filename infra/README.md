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

One command stands up an environment in any subscription or tenant, wires this repository to deploy it, and can be re-run at any time to bring it up to date ([08 §8.9](../docs/design/08-azure-infrastructure.md)):

```sh
az login [--tenant TENANT]                 # and select the subscription, or pass --subscription
azd auth login [--tenant-id TENANT]
gh auth login
scripts/bootstrap.sh prod --alert-email you@example.org --domain usnewsmap.com --ingest
```

Prerequisites: `az`, `azd`, `gh` and `curl`; **Owner** on the subscription (the deployment creates role assignments and policy definitions); admin on the GitHub repository (it creates the GitHub Environment and its variables). No tenant-level objects are created.

What it does:
1. Registers the resource providers.
2. Creates the `azd` environment.
3. Provisions everything except the API, which has no image yet.
4. Creates the GitHub Environment `prod`, restricted to `main`, with its variables, and adds it to `USNM_DEPLOY_ENVIRONMENTS`.
5. Runs `ci` on `main` (every time, so the registry holds the current `main`) and waits for that run.
6. Provisions again on the registry, which deploys the API.
7. Once the registrar delegates `--domain` to the zone, binds the apex and `www` to the Static Web App and `api` to the API, with managed certificates (re-run it after delegating).
8. Deploys the web app and checks both.

Options: `--subscription`, `--location` (default `eastus2`), `--repo` (default: this clone's), `--domain`, `--alert-email` (alerts, and the $80 budget from this month), `--ingest` (the ingest and backfill jobs), `--cleanup-legacy` (removes the repository-level variables and SWA token secret from before per-environment deployment), `--move` (lets an existing environment move to another subscription, starting there with no data). Environment names are 1–16 lowercase letters and digits.

Check the result:

```sh
curl "$(azd env get-value API_URL)/v1/meta"          # "synthetic": true until the corpus is loaded
curl "$(azd env get-value API_URL)/readyz"
```

After that, every green `ci` run on `main` publishes the images to each listed environment and rolls its API onto the commit; `deploy web` does the same for the site. To pin a specific build instead, run `azd env set USNM_IMAGE_TAG <commit sha>` (default `main`) and `azd provision`.

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
| `dnsZoneName` | `USNM_DNS_ZONE` | empty (no zone). The site's domain, e.g. `usnewsmap.com` |
| `githubRepo`, `githubRepoIds` | `USNM_GITHUB_REPO`, `USNM_GITHUB_REPO_IDS` | this repository; bootstrap sets both (the second is GitHub's immutable-ID form, `owner@id/name@id`) |
| `allowedOrigins` | — | empty. Extra CORS origins; the site's own hostname and the domain (with `www`) are always allowed |
| `deployPolicies` | — | `true` (needs Resource Policy Contributor on the subscription) |

## Web app

The `deploy web` workflow (`.github/workflows/deploy-web.yml`) builds `web/` with each environment's API URL and uploads it to that environment's Static Web App. It runs on every push to `main` that touches `web/`, and on demand (`gh workflow run "deploy web" -f environment=prod`). It reads the deployment token at deploy time with the CI identity, so no secret is stored. The API allows the site's `*.azurestaticapps.net` origin, the domain and `www` (with `USNM_DNS_ZONE`), and any `allowedOrigins` for CORS.

## Domain (DNS)

The zone for the site's domain lives in Azure DNS (~$0.50/month):

```sh
azd env set USNM_DNS_ZONE usnewsmap.com
azd provision
azd env get-value NAME_SERVERS      # set these four as the domain's name servers at the registrar
```

The zone holds the apex (an alias to the Static Web App, and CAA records allowing only DigiCert, the managed certificates' CA, with no wildcards), `www` (CNAME to it), and `api` (CNAME to the API app, with the `asuid.api` TXT record Container Apps checks). It also carries over the domain's no-mail records (SPF `v=spf1 -all`, DMARC `p=reject`, and an empty key for every DKIM selector), as they were on the previous DNS host. **Before switching name servers, copy any other records the domain still needs into the zone**: once delegated, only the zone's records resolve. Once the registrar delegates the domain, `scripts/bootstrap.sh` binds the names (it checks the delegation first, and re-running it later finishes the job):
- `www` and the apex are custom domains of the Static Web App. `www` validates by its CNAME; for the apex, bootstrap writes Static Web Apps' validation token to `_dnsauth` as a TXT record. Both get free managed certificates.
- `api` gets a managed certificate from the Container Apps environment, validated by its CNAME. Its id is recorded as `USNM_API_CERT_ID`, and Bicep declares the binding from it, so later provisioning keeps it. `API_URL` (and the site's API base) then becomes `https://api.<domain>`.

## Container registry

Images are private, in ACR Basic (`crusnm{env}…`), with no admin user and no anonymous pull. The apps and jobs pull with their managed identities.

CI pushes from `main` as `id-usnm-ci-{env}`, signing in with OIDC. That identity trusts only jobs in the GitHub Environment `{env}`, and no secret is stored anywhere. `scripts/bootstrap.sh` sets it all up.

The CI identity's rights are AcrPush on its registry, and a custom role, "usnm deployer", on its resource group (`infra/modules/deployer.bicep`). The role can roll Container Apps onto new images and read Static Web App deployment tokens, and nothing else: no data, keys, networking or role assignments. The web deploy uses it to read the site's deployment token at deploy time. It's scoped to the group because Azure rejects role assignments scoped to a Container App or Static Web App itself, and because `az containerapp update` also needs join and assign rights on the app's environment and identity; the group holds only this environment.

`azd provision` sets the API image to `USNM_IMAGE_TAG` again (default `main`, the newest build pushed from `main`, which is normally the one CI last rolled out).

## Ingest jobs

1. Deploy the jobs: `scripts/bootstrap.sh <env> --ingest` (or `azd env set USNM_INGEST_JOBS true` and `azd provision` on an environment already on the registry). Optionally set a weekly schedule with `azd env set USNM_INGEST_CRON "17 3 * * 1"` (UTC) and `USNM_BACKFILL_WORKERS` (default 8).
2. The catalog (`reference/catalog/titles.json`, `places.json`) is built by `titles-sync`, which `run` calls before every release. Coordinate corrections live in git, in `catalog/overrides/places.json` (`[{city, state, lat, lon}]`), and ship in the ingest image; the next run applies them.
3. Backfill, then publish the first version:
   ```sh
   RG=$(azd env get-value AZURE_RESOURCE_GROUP)
   # Queue every batch from LoC's listing and curate it with 8 workers (~22 h).
   az containerapp job start -n "$(azd env get-value BACKFILL_JOB)" -g "$RG"
   # When that has finished: curate any leftovers, build the base index, publish.
   az containerapp job start -n "$(azd env get-value INGEST_JOB)" -g "$RG"
   ```
   Watch with `az containerapp job execution list -n <job> -g "$RG" -o table` and the job logs in Log Analytics.
4. Switch the API to the published indexes: `azd env set USNM_SEARCH_BACKEND quickwit`, then `azd provision`.

The storage and Cosmos accounts stay private throughout: the jobs run inside the VNet.

## Checks

CI runs `bicep lint` on every module and treats any warning as a failure, then `bicep build`. PSRule and `what-if` output on PRs need Azure credentials (an OIDC federated identity for this repo), so they come with the deploy pipeline.
