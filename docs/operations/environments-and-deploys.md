# Environments and deploys

Part of the [operations guide](README.md).

## Settings

An environment's settings live in three places ([`infra/README.md`](../../infra/README.md#settings-and-the-deployment-stack), #295), and [`infra/env/schema.json`](../../infra/env/schema.json) says which setting goes where:

- **Public choices** (flags, sizes, caps, schedules, most `USNM_*` settings) are committed in `infra/env/<env>.json`. A change to a shared environment goes through a pull request like code, and `scripts/provision.sh` deploys only committed settings:

  ```sh
  scripts/settings.sh --env prod --key USNM_JA_LATIN --value true   # edits infra/env/prod.json
  git switch -c settings/prod-ja-latin && git commit -am "prod: USNM_JA_LATIN on" && git push -u origin HEAD
  # open the pull request; once it's merged:
  git switch main && git pull
  scripts/provision.sh --env prod
  ```

  CI's "stack change needs a provision" warning on the merge stays until you run that provision. Every `scripts/settings.sh` example below that sets a public setting works this way. In dev, an experiment's settings go in its own pull request; to try one before committing it, `scripts/provision.sh --env dev --allow-dirty`.

- **Private settings** (`USNM_ALERT_EMAILS`, `USNM_SEARCH_LOG_READERS`, `USNM_OCR_USERS`, `USNM_OCR_NODE_IPS`, `USNM_OCR_NODE_GROUP`, `USNM_OCR_KEYVAULT_ADMINS`, `USNM_ARCHIVE_ACCOUNT`, `USNM_VNET_ADDRESS_PREFIX`, the certificate ids and `USNM_GITHUB_REPO_IDS`) are in the **settings store**, one Azure App Configuration store for every environment in the subscription, `usnm-config-appcs-<suffix>` in `usnm-config-rg`, each environment's under its own label (`dev`, `prod`, ...). The key is the setting's name. Only the operators can reach it, with Entra ID (the store has no access keys); CI never reads it. `scripts/settings.sh` reads and sets them with your sign-in (in the store, removing any local copy), or `az` does directly:

  ```sh
  scripts/settings.sh --env prod --key USNM_ALERT_EMAILS --value ops@example.org
  # or: store=$(scripts/settings.sh --env prod --key SETTINGS_STORE_ENDPOINT)
  #     az appconfig kv set --endpoint "$store" --auth-mode login --label prod --key USNM_ALERT_EMAILS --value ops@example.org --yes
  scripts/provision.sh --env prod
  ```

  `scripts/settings.sh --env prod --key K --unset` clears a private setting everywhere, so it takes its default: it deletes the key from the store and any local copy. `--unset-local` removes only the local file's entry, so the store's value applies again (provision suggests it when a key is in both places). Until the store exists, private settings stay in the local file; once it does, `scripts/settings.sh --env <env> --migrate` moves them in (it stops, moving none, if the store already has a different value for one), and every provision until then warns. Every script that sets a private setting writes it to the store, only if the value changed, and removes any local copy, so a re-run never leaves it in two places (`bootstrap.sh` sets the repository's ids every run, and the certificate ids and alert addresses; `group.sh` sets the nodes' group; `settings.sh` sets the one you give it). An empty value stays a local `""`, which hides the store's value (`teardown.sh` detaches the archive that way); `--migrate` leaves those.

  **The store's lifecycle.** It sits outside every environment's stack, in its own stack `usnm-config` (`infra/settings/main.bicep`), so no provision or teardown of an environment deletes it: `teardown.sh` keeps the environment's private settings under its label, and the next stand-up reads them again. The first provision in a subscription creates it (`scripts/settings-store.sh --deploy`, in that environment's region, registering the `Microsoft.AppConfiguration` provider first if the subscription lacks it), and so does `bootstrap.sh` through its provision; it then waits up to 15 minutes for its new role on the store to apply. `scripts/settings-store.sh --what-if` shows what a deployment would change. It is on the Free tier: $0, 10 MB, 1,000 requests a day. A provision reads an environment's settings in one request and each script's single read is one more, so the cap is far off. The Free tier has no soft delete: a deleted key is gone.

  **Who can read the store.** App Configuration Data Owner goes to the Entra object ids in the store stack's admin list and to the account that last deployed it. The role covers every label: one maintainer operates every environment, so per-environment access isn't worth a store each. A provision reads the store before it deploys anything, and an operator without access stops there with the commands that fix it. To add someone, an operator who has access adds their object id (`az ad signed-in-user show --query id -o tsv`, run by them); `--admins` replaces the list, so give everyone's:

  ```sh
  scripts/settings-store.sh --deploy --admins "<id>,<id>"
  ```

  Without `--admins`, `--deploy` keeps the list it has. With no operator left, an Owner of the subscription grants the role to themselves on the store's group (not on the store, where it would collide with the stack's assignment), adds themselves to the list as above, then deletes that grant: `az role assignment create --assignee-object-id "$(az ad signed-in-user show --query id -o tsv)" --assignee-principal-type User --role "App Configuration Data Owner" --scope "/subscriptions/<subscription id>/resourceGroups/usnm-config-rg"`. On a new machine, whose local file has no `SETTINGS_STORE_ENDPOINT`, every script that reads or writes a private setting (not only provision) takes the endpoint from the store's stack and records it, so none of them mistakes a store it can't find for an unset setting.

  **Why App Configuration, not Key Vault.** These are configuration, not credentials (ADR-0009: there are none), and the Free tier costs nothing. #295 first built a Key Vault per environment; prod's never got past preflight, because the public-network guard-rail policy covers Key Vaults and its exemption was in the same deployment as the vault. No guard-rail definition covers App Configuration, so the store needs no exemption.

- **Everything else** stays in the local file (`~/.config/usnewsmap/<env>/.env` or `.azure/<env>/.env`): the stack's outputs, which every provision rewrites, the subscription and tenant, and what the scripts keep (`USNM_EXPERIMENT`, `USNM_IMAGE_TAG`).

A setting in the wrong place, or in two, stops a provision before it deploys anything. **Moving an environment from before #295** (its public settings are still in the local file) is one command, then a pull request:

```sh
scripts/settings.sh --env prod --migrate   # moves the public settings into infra/env/prod.json
git diff infra/env/prod.json               # check, commit, pull request, merge
scripts/provision.sh --env prod            # creates the settings store if the subscription has none
scripts/settings.sh --env prod --migrate   # moves the private settings into it
```

`scripts/provision.sh --env <env> --what-if` deploys nothing: it reads the settings as a provision would, runs Azure's preflight validation of the stack (a policy denial fails it) and calls GA `az stack-whatif sub create` to preview the stack, including unmanaged-resource and deny-setting changes. In a subscription without the guard-rail stack yet, it prints that stack's changes instead: the environment's preflight needs its policy definitions. What-If results can be retained from `PT1H` to `PT3H`; the scripts request `PT3H`.

Known residual What-If noise includes provider normalization: Static Web Apps provider-generated properties, diagnostic-setting destination defaults, and Azure Files soft-delete retention defaults. Compare each result with the deployed resource and template before classifying it; don't change the template merely to silence a provider-generated/default difference. Always review actual deletions and deny-setting changes; `--only-show-errors` is not a substitute for examining the preview. [Microsoft's deployment-stacks What-If documentation](https://learn.microsoft.com/en-us/azure/azure-resource-manager/bicep/deployment-stacks-what-if) describes the supported preview and retention behavior.

## Deploy the jobs

`scripts/bootstrap.sh --env <env> --ingest` deploys both jobs (or `scripts/settings.sh --env <env> --key USNM_INGEST_JOBS --value true` and `scripts/provision.sh --env <env>` on an environment already on the registry). `USNM_BACKFILL_WORKERS` sets the backfill's workers (default 4), and `USNM_BACKFILL_CRON` an optional schedule for it (a UTC cron such as `"0 9 2-4 10 *"`; empty is manual), which every deployment keeps. The ingest job's Quickwit writer works on the replica's own disk, which holds a weekly delta; a full rebuild adds an NFS share for it, `USNM_INGEST_SCRATCH_GIB` in size (default 0, none; 128 for a rebuild, 08 §8.4), only while the rebuild runs ([Full rebuild](rebuild-and-index-storage.md#full-rebuild)). The weekly schedule comes after the first release ([Weekly updates](catalog-and-releases.md#weekly-updates)).

## Archival storage

Production keeps no batch archives (ADR-0006): LoC is the source of record. The archival account keeps everything we download from outside Azure, so re-curation, benchmarks and other environments never download it again: LoC's batch archives and batch lists now, other sources later. Benchmarks are one use.

**The account** (`infra/archive/`) is its own deployment stack, `usnm-archive`, in its own group, `rg-usnm-archive`, outside every environment's stack. An environment's provision or teardown can't delete it. It is StorageV2, Entra only (no keys, no SAS), with no public network access, versioning, 14-day soft delete, a lifecycle rule that moves `raw/` and `sets/` to the Cold tier, and a `CanNotDelete` lock. Its stack detaches what leaves the template rather than deleting it, and denies deletes outside the stack except of the lock and of private endpoint connections.

**Policy keeps it that way.** The group, `rg-usnm-archive`, has the same guard-rail assignments as an environment's groups (`usnm-deny-*`, `usnm-audit-*`), with public network access denied and no public accounts, so policy denies turning shared keys or public network access back on (ADR-0009). The definitions come from the `usnm-guardrails` stack, which `deploy` creates or updates first, as an environment's provision does. While the account exists, the last environment's teardown keeps that stack. The diagnostic settings audit has an exemption for the account (`usnm-archive-no-diagnostics`, a waiver): its logs have nowhere to go, since every Log Analytics workspace belongs to an environment and goes with its teardown. Once a workspace that outlives the environments exists, the account should send its blob writes and deletes there, and the exemption goes.

Check what a deploy would change, then deploy:

```sh
scripts/archive-store.sh what-if --subscription <id>    # changes nothing
scripts/archive-store.sh deploy --subscription <id>     # prints the account's resource id
```

`what-if` shows the guard-rail definitions' changes, then the account's. Until the definitions exist, the account's what-if can fail on its assignments; `deploy` creates them first.

**An environment uses it** with `USNM_ARCHIVE_ACCOUNT` set to that resource id and a provision. That adds a private endpoint in the environment's VNet (`pe-usnm-archive-blob`, in its Blob private DNS zone) and, in the account's group, a custom role on `raw` for `id-usnm-ingest` that lists, reads and creates blobs but can't delete them (`usnm archive writer`). With the search cluster on, the bench identity (`usnm-qw-bench-id-{env}`) also gets read access on `raw` and the writer role on `sets`. The ingest and backfill jobs get `USNM_RAW_URL` pointing at `raw`. Clearing the setting removes only the environment's endpoint and roles. The account's lock can block removing an endpoint to it, so drop the setting this way. `scripts/teardown.sh` detaches an environment that has the setting on by itself first ([Dev environment lifecycle](#dev-environment-lifecycle)).

```sh
scripts/archive-store.sh unlock
scripts/settings.sh --env dev --key USNM_ARCHIVE_ACCOUNT --unset   # a private setting: deletes it from the settings store
scripts/provision.sh --env dev
scripts/archive-store.sh deploy        # the lock back
```

**What curation does with it:**

- **Every archive it downloads is kept** byte for byte as fetched, at `raw/{batch}/{archive file}`. It is written in 8 MiB blocks as it streams (never held whole) at the Cold tier. Once the archive's sha256 checks out, the upload is committed and `raw/{batch}/manifest.json` records the source URL, bytes, sha256, the time and LoC's response headers (`last-modified`, `etag`, `content-length`, `content-type`). Only then is the batch marked curated. A download that fails its checksum leaves nothing. The commit is create-only: an archive already at the path (another environment's, sharing the account) is never replaced, and the batch curates from its own download with a warning if the two differ.
- **A batch already kept is curated from the copy,** not LoC, when its manifest's sha256 is the one LoC lists (or LoC lists none). It needs no download slot, and the copy is checked again as it's read. The `archive source` line says which (`source: raw` or `loc`), and `archive retained` logs each new copy.
- **Batch lists are kept too:** each remote list an `enqueue` or `run` reads goes to `raw/listings/{time}-{sha}.json`.
- **So are the title records.** Titles-sync keeps the fields it uses from each title record in the environment's `reference/raw/titles.json`, and geocode builds the catalog (`reference/catalog/titles.json`, `places.json`) from them. After each titles-sync (on its own or in `run`), the ingest job saves all three as one create-only copy, `raw/titles/{time}.json` (UTC, as `listings/`), unless the newest copy there holds the same. If another run took the same second's name with different content, this copy fails (and the next titles-sync saves its own). The catalog goes with the records because titles-sync skips titles the catalog already has, and geocode keeps the catalog's ordinals and place ids. A copy that fails is a warning (`the title records weren't copied to the raw store`), not a failed sync, and the next titles-sync tries again; `usnm-ingest titles-archive` makes one by hand. A new environment starts from the newest copy ([Dev environment lifecycle](#dev-environment-lifecycle)). This uses the ingest identity's existing writer role on `raw` (list, read, create); nothing new is granted.

Nothing else is downloaded from outside Azure: the search cluster's sample, sets and bench read only Azure stores.

To keep the archives of batches curated before, queue them again with `--force` (only curated or failed batches at the listed version; one being curated is left alone), then run the backfill job. A re-curation writes a new attempt and replaces the batch's curation when it commits.

Stopping a curate run mid-batch leaves its batches leased for up to 2 hours (the curate worker's lease, set in `crates/usnm-ingest/src/main.rs`; the 45-minute `BATCH_LIMIT` is a running batch's own limit): `--force` leaves a leased batch alone and a new run skips it until the lease runs out. A batch whose runs are stopped repeatedly can reach the attempt cap (5) and be marked failed at its next claim; `enqueue --force` resets its attempts. Found on the dev 1% run (October 2026): after a stopped run, 8 batches needed two more passes.

```sh
scripts/start-job.sh --env dev --job INGEST_JOB -- enqueue --batches "$B" --force
scripts/start-job.sh --env dev --job BACKFILL_JOB -- curate --max-runtime-secs 14400
```

**An environment's own `raw` container** (`USNM_RETAIN_RAW true`) does the same inside the environment's data account. It goes with the environment, and turning the setting off deletes it, so it suits a short trial. `USNM_ARCHIVE_ACCOUNT` takes precedence when both are set. To move archives kept there into the archival account, with both settings on, run the copy in the ingest job. It streams each archive, checks it against its manifest, and skips those already there. A batch for which the destination holds another archive is left as it is and fails the copy at the end:

```sh
scripts/start-job.sh --env dev --job INGEST_JOB -- archive-copy \
  --from "$(az storage account show -n "$(scripts/settings.sh --env dev --key STORAGE_ACCOUNT)" --query primaryEndpoints.blob -o tsv)raw" \
  --batches "$B"
```

Then clear `USNM_RETAIN_RAW` and provision. That deletes the environment's own container, so check the copy's `archives copied` line first.

**Cost** (East US 2 list prices, October 2026): Cold storage is $0.0036 per GB-month, so the 1% set's 23.8 GB is about $0.09 a month. Reading it all back once costs about $0.71 ($0.03 per GB retrieved), and writing it about $0.05 (about 2,900 blocks at $0.18 per 10,000 writes). Cold has a 90-day early-deletion minimum. Each environment's endpoint is about $7.30 a month ($0.01 an hour).

### Sample sets

A set packages a sample for reuse in `sets/{name}/`, built once by the search cluster's bench job and never replaced (build `-v2` instead). A build first claims the name with `building.json`, so two builds never write the same objects. A build that fails keeps its name, so the retry is the next version:

- `raw.tar`: the set's LoC batch archives as kept in `raw/`, with their manifests. It's a plain tar, since the archives are bzip2 already. Curate it offline, or extract it into another raw store.
- `docs.ndjson.zst`: the sample's documents exactly as the release builds them (`text`, `text_cg`, every field; `text_as` and `text_as_cg` only when the manifest says `american_stories`). It's one zstd stream, so it can be loaded into any Quickwit 0.9 index created from `infra/quickwit/pages-index.yaml`.
- `manifest.json`, written last: name and version, how the batches were chosen, the batch list with each archive's bytes and sha256, page and document counts, each file's bytes and sha256, the corpus bounds, the common-word pairs' and American Stories' versions, the index config's sha256, the builder's commit and `created_at`.

```sh
J() { scripts/start-job.sh --env dev --job SEARCH_CLUSTER_JOB -- "$@"; }
J bundle --name loc-1pct-v1 --sample dev1pct \
  --selection "every 100th batch of LoC's listing sorted by name, from offset 39 (30 batches)"
J load --set loc-1pct-v1 --index s1ixset          # the cluster loads straight from the set
```

**From a container or a laptop with access** (Storage Blob Data Reader on `sets`, and a network path to the account: an environment's VNet, or a private endpoint of your own; public access is off):

```sh
az storage blob download --auth-mode login --account-name <archive account> -c sets -n loc-1pct-v1/docs.ndjson.zst -f docs.ndjson.zst
zstd -dc docs.ndjson.zst | split -C 8m - chunk-        # request bodies under Quickwit's 10 MiB limit
for f in chunk-*; do curl -sf -XPOST "$QW/api/v1/$INDEX/ingest" --data-binary @"$f"; done
```

Check each file against the manifest's sha256 (`shasum -a 256`). `tar -xf raw.tar` gives `{batch}/{archive}` and `{batch}/manifest.json`, the layout of `raw/`.

## Dev environment lifecycle

A first stand-up accepts Azure’s missing-stack responses (`DeploymentStackNotFound`, or `ResourceNotFound` naming the stack itself); other inventory read errors still block the protected-resource guard.

Reprovisioning the same experiment preserves the project group's original `started` timestamp, including stand-up's second provision. If that timestamp is missing, status reports an unknown start and falls back to month-to-date cost; an unreadable group is an error, not a missing tag.

**The rule** (#275): `dev` (the stack `usnm-dev`, the group `usnm-core-rg-dev`) holds no resources unless a focused experiment is running. Stand it up for the experiment, and tear it down completely once the results are saved. Scaling to zero isn't enough: with no replicas running it still costs about $2 a day, about $60 a month. That's the 4 private endpoints ($0.24 a day each), the Container Apps environment's load balancer (about $0.45 to $0.60 a day) and public IP ($0.12), the registry (ACR Basic, about $0.17), the NFS share (64 GiB, about $0.21), 3 private DNS zones (about $0.05) and storage (Cost Management, October 2026).

**The marker.** Provisioning `dev` needs `USNM_EXPERIMENT`, the experiment's issue number. It tags dev's project group, `usnm-core-rg-dev`, with `experiment` and `started` (UTC), which stays the same while the experiment does. `scripts/dev-status.sh` lists the stack, what's in `usnm-core-rg-dev`, `usnm-core-infra-rg-dev` (the Container Apps environment's own) and `rg-usnm-dev-spot` (and the legacy `rg-usnm-dev` and `ME_cae-usnm-dev_*`, ADR-0017), the tag and its age, and what those groups have cost since the experiment started. It exits 1 if anything is there without the tag, and warns when the tag is older than 7 days. The guard-rail stack, which stays up between experiments, holds a budget on dev's groups (in both name forms), `budget-usnm-dev-groups`, that mails the alert recipients at $5 of actual spend in a month. It needs no `dev`, only an environment with `USNM_ALERT_EMAILS` provisioned since the budget was added. The legacy Container Apps group's name (`ME_cae-usnm-dev_rg-usnm-dev_<region>`) ends in dev's region: each provision of `dev` records its `AZURE_LOCATION` in the budget, and other environments' provisions keep it.

When Cost Management throttles `dev-status.sh` (429), the script makes up to four attempts with exponential backoff (2, 4, 8 seconds by default), honouring `Retry-After` (seconds or HTTP date) and `x-ms-ratelimit-microsoft.costmanagement-*-retry-after` values when they appear in the error text. Azure CLI normally prints only the HTTP reason and response body, not the response headers, and the script turns on no verbose or debug logging. Retry waits total at most 60 seconds; if a requested wait would exceed that budget, no early retry is made. Once the attempts are exhausted, the script warns on stderr that cost is unknown, without changing the environment status or treating it as zero cost. Missing Cost Management Reader permissions are a warning; other query errors still fail.

**Stand it up:**

```sh
scripts/settings.sh --env dev --key USNM_SEARCH_CLUSTER --value true   # the experiment's own settings, if any, first
scripts/stand-up.sh --env dev --experiment 240 [--archive]
```

The experiment's public settings, and the ones `stand-up.sh` and `teardown.sh` write themselves (`USNM_USE_ACR`, `USNM_COSMOS_FREE_TIER`, `USNM_SEARCH_CLUSTER`), land in `infra/env/dev.json` in your checkout ([Settings](#settings)): commit the experiment's in its pull request. Both scripts provision without the uncommitted-changes check; on its own, use `scripts/provision.sh --env dev --allow-dirty`.

`scripts/stand-up.sh` starts from the settings the last teardown kept (`.env.deleted-<time>`, beside the settings file), or new ones. It provisions `dev` without images (registry, data, network, identities: about 7 minutes), copies the pinned Quickwit image into the registry, builds the checkout's API and ingest images there with `az acr build` (tag `exp-<commit>`; the Japanese OCR image too with `USNM_JA_OCR_JOB`), sets `USNM_IMAGE_TAG` and `USNM_USE_ACR`, and provisions again. `--archive` sets `USNM_ARCHIVE_ACCOUNT` from the `usnm-archive` stack. With the archival account and the ingest job on (`USNM_INGEST_JOBS`), it then seeds the title records and catalog from the account's newest `raw/titles/{time}.json` (`scripts/titles-seed.sh`, below). It never creates a GitHub Environment or adds `dev` to `USNM_DEPLOY_ENVIRONMENTS`, so no push to main rolls it: an experiment runs a branch's images. It refuses `prod`, and a checkout with uncommitted changes. To run a newer commit, run it again. `scripts/bootstrap.sh --env dev` would also work, but it wires `dev` to deploy from main.

**Data**, depending on the experiment:

- **Search experiments:** load a packaged set (`sets/loc-1pct-v1/docs.ndjson.zst`, [Sample sets](#sample-sets)) straight into Quickwit. No curation, no titles-sync, no LoC traffic.
- **Curation or release experiments:** enqueue the set's batches. With `USNM_ARCHIVE_ACCOUNT` set, curation reads them from `raw/` without LoC's download pacing. The 1% (30 batches, 23.8 GB) is roughly 2 worker-hours of bzip2, so under an hour on the backfill job's 4 workers. A release also needs the title catalog. Titles-sync is paced against LoC and can take up to 8 hours, and its output (`reference/raw/titles.json` and the catalog) is in dev's own data account, so it goes at teardown. With the archival account, each titles-sync also saves a copy to `raw/titles/{time}.json` ([Archival storage](#archival-storage)), and the next stand-up starts from the newest one, so the release's titles-sync fetches only titles the copy lacks.

**Seeding the titles.** `scripts/titles-seed.sh --env dev` (which `stand-up.sh` runs) starts one ingest job execution of `usnm-ingest titles-seed` and waits for it (`--wait-mins`, default 20). The job reads the newest copy through dev's endpoint with the ingest identity, checks that the pipeline can read it (every record, and the catalog as geocode loads it: both files or neither, unique ids and ordinals, every title's place listed; a copy that fails writes nothing), and writes the catalog files, then `raw/titles.json` last, each create-only. It writes only into an environment with no record cache: a cache dev already has, newer or older than the copy, is never replaced, and a seed stopped partway finishes when run again. A catalog file already there is kept only if it is the copy's; one that differs stops the seed before the cache is written, so dev never gets one copy's records with another catalog. It logs the copy it read (path, time, records, titles, places) and each file it wrote or kept:

```sh
scripts/titles-seed.sh --env dev                       # again, or after turning the ingest job on
scripts/logs.sh --env dev --query-name titles-archive --span 1h
```

**ACR build verification.** All build wrappers (`stand-up.sh`, `build-quickwit.sh`,
`build-boinc-images.sh`, and `boinc-app-release.sh`) use `scripts/lib/acr-build.sh`.
It queues once with `--no-wait`, polls that run's canonical `az acr task show-run`
status, and requires `Succeeded` plus the requested output image's SHA-256 digest
matching the registry tag. A nonzero submission exit is recoverable only when a
valid run ID was returned; no run ID, a failed/canceled run, an unreadable run at
the deadline, or a missing/mismatched digest fails closed. An existing tag cannot
prove that a new build succeeded. Polling defaults to every 10 seconds, bounded
by the build timeout plus 600 seconds (`ACR_POLL_INTERVAL` / `ACR_WAIT_SECONDS`
override these in seconds). Each status and digest read is also bounded by the
remaining budget, including its CLI child processes. A local timeout does not cancel the remote run; use
the printed registry/run ID to inspect it before deciding whether to rebuild.
These checks do not deploy images. For manual builds, source the helper in Bash
and call `acr_build_verified az REGISTRY REPOSITORY:TAG TIMEOUT [build arguments]`
as in the examples below; do not infer remote success or failure from the log
streaming CLI exit status (#549).

**Cost of standing up:** two provisions and two or three `az acr build` runs, a few cents. Reading the 1% set back from Cold is about $0.71 ($0.03 per GB retrieved) plus about $0.24 of private endpoint processing. Then the idle floor (about $2 a day) and whatever the experiment runs. **Time:** about 40 to 60 minutes to a running environment with images. Data takes minutes for a set, about an hour to curate the 1% from the archive, and a few minutes to seed the titles from the archive (up to 8 hours for a titles-sync when there is no copy yet, or the catalog needs titles the copy lacks).

**Tear it down** once the results are saved: reports copied out of `qw-bench/runs/`, logs queried, and, if titles-sync ran with the archival account, its copy in the archive. Save that copy first, and don't start the teardown until the command below exits 0:

```sh
scripts/start-job.sh --env dev --job INGEST_JOB --wait-mins 20 -- titles-archive
```

Without `--wait-mins`, `start-job.sh` returns as soon as Azure accepts the execution, before anything is written, and a teardown started then would delete the record cache before the copy is made. With it, the script waits for the execution and exits 0 only once it has `Succeeded`, so the copy is in `raw/titles/` (or the newest copy there already held the same records, and nothing was written). It exits 1 if the execution ends `Failed`, `Stopped` or `Degraded` or is still running after 20 minutes: then don't tear down. Check the job's log (`scripts/logs.sh --env dev --query-name job-executions`; without `USNM_ARCHIVE_ACCOUNT` the job has no raw store, and the command fails with `titles-archive needs USNM_RAW_URL`), fix it and run it again. The command is create-only and skips a copy the archive already has, so running it when titles-sync's own copy succeeded costs one short execution. Then:

```sh
scripts/teardown.sh --env dev
scripts/dev-status.sh                                  # expect: dev holds nothing
```

With `USNM_ARCHIVE_ACCOUNT` set, `teardown.sh` first detaches `dev` from the archival account. It lifts the account's delete lock, clears `USNM_ARCHIVE_ACCOUNT` and `USNM_SEARCH_CLUSTER` (the settings that grant roles there) and provisions, so the stack deletes its endpoint and roles itself. It then checks that no role assignment in the account's group belongs to one of dev's identities and that no endpoint connection from dev's groups remains, and puts the lock back (`scripts/archive-store.sh deploy`) whatever happened. The account and its data are never changed. Run it again if it stops partway: it picks up where it stopped. Then it removes `dev` from `USNM_DEPLOY_ENVIRONMENTS` if it's there, waiting for any deploy (`ci` on main) that could still roll it; detaches the stack and deletes the groups it manages, including `usnm-core-rg-dev` (the Container Apps environment's `usnm-core-infra-rg-dev` goes with the environment), the custom roles and any role assignment the stack made outside them; keeps `usnm-guardrails` while another environment (or `usnm-archive`) uses it; deletes the GitHub Environment `dev` if there is one; and keeps the settings file as `.env.deleted-<time>`, without the experiment marker. The retired `rg-usnm-dev-spot` is no longer declared; if a provision has already detached it, follow the [Spot group cleanup](../../infra/README.md#retired-spot-resource-group) separately. `dev-status.sh` still detects it until it is deleted. `az deployment group delete -g rg-usnm-archive -n archive-grant-dev` removes the stale deployment record, if wanted.

**Protected removals during archive detachment:** with `USNM_SEARCH_LOCAL_MODE=nfs` and `USNM_INGEST_SCRATCH_GIB=0`, disabling the search cluster also removes its scratch account and file private-DNS zone in that partial provision. Inspect the managed IDs and guard output, then pass repeated `--allow-rename=<exact-old-name>` to `teardown.sh` for each intentional removal (both account and zone). The environment-name confirmation does not grant these approvals. A missing, failed or unsupported plan still blocks, even with overrides; the archive lock is restored and the saved detach marker allows an approved rerun. See [the guard and teardown examples](../../infra/README.md#protected-resource-gate); ordinary partial provisions use the same guard, with no automatic teardown bypass.

Log Analytics keeps the workspace soft-deleted for 14 days at no cost, and a stand-up within that window recovers it. Storage account names are reusable straight away, and the resource names come back the same, since the suffix is `uniqueString(subscription, env)`. Dev's data goes with it: Cosmos batch state, curated parts, indexes and `qw-bench`. The archive keeps what was downloaded from LoC. With `USNM_OCR` on, the Document Intelligence account would need a purge before it could be recreated; it's off in dev.

**Cost of tearing down:** nothing billed. **Time:** about 25 to 40 minutes, mostly the group delete, plus about 10 minutes for the archive's provision.

**Settings.** Keep dev's settings in one place, `~/.config/usnewsmap/dev/.env` ([infra/README.md](../../infra/README.md#settings-and-the-deployment-stack)), rather than in each worktree's `.azure/dev/.env`, where a stale copy can provision dev with old settings.

## Search cluster experiment

An experimental Quickwit cluster on Container Apps (#238, #239), measured on a 1% sample: indexing with 1 and 2 indexers, then searching with 1, 2 and 3 searchers. It runs in its own environment (`dev`), never in prod (the template ignores the setting there), and everything it adds sits behind `USNM_SEARCH_CLUSTER`. With the setting off, the stack is exactly what it was (the module is `if (searchCluster && useAcr && env != 'prod')`).

**What it deploys** (`infra/modules/searchcluster.bicep`):

- **Nodes** `usnm-qw-{i}-ca-{env}`, `USNM_SEARCH_CLUSTER_NODES` of them (0 to 4), each exactly one replica of `USNM_SEARCH_CLUSTER_NODE_VCPU` vCPU (default 2, the API sidecar's size) with 2 GiB per vCPU, on the Consumption profile. Node 0 is the only metastore (the single writer of the file-backed metastore in `qw-cluster`), the control plane, the janitor, an indexer, a searcher, and the root: searches enter there through its internal ingress, `http://usnm-qw-0-ca-{env}` inside the environment. Nodes 1 to `USNM_SEARCH_CLUSTER_INDEXERS - 1` also index; the rest only search. Their node config is `infra/quickwit/cluster-node.yaml`; its searcher settings are the sidecar's (`searcher.yaml`), which a test checks.
- **Addresses.** Quickwit gossips over UDP and calls leaves over gRPC, both on replica IPs, which change whenever a replica restarts (#239). Each node starts as `usnm-qwcluster node`. That writes its replica IP to `qw-bench/seeds/qw-{i}.json`, reads the other nodes' entries as `QW_PEER_SEEDS`, sets `QW_ADVERTISE_ADDRESS`, and then becomes Quickwit. Every node writes before it reads, so of two nodes starting together at least one sees the other. A node that restarts at a new IP joins through the others, and they learn its new address by gossip.
- **Bench job** `usnm-qw-bench-caj-{env}` (manual, 4 vCPU / 8 GiB, 6 h timeout), which runs `usnm-qwcluster` with each step's arguments (`scripts/start-job.sh`).
- **Storage, Entra only.** Containers `qw-cluster` (splits and metastore; Blob Data Contributor for each node's system-assigned identity, since Quickwit can use no other) and `qw-bench` (seeds, the sample, reports; Contributor for `usnm-qw-node-id-{env}` and `usnm-qw-bench-id-{env}`). The bench identity also reads `curated` and `reference`. Nothing can write `qw-index`, `reference` or `curated`. Turning the setting off deletes both containers (soft delete keeps them 14 days).

**The steps.** Each `usnm-qwcluster` step logs one `qwcluster report` line and stores the full report in `qw-bench/runs/`.

| Step      | Command                                                       | What it measures                                                                                                                                                                                                                                                                                                               |
| --------- | ------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `sample`  | `sample --name N --pct P [--american-stories]`                | Takes `P`% of the published version's pages (SHA-256 of `doc_id`, mod 10,000, below `P` × 100, as the OCR audit samples) as the documents a full release builds, with duplicate copies settled as the release does. Reads every curated part once.                                                                             |
| `load`    | `load --sample N --index I [--split-docs 3000] [--senders 4]` | Creates `I` from `pages-index.yaml` with `split_num_docs_target` set and one ingest shard per indexer, sends the sample, and times sending, all documents committed, merges settled, and sealed. Also records documents by node, splits and footer sizes, and retries on 429 and 503.                                          |
| `bench`   | `bench --label L --index I --sample N --expect-searchers K`   | The 13 benchmark searches of `bench-cold-searches.py` and `load-cold-searches.py`, run as `/v1/aggregate` runs them (`plan::aggregate`) against node 0: `first`, `warm`, then concurrency 1, 2, 4 and 10, each with its windows a day further back. Reports median, p90, throughput, and each node's leaf searches and splits. |
| `members` | `members`                                                     | The cluster as node 0 sees it: members, addresses, generations, services.                                                                                                                                                                                                                                                      |
| `dump`    | `dump [--hold-secs 600]`                                      | Prints every stored report, for a log stream when Log Analytics is over its cap.                                                                                                                                                                                                                                               |

**Split target.** At the production 30,000 pages, a 1% sample makes about 8 splits, too few for a root to spread over three nodes the way production spreads about 800. The loader's default of 3,000 makes about 80 splits of a tenth the size. Per-split fixed costs then weigh more than in production, so a second index at 30,000 (`--split-docs 30000`) brackets the result.

**Run it** (in `dev`; prod's data account is private, so dev curates its own 1% from LoC):

```sh
scripts/archive-store.sh deploy                       # once per subscription; prints the account's id
ARCHIVE_ID=<the id it printed>
scripts/settings.sh --env dev --key USNM_INGEST_JOBS --value true
scripts/settings.sh --env dev --key USNM_INGEST_SCRATCH_GIB --value 0            # 1% fits the replica's disk
scripts/settings.sh --env dev --key USNM_LOG_DAILY_CAP_GB --value 1              # blob write logs; clear afterwards
scripts/settings.sh --env dev --key USNM_ARCHIVE_ACCOUNT --value "$ARCHIVE_ID"   # keep every LoC download (Archival storage, above; private: the settings store)
scripts/provision.sh --env dev --allow-dirty    # the experiment's settings, before they're committed
RG=$(scripts/settings.sh --env dev --key AZURE_RESOURCE_GROUP); ACR=$(scripts/settings.sh --env dev --key ACR_NAME)
TAG=qwc-$(git rev-parse --short HEAD)
. scripts/lib/acr-build.sh
acr_build_verified az "$ACR" "usnewsmap-ingest:$TAG" 3600 -f docker/Dockerfile.ingest --build-arg USNM_GIT_SHA=$(git rev-parse HEAD) .
acr_build_verified az "$ACR" "usnewsmap-api:$TAG" 3600 -f docker/Dockerfile.api .
scripts/settings.sh --env dev --key USNM_IMAGE_TAG --value $TAG
scripts/settings.sh --env dev --key USNM_USE_ACR --value true
scripts/provision.sh --env dev --allow-dirty

# The 1%: every 100th batch of LoC's listing by name, from the 40th (offset 39):
# 30 batches, 238,057 pages of 23,838,683 (0.999%), 26 awardees, 23.8 GB of archives.
B=arhi_beatles_ver01,az_fireant_ver01,ct_fairfield_ver01,curiv_plasse_ver01,dlc_alpha_ver03,dlc_debaptiste_ver01,dlc_goldenrod_ver01,dlc_leibovitz_ver01,dlc_saluki_ver01,gu_drteeth_ver01,iahi_hypno_ver01,in_irvington_ver01,khi_garwood_ver02,lu_juggernaut_ver01,me_calais_ver03,mnhi_dassel_ver01,mohi_berenice_ver01,mthi_goldeneye_ver01,ncu_cotton_ver04,nhd_lafayette_ver01,nn_keddy_ver01,ohi_himilco_ver01,oru_longspur_ver01,rp_hobgoblin_ver02,tu_eddie_ver02,uuml_anderson_ver01,vi_fezza_ver01,vnstcsc_duggan_ver01,whi_brie_ver01,wvu_jolie_ver02
scripts/start-job.sh --env dev --job INGEST_JOB -- enqueue --batches $B --force   # --force: batches curated before are curated again, now kept
scripts/start-job.sh --env dev --job BACKFILL_JOB -- curate --max-runtime-secs 14400
scripts/start-job.sh --env dev --job INGEST_JOB -- run --batches $B --full --curate-max-runtime-secs 21600 \
  --titles-max-runtime-secs 28800 --quickwit-bin /usr/local/bin/quickwit \
  --quickwit-metastore azure://qw-index --quickwit-index-root azure://qw-index
scripts/logs.sh --env dev --query-name release-progress --span 6h    # the single-writer baseline on the same pages

# One node, one indexer.
scripts/settings.sh --env dev --key USNM_SEARCH_CLUSTER --value true
scripts/settings.sh --env dev --key USNM_SEARCH_CLUSTER_NODES --value 1
scripts/settings.sh --env dev --key USNM_SEARCH_CLUSTER_INDEXERS --value 1
scripts/provision.sh --env dev --allow-dirty
J() { scripts/start-job.sh --env dev --job SEARCH_CLUSTER_JOB -- "$@"; }
J members
J sample --name dev1pct --pct 100          # dev's whole version is the 1%
J bundle --name loc-1pct-v1 --sample dev1pct \
  --selection "every 100th batch of LoC's listing sorted by name, from offset 39 (30 batches)"
J load --sample dev1pct --index s1ix
J bench --label n1 --index s1ix --sample dev1pct --expect-searchers 1
# Two nodes, both indexing.
scripts/settings.sh --env dev --key USNM_SEARCH_CLUSTER_NODES --value 2
scripts/settings.sh --env dev --key USNM_SEARCH_CLUSTER_INDEXERS --value 2
scripts/provision.sh --env dev --allow-dirty
J load --sample dev1pct --index s2ix
J bench --label n2 --index s1ix --sample dev1pct --expect-searchers 2
# Three nodes; node 2 only searches. Then the restart test.
scripts/settings.sh --env dev --key USNM_SEARCH_CLUSTER_NODES --value 3
scripts/provision.sh --env dev --allow-dirty
J bench --label n3 --index s1ix --sample dev1pct --expect-searchers 3
J members
az containerapp revision restart -n usnm-qw-2-ca-dev -g "$RG" \
  --revision "$(az containerapp revision list -n usnm-qw-2-ca-dev -g "$RG" --query "[?properties.active].name | [0]" -o tsv)"
J bench --label n3-restart --index s1ix --sample dev1pct --expect-searchers 3 --levels 1,4 --levels-only
J members                                  # qw-2 has a new address and generation

# The report: Log Analytics, Azure Monitor per node app, list prices.
scripts/qwcluster-report.py --env dev --since 2d --out qwcluster-report.md
```

Start each step once the previous execution has ended (`az containerapp job execution list -n "$(scripts/settings.sh --env dev --key SEARCH_CLUSTER_JOB)" -g "$RG" -o table`). For a cold `first` pass, restart the node revisions before a bench. The first provision with the cluster may restart node 0 a few times while its identity's role on `qw-cluster` propagates. If Log Analytics is over its cap, stream the stored reports with `J dump --hold-secs 600` and `az containerapp job logs show -n <job> -g "$RG" --container qwbench --follow > lines.txt`, then run `scripts/qwcluster-report.py --env dev --from-file lines.txt`. Collect the report before scaling down: a deleted app's metrics can't be queried.

**Report lines.** Container Apps keeps about 16 KiB of a console line. A bench report therefore goes out as several `qwcluster report` lines (`bench`, one `bench_pass` per pass, `bench_probe`, `bench_profile`), which `scripts/qwcluster-report.py` puts back together by label. Images before this change logged a whole run on one line, and a run with `--probe` passed the limit (October 2026); the report salvages such a line's passes, and takes its Quickwit version from the run's `bench root` line, but its probe and profile are only in `qw-bench` (`runs/{label}/bench.json`). `J dump` with a current image logs those stored reports again, in parts.

**Idle and teardown.** `USNM_SEARCH_CLUSTER_NODES 0` and a provision remove the node apps but keep the containers and the bench job, so nothing is billed but storage. `USNM_SEARCH_CLUSTER ""` and a provision remove everything the setting added, both containers included. Clear `USNM_LOG_DAILY_CAP_GB` afterwards. Once the experiment's results are saved, tear `dev` down ([Dev environment lifecycle](#dev-environment-lifecycle)).

**Threads.** Every node runs one main-runtime thread and one search-pool thread per vCPU (`QW_TOKIO_RUNTIME_NUM_THREADS`, `RAYON_NUM_THREADS`), as the API's sidecar does with 4 and 4 at 3.75 vCPU (#251): 2 and 2 at the default 2 vCPU. Before October 2026 the nodes ran Quickwit's own default, a third of the CPUs rounded up for the main runtime (one thread at 2 vCPU), so the earlier benches had a single thread for downloads and split opening. `USNM_SEARCH_CLUSTER_RUNTIME_THREADS` and `USNM_SEARCH_CLUSTER_SEARCH_THREADS` override the counts (0: one per vCPU).

### Quickwit version comparison

Whether a pre-0.9 Quickwit searches our splits with less work than 0.9.1 (#251; upstream quickwit-oss/quickwit#6883 reports 0.9.1 1.25 to 2x slower than a 0.8 nightly on identical splits). The released 0.8.2 can't take part: it reads tantivy index formats 4 to 6 and 0.9.1 writes 7, and it authenticates to Azure only with an account key, which our accounts refuse (ADR-0009). The baseline is the `qw-azure-fix` nightly (`quickwit/quickwit:qw-azure-fix@sha256:d4f75ebc…`, main of 23 September 2025 plus the Azure multipart fix, which reports 0.8.0): it writes and reads format 7, uses managed identity, reads the 0.9 metastore, and takes every searcher key of `searcher.yaml` but `predicate_cache_capacity` and `leaf_request_timeout_secs`.

**What it deploys.** `USNM_SEARCH_COMPARE_TAGS`, ingest image tags separated by commas, gives one standalone searcher per tag, `usnm-qw-cmp-{i}-ca-{env}` (root `http://usnm-qw-cmp-{i}-ca-{env}` inside the environment), of `USNM_SEARCH_CLUSTER_NODE_VCPU` vCPU each. Each runs as the API sidecar does: searcher and metastore, polling the file-backed metastore in `qw-cluster` every 30 s, with Blob Data Reader on `qw-cluster` only, so it searches the cluster's indexes and can't change them. Each has a cluster id of its own and no seeds, so the two never join and a search never mixes versions. The node wrapper reads `quickwit --version` and, below 0.9, drops the two searcher keys and pins `warmup_single_split_initial_allocation` to 0.9.1's 300 MB (the nightly's default is 1 GB). The bench records the root's Quickwit version, each node's main-runtime busy time and Blob bytes downloaded per pass, and samples the search pool's running and queued split searches every 2 s.

**Images.** The second tag is the ingest image on the nightly's Quickwit (`docker/Dockerfile.ingest`'s `QUICKWIT_IMAGE`; CI always builds the default, v0.9.1):

```sh
cd ~/code/usnewsmap.com-qwcluster
RG=$(scripts/settings.sh --env dev --key AZURE_RESOURCE_GROUP); ACR=$(scripts/settings.sh --env dev --key ACR_NAME)
TAG=qwc-$(git rev-parse --short HEAD); SHA=$(git rev-parse HEAD)
. scripts/lib/acr-build.sh
NIGHTLY=quickwit/quickwit:qw-azure-fix@sha256:d4f75ebc6d6ee7db4732b8a20be544ba3455bdc08ef0ccd7683f4aa854243ff6
acr_build_verified az "$ACR" "usnewsmap-ingest:$TAG" 3600 -f docker/Dockerfile.ingest --build-arg USNM_GIT_SHA=$SHA .
acr_build_verified az "$ACR" "usnewsmap-ingest:$TAG-qw2509" 3600 -f docker/Dockerfile.ingest --build-arg USNM_GIT_SHA=$SHA --build-arg QUICKWIT_IMAGE=$NIGHTLY .
acr_build_verified az "$ACR" "usnewsmap-api:$TAG" 3600 -f docker/Dockerfile.api .
```

**Smoke test** on an index the cluster already holds (`s1ixb`, the 1% without American Stories, 58 splits): cluster nodes off, so nothing writes while they search; both searchers at the sidecar's 4 vCPU.

```sh
scripts/settings.sh --env dev --key USNM_IMAGE_TAG --value $TAG
scripts/settings.sh --env dev --key USNM_SEARCH_CLUSTER --value true
scripts/settings.sh --env dev --key USNM_SEARCH_CLUSTER_NODES --value 0
scripts/settings.sh --env dev --key USNM_SEARCH_CLUSTER_NODE_VCPU --value 4
scripts/settings.sh --env dev --key USNM_SEARCH_COMPARE_TAGS --value "$TAG,$TAG-qw2509"
scripts/provision.sh --env dev --allow-dirty
J() { scripts/start-job.sh --env dev --job SEARCH_CLUSTER_JOB -- "$@"; }
JOB=$(scripts/settings.sh --env dev --key SEARCH_CLUSTER_JOB)
wait_for() {  # until execution $1 has ended; prints how
  local s; while :; do
    s=$(az containerapp job execution show -n "$JOB" -g "$RG" --job-execution-name "$1" --query properties.status -o tsv)
    case "$s" in Running|Processing|Unknown|"") sleep 20 ;; *) echo "$1 $s"; return ;; esac
  done
}
restart() { for a in usnm-qw-cmp-0-ca-dev usnm-qw-cmp-1-ca-dev; do az containerapp revision restart -n $a -g "$RG" --revision "$(az containerapp revision list -n $a -g "$RG" --query "[?properties.active].name | [0]" -o tsv)"; done; sleep 90; }
wait_for "$(J members --cluster http://usnm-qw-cmp-0-ca-dev)"; wait_for "$(J members --cluster http://usnm-qw-cmp-1-ca-dev)"
bench() { wait_for "$(J bench --label "$1" --cluster "http://usnm-qw-cmp-$2-ca-dev" --index s1ixb --sample dev1pct --levels 1,4,10)"; }
restart; bench cmp-v091-r1 0; bench cmp-n2509-r1 1
restart; bench cmp-n2509-r2 1; bench cmp-v091-r2 0
restart; bench cmp-v091-r3 0; bench cmp-n2509-r3 1
scripts/qwcluster-report.py --env dev --since 6h --out qwcompare.md
```

The two `members` lines show each searcher alone in its cluster, with `main_threads` 4 in its counters (the nightly reports the same metric names as 0.9.1). The report's **Quickwit version comparison** section has, per pass, the main runtime's busy seconds per search and per GB downloaded, its busy share, and the search pool's peaks, then each version's averages with ratios to 0.9.1, then whether both versions found the same pages for every search. If they didn't, the timings don't compare. On dev's 1% the searches wait on Blob, not CPU (0.1 to 0.4 vCPU in October 2026), so the latency ratios don't carry to production; busy seconds per GB do, since production's cold searches wait on the main runtime (about 95% busy on 4 threads with an idle search pool, #251). A nightly ratio well under 1 (0.8 or less) on busy seconds per GB is worth the full run; near 1, the regression of #6883 isn't what holds production back.

**Full run**, if the smoke test points that way: indexes shaped like production's, built by 0.9.1 (node 0) from the 1% with `text_as` and `text_as_cg` filled from `text` and `text_cg` (`load --as-from-text`; LoC's OCR twice, the shape of production's work but not its matches), at the loader's 3,000 pages per split (production's fan-out per search) and at production's 30,000 (production's split size):

```sh
scripts/settings.sh --env dev --key USNM_SEARCH_CLUSTER_NODES --value 1
scripts/provision.sh --env dev --allow-dirty
wait_for "$(J load --sample dev1pct --index asdup3k --as-from-text)"
wait_for "$(J load --sample dev1pct --index asdup30k --split-docs 30000 --as-from-text)"
scripts/settings.sh --env dev --key USNM_SEARCH_CLUSTER_NODES --value 0
scripts/provision.sh --env dev --allow-dirty
bench() { wait_for "$(J bench --label "$1" --cluster "http://usnm-qw-cmp-$2-ca-dev" --index "$3" --sample dev1pct --american-stories --levels 1,4,10)"; }
# then the same three rounds as above for asdup3k and asdup30k
```

**Afterwards** `USNM_SEARCH_COMPARE_TAGS ""` and a provision remove the searchers (collect the report first). Two 4 vCPU searchers cost about $0.86 an hour while they exist.

### Local-disk test

Whether reading splits from somewhere other than Blob removes the main runtime's CPU cost (#251): the same index and bench on Quickwit 0.9.1 with the sidecar's 4 and 4 threads, one searcher, `usnm-qw-local-ca-dev`, in one mode at a time (`USNM_SEARCH_LOCAL_MODE`):

- `blob`: splits read from Blob, as the sidecar reads them.
- `nfs`: the index (`USNM_SEARCH_LOCAL_INDEX`) copied once onto an NFS Azure Files share and served from there as `file://`. The share, `qw-search` (`USNM_SEARCH_NFS_GIB`, default 64), is a second share in the ingest scratch share's provisioned v2 SSD account, reached through the same private endpoint; it exists while the mode is `nfs` (or the scratch share is on). The copy is made at the first start (`crates/usnm-ingest/src/cluster/local.rs`) and kept: a restart, or another index, copies only what isn't there.
- `cache`: Quickwit's split cache (`searcher.split_cache`, `USNM_SEARCH_LOCAL_CACHE_GIB`, default 40) on the replica's disk. A search reads from Blob and marks its splits; the cache downloads them whole in the background (4 at a time) and later searches read the local files.
- `copy`: the index copied to the replica's disk before Quickwit starts and served as `file://`, so no Blob client runs at all.

**Where it runs.** `blob` and `nfs` run on Consumption at the sidecar's 3.75 vCPU / 7.5 GiB (with `nfs`'s 0.25 vCPU init container that hands the share's directory to the node's user, 4 / 8 in all), the shape production searches with. A Consumption replica can mount an NFS share: the environment has its own VNet, and the ingest job mounts its scratch share on Consumption the same way ([storage mounts](https://learn.microsoft.com/azure/container-apps/storage-mounts#azure-files-volume)). `cache` and `copy` need more disk than a Consumption replica's 8 GiB ([ephemeral storage](https://learn.microsoft.com/azure/container-apps/storage-mounts#ephemeral-storage)), and a VM is out (`usnm-deny-iaas-compute` denies them in dev's groups), so they run on the E4 dedicated profile (`USNM_DEDICATED_PROFILE`), where Microsoft reported about 80 GiB of ephemeral storage per replica in September 2026 (microsoft/azure-container-apps#1779; the node logs `data directory disk` with `df` at start): 3.25 vCPU / 24 GiB, an E4 replica's most, its disk emptied by a restart. The E4 node costs about $0.39 an hour plus the $0.10 Dedicated plan fee while the profile exists; the share about $0.10 per GiB a month while it exists.

**One E4 mode at a time, with a provision in between.** The E4 profile has one node. A new revision there can't start while the old one holds it, and the old one keeps the traffic: on the evening of 9 October 2026 (ET) the `cache` and `copy` runs were all served by the `blob` revision (`AssigningReplicaFailed: Insufficient capacity on workload profile 'ingest-e4'`, then `Deployment Progress Deadline Exceeded`), and repeated the `blob` runs' windows, which Quickwit's partial request cache answered. Their results don't stand. To switch between `cache` and `copy`, clear the mode (`--unset`), provision, then set the new one and provision. The bench's `--variant blob|nfs|cache|copy` now reads the root's node config (`/api/v1/config`) and stops if the searcher isn't in that mode. Give each run its own `--offset` too, so no two runs share windows.

**What the bench adds.** `--probe` sends each search's summary request once more over Quickwit's gRPC API, the only place 0.9.1 reports its per-split resource stats (`SearchResponse.resource_stats`, quickwit #6416: not in the REST response, not in its metrics, in its logs only for memory-hungry queries): warm-up (the main runtime's downloading and decoding), waiting for the search pool, CPU search, waiting for a search permit, and bytes downloaded (`crates/usnm-ingest/src/cluster/grpc.rs`). `--profile-secs` runs searches with fresh windows under Quickwit's own CPU profiler (`/api/developer/pprof`, in its release builds) and stores the flame graph in `qw-bench` at `runs/{label}/flamegraph.svg`; the report lists samples by thread and the main runtime's busiest functions. `--wait-split-cache-secs` waits for the split cache to hold every split before the passes, and `--variant` names the mode in the report.

With `RG`, `J`, `JOB` and `wait_for` as in the version comparison above, and the image tag that has these flags in `USNM_IMAGE_TAG`:

```sh
lbench() { wait_for "$(J bench --label "$1" --cluster http://usnm-qw-local-ca-dev --index "$IX" --sample dev1pct --variant "$2" --levels 1,4,10 --probe --profile-secs 120 "${@:3}")"; }
# Until the searcher's newest revision is the ready one (a first nfs copy takes minutes).
ready() { until [ "$(az containerapp show -n usnm-qw-local-ca-dev -g "$RG" --query "properties.latestReadyRevisionName == properties.latestRevisionName" -o tsv)" = true ]; do sleep 30; done; }
scripts/settings.sh --env dev --key USNM_SEARCH_CLUSTER_NODES --value 0
scripts/settings.sh --env dev --key USNM_SEARCH_COMPARE_TAGS --unset
IX=s1ixb; scripts/settings.sh --env dev --key USNM_SEARCH_LOCAL_INDEX --value $IX
scripts/settings.sh --env dev --key USNM_SEARCH_LOCAL_MODE --value blob; scripts/provision.sh --env dev --allow-dirty; ready
lbench c-blob-r1 blob --offset 200; lbench c-blob-r2 blob --levels-only --offset 210
scripts/settings.sh --env dev --key USNM_SEARCH_LOCAL_MODE --value nfs; scripts/provision.sh --env dev --allow-dirty; ready
lbench c-nfs-r1 nfs --offset 220; lbench c-nfs-r2 nfs --levels-only --offset 230
az containerapp logs show -n usnm-qw-local-ca-dev -g "$RG" --container qwnode --tail 300 | grep -E 'data directory disk|local copy disk|index copied'
scripts/qwcluster-report.py --env dev --since 12h --out qwlocal.md
scripts/settings.sh --env dev --key USNM_SEARCH_LOCAL_MODE --unset; scripts/provision.sh --env dev --allow-dirty   # removes the searcher and the share
```

The report's comparison tables group the runs as `0.9.1 blob` and `0.9.1 nfs` (and `cache`, `copy`) with ratios to blob, and its bench rows name each run's mode. Compare the `r2` runs: if `nfs` cuts the main runtime's busy seconds per search and the probe's warm-up seconds as `copy` was meant to show, the Blob path (HTTP, TLS, the Azure client) is the main runtime's cost and an NFS share is a way around it in production; if it barely moves them, the cost is in decoding what a split needs. The flame graphs show which functions those seconds went to.
