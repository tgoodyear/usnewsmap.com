# Environments and deploys

Part of the [operations guide](README.md).

## Settings

Settings live in three places ([`infra/README.md`](../../infra/README.md#settings-and-the-deployment-stack), #295); [`infra/env/schema.json`](../../infra/env/schema.json) says which goes where. A setting in the wrong place, or in two, stops a provision before it deploys anything.

- **Public settings** (flags, sizes, caps, schedules, most `USNM_*`) are committed in `infra/env/<env>.json` and change by pull request. `scripts/azure/provision.sh` deploys only committed settings, and CI's "stack change needs a provision" warning stays on the merge until it runs. Every public `settings.sh` example on these pages works this way. To try a dev setting before committing it, run `provision.sh --env dev --allow-dirty`.

  ```sh
  scripts/azure/settings.sh --env prod --key USNM_JA_LATIN --value true   # edits infra/env/prod.json
  git switch -c settings/prod-ja-latin && git commit -am "prod: USNM_JA_LATIN on" && git push -u origin HEAD
  # once the pull request is merged:
  git switch main && git pull && scripts/azure/provision.sh --env prod
  ```

- **Private settings** (`USNM_ALERT_EMAILS`, `USNM_SEARCH_LOG_READERS`, `USNM_OCR_USERS`, `USNM_OCR_NODE_IPS`, `USNM_OCR_NODE_GROUP`, `USNM_OCR_KEYVAULT_ADMINS`, `USNM_ARCHIVE_ACCOUNT`, `USNM_VNET_ADDRESS_PREFIX`, the certificate ids, `USNM_GITHUB_REPO_IDS`) are in the **settings store**: one App Configuration store per subscription, `usnm-config-appcs-<suffix>` in `usnm-config-rg`, keyed by setting name under each environment's label (`dev`, `prod`, ...). Entra ID only (no access keys); CI never reads it.

  ```sh
  scripts/azure/settings.sh --env prod --key USNM_ALERT_EMAILS --value ops@example.org
  # or: store=$(scripts/azure/settings.sh --env prod --key SETTINGS_STORE_ENDPOINT)
  #     az appconfig kv set --endpoint "$store" --auth-mode login --label prod --key USNM_ALERT_EMAILS --value ops@example.org --yes
  scripts/azure/provision.sh --env prod
  ```

  - `--unset` deletes the key from the store and any local copy; `--unset-local` removes only the local entry (provision suggests it when a key is in both). An empty value stays a local `""` that hides the store's value (`teardown.sh` detaches the archive that way).
  - Before the store exists, private settings stay local and every provision warns; `settings.sh --env <env> --migrate` then moves them (none if the store holds a different value for one).
  - Scripts write to the store only on change and drop any local copy (`bootstrap.sh`: repository ids every run, certificate ids, alert addresses; `group.sh`: the nodes' group; `settings.sh`).
  - **Lifecycle:** own stack `usnm-config` (`infra/settings/main.bicep`), so teardowns keep it and the label. The first provision in a subscription (or `bootstrap.sh`) runs `scripts/azure/settings-store.sh --deploy` in that region, registering `Microsoft.AppConfiguration` if needed, and waits up to 15 minutes for its role; `--what-if` previews. Free tier: $0, 10 MB, 1,000 requests a day (one per provision or script read), no soft delete.
  - **Access:** App Configuration Data Owner on all labels for the stack's admin list and the last deployer. A provision reads the store before it deploys anything; without access it stops with the fix. To add someone, they run `az ad signed-in-user show --query id -o tsv` and an operator with access adds that id; `--admins` replaces the list, and without it `--deploy` keeps it: `scripts/azure/settings-store.sh --deploy --admins "<id>,<id>"`.
  - **No operator left:** a subscription Owner grants themselves the role on the group (not the store, which collides with the stack's assignment), adds themselves to `--admins`, then deletes the grant: `az role assignment create --assignee-object-id "$(az ad signed-in-user show --query id -o tsv)" --assignee-principal-type User --role "App Configuration Data Owner" --scope "/subscriptions/<subscription id>/resourceGroups/usnm-config-rg"`.
  - Without `SETTINGS_STORE_ENDPOINT` locally, scripts read it from the store's stack and record it, so a missing store never reads as unset.
  - These aren't credentials, so they aren't in Key Vault (ADR-0009); the public-network guard-rail also blocked #295's per-environment vaults.

- **Everything else** stays in the local file (`~/.config/usnewsmap/<env>/.env` or `.azure/<env>/.env`): stack outputs, subscription and tenant, `USNM_EXPERIMENT`, `USNM_IMAGE_TAG`.

Prod pins its existing resource names; dev and experiments use ADR-0017's. Read names from the stack outputs (`settings.sh --env <env> --key <OUTPUT>`), never rebuild them ([naming guard](../../infra/README.md#naming-guard-and-phase-12-rollout)).

**The API's pinned image tag.** On-prem serving follows attested `main`: a merge publishes and attests the image. Prod's Azure API is the spare at zero replicas (ADR-0029); no merge rolls it (#308), only the `spare-roll` workflow ([Rolling the spare](search-serving.md#rolling-the-spare)) or a dispatched `ci` run with `roll_api`. Neither updates `USNM_API_IMAGE_TAG`, so a provision for any other change would put the spare back on the pinned image. `provision.sh` (and `--what-if`) therefore refuses when the pin differs from the tag the app serves (#1071); details in [Container registry](../../infra/README.md#container-registry).

```sh
scripts/azure/provision.sh --env prod --api-tag=live    # keep the image that runs now
scripts/azure/provision.sh --env prod --api-tag=<tag>   # deploy <tag> over the pin
```

- To roll the spare to a new image, move the pin through a pull request first, then provision with `--api-tag=<the new pin>`.
- The app and its group come from the outputs the last provision saved, or from the stack when the local file has none.
- Skipped: no pin (dev), no API app in Azure, any image other than ACR `usnewsmap-api:<tag>` (a note), `teardown.sh`'s partial provision.
- Refuses when unsure: no API app named, no ready revision, a failed read (`--api-tag=<tag>` passes).
- The flags don't edit `prod.json`; plain provisions, `bootstrap.sh` and `stand-up.sh` keep refusing until a pull request sets the pin to the tag that runs.

**What-If.** `provision.sh --env <env> --what-if` deploys nothing: it reads settings, runs preflight (a policy denial fails it) and `az stack-whatif sub create`, including unmanaged-resource and deny-setting changes; without the guard-rail stack it previews that stack. Known provider noise: Static Web Apps generated properties, diagnostic-setting destination defaults, Azure Files soft-delete retention defaults. Compare these with the deployed resource; don't change the template to silence them. Always review deletions and deny-setting changes, even with `--only-show-errors` ([deployment-stacks What-If](https://learn.microsoft.com/en-us/azure/azure-resource-manager/bicep/deployment-stacks-what-if)). A deletion of a protected resource also needs `--allow-rename=<exact old name>` ([Protected-resource gate](../../infra/README.md#protected-resource-gate)).

## Deploy the jobs

**Prod has no Azure ingest jobs** (#917): `USNM_INGEST_JOBS` is `false`, so there is no `caj-usnm-ingest`, `caj-usnm-backfill`, `usnm-ocrs-caj`, Japanese OCR or audit job, Congress job, `id-usnm-ingest` or its grants, ingest alerts or pipeline workbook. The hourly OCR pickup, one-off ingest commands and backfill run on-prem as the batch container's Arc identity (`USNM_OCR_SYNC_ONPREM`, `USNM_INGEST_ONPREM`, `USNM_BACKFILL_ONPREM`; home-infra `docs/lab.md`, "Ingest commands"), and full releases are built on-prem ([Full rebuild on-prem](rebuild-and-index-storage.md#full-rebuild-on-prem)). Without a VNet (`USNM_CAE_VNET` is `false`), prod can't take the jobs back: the provision refuses Azure ingest jobs then. Re-enabling them in an environment with a VNet restores them with a new identity id.

**Elsewhere** (a dev experiment), `scripts/azure/bootstrap.sh --env <env> --ingest` deploys the ingest and backfill jobs (or, on an environment already on the registry, set `USNM_INGEST_JOBS` `true` and provision). `USNM_BACKFILL_WORKERS` defaults to 4; `USNM_BACKFILL_CRON` is an optional UTC cron (`"0 9 2-4 10 *"`; empty is manual), which every deployment keeps. The ingest writer uses the replica's disk; a full rebuild adds an NFS share of `USNM_INGEST_SCRATCH_GIB` (default 0; 128 for a rebuild, 08 §8.4) while it runs ([Full rebuild on the ingest job](rebuild-and-index-storage.md#full-rebuild-on-the-ingest-job)).

**Backfill in prod:** set `USNM_BACKFILL_ONPREM` `true` and provision (allowing creates under `curated/pages/` and Cosmos `batches`/`issues` writes), then start a worker or two. Run this as root on the Proxmox host that holds the batch container (home-infra `docs/lab.md`). Each worker drains the Cosmos queue until it is empty or out of time; a dead worker's batch returns when its lease expires.

```sh
pct exec 215 -- /usr/local/bin/usnm-ingest-run --command curate -- --enqueue --max-runtime-secs 79200
```

Set `USNM_BACKFILL_ONPREM` back to `false` when the queue is empty. Prod has no raw store, so nothing is archived.

## Archival storage

Production keeps no batch archives (ADR-0006; LoC is the source of record). The archival account keeps what an environment downloads from outside Azure (LoC batch archives and lists), so nothing is fetched twice. Only a dev experiment attaches it: it is reached through a private endpoint in the environment's VNet, and prod has none (`USNM_ARCHIVE_ACCOUNT` with `USNM_CAE_VNET=false` fails the provision).

**The account** (`infra/archive/`): own stack `usnm-archive` in `rg-usnm-archive`, so no environment's provision or teardown can delete it. StorageV2, Entra only (no keys, no SAS), no public network access, versioning, 14-day soft delete, `raw/` and `sets/` to Cold, a `CanNotDelete` lock. The stack detaches rather than deletes and denies outside deletes except of the lock and private endpoint connections. The group has the environment guard-rails (`usnm-deny-*`, `usnm-audit-*`), so policy denies turning shared keys or public network access back on (ADR-0009). Their definitions are in `usnm-guardrails`, which `deploy` creates or updates first and teardowns keep while the account exists. The diagnostics audit is waived (`usnm-archive-no-diagnostics`) until a workspace outlives the environments.

```sh
scripts/azure/archive-store.sh what-if --subscription <id>    # definitions, then the account (fails on assignments until deployed)
scripts/azure/archive-store.sh deploy --subscription <id>     # prints the account's resource id
```

**An environment uses it** with `USNM_ARCHIVE_ACCOUNT` set to that id: a provision adds `pe-usnm-archive-blob` (in the Blob private DNS zone) and, in the account's group, the custom role `usnm archive writer` on `raw` for `id-usnm-ingest` (list, read, create; no delete). With the search cluster on, `usnm-qw-bench-id-{env}` reads `raw` and writes `sets`. Jobs get `USNM_RAW_URL`. Clearing the setting removes only the environment's endpoint and roles, but the lock can block endpoint removal, so detach like this (`teardown.sh` does it itself):

```sh
scripts/azure/archive-store.sh unlock
scripts/azure/settings.sh --env dev --key USNM_ARCHIVE_ACCOUNT --unset   # private: deletes it from the settings store
scripts/azure/provision.sh --env dev
scripts/azure/archive-store.sh deploy        # the lock back
```

**What curation keeps:**

- **Archives** at `raw/{batch}/{archive file}`, byte for byte, streamed in 8 MiB blocks at Cold. Committed once the sha256 checks pass, then `raw/{batch}/manifest.json` (URL, bytes, sha256, time, LoC's `last-modified`, `etag`, `content-length`, `content-type`), then the batch is curated. A bad checksum leaves nothing. Create-only: an existing archive is never replaced; if it differs, the batch curates from its own download with a warning.
- **Reuse:** a kept batch whose sha256 matches LoC's listing (or LoC lists none) curates from the copy, with no download slot, rechecked on read. Logs: `archive source` (`source: raw` or `loc`), `archive retained`.
- **Batch lists** read by `enqueue` or `run`: `raw/listings/{time}-{sha}.json`.
- **Title records:** after each titles-sync, `reference/raw/titles.json` and the catalog (`reference/catalog/titles.json`, `places.json`) go into one create-only `raw/titles/{time}.json` (UTC), unless the newest matches. The catalog goes too because titles-sync skips titles the catalog already has and geocode keeps the catalog's ordinals and place ids. A same-second clash fails the copy; a failure warns (`the title records weren't copied to the raw store`) and the next titles-sync retries; `usnm-ingest titles-archive` makes one by hand.

Search cluster samples, sets and benches read only Azure stores.

**Re-curating older batches** to keep them: `enqueue --force` (curated or failed batches at the listed version; in-progress ones are left), then backfill. A re-curation writes a new attempt and replaces the batch's curation when it commits. A stopped run leaves batches leased up to 2 hours (`crates/usnm-ingest/src/cli/curate.rs`; `BATCH_LIMIT` is 45 minutes per batch), skipped until then. Repeated stops can hit the attempt cap (5) and fail the batch; `enqueue --force` resets it.

```sh
scripts/azure/start-job.sh --env dev --job INGEST_JOB -- enqueue --batches "$B" --force
scripts/azure/start-job.sh --env dev --job BACKFILL_JOB -- curate --max-runtime-secs 14400
```

**An environment's own `raw`** (`USNM_RETAIN_RAW true`) does the same in its data account; it is deleted with the environment, or when the setting is turned off. `USNM_ARCHIVE_ACCOUNT` wins if both are set. With both on, `archive-copy` moves archives across, checking manifests, skipping those present, failing at the end on a differing destination; check its `archives copied` line, then clear `USNM_RETAIN_RAW` and provision:

```sh
scripts/azure/start-job.sh --env dev --job INGEST_JOB -- archive-copy \
  --from "$(az storage account show -n "$(scripts/azure/settings.sh --env dev --key STORAGE_ACCOUNT)" --query primaryEndpoints.blob -o tsv)raw" \
  --batches "$B"
```

**Cost** (East US 2, October 2026): Cold $0.0036/GB-month (the 23.8 GB 1% set: about $0.09/month); read-back about $0.71 ($0.03/GB); write about $0.05; 90-day early-deletion minimum; each endpoint about $7.30/month ($0.01/h).

### Sample sets

A set is a sample in `sets/{name}/`, built once by the bench job and never replaced (next is `-v2`). `building.json` claims the name; a failed build keeps it, so the retry is the next version.

- `raw.tar`: the LoC archives and manifests from `raw/` (plain tar of bzip2 files).
- `docs.ndjson.zst`: documents as the release builds them (`text_as`, `text_as_cg` only with `american_stories`), loadable into any Quickwit 0.9 index from `infra/quickwit/pages-index.yaml`.
- `manifest.json`, last: name, version, selection, batches with bytes and sha256, page and document counts, file bytes and sha256, corpus bounds, common-word pairs' and American Stories' versions, index config sha256, builder commit, `created_at`.

```sh
J() { scripts/azure/start-job.sh --env dev --job SEARCH_CLUSTER_JOB -- "$@"; }
J bundle --name loc-1pct-v1 --sample dev1pct \
  --selection "every 100th batch of LoC's listing sorted by name, from offset 39 (30 batches)"
J load --set loc-1pct-v1 --index s1ixset          # the cluster loads straight from the set
# elsewhere: Storage Blob Data Reader on sets, plus a VNet or private endpoint path
az storage blob download --auth-mode login --account-name <archive account> -c sets -n loc-1pct-v1/docs.ndjson.zst -f docs.ndjson.zst
zstd -dc docs.ndjson.zst | split -C 8m - chunk-        # request bodies under Quickwit's 10 MiB limit
for f in chunk-*; do curl -sf -XPOST "$QW/api/v1/$INDEX/ingest" --data-binary @"$f"; done
```

Check each file against the manifest's sha256 (`shasum -a 256`). `tar -xf raw.tar` gives the `raw/` layout.

## Dev environment lifecycle

**The rule** (#275): `dev` (stack `usnm-dev`, group `usnm-core-rg-dev`) is empty unless a focused experiment runs. Scaled to zero it still costs about $2/day: private endpoints, load balancer and public IP, ACR Basic, the NFS share, private DNS zones and storage (October 2026).

**The marker.** Provisioning `dev` needs `USNM_EXPERIMENT` (issue number), which tags `usnm-core-rg-dev` with `experiment` and `started` (UTC, kept across reprovisions). `scripts/azure/dev-status.sh` lists the stack, `usnm-core-rg-dev`, `usnm-core-infra-rg-dev`, `rg-usnm-dev-spot` (and legacy `rg-usnm-dev`, `ME_cae-usnm-dev_*`, ADR-0017), the tag's age and cost since `started`.

- Exits 1 for untagged resources; warns past 7 days. No `started`: unknown start, month-to-date cost. Unreadable group: error.
- A Cost Management 429 gets up to four tries (2, 4, 8 s, honouring `Retry-After`), waits capped at 60 s in all, then a stderr warning that cost is unknown (never zero). Missing Cost Management Reader warns; other errors fail.
- `budget-usnm-dev-groups` (guard-rail stack) mails alert recipients at $5 actual monthly spend on dev's groups (both name forms). It needs no `dev`, only an environment with `USNM_ALERT_EMAILS` provisioned since. Each `dev` provision records its `AZURE_LOCATION` for the legacy group name `ME_cae-usnm-dev_rg-usnm-dev_<region>`, and other environments' provisions keep it.

**Stand it up:**

```sh
scripts/azure/settings.sh --env dev --key USNM_SEARCH_CLUSTER --value true   # the experiment's own settings first
scripts/azure/stand-up.sh --env dev --experiment 240 [--archive]
```

Public settings, including `USNM_USE_ACR`, `USNM_COSMOS_FREE_TIER` and `USNM_SEARCH_CLUSTER` written by `stand-up.sh`/`teardown.sh`, land in `infra/env/dev.json`; commit the experiment's in its pull request. Both scripts skip provision's own uncommitted-settings check, but `stand-up.sh` itself refuses a checkout with uncommitted changes. `stand-up.sh`:

1. Starts from the last teardown's `.env.deleted-<time>`, or fresh. Refuses `prod`.
2. Provisions without images (about 7 minutes). On a first stand-up, `DeploymentStackNotFound` or `ResourceNotFound` naming the stack is accepted; other inventory errors block the protected-resource guard.
3. Copies the pinned Quickwit image, builds API and ingest images (`exp-<commit>`; Japanese OCR with `USNM_JA_OCR_JOB`), sets `USNM_IMAGE_TAG` and `USNM_USE_ACR`, provisions again.
4. With `--archive`, sets `USNM_ARCHIVE_ACCOUNT` from `usnm-archive` and, with `USNM_INGEST_JOBS`, seeds the titles.

It never adds `dev` to `USNM_DEPLOY_ENVIRONMENTS` or creates a GitHub Environment, so CI never publishes main to it; rerun for a newer commit. (`bootstrap.sh --env dev` would wire dev to CI's publish.)

**Data:** search experiments load a set ([Sample sets](#sample-sets)) straight into Quickwit. Curation or release experiments enqueue its batches; from the archive, the 1% (30 batches, 23.8 GB) curates in under an hour on 4 workers. Releases need the catalog; titles-sync is LoC-paced (up to 8 hours), so seed from the archive's copy and fetch only missing titles.

**Seeding the titles.** `scripts/azure/titles-seed.sh --env dev` (`--wait-mins`, default 20) runs `usnm-ingest titles-seed`, which reads the newest `raw/titles/{time}.json`, checks it (every record; catalog both files or neither, unique ids and ordinals, every place listed; failure writes nothing), and writes the catalog then `raw/titles.json`, create-only. An existing record cache is never replaced; a partial seed finishes on rerun; a differing catalog file stops it before the cache.

```sh
scripts/azure/titles-seed.sh --env dev                       # again, or after turning the ingest job on
scripts/azure/logs.sh --env dev --query-name titles-archive --span 1h
```

**ACR build verification** (#549). `stand-up.sh`, `build-quickwit.sh`, `build-boinc-images.sh` and `boinc-app-release.sh` use `scripts/lib/acr-build.sh` (by hand: source it, `acr_build_verified az REGISTRY REPOSITORY:TAG TIMEOUT [build arguments]`). It queues with `--no-wait`, polls `az acr task show-run` every 10 s up to the timeout plus 600 s (`ACR_POLL_INTERVAL`, `ACR_WAIT_SECONDS`), and needs `Succeeded` plus a matching SHA-256 digest. It fails closed on no run ID, a failed or canceled run, an unreadable run at the deadline, or a missing or mismatched digest; an existing tag proves nothing. A local timeout doesn't cancel the remote run: check the printed run ID before rebuilding.

**Stand-up cost and time:** a few cents of builds; reading the 1% from Cold about $0.71 plus $0.24 endpoint processing; 40 to 60 minutes to running images; data from minutes (set, title seed) to an hour (1% curation) or 8 hours (titles-sync with no usable copy).

**Tear it down** after saving `qw-bench/runs/` reports and logs. If titles-sync ran with the archival account, save its titles copy first; the `&&` stops the teardown unless it exits 0:

```sh
scripts/azure/start-job.sh --env dev --job INGEST_JOB --wait-mins 20 -- titles-archive &&
  scripts/azure/teardown.sh --env dev &&
  scripts/azure/dev-status.sh                                # expect: dev holds nothing
```

Without an ingest job or `USNM_ARCHIVE_ACCOUNT` there is no titles copy to save: run `teardown.sh --env dev` and `dev-status.sh` directly. The copy's exit 1 means `Failed`, `Stopped`, `Degraded` or still running after 20 minutes: check `scripts/azure/logs.sh --env dev --query-name job-executions` (without the archive it fails with `titles-archive needs USNM_RAW_URL`), fix it and run it again; it is create-only.

`teardown.sh`, rerunnable from where it stopped:

1. With the archive set: unlocks, clears `USNM_ARCHIVE_ACCOUNT` and `USNM_SEARCH_CLUSTER`, provisions, verifies no dev role or endpoint connection remains, relocks (`archive-store.sh deploy`) whatever happens. Account data is untouched.
2. Removes `dev` from `USNM_DEPLOY_ENVIRONMENTS`, waiting for any `ci` publish that could reach it.
3. Detaches the stack and deletes its groups, custom roles and outside role assignments; keeps `usnm-guardrails` while another environment or `usnm-archive` uses it; deletes the `dev` GitHub Environment; keeps settings as `.env.deleted-<time>` without the marker.

Retired `rg-usnm-dev-spot` is undeclared; if detached, see [Spot group cleanup](../../infra/README.md#retired-spot-resource-group). `az deployment group delete -g rg-usnm-archive -n archive-grant-dev` drops the stale record.

**Protected removals.** With `USNM_SEARCH_LOCAL_MODE=nfs` and `USNM_INGEST_SCRATCH_GIB=0`, the detach also removes the scratch account and file private-DNS zone: check the guard output and pass `--allow-rename=<exact-old-name>` for each. A missing, failed or unsupported plan blocks regardless, the lock is restored, and the detach marker allows an approved rerun.

**After teardown:** the workspace stays soft-deleted 14 days (free; a stand-up recovers it); resource names come back the same (`uniqueString(subscription, env)`). Dev's Cosmos state, curated parts, indexes and `qw-bench` go; the archive keeps LoC downloads. Teardown is free and takes 25 to 40 minutes, plus about 10 for the archive's provision. Keep dev's local settings in `~/.config/usnewsmap/dev/.env`, not per-worktree `.azure/dev/.env`.

## Search cluster experiment

An experimental Quickwit cluster on Container Apps (#238, #239, both closed) on a 1% sample: 1 and 2 indexers, 1 to 3 searchers. `dev` only, behind `USNM_SEARCH_CLUSTER` (`if (searchCluster && useAcr && env != 'prod')`). Its results, and those of the two tests below, are in [the searcher notes of 9 and 10 October 2026](../notes/2026-10-10-searcher/README.md) and on #251.

**What it deploys** (`infra/modules/searchcluster.bicep`):

- **Nodes** `usnm-qw-{i}-ca-{env}`: `USNM_SEARCH_CLUSTER_NODES` (0 to 4) node apps, each exactly one replica of `USNM_SEARCH_CLUSTER_NODE_VCPU` (default 2), 2 GiB per vCPU, Consumption. Node 0 is sole metastore (file-backed in `qw-cluster`), control plane, janitor, indexer, searcher and root (`http://usnm-qw-0-ca-{env}`); nodes up to `USNM_SEARCH_CLUSTER_INDEXERS - 1` also index, and the rest only search. Config `infra/quickwit/cluster-node.yaml` (a test matches it to `searcher.yaml`).
- **Addresses:** replica IPs change on restart (#239), so `usnm-qwcluster node` writes its IP to `qw-bench/seeds/qw-{i}.json`, reads the others' as `QW_PEER_SEEDS`, sets `QW_ADVERTISE_ADDRESS`, then `exec`s Quickwit with the node config filled in. Every node writes before it reads, so of two nodes starting together at least one sees the other.
- **Bench job** `usnm-qw-bench-caj-{env}` (manual, 4 vCPU / 8 GiB, 6 h).
- **Storage, Entra only:** `qw-cluster` (Blob Data Contributor per node system identity) and `qw-bench` (Contributor for `usnm-qw-node-id-{env}`, `usnm-qw-bench-id-{env}`; bench also reads `curated`, `reference`). Nothing writes `qw-index`, `reference` or `curated`. Off deletes both (14-day soft delete).
- **Threads:** one main-runtime and one search-pool thread per vCPU (`QW_TOKIO_RUNTIME_NUM_THREADS`, `RAYON_NUM_THREADS`); override with `USNM_SEARCH_CLUSTER_RUNTIME_THREADS`, `USNM_SEARCH_CLUSTER_SEARCH_THREADS` (0: per vCPU).

**The steps** each log a `qwcluster report` line and store the report in `qw-bench/runs/`.

| Step      | Command                                                       | What it measures                                                                                                                                                                                                    |
| --------- | ------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `sample`  | `sample --name N --pct P [--american-stories]`                | `P`% of pages (SHA-256 of `doc_id` mod 10,000 below `P` × 100), built and deduplicated as a release does; reads every curated part once.                                                                            |
| `load`    | `load --sample N --index I [--split-docs 3000] [--senders 4]` | Creates `I` from `pages-index.yaml` with `split_num_docs_target` set, one shard per indexer; times send, commit, merges, sealed; docs per node, splits, footers; retries 429/503.                                   |
| `bench`   | `bench --label L --index I --sample N --expect-searchers K`   | 13 searches of `bench-cold-searches.py`/`load-cold-searches.py` via `plan::aggregate` on node 0: `first`, `warm`, concurrency 1, 2, 4, 10, each with windows a day further back. Median, p90, throughput, per node. |
| `members` | `members`                                                     | Node 0's view: members, addresses, generations, services.                                                                                                                                                           |
| `dump`    | `dump [--hold-secs 600]`                                      | Prints stored reports when Log Analytics is capped.                                                                                                                                                                 |

The default 3,000 pages per split gives the 1% about 80 splits; a second index at `--split-docs 30000` brackets the result.

**Run it** in `dev` (prod's data is private). These helpers are used by every recipe in this section:

```sh
S() { scripts/azure/settings.sh --env dev --key "$1" --value "$2"; }; P() { scripts/azure/provision.sh --env dev --allow-dirty; }
J() { scripts/azure/start-job.sh --env dev --job SEARCH_CLUSTER_JOB -- "$@"; }
RG=$(scripts/azure/settings.sh --env dev --key AZURE_RESOURCE_GROUP); ACR=$(scripts/azure/settings.sh --env dev --key ACR_NAME)
JOB=$(scripts/azure/settings.sh --env dev --key SEARCH_CLUSTER_JOB)
wait_for() { local s; while :; do s=$(az containerapp job execution show -n "$JOB" -g "$RG" --job-execution-name "$1" --query properties.status -o tsv); case "$s" in Running|Processing|Unknown|"") sleep 20 ;; *) echo "$1 $s"; return ;; esac; done; }  # until execution $1 ends
restart_app() { az containerapp revision restart -n "$1" -g "$RG" --revision "$(az containerapp revision list -n "$1" -g "$RG" --query "[?properties.active].name | [0]" -o tsv)"; }
. scripts/lib/acr-build.sh
```

```sh
scripts/azure/archive-store.sh deploy                       # once per subscription; prints the account's id
S USNM_INGEST_JOBS true; S USNM_INGEST_SCRATCH_GIB 0        # 1% fits the replica's disk
S USNM_LOG_DAILY_CAP_GB 1                                   # blob write logs; clear afterwards
S USNM_ARCHIVE_ACCOUNT "<the id it printed>"; P             # private: the settings store
TAG=qwc-$(git rev-parse --short HEAD)
acr_build_verified az "$ACR" "usnewsmap-ingest:$TAG" 3600 -f docker/Dockerfile.ingest --build-arg USNM_GIT_SHA=$(git rev-parse HEAD) .
acr_build_verified az "$ACR" "usnewsmap-api:$TAG" 3600 -f docker/Dockerfile.api .
S USNM_IMAGE_TAG $TAG; S USNM_USE_ACR true; P

# The 1%: every 100th batch of LoC's listing by name, from offset 39:
# 30 batches, 238,057 pages of 23,838,683 (0.999%), 26 awardees, 23.8 GB of archives.
B=arhi_beatles_ver01,az_fireant_ver01,ct_fairfield_ver01,curiv_plasse_ver01,dlc_alpha_ver03,dlc_debaptiste_ver01,dlc_goldenrod_ver01,dlc_leibovitz_ver01,dlc_saluki_ver01,gu_drteeth_ver01,iahi_hypno_ver01,in_irvington_ver01,khi_garwood_ver02,lu_juggernaut_ver01,me_calais_ver03,mnhi_dassel_ver01,mohi_berenice_ver01,mthi_goldeneye_ver01,ncu_cotton_ver04,nhd_lafayette_ver01,nn_keddy_ver01,ohi_himilco_ver01,oru_longspur_ver01,rp_hobgoblin_ver02,tu_eddie_ver02,uuml_anderson_ver01,vi_fezza_ver01,vnstcsc_duggan_ver01,whi_brie_ver01,wvu_jolie_ver02
scripts/azure/start-job.sh --env dev --job INGEST_JOB -- enqueue --batches $B --force   # re-curate, now kept
scripts/azure/start-job.sh --env dev --job BACKFILL_JOB -- curate --max-runtime-secs 14400
scripts/azure/start-job.sh --env dev --job INGEST_JOB -- run --batches $B --full --curate-max-runtime-secs 21600 \
  --titles-max-runtime-secs 28800 --quickwit-bin /usr/local/bin/quickwit \
  --quickwit-metastore azure://qw-index --quickwit-index-root azure://qw-index
scripts/azure/logs.sh --env dev --query-name release-progress --span 6h    # single-writer baseline

S USNM_SEARCH_CLUSTER true; S USNM_SEARCH_CLUSTER_NODES 1; S USNM_SEARCH_CLUSTER_INDEXERS 1; P   # one node
J members
J sample --name dev1pct --pct 100          # dev's whole version is the 1%
J load --sample dev1pct --index s1ix
J bench --label n1 --index s1ix --sample dev1pct --expect-searchers 1
S USNM_SEARCH_CLUSTER_NODES 2; S USNM_SEARCH_CLUSTER_INDEXERS 2; P                              # two indexers
J load --sample dev1pct --index s2ix
J bench --label n2 --index s1ix --sample dev1pct --expect-searchers 2
S USNM_SEARCH_CLUSTER_NODES 3; P                                                                # node 2 only searches
J bench --label n3 --index s1ix --sample dev1pct --expect-searchers 3
restart_app usnm-qw-2-ca-dev
J bench --label n3-restart --index s1ix --sample dev1pct --expect-searchers 3 --levels 1,4 --levels-only
J members                                  # qw-2 has a new address and generation
scripts/azure/qwcluster-report.py --env dev --since 2d --out qwcluster-report.md   # Log Analytics, Azure Monitor, list prices
```

- Start each step after the last execution ends (`az containerapp job execution list -n "$JOB" -g "$RG" -o table`); restart node revisions for a cold `first`. Node 0 may restart while its `qw-cluster` role propagates.
- Over the Log Analytics cap: `J dump --hold-secs 600`, `az containerapp job logs show -n <job> -g "$RG" --container qwbench --follow > lines.txt`, then `qwcluster-report.py --env dev --from-file lines.txt`. Collect before scaling down; deleted apps lose metrics.
- Console lines cap at about 16 KiB, so reports split into `bench`, `bench_pass`, `bench_probe` and `bench_profile` lines; `J dump` re-logs stored ones.
- Idle: `USNM_SEARCH_CLUSTER_NODES 0` and a provision (storage only). Remove: `USNM_SEARCH_CLUSTER ""` and a provision. Clear `USNM_LOG_DAILY_CAP_GB`, then tear down `dev`.

### Quickwit version comparison

Tested whether a pre-0.9 Quickwit searches our splits with less work than 0.9.1 (#251; quickwit-oss/quickwit#6883). The baseline is the `qw-azure-fix` nightly (`quickwit/quickwit:qw-azure-fix@sha256:d4f75ebc…`, main of 23 September 2025 plus the Azure multipart fix), which reads the 0.9 metastore and accepts every `searcher.yaml` key except `predicate_cache_capacity` and `leaf_request_timeout_secs`. On our splits 0.9.1 was about 3 times faster ([version comparison](../notes/2026-10-10-searcher/version-comparison.md)).

`USNM_SEARCH_COMPARE_TAGS` (comma-separated ingest tags) deploys one standalone searcher per tag, `usnm-qw-cmp-{i}-ca-{env}` (root `http://usnm-qw-cmp-{i}-ca-{env}`), at `USNM_SEARCH_CLUSTER_NODE_VCPU`: polling `qw-cluster`'s metastore every 30 s, Blob Data Reader only, own cluster id, no seeds. Below 0.9 the wrapper drops the two keys and pins `warmup_single_split_initial_allocation` to 300 MB. The bench records version, main-runtime busy time and Blob bytes per pass. The second image uses `docker/Dockerfile.ingest`'s `QUICKWIT_IMAGE`. With the helpers above, on `s1ixb` (1% without American Stories), cluster nodes off:

```sh
SHA=$(git rev-parse HEAD); TAG=qwc-$(git rev-parse --short HEAD)
NIGHTLY=quickwit/quickwit:qw-azure-fix@sha256:d4f75ebc6d6ee7db4732b8a20be544ba3455bdc08ef0ccd7683f4aa854243ff6
acr_build_verified az "$ACR" "usnewsmap-ingest:$TAG" 3600 -f docker/Dockerfile.ingest --build-arg USNM_GIT_SHA=$SHA .
acr_build_verified az "$ACR" "usnewsmap-ingest:$TAG-qw2509" 3600 -f docker/Dockerfile.ingest --build-arg USNM_GIT_SHA=$SHA --build-arg QUICKWIT_IMAGE=$NIGHTLY .
acr_build_verified az "$ACR" "usnewsmap-api:$TAG" 3600 -f docker/Dockerfile.api .
S USNM_IMAGE_TAG $TAG; S USNM_SEARCH_CLUSTER true; S USNM_SEARCH_CLUSTER_NODES 0
S USNM_SEARCH_CLUSTER_NODE_VCPU 4; S USNM_SEARCH_COMPARE_TAGS "$TAG,$TAG-qw2509"; P
restart() { restart_app usnm-qw-cmp-0-ca-dev; restart_app usnm-qw-cmp-1-ca-dev; sleep 90; }
bench() { wait_for "$(J bench --label "$1" --cluster "http://usnm-qw-cmp-$2-ca-dev" --index s1ixb --sample dev1pct --levels 1,4,10)"; }
restart; bench cmp-v091-r1 0; bench cmp-n2509-r1 1
restart; bench cmp-n2509-r2 1; bench cmp-v091-r2 0
restart; bench cmp-v091-r3 0; bench cmp-n2509-r3 1
scripts/azure/qwcluster-report.py --env dev --since 6h --out qwcompare.md
```

The report's **Quickwit version comparison** section gives main-runtime busy seconds per search and per GB, ratios to 0.9.1, and whether both found the same pages (if not, timings don't compare). `J load ... --as-from-text` fills `text_as`/`text_as_cg` from LoC's text for production-shaped indexes. Afterwards collect the report, then `USNM_SEARCH_COMPARE_TAGS ""` and provision; two 4 vCPU searchers cost about $0.86/h.

### Local-disk test

Tested whether reading splits from somewhere other than Blob removes the main runtime's CPU cost (#251). One 0.9.1 searcher, `usnm-qw-local-ca-dev`, 4 and 4 threads, one `USNM_SEARCH_LOCAL_MODE` at a time:

| Mode    | Splits from                                                                                                                                                                    | Runs on                                |
| ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------- |
| `blob`  | Blob                                                                                                                                                                           | Consumption, 3.75 vCPU / 7.5 GiB       |
| `nfs`   | `USNM_SEARCH_LOCAL_INDEX` copied once (`crates/usnm-ingest/src/cluster/local.rs`) to NFS share `qw-search` (`USNM_SEARCH_NFS_GIB`, default 64) in the ingest scratch's account | Consumption plus 0.25 vCPU init, 4 / 8 |
| `cache` | Quickwit's split cache (`searcher.split_cache`, `USNM_SEARCH_LOCAL_CACHE_GIB`, default 40)                                                                                     | E4, 3.25 vCPU / 24 GiB                 |
| `copy`  | the index copied to local disk before start, `file://`, no Blob client                                                                                                         | E4, 3.25 vCPU / 24 GiB                 |

- `cache` and `copy` need more than Consumption's 8 GiB of disk and VMs are denied (`usnm-deny-iaas-compute`), so they use the E4 profile (`USNM_DEDICATED_PROFILE`). Run one E4 mode at a time with a provision between: a new revision can't start while the old one holds E4's one node, and the old one keeps the traffic. The `cache` and `copy` runs of 9 October 2026 never started and are void. `--variant blob|nfs|cache|copy` reads `/api/v1/config` and stops if the searcher isn't in that mode; give each run its own `--offset`.
- `--probe` repeats each search over gRPC for `SearchResponse.resource_stats` (`crates/usnm-ingest/src/cluster/grpc.rs`); `--profile-secs` stores a `/api/developer/pprof` flame graph at `runs/{label}/flamegraph.svg`; `--wait-split-cache-secs` waits for a full cache.

With the helpers above and an image with these flags in `USNM_IMAGE_TAG`:

```sh
lbench() { wait_for "$(J bench --label "$1" --cluster http://usnm-qw-local-ca-dev --index "$IX" --sample dev1pct --variant "$2" --levels 1,4,10 --probe --profile-secs 120 "${@:3}")"; }
ready() { until [ "$(az containerapp show -n usnm-qw-local-ca-dev -g "$RG" --query "properties.latestReadyRevisionName == properties.latestRevisionName" -o tsv)" = true ]; do sleep 30; done; }  # a first nfs copy takes minutes
S USNM_SEARCH_CLUSTER_NODES 0; scripts/azure/settings.sh --env dev --key USNM_SEARCH_COMPARE_TAGS --unset
IX=s1ixb; S USNM_SEARCH_LOCAL_INDEX $IX
S USNM_SEARCH_LOCAL_MODE blob; P; ready
lbench c-blob-r1 blob --offset 200; lbench c-blob-r2 blob --levels-only --offset 210
S USNM_SEARCH_LOCAL_MODE nfs; P; ready
lbench c-nfs-r1 nfs --offset 220; lbench c-nfs-r2 nfs --levels-only --offset 230
scripts/azure/qwcluster-report.py --env dev --since 12h --out qwlocal.md
scripts/azure/settings.sh --env dev --key USNM_SEARCH_LOCAL_MODE --unset; P   # removes the searcher and the share
```

The report groups runs by mode with ratios to `blob`; compare the `r2` runs. The flame graphs show which functions the seconds went to. The [Blob, NFS and patched-Quickwit report](../notes/2026-10-10-searcher/blob-nfs-patched.md) led to the patched build in [The Quickwit image](search-serving.md#the-quickwit-image).
