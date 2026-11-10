# Search serving

Part of the [operations guide](README.md). Public serving runs on-prem ([`scripts/onprem/serving/`](../../scripts/onprem/serving/README.md)); this page covers what applies to both it and the [Azure cold spare](#the-azure-cold-spare), and the spare itself.

## Cloudflare API schema

The upload artifact is [`ops/api/usnewsmap-cloudflare-api-schema.yaml`](../../ops/api/usnewsmap-cloudflare-api-schema.yaml), a reviewed OpenAPI 3.0.3 snapshot. Nothing deploys it automatically. The public contract is [`/v1/openapi.json`](https://api.usnewsmap.com/v1/openapi.json), generated as OpenAPI 3.1 by `crates/usnm-api/src/openapi.rs`.

The upload lists both public hosts and both `/v1` and `/api/v1` prefixes, including the two internal POST routes left out of the public document. Its `x-source` records the source checkout revision, router hash, source URL and SHA-256 of the source JSON serialized with sorted keys and compact separators. The revision is the code used to check route coverage; it doesn't attest the deployed binary.

Regenerate from a checkout whose API code matches the intended release, then review and commit the diff:

```sh
python3 -m venv /tmp/usnm-cloudflare-schema-venv
/tmp/usnm-cloudflare-schema-venv/bin/python -m pip install -r scripts/ops/requirements-cloudflare.txt
/tmp/usnm-cloudflare-schema-venv/bin/python scripts/ops/export-cloudflare-schema.py \
  --checkout . \
  --source https://api.usnewsmap.com/v1/openapi.json \
  --output ops/api/usnewsmap-cloudflare-api-schema.yaml
python3 -m unittest -v scripts/ops/test_export_cloudflare_schema.py
git diff -- ops/api/usnewsmap-cloudflare-api-schema.yaml
```

`--source` also accepts a saved public OpenAPI JSON file. The exporter refuses dirty API code, an unsupported router layout, mismatched routes, methods or hosts, or changed internal POST coverage, and replaces the output once conversion succeeds. It doesn't prove that the live parameter semantics match the source revision; its unit tests, which run in the script-test suite, exercise conversion, not the live service or Cloudflare's evaluator.

Regenerate and review after a change to API routes, parameters, parsers or deployment configuration (for example, whether queryless browsing is on). Committing the file doesn't upload it, register endpoints or enable enforcement. Upload the reviewed file by hand under Cloudflare's **Web Assets > Schema validation**, with blocking rules off, and inspect **Profile Analysis** before enforcing it ([schema validation](https://developers.cloudflare.com/api-shield/security/schema-validation/)).

**Limits.** Comma lists are strings, because Cloudflare doesn't support their `explode: false` encoding. Empty optional values, signed integers and unpadded dates follow the origin's permissive parsing; query lengths, list counts, membership, repeated or unknown keys and cross-parameter rules stay origin checks. POST bodies aren't validated at the edge: the origin accepts JSON sent as `text/plain` or `application/json`, with limits above the Free plan's 1 KiB validation limit. Cloudflare doesn't validate responses. Health probes, static pages, implicit `HEAD` and CORS `OPTIONS` aren't in this inventory, so don't use it as a whole-host allowlist. Review encoding and legitimate-traffic violations before adding any block rule.

## Sitemap eligibility

The durable sitemap publishes the four canonical application pages and only unfiltered canonical search URLs with both a positive current match count and server-rendered sample results. Curated examples and previously promoted searches are rechecked against the pinned index; an empty search is left out, removed from the home page's discovery links and marked `noindex`, but stays usable as an interactive search. New recurring searches still need finalized activity on two distinct days and more than 50 matches. A backend failure doesn't count as an empty search: the refresh fails and keeps the last complete generation.

Redirect aliases, filtered variants and noncanonical paths are never sitemap entries, including when reading older saved generations. Proximity operators use a literal `~` in canonical paths: Cloudflare normalizes unreserved `%7E` before forwarding, so re-encoding it would cause a permanent self-redirect. The web builder's no-backend fallback sitemap lists static pages only.

When verifying a release, fetch every sitemap URL without following redirects: require HTTP 200, robots.txt crawl permission, no applicable `noindex`/`none` directives, a self-referencing canonical and nonempty search content, and check for duplicate or alternate content. Google's [URL Inspection guidance](https://support.google.com/webmasters/answer/9012289#will_i_be_indexed) doesn't guarantee indexing even for a valid live test; Google's selected canonical, quality assessment, manual actions, legal removals and temporary blocks need Search Console evidence that public HTTP checks can't give.

## Searcher caches

The Quickwit searcher (on-prem, `usnm-searcher.service` in the serving container; in Azure, the spare's `usnm-qw-spare-ca-prod`) keeps the split footers, fast fields and partial and predicate results in memory, with the capacities in `infra/quickwit/searcher.yaml` (08 §8.4). The API reads the searcher's metrics every minute and reports them as `api.searcher_cache_*` (08 §8.1.2) to the environment's Application Insights, which the on-prem API also writes to when `scripts/onprem/serving/gen-env.sh` finds its connection string:

```sh
scripts/azure/logs.sh --env prod --query-name searcher-caches --span 1d        # per searcher and cache: MB and items held, hits, misses, evictions, hit rate
```

The same table, and charts of MB held and of misses and evictions, are in the "usnm API (<env>)" workbook. The saved queries and the workbook select one reporting API replica per searcher per UTC minute before aggregating, so don't add the replicas' raw metrics together. Remote searchers are identified by `Properties["usnm.searcher"]`, the origin of `USNM_QUICKWIT_URL` (without credentials, path or query); loopback searchers keep their per-API-replica rows. These are sampled metrics: independently timed scrapes and a reporter's first report (which includes the searcher's lifetime totals, the warm-up included) can skew bins and hand-offs, so don't treat their sums as exact accounting. For cache gauges the table reads the latest report. Split footers are one item per split.

**Sizing `split_footer_cache_capacity`.** Size it from the release's footer total, and check it against what the cache does after the warm-up. Evictions on their own don't show the footers don't fit: Quickwit 0.9.1 also counts replacing an entry as an eviction, and after a publish the cache drops the old version's footers. The API logs `split footer cache evictions seen` once per searcher run, without a cause. The footers don't fit when the total is above the capacity, or when footer misses and evictions keep coming hours after the warm-up, with no publish or restart in between. To size it:

1. Add up `footer_bytes` on the version's `index layout` lines, or under `indexes` in `reference/<version>/manifest.json`, over every index the version lists: the base, its deltas and the Japanese index.
2. Set `split_footer_cache_capacity` in `infra/quickwit/searcher.yaml` to that total plus about 25% for deltas, rounded up. Quickwit reads `256MB` as 256,000,000 bytes.
3. Check it fits the searcher's memory next to the fast field cache (1 GB), the aggregation memory limit (768 MB) and the partial request and predicate caches (32 MB each). If it doesn't, shrink the fast field cache first.

The change reaches on-prem when the serving container's config is regenerated and installed (`gen-env.sh`, then `install.sh`, which restarts the searcher when its config changed; [`scripts/onprem/serving/`](../../scripts/onprem/serving/README.md)), and the spare at its next roll (`spare-roll` applies `searcher.yaml` to the running app). Confirm with `searcher-caches` a day later: after the warm-up, footer misses and evictions stay near 0 (each start misses about once per split), and the footer cache's MB held is about the footer total.

## Searcher threads

The searcher runs each split's search on its `search` thread pool, and downloads and opens the splits on its main Tokio runtime. The API reads both with the caches, every minute, and reports them as `api.searcher_pool_tasks` and `api.searcher_runtime_*` (08 §8.1.2):

```sh
scripts/azure/logs.sh --env prod --query-name searcher-threads --span 2h       # per searcher and minute: search pool ongoing and pending, main runtime threads and busy share
```

Read it while cold searches run (`scripts/ops/load-cold-searches.py`):

- `SearchPending` above 0 with `SearchOngoing` at the pool's size: searches wait for cores. The pool has `RAYON_NUM_THREADS` threads (4, set in `infra/modules/containerapp.bicep` and in `gen-env.sh`'s searcher environment; without it, one per CPU Rust counts). Quickwit doesn't report the size; it is the most `SearchOngoing` reaches.
- `MainBusyPercent` near 100 with the search pool below its size and nothing pending: searches wait on the main runtime. It has `QW_TOKIO_RUNTIME_NUM_THREADS` threads (4, same files; `MainThreads`).
- Neither: look at storage reads, and at `max_num_concurrent_split_searches` in `infra/quickwit/searcher.yaml`, which limits the split searches downloading at once.

The pool numbers are a reading once a minute, not an average, so a short burst can fall between two. `MainBusyPercent` is busy time over the thread time the runtime had (`api.searcher_runtime_capacity_ms`); it is empty for a searcher's first minute.

## Search capacity

A second searcher, a Quickwit cluster (#238, #239), was closed as not planned on 15 October 2026, to be reopened if the p90 wait for a computation slot (`api.search_queue_wait_seconds`) stays above a few seconds, or the searcher's CPU stays above 3 vCPU outside releases, for a week. With alert emails set, two severity 4 alerts watch those (08 §8.1.2):

- _Search capacity: searches waiting for a slot_, a log alert on Application Insights, emails once a day while each of the last two days had more than 10% of its searches (at least 20) wait over 3 s.
- `usnm-api-replica-cpu-alert-<env>` reads the Container Apps CPU metric, so it watches only the Azure spare's replicas.

A log search alert reads at most 2 days, so judge a week in the "usnm API (<env>)" workbook's Search capacity section (the last 14 days by day). Leave out days with a release or a new index version, when every search is cold, and check `searcher-threads` for whether the search pool or the main runtime is the one that's full.

**Computation slots.** The API divides the searcher-wide visitor budget (`USNM_COMPUTE_CONCURRENCY`, default 2) by `USNM_API_MAX_REPLICAS` (default 2), rounding down, when its searcher is shared (the searcher app, or the index-account searcher with `USNM_INDEX_STORAGE_SERVE`; #364): each Azure API replica then gets 1 slot. A budget below the replica maximum is rejected; a remainder is unused. The division runs in `infra/main.bicep` from deployment parameters, so it changes only with a provision. Keep the helper outside the API module: importing it there makes ARM what-if short-circuit other modules and omit protected resources, which the rename guard refuses. Warm-ups keep their own slots, and while old and new revisions overlap they can briefly exceed the budget.

## Searcher app

`USNM_SEARCHER_APP` runs the searcher as its own Container App, `usnm-search-ca-<env>`, reading the lake's `qw-index`, instead of a sidecar in each API replica (#267), so API and web deploys don't restart it. Prod used it from October 2026 until the index account's searcher took over; the legacy app was retired on 25 October 2026 (#1136), and the spare's environment needs the setting off (`USNM_SPARE_ENVIRONMENT needs USNM_DATA_ALL_NETWORKS and USNM_SEARCHER_APP=false`). The spare's searcher, `usnm-qw-spare-ca-prod`, is the index-account searcher of the same layout (`infra/modules/containerapp.bicep`): one replica, internal ingress, Blob Data Reader on the index account's `qw-index` only, scaling to zero with the API.

With either searcher app, CI's roll (`scripts/ci/roll-api.sh`) finds the searcher from the API's `USNM_QUICKWIT_URL`, applies a changed `searcher.yaml` to it and waits for its revision, then rolls the API, whose start warms the new searcher; otherwise it leaves the searcher alone. A new Quickwit image moves with a provision. The API sees a searcher restart in its metrics within a minute and runs a `searcher_restart` warm-up ([Cache warm-up](#cache-warm-up)).

To leave the searcher app in an environment that has it on, roll back in two provisions (#362) so search never drops: first set `USNM_SEARCHER_APP_RETAIN=true` and unset `USNM_SEARCHER_APP` together, commit and provision. That recreates the API's system identity and its Blob Data Reader grants and starts a sidecar revision while the old revision still searches the retained app. Check that the API's `latestRevisionName` equals `latestReadyRevisionName`, its template has both `api` and `quickwit` with `USNM_QUICKWIT_URL=http://127.0.0.1:7280`, and `/readyz` and a real search pass. Then unset `USNM_SEARCHER_APP_RETAIN`, commit and provision again, which deletes the app and its role.

## The Azure cold spare

[ADR-0029](../design/adr/0029-onprem-primary-azure-failover.md) keeps an Azure API and searcher at zero replicas for failover, in their own Container Apps environment without a VNet: `usnm-core-spare-cae-prod`, `usnm-api-spare-ca-prod` and `usnm-qw-spare-ca-prod` (#308). The lake, the index account and Cosmos accept every network (`USNM_DATA_ALL_NETWORKS`), with Entra ID roles the only way in: no keys, no SAS, no anonymous access. The index stays Hot.

### What a failover needs

| Azure service                         | Used for                                                                                                      | Without it                                                                    |
| ------------------------------------- | ------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------- |
| Index account, `qw-index`             | The searcher's metastore and splits. `/readyz` asks the searcher's health, so the Worker's readiness needs it | `/readyz` fails; `/v1/aggregate`, `/v1/hits` and `/v1/days` fail; no failover |
| Lake, `reference`                     | Loaded at start and refreshed every 10 minutes: `current.json`, catalog, places, baselines                    | The API can't start; every route fails                                        |
| Lake, `cache`                         | Stored responses and search screenshots, read with a 2 s limit                                                | Searches are computed each time; nothing else breaks                          |
| Lake, `searches`                      | The anonymous search log, and `/v1/search-report`                                                             | Searches still answer; the log loses them                                     |
| Lake, `ocr-requests`                  | Page ids from hit results                                                                                     | Hits still answer; the requests are lost                                      |
| Cosmos DB                             | `/v1/status` only                                                                                             | The status page has no pipeline state; search is unaffected                   |
| Application Insights                  | `POST /v1/beacon` (page views), telemetry, the API alerts                                                     | Beacons are accepted and dropped                                              |
| Registry                              | The API and searcher images, pulled on each start                                                             | A start can't pull an image a node doesn't hold                               |
| OCR exchange account and scan archive | Nothing. The API has no setting for either                                                                    | Nothing                                                                       |

Page images come from the Library of Congress. The site itself is in the API image.

### Index access tier

The maintainer kept the index Hot in November 2026, because October's releases came 2 to 12 days apart and Cool loses money at that pace. `USNM_INDEX_ACCESS_TIER` stays in the settings so this can be revisited. On the 1,086.7 GiB index, Cool saves about $0.30 a day in storage ($0.66 against $0.36). Against that:

- Writes cost $0.10 per 10,000 against $0.05, so copying a version in costs $1.40 against $0.70.
- A blob deleted within 30 days of arriving in Cool is billed for the rest of the 30, about $0.36 a day for the whole index.
- Reads cost $0.01 per 10,000 against $0.004, and retrieval $0.01/GB against nothing, so a failover day costs $2.70 to $8.20 more.
- Moving back to Hot costs about $11.

With releases `d` days apart and each old version deleted at once, Cool saves $0.30 × `d` and costs $0.70 + $0.36 × (30 - `d`) per release. It breaks even at about 18 days. Cold isn't worth it, at $0.10 per 10,000 reads.

### What wakes it

Each wake starts the API, which starts the searcher. The API stays about 5 minutes after its last request and the searcher about 9 minutes more. These reach the spare's ingress:

- The Worker, when the primary doesn't answer `/readyz` with `ready` within 3 s: the minute heartbeat, `healthcheck.usnewsmap.com`, or a visitor's request after a 5xx or a failed probe. Each polls Azure's `/readyz` until it's ready, and those polls keep the app up.
- `usnm-fallback-drill`, which has the same Azure origin. Its `dev.usnewsmap.com/*` route is removed between drills, because with the dev connector stopped any request there wakes production's spare. The Worker itself stays deployed.
- Anything that reaches the app's default hostname directly.
- A roll of the API (`scripts/ci/roll-api.sh`), which ends with a `/readyz` check, and the start's warm-up of about 5 minutes after it.

The Worker selects Azure when the primary fails a second readiness probe, 1 second after the first, and a GET's 5xx, timeout or dropped connection reaches Azure if the primary's readiness then fails too (#308). Otherwise the primary's 5xx passes through, or the Worker answers 503 with `Retry-After`. A real outage costs the first request at most about 4 seconds more before the Azure polling starts; the polling keeps its 180 s budget.

`/readyz` isn't recorded in Application Insights, and a request abandoned before a replica is up isn't either, so a wake can show no request. `ContainerAppSystemLogs` shows wakes as `KEDAScaleTargetActivated`, `triggered by s0-upstream_rq_total`.

### Rolling the spare

Main merges don't roll the spare (#308): from 1 to 6 November 2026, merge-driven rolls caused about half of its wakes. Pushes to `main` still publish the images to the registry and attest the API image, and on-prem serving deploys an attested image (`scripts/onprem/serving/update-api.sh`). The spare runs the image of its last roll until the next one. Roll it onto the image on-prem follows:

- after an index publish to the spare, once the new version is promoted;
- before a cold drill, then let it return to zero replicas before the drill's first step;
- when a change must reach the spare, such as a fix that a failover would need, or a `searcher.yaml` change.

```sh
gh workflow run spare-roll --ref main                  # the commit usnewsmap-api:main names
gh workflow run spare-roll --ref main -f tag=<commit>  # any published commit's image
```

It runs `scripts/ci/roll-api.sh` from the image's own commit, in the concurrency group of CI's roll job, so two rolls never interleave. It finds the app from the `prod` GitHub Environment's `USNM_API_APP` (`usnm-api-spare-ca-prod`). `gh workflow run ci --ref main -f roll_api=true` also rolls, after a full CI run; `scripts/azure/bootstrap.sh` uses that. A provision keeps the running tag with `--api-tag live`.

### Rollout

The spare's private endpoints were removed in two provisions on 6 November 2026 (#1425, #308): stage 1 (#1600) added `USNM_DATA_SUBNET_ACCESS`'s service-endpoint access beside the endpoints, and stage 2 (#1602) set `USNM_DATA_PRIVATE_ENDPOINTS=false`, deleting the three endpoints, their NICs and the two privatelink zones ([ADR-0008, amendment of November 2026](../design/adr/0008-private-networking.md)). Stage 2 of the [environment without a VNet](#the-spares-environment-without-a-vnet) then removed the subnet itself, so `USNM_DATA_SUBNET_ACCESS` no longer does anything in prod.

The pattern for changes like these: each stage is a pull request, then `scripts/azure/provision.sh --env prod --what-if --api-tag live` from a clean `origin/main`, with `--allow-rename=<name>` for each protected resource the stage deletes. Check the what-if's delete list against the plan, stop on anything new, then provision with the same flags. Don't start during an on-prem incident or while an index is being published or promoted. Provision refuses uncommitted settings; `--allow-dirty` is for an emergency that can't wait for a pull request.

### Rollback

Bringing the private endpoints back needs the VNet first: set `USNM_CAE_VNET` back to `true` ([below](#the-spares-environment-without-a-vnet)), then `USNM_DATA_PRIVATE_ENDPOINTS=true`, and provision with `--api-tag live`. The network module links the new zones before the endpoints write their records, and an empty zone answers for the account names in the VNet, so the spare can lose its data path for a while: check that the account names resolve to the endpoints from inside the VNet, then run the [cold drill](#cold-drill).

### The spare's environment without a VNet

On 6 November 2026 the spare moved to a Container Apps environment without a VNet, so its load balancer and public IP went (#308, [ADR-0008, amendment "no VNet for the spare"](../design/adr/0008-private-networking.md#amendment-november-2026-no-vnet-for-the-spare-308)). Stage 1 (#1611) added `usnm-core-spare-cae-prod` with `usnm-api-spare-ca-prod` and `usnm-qw-spare-ca-prod` beside the old environment (`USNM_SPARE_ENVIRONMENT`, `USNM_DATA_ALL_NETWORKS`) and retired the BOINC hand-off job; the cutover pointed the production Worker at the new API (`scripts/ci/deploy-edge-worker.sh` reads `usnm-api-spare-ca-prod`'s default name) and set the `prod` Environment's `USNM_API_APP`; stage 2 (#1615, `USNM_CAE_VNET=false`) deleted `cae-usnm-prod`, `ca-usnm-prod`, `usnm-qw-search-ca-prod` and `vnet-usnm-prod`.

Without a VNet, prod can't have Azure ingest jobs, the BOINC hand-off job, the lake searcher app, the archival account or an NFS scratch share: the provision refuses them. No Azure app carries the custom domains, so the origin certificate's weekly renewal (`origin-tls`) keeps it current in Key Vault without uploading it (`AZURE_UPLOAD` in `scripts/azure/origin_tls.py`). To bind the domains to Azure again, set it, rerun `scripts/azure/origin_tls_bootstrap.py` so the renewal identity's certificate role covers `usnm-core-spare-cae-prod`, and run `origin-tls` once before relying on it ([origin TLS](origin-tls.md)).

**Rollback** is a rebuild, so prefer fixing forward. Set `USNM_CAE_VNET` back to `true`, and clear the three certificate settings (`USNM_SITE_CERT_ID`, `USNM_WWW_CERT_ID`, `USNM_API_CERT_ID`), which named certificates in the deleted environment. Then provision with `--api-tag live`. That recreates the VNet and a VNet-integrated environment, with new apps under the old names and no custom domains. Check them cold as in the drill, point `scripts/ci/deploy-edge-worker.sh` back at `ca-usnm-prod`, and set the `prod` Environment's `USNM_API_APP` back when the Worker switches.

### Cold drill

Use the drill Worker, which sends `DNT: 1`, so the drill stays out of the search log and off the public hostnames. Its route doesn't exist between drills, so add it first. `deploy.sh` deploys the current Worker and adds the route; look at the command with `--dry-run` first:

```bash
edge/fallback-worker/deploy.sh --account-id <id> --azure-origin https://<default name> \
  --wrangler-version <X.Y.Z> --vault <vault> --token-secret <secret> \
  --drill-hostname dev.usnewsmap.com --health-failover --health-hostname dev.usnewsmap.com \
  --allowed-origins <the API's USNM_ALLOWED_ORIGINS> --native-metrics --worker-logs
```

Leave out `--health-heartbeat`: the drill Worker needs no cron. Then stop the dev connector, so the Worker's primary fails.

1. Roll the spare onto the image on-prem serves ([Rolling the spare](#rolling-the-spare)) if it runs an older one. Then confirm both apps have no replicas (`az containerapp replica list` for the API's and the searcher's latest revisions), and time a wake of a revision that has started once already: the first start of a new revision pulls its image cold (207 s on 6 November 2026, against about 70 s otherwise).
2. Poll `https://dev.usnewsmap.com/readyz`. It must answer 200 with `X-USNM-Origin: azure` inside the Worker's 180 s budget. Record the time.
3. Through the same hostname, each with `X-USNM-Origin: azure`:
   - a real search on `/v1/aggregate` and `/v1/hits`, which proves the index account;
   - `/v1/meta`, which must name the version on-prem serves, and proves `reference`;
   - `/v1/status`, compared with the public one, which proves Cosmos;
   - `POST /v1/beacon`, which must answer 204.
4. Check a write: `StorageBlobLogs` shows a `PutBlob` under `cache/` from the app's identity after an uncached search of more than 500 ms.
5. Confirm that `ContainerAppConsoleLogs` shows no storage or Cosmos 403 from the API or the searcher.
6. Remove the route: find its id with `GET /zones/{zone_id}/workers/routes`, and send `DELETE /zones/{zone_id}/workers/routes/{route_id}` for `dev.usnewsmap.com/*`. Then restart the dev connector.
7. Confirm both apps return to zero with no command, usually within 20 minutes, and stay there.

Every step must pass against the production spare's apps, starting from zero replicas (step 1).

## The Quickwit image

The serving searcher, the spare's searcher and the release writer run our build of Quickwit v0.9.1 ([ADR-0013](../design/adr/0013-quickwit-from-our-fork.md)). `docker/Dockerfile.quickwit-patched` builds it from our fork, [`tgoodyear/quickwit`](https://github.com/tgoodyear/quickwit), at a pinned commit. `infra/quickwit-image.json` records its digest for each environment, and `infra/main.bicep` and CI's publish read it there; an environment the file doesn't list runs upstream v0.9.1. On-prem hosts install the pinned build from the registry with `scripts/onprem/install-quickwit.sh --registry <ACR_NAME>`.

Build it in an environment's registry, which records the digest in the file:

```sh
scripts/azure/build-quickwit.sh --env prod              # ACR Tasks, about an hour; the build log follows
scripts/azure/build-quickwit.sh --env dev --from prod   # or import prod's build (same digest, no build)
```

If your `az` session expires during the build, ACR still finishes it. Once it has, run the same command again: it finds the tag and records its digest without building (`--rebuild` builds again). Then commit the file in a pull request. Once it merges, CI's publish checks that the registry has the digest (it fails if not) and rebuilds the ingest image on it. The spare's searcher moves with `scripts/azure/provision.sh --env <env> --api-tag live`, run once publish has finished; on-prem hosts move when they install it again.

**A new patch, or a Quickwit upgrade:**

1. On the fork, commit to the `usnm/v0.9.1-pool` branch (for an upgrade, rebase the commit onto the new upstream tag in a new branch, such as `usnm/v0.9.2-pool`) and tag it with a new `usnm-*` tag, such as `usnm-v0.9.1-pool2`. Names starting `usnm` trigger none of Quickwit's own workflows.
2. In `docker/Dockerfile.quickwit-patched`, set `QW_COMMIT_HASH` to the tagged commit and `QW_COMMIT_TAGS` to the tag without `usnm-` (`v0.9.1-pool2`), which is also the image's tag. For an upgrade, also take the new version's Dockerfile changes (base images, packages) and its `QW_COMMIT_DATE`, and move the `upstream` pin in `infra/quickwit-image.json` and `docker/Dockerfile.ingest`'s default to the new release (CI checks they match).
3. `scripts/azure/build-quickwit.sh --env prod`, then commit, merge and install, as above.
4. Re-run the cold-search test (`scripts/ops/load-cold-searches.py`) and read [the searcher's threads](#searcher-threads).

**Back to upstream:** remove the environment from `environments` in `infra/quickwit-image.json` (or put its previous digest back), merge, and provision once publish has finished. Publish copies upstream v0.9.1 into the registry if it isn't there and rebuilds the ingest image on it. To turn pooling off in an Azure searcher alone, without a new image, set `QW_AZURE_POOL=false` on it (`infra/modules/containerapp.bicep`) and provision.

## Cache warm-up

Each API start, and each publish before it swaps the new version in, warms the caches with the home page's examples and the most frequent logged searches (06 §6.5), as does a searcher restart under a running API (`searcher_restart`). They run one at a time: one that starts during another waits for its turn, and none is dropped. One line per run:

```sh
scripts/azure/logs.sh --env prod --query-name warm-up --span 2d                # per run: examples warm, cached, computed, skipped, ms
```

A start isn't ready until its run ends, so in a rollout the old revision serves meanwhile and the new one takes visitors once it's warm (06 §6.6); a start that finds everything cached is ready in seconds, and one that computes uses up to its 5-minute budget (`Ms` about 300000). A start still running at the readiness cap (6 minutes in prod, 1 minute where the app scales to zero) logs `warm-up still running at the readiness cap; reporting ready` and serves visitors while it finishes.

Read `Warm` against `Examples` first: equal means every example's search was in the in-process cache when the run ended (it is counted then, so evictions show). Then:

- **A start that read everything:** `Cached` is the examples plus the logged searches, `Computed` and `Skipped` are 0, and `Ms` is a few seconds. This is the usual start once a version and release have been warmed once.
- **A start that computed some:** `Computed` above 0 means searches no cache held: examples added since, or every search after a release that changed the response format (`RESPONSE_FORMAT` in `crates/usnm-api/src/routes/mod.rs`). Each takes about 11 s, so about 26 fit in the 5-minute budget. `Skipped` above 0 means the budget ran out with that many searches left; the next start reads what this one computed and goes on from there, and a visitor who opens a skipped example computes and persists it.
- **A publish:** `Cached` is 0 or close to it (nothing is cached for a new version yet, unless a visitor computed it meanwhile), and `Computed` is the searches reached in 15 minutes. On 7 October 2026 that was 74 of the then 100 examples; there are [200 now](../notes/2026-10-26-100-more-historical-examples.md) with the same budgets, so one publish and one start may not reach them all, and later starts or visitors compute the rest.
- **A searcher restart:** `Cached` is 0, and `Computed` counts attempted searches even when their responses were already cached. `Skipped` counts searches the startup budget did not reach, including time spent giving way. `Warm` still measures the API response cache, not Quickwit cache readiness.
- **`GaveWayMs`** is how long a start's or searcher restart's computations waited for visitors' searches: these runs compute a search only while no visitor's is running or queued. Large values mean visitors kept the searcher busy during the start.
- **`TimedOut` or `Failed`** above 0: the `slow warm-up query` and `warm-up query failed` lines of that run name the example (`api-errors` lists the failures).

Each computed search also logs `slow warm-up query` with its `ms` when it takes 1 s or more, and `api.prewarm_query_seconds` (by `endpoint` and `source`, `cache` or `computed`) has the time of every warm-up query.

## Search log

The API keeps every search from the site, with only its filters, page count and UTC day, in the `searches` container ([ADR-0012](../design/adr/0012-anonymous-search-log.md), 06 §6.8). Nothing expires `searches/days/` and `searches/import/`; staged batches are deleted after 7 days and stay in soft delete for 14 more. To read it:

```sh
scripts/azure/settings.sh --env prod --key USNM_SEARCH_LOG_READERS \
  --value "$(az ad signed-in-user show --query id -o tsv)"   # private: the settings store
scripts/azure/provision.sh --env prod
scripts/azure/searches.sh --env prod --days 30      # searches per day, top queries, queries that found nothing, date spans
```

The last two tables count searches by date span (the whole corpus range, over 10 years, 1–10 years, a year or less) and by the decade partitions they touch, bucketed as `usnm_core::decade` (pre-1820 is one). They hold counts and shares only, never queries. A search is whole range when it covers the corpus bounds the site serves (`/v1/meta`, read from the environment's `SITE_URL`); if those can't be read, the script says so and takes each version's widest search instead. They decide whether to build the next full base with `USNM_PARTITION_DECADE` (#123): only date-limited searches skip splits.

The lake accepts every network, with Entra ID roles the only way in (`USNM_DATA_ALL_NETWORKS`, #308), so `searches.sh` runs from anywhere your `az` sign-in holds a role on `searches`.

## Search log: the copy on tank

The day files can't be rebuilt. Since 26 October 2026 (#1097), the lake's Standard_LRS copy (14-day soft delete) has a second copy: `searches/days/` on a restricted dataset on `tank`, under [ADR-0027](../design/adr/0027-onprem-serving-behind-cloudflare.md) settled decision 3 (`USNM_SEARCH_LOG_COPY`). `usnm-ingest searchlog-copy` runs daily on a dedicated non-serving host with its own read-only Arc identity. [`scripts/onprem/searchlog-copy/`](../../scripts/onprem/searchlog-copy/README.md) has the acceptance details and the rebuild procedure.

**What it does.** Each run lists `days/` in the lake and copies every `{day}.jsonl` the dataset lacks. A day is streamed into `days/{day}.jsonl.partial`, its size checked against the listing and its sha256 against the file as written, and then linked into place as a read-only file; the link fails if the name exists, so a copy is never replaced. A day already there is compared by size (`--verify` also hashes the lake's bytes again). The tool deletes nothing from the lake or the copy: a copy that differs, or a day that has left the lake, is reported and kept, and the only file it removes is its own `.partial` from an interrupted run (a regular file: a link or directory by that name is left alone, and its day is reported missing until an operator removes it). There is no stored hash of a day file to compare with (the listing gives a size, an ETag and a creation time, and the API sends no Content-MD5 when it writes one), so the checks are of the bytes received and of the file written, and `--verify` is the check against the lake itself.

**The dataset.** `tank/azure/search-log`, by the layout in [the cold-storage note](../notes/2026-10-18-onprem-cold-storage.md): `atime=off`, `xattr=sa`, `acltype=posixacl`, `compression=zstd-3` and `recordsize=128K` (text), a `quota` of 10 GB (the cold-storage note's inventory of 18 October 2026 puts the container at about 0, so the quota only stops a fault filling the pool), `sharenfs` and `sharesmb` off. It is not shared by `serve-scans`, the serving container or any other server, and not replicated to `usbbak`: with the lake's file, this is the second copy and there is no third. Snapshots (sanoid): daily 14, weekly 8, monthly 12; nothing hourly, since a file arrives once a day. Mode `2750`, user `usnm-searchlog` (the copy's service user), group `searchlog-readers`: the people in `USNM_SEARCH_LOG_READERS`, kept the same by hand.

**Privacy.** The copy puts search text on the home host. [ADR-0012](../design/adr/0012-anonymous-search-log.md)'s rules carry over: no identifiers are added, and the readers are the readers of the `searches` container, no more. The tool never parses a day file; its logs, its errors and its status file hold day names, sizes and counts (and checksums when a check fails), never a search. Search text rests only in the dataset: the destination must be a mount point (otherwise the tool refuses to write), `days/` must be a real directory (a symbolic link is refused, since the service's state directory is writable and a link to it would take the text out of the dataset, and no file in it is reached through a link), the unit has a private `/tmp`, and the partial copy is a file in the dataset. Keep the dataset out of container and VM backups (a bind mount, with the backup job checked) and out of any share or export.

**The alert.** A run fails, and the unit with it, when a day file is still missing from the copy, or the copy differs, more than 24 hours after the lake wrote it (`--alert-after-hours`). The journal then has the day names (`journalctl -u usnm-searchlog-copy`; the failure is a JSON line with `"outcome":"failed"`), and `/var/lib/usnm-searchlog-copy/status.json` has `ok`, `overdue_days`, `missing_days`, `mismatched_days`, `local_only_days` and `last_success`. A run that stops before it can finish (the lake won't list, the dataset isn't mounted, the binary is missing or not executable) fails the unit and leaves the previous status file as it was, so `checked_at` is the last run that got through. A host that is down or a timer that is disabled runs nothing, so it doesn't fail: a watcher should treat a `checked_at` more than two days old as the alert. A day with no file in the lake (no searches that day, or one the API lost) is not a missing copy and isn't reported.

The independent `home-searchlog-watch.timer` on the Proxmox host checks this status and the copy unit and timer each hour, without reading day files, and sends failures through root's Proxmox notification route, with daily reminders while one lasts. The dashboard's data-flow observer doesn't read the status yet.

**When it reports a day.**

- _Missing_: look at the journal for why the copy failed (the identity's grant, the lake's firewall, the dataset's quota, or a link or directory named `{day}.jsonl.partial` in `days/`, which the tool leaves for an operator to remove), fix that and run `sudo systemctl start usnm-searchlog-copy`.
- _Mismatched_: the copy is not replaced. Check it against a snapshot, and against the lake with `--verify`. When the lake's file is the right one, move the bad copy aside by hand (`mv`, as an operator; the tool never removes one) and run the copy again.
- _Local only_: the lake has lost a day file (soft delete keeps it for 14 days: restore it there first). The copy is then the only one: snapshot the dataset (`zfs snapshot`) before anything else.

**Restoring.** A damaged copy comes back from a snapshot (`.zfs/snapshot/<name>/days/`, or `zfs clone`) or, for a day still in the lake, from the next run. A day lost from the lake is written back from the copy, create-only, by an operator with Storage Blob Data Contributor on the container and their own sign-in (no SAS, no key): `az storage blob upload --auth-mode login --account-name <account> --container-name searches --name days/<day>.jsonl --file <copy> --overwrite false`. A lost dataset is rebuilt by running the copy into an empty one (`--verify` afterwards), as long as the lake still has the files.
