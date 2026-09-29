# Infrastructure (Bicep, one deployment stack per environment)

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
| `identities`, `rbac` | `id-usnm-app`: Blob Data **Reader** on `reference` and `qw-index`, Blob Data **Contributor** on `cache`, Cosmos Built-in Data **Reader** on `usnm` (the status page), and Monitoring Metrics Publisher on Application Insights. `id-usnm-ingest`: Blob Data Contributor on `curated`, `reference` and `qw-index`, Cosmos Built-in Data Contributor on `usnm`, and Monitoring Metrics Publisher on Application Insights |
| `containerapps-env` | VNet-integrated, workload-profiles environment that uses only the Consumption profile (no management fee) |
| `containerapp` | The API (`ca-usnm-{env}`): 0.25 vCPU / 0.5 GiB, external ingress, health probes, and 0–1 to 2 replicas on an HTTP scaler, and the Application Insights connection string (requests, traces and metrics, sent as `id-usnm-app`). With `searchBackend: quickwit`, also a read-only Quickwit 0.9.1 sidecar (1 vCPU / 2 GiB, localhost only) and a system-assigned identity with Blob Data **Reader** on `qw-index` only |
| `ingestjobs` (with `ingestJobs: true`) | `caj-usnm-ingest-{env}` (`usnm-ingest run`, weekly or manual) and `caj-usnm-backfill-{env}` (N parallel `curate` workers, manual), both in the VNet with `id-usnm-ingest`. The ingest job's system-assigned identity, used by its Quickwit writer, gets Blob Data Contributor on `qw-index` only |
| `monitoring` | Log Analytics (30-day retention, ~150 MB/day cap) and Application Insights, both with local (key) auth disabled. An action group is created when alert emails are set |
| `diagnostics` | Diagnostic settings, sending resource logs to Log Analytics for every resource that has them: the workspace, registry, VNet, Container Apps environment (app and job console output, platform events), Cosmos control plane, and both storage accounts (blob writes and deletes; queue, table and file services in full). Per-request categories are left out to stay under the cap (08 §8.1.1) |
| `ingest-alerts` (with `ingestJobs: true` and alert emails) | Log search alerts on the workspace: an ingest or backfill job failed, a release stalled, a backfill stalled (08 §8.1.2) |
| `api-alerts` (with the API and alert emails) | Log search alerts on API 5xx and slow `/v1/aggregate`; with `dnsZoneName`, standard availability tests of the site and `api.{domain}/readyz` (each once its certificate is bound) and an alert when 2 of 3 locations fail (08 §8.1.2) |
| `budget`, `alerts` | $80 monthly budget (alerts at $40, $60, $75, plus an $80 forecast alert; needs alert emails and `USNM_BUDGET_START`) and an alert on control-plane writes to the data accounts (needs alert emails) |
| `policy-*` | Custom policies assigned to both resource groups. They deny IaaS compute, deny storage shared keys, deny Cosmos, Log Analytics and Application Insights local auth, deny a registry admin user or anonymous pull, audit public network access on the data accounts, and audit resources that have resource logs but no diagnostic setting |

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
gh auth login
scripts/bootstrap.sh prod --alert-email you@example.org --domain usnewsmap.com --ingest
```

Prerequisites: `az` (2.61 or later), `gh` and `curl`; **Owner** on the subscription (the deployment creates role assignments and policy definitions); admin on the GitHub repository (it creates the GitHub Environment and its variables). No tenant-level objects are created.

What it does:
1. Registers the resource providers.
2. Writes the environment's settings to `.azure/<env>/.env`.
3. Deploys the stack `usnm-<env>`: everything except the API, which has no image yet.
4. Creates the GitHub Environment `prod`, restricted to `main`, with its variables, and adds it to `USNM_DEPLOY_ENVIRONMENTS`.
5. Runs `ci` on `main` (every time, so the registry holds the current `main`) and waits for that run.
6. Deploys the stack again on the registry, which deploys the API.
7. Once the registrar delegates `--domain` to the zone, binds the apex, `www` and `api` to the API app, which serves the site and the API, with managed certificates (re-run it after delegating). It also deletes the Static Web App that used to serve the site.
8. Checks the API's `/readyz` and the site.

Options: `--subscription`, `--location` (default `eastus2`), `--repo` (default: this clone's), `--domain`, `--alert-email` (alerts, and the $80 budget from this month), `--ingest` (the ingest and backfill jobs), `--cleanup-legacy` (removes the repository-level variables and the old Static Web Apps token secret from before per-environment deployment), `--move` (lets an existing environment move to another subscription, starting there with no data). Environment names are 1–16 lowercase letters and digits.

Check the result:

```sh
curl "$(scripts/settings.sh prod API_URL)/v1/meta"   # "synthetic": true until the corpus is loaded
curl "$(scripts/settings.sh prod API_URL)/readyz"
curl "$(scripts/settings.sh prod API_URL)/v1/status"  # "pipeline": {"available": true, …} once the Cosmos role has propagated
```

After that, every green `ci` run on `main` publishes the images to each listed environment and rolls its API app, which carries the site, onto the commit. To pin a specific build instead, run `scripts/settings.sh prod USNM_IMAGE_TAG <commit sha>` (default `main`) and `scripts/provision.sh prod`.

| Parameter | Setting | Default |
|-----------|---------|---------|
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
| `availabilityFrequency` | `USNM_AVAILABILITY_FREQUENCY` | `900`. Seconds between availability test runs (300, 600 or 900); the one test (the site home page) from 3 locations costs about $4 a month at 900 and $13 at 300 |
| `githubRepo`, `githubRepoIds` | `USNM_GITHUB_REPO`, `USNM_GITHUB_REPO_IDS` | this repository; bootstrap sets both (the second is GitHub's immutable-ID form, `owner@id/name@id`) |
| `allowedOrigins` | — | empty. Extra CORS origins; the site's own hostname and the domain (with `www`) are always allowed |
| `deployPolicies` | — | `true`: assign the guard-rail policies (the `usnm-guardrails` stack defines them) |

### Settings and the deployment stack

An environment's settings are `KEY="value"` lines in `.azure/<env>/.env`, which is git-ignored. It uses the layout azd used, so environments created with azd carry over. `infra/main.bicepparam` reads the settings from the environment (bootstrap exports those with a value; the rest take the defaults above), and the template's outputs (`API_URL`, `ACR_NAME`, …) are written back after each deployment.

```sh
scripts/settings.sh prod                      # show all
scripts/settings.sh prod USNM_SEARCH_BACKEND quickwit
scripts/provision.sh prod                     # deploy the stack with the new settings
```

Everything is one **deployment stack** at subscription scope, `usnm-<env>` ([ADR-0011](../docs/design/adr/0011-deployment-stacks.md)):

- **Removed from the template means removed from Azure.** Each deployment runs with `--action-on-unmanage deleteResources`, so a resource dropped from the Bicep (or switched off by a setting, such as `USNM_INGEST_JOBS=false`) is deleted, not left running.
- **Nothing the stack manages can be deleted outside it.** `--deny-settings-mode denyDelete` puts a deny assignment on every managed resource, the resource groups included, that blocks deletes by anyone, Owners too. Changes still go through the stack. To delete by hand in an emergency, deploy once with `--deny-settings-mode none`.
- Resources the template doesn't declare (the certificates bootstrap binds, anything made in the portal) aren't managed: the stack neither protects nor deletes them.
- The guard-rail policy definitions every environment in the subscription assigns (`infra/guardrails.bicep`) are a stack of their own, `usnm-guardrails`, deployed just before the environment's. Two stacks never manage the same resource.
- `scripts/teardown.sh <env>` deletes an environment: its resource groups, its custom role, the guard-rail stack (`usnm-guardrails`, the policy definitions) when no other environment in the subscription uses it, and its GitHub Environment. It asks for the name first, and works in the environment's recorded subscription. `--subscription OLD` removes only the Azure copy left in an old subscription after a move.

## Web app

The API app serves the site ([ADR-0010](../docs/design/adr/0010-site-served-by-the-api.md)). The API image's build compiles `web/` into `/srv/site` (`USNM_SITE_DIR`), with brotli and gzip copies of each text file. So every image `ci` publishes carries the site, and each rollout ships both together. The site calls `/v1` on its own origin, so it needs no API URL and no CORS. The API still allows the domain, `www`, the app's own hostname and any `allowedOrigins` for other callers. There is no separate web deploy and no deployment token.

## Domain (DNS)

The zone for the site's domain lives in Azure DNS (~$0.50/month):

```sh
scripts/bootstrap.sh prod --domain usnewsmap.com
scripts/settings.sh prod NAME_SERVERS   # set these four as the domain's name servers at the registrar
```

The zone holds:
- the apex: an A record to the Container Apps environment's static IP, plus CAA records allowing only DigiCert (the managed certificates' CA), with no wildcards;
- `www` and `api`: CNAMEs to the API app;
- `asuid`, `asuid.www` and `asuid.api`: the TXT records Container Apps checks before binding each name.

 It also carries over the domain's no-mail records (SPF `v=spf1 -all`, DMARC `p=reject`, and an empty key for every DKIM selector), as they were on the previous DNS host. **Before switching name servers, copy any other records the domain still needs into the zone**: once delegated, only the zone's records resolve. Once the registrar delegates the domain, `scripts/bootstrap.sh` binds the names (it checks the delegation first, and re-running it later finishes the job):
- Each name gets a free managed certificate from the Container Apps environment. `www` and `api` are validated by their CNAMEs, and the apex over HTTP through its A record.
- The certificate ids are recorded as `USNM_SITE_CERT_ID`, `USNM_WWW_CERT_ID` and `USNM_API_CERT_ID`. Bicep declares the bindings from them, so later deployments keep them.
- `SITE_URL` then becomes `https://<domain>`, and `API_URL` becomes `https://api.<domain>`.
- If a name's DNS record changed within the last hour, resolvers may still hold the old one and validation can fail. Re-run bootstrap later.

## Container registry

Images are private, in ACR Basic (`crusnm{env}…`), with no admin user and no anonymous pull. The apps and jobs pull with their managed identities.

CI pushes from `main` as `id-usnm-ci-{env}`, signing in with OIDC. That identity trusts only jobs in the GitHub Environment `{env}`, and no secret is stored anywhere. `scripts/bootstrap.sh` sets it all up.

The CI identity's rights are AcrPush on its registry, and a custom role, "usnm deployer", on its resource group (`infra/modules/deployer.bicep`). The role can roll Container Apps onto new images (which carry the site too), and nothing else: no data, keys, networking or role assignments. It's scoped to the group because Azure rejects role assignments scoped to a Container App itself, and because `az containerapp update` also needs join rights on the app's environment; the group holds only this environment. The update also needs assign rights on the app's identity: CI has Managed Identity Operator on `id-usnm-app-{env}` alone, never on the group, so it can't attach the ingest identity (and its data access) to the app.

Each stack deployment sets the API image to `USNM_IMAGE_TAG` again (default `main`, the newest build pushed from `main`, which is normally the one CI last rolled out).

## Ingest jobs

1. Deploy the jobs: `scripts/bootstrap.sh <env> --ingest` (or `scripts/settings.sh <env> USNM_INGEST_JOBS true` and `scripts/provision.sh <env>` on an environment already on the registry). Optionally set a weekly schedule with `scripts/settings.sh <env> USNM_INGEST_CRON "17 3 * * 1"` (UTC) and `USNM_BACKFILL_WORKERS` (default 4).
2. The catalog (`reference/catalog/titles.json`, `places.json`) is built by `titles-sync`, which `run` calls before every release. Coordinate corrections live in git, in `catalog/overrides/places.json` (`[{city, state, lat, lon}]`), and ship in the ingest image; the next run applies them.
3. Backfill, then publish the first version:
   ```sh
   RG=$(scripts/settings.sh prod AZURE_RESOURCE_GROUP)
   # Queue every batch from LoC's listing and curate it. Downloads are paced to
   # LoC's limit (10 bulk requests per 10 minutes per IP), one every 75 s across
   # all workers, so this takes about 2.5 days. Each execution stops after 24 h:
   # start it again until the queue is empty (it resumes where it left off).
   az containerapp job start -n "$(scripts/settings.sh prod BACKFILL_JOB)" -g "$RG"
   # When that has finished: curate any leftovers, build the base index, publish.
   az containerapp job start -n "$(scripts/settings.sh prod INGEST_JOB)" -g "$RG"
   ```
   Watch with `az containerapp job execution list -n <job> -g "$RG" -o table` and the saved log queries:
   ```sh
   scripts/logs.sh prod list                      # the saved queries (ops/queries/)
   scripts/logs.sh prod release-progress 6h       # docs sent, rate, retries, free disk, memory
   scripts/logs.sh prod curation-throughput 1d    # batches and pages per hour
   scripts/logs.sh prod errors-by-batch 1d
   scripts/logs.sh prod job-executions 2d
   ```
   The workspace id is the stack output `LOG_ANALYTICS_WORKSPACE_ID`; the queries run with your own sign-in (Log Analytics Reader or more). Traces (`release`, `curate`) and metrics are in Application Insights `appi-usnm-<env>`. With alert emails set, a failed job, a stalled release or a stalled backfill sends an email (08 §8.1.2).
4. Switch the API to the published indexes: `scripts/settings.sh <env> USNM_SEARCH_BACKEND quickwit`, then `scripts/provision.sh <env>`.

The storage and Cosmos accounts stay private throughout: the jobs run inside the VNet.

## Checks

CI runs `bicep lint` on every module and treats any warning as a failure, then `bicep build`. PSRule needs Azure credentials (an OIDC federated identity for this repo), so it comes with the deploy pipeline. There's no `what-if` preview: deployment stacks don't support it yet (ADR-0011).
