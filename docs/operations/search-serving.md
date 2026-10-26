# Search serving

Part of the [operations guide](README.md).

## Searcher caches

The Quickwit searcher (a sidecar or its own app) keeps the split footers, fast fields and partial and predicate results in memory, with the capacities in `infra/quickwit/searcher.yaml` (08 §8.4). The API reads the searcher's metrics every minute and reports them as `api.searcher_cache_*` (08 §8.1.2):

```sh
scripts/azure/logs.sh --env prod --query-name searcher-caches --span 1d        # per replica and cache: MB and items held, hits, misses, evictions, hit rate
```

The same table, and charts of MB held and of misses and evictions, are in the "usnm API (<env>)" workbook ("usnewsmap API" before the Phase 2 provision). The saved queries and these workbook panels select one reporting API replica per searcher per UTC minute, before aggregating any instruments. Remote searchers are identified by `Properties["usnm.searcher"]`, the origin of `USNM_QUICKWIT_URL` (without credentials, path or query). Loopback sidecars have no shared identity and keep their per-API-replica rows. The `Replica` column is the shared searcher's origin in app mode and the API replica in sidecar mode; chart series likewise keep sidecars separate. When the reporting replica disappears, the next minute can use another replica. Selection always keeps all rows from the chosen reporter, not just one metric row.

The `usnm.searcher` property is emitted only by API images that carry it; older data without it remains per replica, since its mode cannot safely be inferred. For that older shared-app data only, filter to one reporting replica before summing. These remain sampled metrics: independently timed scrapes and a reporter's first cache-counter report (which includes the searcher's lifetime totals) can skew bins and hand-offs, so do not treat their sums as exact accounting. For cache gauges the table reads the latest report, not the sum of gauge values. Split footers are one item per split. A replica's first report counts everything since its searcher started, the warm-up included.

**Sizing `split_footer_cache_capacity`.** Size it from the release's footer total, and check it against what the cache does after the warm-up. Evictions on their own don't show the footers don't fit: Quickwit 0.9.1 also counts replacing an entry as an eviction, and after a publish the cache drops the old version's footers. The API logs `split footer cache evictions seen` once per searcher run, for the record, without a cause. The footers don't fit when the total is above the capacity, or when footer misses and evictions keep coming hours after the warm-up, with no publish or restart in between. To size it:

1. Add up `footer_bytes` on the version's `index layout` lines (`scripts/azure/logs.sh --env prod --query-name index-layout --span 2d`, column `FooterMb`), or under `indexes` in `reference/<version>/manifest.json`, over every index the version lists: the base, its deltas and the Japanese index.
2. Set `split_footer_cache_capacity` in `infra/quickwit/searcher.yaml` to that total plus about 25% for the deltas the next weekly releases add, rounded up. Quickwit reads `256MB` as 256,000,000 bytes, the unit `FooterMb` uses.
3. Check it fits the sidecar's 4 GiB next to the fast field cache (1 GB), the aggregation memory limit (768 MB) and the partial request and predicate caches (32 MB each). If it doesn't, the fast field cache is the one to shrink first; then the container's memory (`infra/modules/containerapp.bicep`).

The change goes live when it merges (CI applies `searcher.yaml` to the running app). Confirm with `searcher-caches` a day later: after the warm-up, footer misses and evictions stay near 0 (each replica start misses about once per split), and the footer cache's MB held is about the footer total.

## Searcher threads

The sidecar runs each split's search on its `search` thread pool, and downloads from Blob and opens the splits on its main Tokio runtime. The API reads both with the caches, every minute, and reports them as `api.searcher_pool_tasks` and `api.searcher_runtime_*` (08 §8.1.2):

```sh
scripts/azure/logs.sh --env prod --query-name searcher-threads --span 2h       # per replica and minute: search pool ongoing and pending, main runtime threads and busy share
```

Read it while cold searches run (`scripts/ops/load-cold-searches.py`):

- `SearchPending` above 0 with `SearchOngoing` at the pool's size: searches wait for cores. The pool has `RAYON_NUM_THREADS` threads (4, set on the sidecar in `infra/modules/containerapp.bicep`; without it, one per CPU Rust counts, which rounds the 3.75 vCPU quota down to 3). Quickwit doesn't report the size; it is the most `SearchOngoing` reaches.
- `MainBusyPercent` near 100 with the search pool below its size and nothing pending: searches wait on the main runtime. It has `QW_TOKIO_RUNTIME_NUM_THREADS` threads (4, same file; `MainThreads`).
- Neither: look at Blob reads, and at `max_num_concurrent_split_searches` in `infra/quickwit/searcher.yaml`, which limits the split searches downloading at once.

The pool numbers are a reading once a minute, not an average, so a short burst can fall between two. `MainBusyPercent` is busy time over the thread time the runtime had (`api.searcher_runtime_capacity_ms`), so it stays a share however the reports fall into the minutes; it is empty for a replica's first minute and after a searcher restart.

## Search capacity

A second searcher (a Quickwit cluster, #239) is deferred until either of these holds for a week (#238, 10 October 2026): the p90 of the wait for a computation slot (`api.search_queue_wait_seconds`) above 3 s, or a replica's CPU above 3 vCPU outside releases. With alert emails set, two severity 4 alerts watch them (08 §8.1.2):

- _Search capacity: searches waiting for a slot_ emails once a day while each of the last two days had more than 10% of its searches (at least 20) wait over 3 s. The first email covers two days and each later one adds a day, so six emails in a row are the week.
- `usnm-api-replica-cpu-alert-<env>` emails when the replicas' CPU averages over 3 vCPU for 6 hours, and again when it clears (prod only: where the app scales to zero, a window could hold only a start's samples). It averages the replicas together, so with a second replica sharing the load a hot one can sit under it: the workbook's per-replica chart is the record.

An alert can't look back a week (a log search alert reads at most 2 days), so judge the week in the "usnm API (<env>)" workbook's Search capacity section, which covers the last 14 days by day whatever the time range. Leave out days with a release or a new index version (every search is cold then), and check `scripts/azure/logs.sh --env prod --query-name searcher-threads --span 24h` for whether the search pool or the main runtime is the one that's full. If it holds, the next step is moving Quickwit out of the API's replica (#267, [the searcher app](#searcher-app)), then the cluster in #239.

## Searcher app

By default the Quickwit searcher is a sidecar in each API replica, so every new API image, which also carries the site, starts a new searcher with empty caches and a warm-up (#267). `USNM_SEARCHER_APP` runs the searcher as its own Container App instead, `usnm-search-ca-<env>`, which API and web deploys never restart. The maintainer decided on 15 October 2026 to keep the separate searcher app permanently (#365). The setting still defaults to off for environments that use the sidecar; the decision is no longer pending measurements (below).

**What it deploys** (`infra/modules/containerapp.bicep`). The searcher app runs the sidecar's container: the same image, `infra/quickwit/searcher.yaml`, 3.75 vCPU / 7.5 GiB, thread counts and startup and liveness probes, plus a readiness probe on `/health/readyz`. It listens on all interfaces and advertises its replica's address, and still runs alone (its own cluster id, no peers). Its ingress is internal only: `http://usnm-search-ca-<env>` (port 80, to Quickwit's 7280) inside the environment. It runs one replica, always in prod (elsewhere it scales to zero with the API): a second would start cold and split the cache hits; more search capacity is a cluster (#239). Its system-assigned identity has Blob Data Reader on `qw-index` alone, as the sidecar's had, and the API's user-assigned identity pulls the image. The API app keeps the `api` container alone (0.25 vCPU / 0.5 GiB), and its `USNM_QUICKWIT_URL` names the searcher app. It has no system-assigned identity or `qw-index` reader grants; its user-assigned identity is unchanged. The replica CPU alert and the workbook's replica CPU chart follow the searcher app.

**Turn it on:**

```sh
scripts/azure/settings.sh --env prod --key USNM_SEARCHER_APP --value true
scripts/azure/provision.sh --env prod
scripts/azure/settings.sh --env prod --key SEARCHER_APP          # usnm-search-ca-prod
```

The provision creates the searcher app and its role first, then rolls the API onto a revision without the sidecar. On a first switch from the sidecar, removing the app-level system identity cuts off the old revision's sidecar before the new revision is ready, so schedule that switch for a maintenance window; the permanent-searcher decision applies to prod, which already uses the searcher app. The searcher's first revision can start before its role has reached Blob and restart once or twice.

**The API's unused identity and grants are removed (#365).** With the searcher app kept, `apiSystemIdentity = quickwit && !searcherApp` in `infra/modules/containerapp.bicep`. The next provision removes the API's system-assigned identity and its Blob Data Reader assignments on `qw-index` in both the lake and, if configured, the separate index account. The maintainer approved this change to production infrastructure on 15 October 2026; it takes effect at the next provision. The searcher app's own identity and grants are unchanged. With `USNM_SEARCHER_APP=false`, the API retains its identity and grants even when `USNM_INDEX_STORAGE_SERVE=true` removes its sidecar: that separate cutover still needs the old revision's lake-sidecar permissions (ADR-0021). A rollback now recreates the API's identity and grants, so it must keep the searcher app until the permissions have propagated and the sidecar is ready (below).

**Deploys with it on.** CI's publish (`scripts/ci/roll-api.sh`) finds the searcher app from the API's `USNM_QUICKWIT_URL`. When `searcher.yaml` changed, it applies it to the searcher app, waits for its new revision, then rolls the API, whose start warms the new searcher; otherwise it leaves the searcher app alone. A new Quickwit image still moves with a provision, and restarts the searcher alone. The API sees the restart in the searcher's metrics within a minute (its counters start again from 0) and runs a searcher-restart warm-up while it stays ready: `trigger` is `searcher_restart` in `scripts/azure/logs.sh --env prod --query-name warm-up --span 2d`. When another warm-up is running then (a start's or a publish's), that run comes after it, and a publish in the meantime waits for it; the API's log says `the searcher restarted during a warm-up; warming it again after that one`. Unlike a start's, that run bypasses both response caches for searches, including responses the API already holds, so Quickwit's own caches fill too. It keeps cached responses available to visitors, including after a failed refresh, and uses the startup budget (5 minutes by default), giving way while visitor searches are running or queued. Places and coverage still use their response caches because they need no searcher.

**Measurements and decision (15 October 2026):** the maintainer chose to keep the separate searcher app permanently and approved dropping the API's unused identity and grants (#365). No new measurements are claimed here. The original comparison checklist (a week with it against a week without, leaving out days with a release or a new index version) is still useful for monitoring, though no decision waits on it.

- **API to searcher latency:** `scripts/azure/logs.sh --env prod --query-name backend-latency --span 14d`, `AvgMs` and `MinMs` per endpoint. The network hop should add a few milliseconds per call to searches of seconds.
- **Deploys:** after a merge that changes only the site, the API's log has no `the searcher restarted` line, and `scripts/azure/logs.sh --env prod --query-name searcher-caches --span 1d` shows the caches kept their contents.
- **Idle billing of each app:** Cost Management, per resource and meter, daily: idle and active vCPU-seconds of `usnm-search-ca-prod` and of `ca-usnm-prod`, against about 180,000 idle and 14,000 to 32,000 active vCPU-seconds a day for the one app at 2.25 vCPU (3 to 8 October 2026, 09 §9.5). The searcher still polls the metastore every 30 s and answers the API's metrics read every minute; the API app now bills on its own between visitors.
- **Capacity:** with two API replicas, both send searches to the one searcher (each with its own computation slots, 06 §6.6), and both report its metrics. `searcher-caches`, `searcher-threads` and the workbook cache panels automatically take one reporter per shared searcher per minute (see [Searcher caches](#searcher-caches)); do not add the replicas' raw metrics together. The selected capacity policy (#364) divides the configured searcher-wide visitor budget (`USNM_COMPUTE_CONCURRENCY`, default 2) by `USNM_API_MAX_REPLICAS` (default 2), rounding down: each API replica gets 1 slot by default. Provision uses the same maximum for scaling, even while only one replica actually runs. Sidecars keep the full budget per replica; a retained but unused searcher app does not trigger division. A budget that doesn't divide evenly leaves the remainder unused; a budget below the replica maximum is rejected. The partition is static, with no global lease: warm-ups keep their separate slots, and when old and new revisions overlap beyond the configured maximum they can temporarily exceed the visitor budget. The settings take effect when an operator runs a provision; a rollout of the site or API image alone doesn't apply them. Each replica also warms the searcher after it restarts, one warm-up at a time within a replica but not across them: while two replicas run (a rollout, or the HTTP rule's second replica), a searcher restart gets a warm-up from each, which compute the same misses side by side for up to the warm-up's budget (5 minutes). The searcher app runs one replica in prod and the API usually one, so that's a rollout's few minutes.

Budget division runs in `infra/main.bicep` using deployment parameters only; the API module receives the allocated per-replica slots. Keep the helper outside the runtime-dependent API module: importing it there makes ARM what-if short-circuit other modules and omit protected resources, which the rename guard correctly refuses. Do not bypass that guard.

**Roll back in two provisions (#362):** Keep `USNM_SEARCH_BACKEND=quickwit` and leave unrelated settings and images unchanged throughout. `USNM_SEARCHER_APP_RETAIN` keeps the searcher app and its Blob Data Reader role without routing the new API revision to it. It defaults to false and is a committed public setting, like `USNM_SEARCHER_APP`.

1. Set retention and turn off routing **together**, then commit `infra/env/prod.json`, merge its pull request and pull before provisioning:

   ```sh
   scripts/azure/settings.sh --env prod --key USNM_SEARCHER_APP_RETAIN --value true
   scripts/azure/settings.sh --env prod --key USNM_SEARCHER_APP --unset
   scripts/azure/provision.sh --env prod
   ```

   The first provision recreates the API's system-assigned identity (a new principal) and its Blob Data Reader assignments on `qw-index`, including the separate index account when configured. The assignments depend on the API resource, so the sidecar can start before its new grants reach Blob and may restart or log authorization failures during propagation. It must not take traffic until its startup and readiness checks pass. Allow several minutes for role propagation, in addition to loading and warming (normally up to about 13 minutes); do not assume provision returning means the grants are usable. The old API revision still searches the retained app throughout. If provisioning or startup fails, leave retention on and retry the same first provision before proceeding; do not delete and recreate the identity by hand. `SEARCHER_APP` becomes empty because it describes the new API's backend, not the retained resource. CPU alerts and the workbook also return to the API.

2. **Do not remove retention just because provision returned.** Check that the API's `latestRevisionName` is nonempty and equals `latestReadyRevisionName`, and that its template has both `api` and `quickwit` and `USNM_QUICKWIT_URL=http://127.0.0.1:7280`. For prod:

   ```sh
   az containerapp show --name ca-usnm-prod --resource-group rg-usnm-prod \
     --query '{latest:properties.latestRevisionName,ready:properties.latestReadyRevisionName,containers:properties.template.containers[].{name:name,quickwitUrl:env[?name==`USNM_QUICKWIT_URL`].value}}' -o json
   curl --fail --show-error https://api.usnewsmap.com/readyz
   ```

   Verify a real search succeeds too. In single-revision mode the ready sidecar revision takes over traffic before the old revision stops. If readiness or search fails, **leave retention on**, investigate and retry; the old revision keeps its searcher. In a scale-to-zero environment send a request to wake the API before checking readiness.

3. Only after those checks, unset retention, commit and merge that **second** settings change, pull, then provision again:

   ```sh
   scripts/azure/settings.sh --env prod --key USNM_SEARCHER_APP_RETAIN --unset
   scripts/azure/provision.sh --env prod
   ```

   The stack now deletes the unused searcher app and its role without changing the API's sidecar template or its identity. Keeping retention on longer is safe but continues billing for the extra searcher. Unsetting both settings in the first provision skips this protection and can drop search until the sidecar is ready.

## The Quickwit image

The searcher sidecar, the ingest image (whose Quickwit is the release's writer) and the ingest job's init container run our build of Quickwit v0.9.1 ([ADR-0013](../design/adr/0013-quickwit-from-our-fork.md)). `docker/Dockerfile.quickwit-patched` builds it from our fork, [`tgoodyear/quickwit`](https://github.com/tgoodyear/quickwit), at a pinned commit. `infra/quickwit-image.json` records its digest for each environment, and `infra/main.bicep` and CI's publish read it there; an environment the file doesn't list runs upstream v0.9.1.

Build it in an environment's registry, which records the digest in the file:

```sh
scripts/azure/build-quickwit.sh --env prod              # ACR Tasks, about an hour; the build log follows
scripts/azure/build-quickwit.sh --env dev --from prod   # or import prod's build (same digest, no build)
```

If your `az` session expires during the build, ACR still finishes it. Once it has, run the same command again: it finds the tag and records its digest without building (`--rebuild` builds again). Then commit the file in a pull request. Once it merges, CI's publish checks that the registry has the digest (it fails if not) and rebuilds the ingest image on it, which the jobs pick up at their next execution. The sidecar and the init container move with `scripts/azure/provision.sh --env <env>`, run once publish has finished.

**A new patch, or a Quickwit upgrade:**

1. On the fork, commit to the `usnm/v0.9.1-pool` branch (for an upgrade, rebase the commit onto the new upstream tag in a new branch, such as `usnm/v0.9.2-pool`) and tag it with a new `usnm-*` tag, such as `usnm-v0.9.1-pool2`. Names starting `usnm` trigger none of Quickwit's own workflows.
2. In `docker/Dockerfile.quickwit-patched`, set `QW_COMMIT_HASH` to the tagged commit and `QW_COMMIT_TAGS` to the tag without `usnm-` (`v0.9.1-pool2`), which is also the image's tag. For an upgrade, also take the new version's Dockerfile changes (base images, packages) and its `QW_COMMIT_DATE`, and move the `upstream` pin in `infra/quickwit-image.json` and `docker/Dockerfile.ingest`'s default to the new release (CI checks they match).
3. `scripts/azure/build-quickwit.sh --env prod`, then commit, merge and provision, as above.
4. Re-run the cold-search test (`scripts/ops/load-cold-searches.py`) and read [the searcher's threads](#searcher-threads).

**Back to upstream:** remove the environment from `environments` in `infra/quickwit-image.json` (or put its previous digest back), merge, and run `scripts/azure/provision.sh --env <env>` once publish has finished. Publish copies upstream v0.9.1 into the registry if it isn't there and rebuilds the ingest image on it. To turn pooling off in the searcher alone, without a new image, set `QW_AZURE_POOL=false` on the sidecar (`infra/modules/containerapp.bicep`) and provision.

## Cache warm-up

Each API start, and each publish before it swaps the new version in, warms the caches with the home page's examples and the most frequent logged searches (06 §6.5), as does a searcher restart under a running API (`searcher_restart`, [the searcher app](#searcher-app)). They run one at a time: one that starts during another waits for its turn, and none is dropped. One line per run:

```sh
scripts/azure/logs.sh --env prod --query-name warm-up --span 2d                # per run: examples warm, cached, computed, skipped, ms
```

A start isn't ready until its run ends, so in a rollout the old revision serves meanwhile and the new one takes visitors only once it's warm (06 §6.6); every prod start from 3 to 10 October 2026 used its whole 5-minute budget (`Ms` about 300000), but a start that finds everything cached is ready in seconds (below). A start still running at the readiness cap (6 minutes in prod, 1 minute where the app scales to zero) logs `warm-up still running at the readiness cap; reporting ready` and serves visitors while it finishes.

Read `Warm` against `Examples` first: equal means every example's search was in the in-process cache when the run ended (it is counted then, so evictions show). Then:

- **A start that read everything:** `Cached` is the examples plus the logged searches, `Computed` and `Skipped` are 0, and `Ms` is a few seconds. This is the usual start once a version and release have been warmed once.
- **A start that computed some:** `Computed` above 0 means searches no cache held: examples added since, or every search after a release that changed the response format (`RESPONSE_FORMAT` in `crates/usnm-api/src/routes/mod.rs`). Each takes about 11 s, so about 26 fit in the 5-minute budget. `Skipped` above 0 means the budget ran out with that many searches left; the next start reads what this one computed and goes on from there, and a visitor who opens a skipped example computes and persists it.
- **A publish:** `Cached` is 0 or close to it (nothing is cached for a new version yet, unless a visitor computed it meanwhile), and `Computed` is the searches reached in 15 minutes, about 75 of the 100 examples. The next start computes the rest.
- **A searcher restart:** `Cached` is 0, and `Computed` counts attempted searches even when their responses were already cached. `Skipped` counts searches the startup budget did not reach, including time spent giving way. `Warm` still measures the API response cache, not Quickwit cache readiness.
- **`GaveWayMs`** is how long a start's or searcher restart's computations waited for visitors' searches: these runs compute a search only while no visitor's is running or queued. Large values mean visitors kept the searcher busy during the start.
- **`TimedOut` or `Failed`** above 0: the `slow warm-up query` and `warm-up query failed` lines of that run name the example (`api-errors` lists the failures).

Each computed search also logs `slow warm-up query` with its `ms` when it takes 1 s or more, and `api.prewarm_query_seconds` (by `endpoint` and `source`, `cache` or `computed`) has the time of every warm-up query.

## Search log

The API keeps every search from the site, with only its filters, page count and UTC day, in the `searches` container ([ADR-0012](../design/adr/0012-anonymous-search-log.md), 06 §6.8). Nothing expires `searches/days/` and `searches/import/`; staged batches are deleted after 7 days and stay in soft delete for 3 more. To read it:

```sh
scripts/azure/settings.sh --env prod --key USNM_SEARCH_LOG_READERS \
  --value "$(az ad signed-in-user show --query id -o tsv)"   # private: the settings store (Settings, above)
scripts/azure/provision.sh --env prod
scripts/azure/searches.sh --env prod --days 30      # searches per day, top queries, queries that found nothing, date spans
```

The last two tables count searches by date span (the whole corpus range, over 10 years, 1–10 years, a year or less) and by the decade partitions they touch, bucketed as `usnm_core::decade` (pre-1820 is one). They hold counts and shares only, never queries. A search is whole range when it covers the corpus bounds the site serves (`/v1/meta`, read from the environment's `SITE_URL`); if those can't be read, the script says so and takes each version's widest search instead. They decide whether to build the next full base with `USNM_PARTITION_DECADE` (#123): only date-limited searches skip splits.

The data account is reachable only through its private endpoint, so run `searches.sh` from a network that reaches it. The first day file appears an hour after the first full UTC day with the log deployed.

## Search log: the copy on tank

The day files can't be rebuilt, and the lake holds the only copy, on Standard_LRS with 3 days of soft delete. [ADR-0027](../design/adr/0027-onprem-serving-behind-cloudflare.md) (settled decision 3, under [ADR-0022](../design/adr/0022-onprem-cold-storage.md) decision 5) adds a second one: a daily copy of `searches/days/` to a restricted dataset on `tank` (#1097). `usnm-ingest searchlog-copy` makes it, run by `usnm-searchlog-copy.timer` on a host with its own Arc identity ([`scripts/onprem/searchlog-copy/`](../../scripts/onprem/searchlog-copy/README.md) has the units, the install steps and why it runs where it does). The code and the units are written and tested against a local store, not yet against the lake; nothing runs them yet. Creating the dataset and the host, granting the identity and enabling the timer are operator steps, listed there.

**What it does.** Each run lists `days/` in the lake and copies every `{day}.jsonl` the dataset lacks. A day is streamed into `days/{day}.jsonl.partial`, its size checked against the listing and its sha256 against the file as written, and then linked into place as a read-only file; the link fails if the name exists, so a copy is never replaced. A day already there is compared by size (`--verify` also hashes the lake's bytes again). The tool deletes nothing from the lake or the copy: a copy that differs, or a day that has left the lake, is reported and kept, and the only file it removes is its own `.partial` from an interrupted run (a regular file: a link or directory by that name is left alone, and its day is reported missing until an operator removes it). There is no stored hash of a day file to compare with (the listing gives a size, an ETag and a creation time, and the API sends no Content-MD5 when it writes one), so the checks are of the bytes received and of the file written, and `--verify` is the check against the lake itself.

**The dataset.** `tank/azure/search-log`, by the layout in [the cold-storage note](../notes/2026-10-18-onprem-cold-storage.md): `atime=off`, `xattr=sa`, `acltype=posixacl`, `compression=zstd-3` and `recordsize=128K` (text), a `quota` of 10 GB (the cold-storage note's inventory of 18 October 2026 puts the container at about 0, so the quota only stops a fault filling the pool), `sharenfs` and `sharesmb` off. It is not shared by `serve-scans`, the serving container or any other server, and not replicated to `usbbak`: with the lake's file, this is the second copy and there is no third. Snapshots (sanoid): daily 14, weekly 8, monthly 12; nothing hourly, since a file arrives once a day. Mode `2750`, user `usnm-searchlog` (the copy's service user), group `searchlog-readers`: the people in `USNM_SEARCH_LOG_READERS`, kept the same by hand.

**Privacy.** The copy puts search text on the home host. [ADR-0012](../design/adr/0012-anonymous-search-log.md)'s rules carry over: no identifiers are added, and the readers are the readers of the `searches` container, no more. The tool never parses a day file; its logs, its errors and its status file hold day names, sizes and counts (and checksums when a check fails), never a search. Search text rests only in the dataset: the destination must be a mount point (otherwise the tool refuses to write), `days/` must be a real directory (a symbolic link is refused, since the service's state directory is writable and a link to it would take the text out of the dataset, and no file in it is reached through a link), the unit has a private `/tmp`, and the partial copy is a file in the dataset. Keep the dataset out of container and VM backups (a bind mount, with the backup job checked) and out of any share or export.

**The alert.** A run fails, and the unit with it, when a day file is still missing from the copy, or the copy differs, more than 24 hours after the lake wrote it (`--alert-after-hours`). The journal then has the day names (`journalctl -u usnm-searchlog-copy`; the failure is a JSON line with `"outcome":"failed"`), and `/var/lib/usnm-searchlog-copy/status.json` has `ok`, `overdue_days`, `missing_days`, `mismatched_days`, `local_only_days` and `last_success`, for whatever watches the host (the data-flow observer is the place the cold-storage note names; it doesn't read this file yet). A run that stops before it can finish (the lake won't list, the dataset isn't mounted, the binary is missing or not executable) fails the unit and leaves the previous status file as it was, so `checked_at` is the last run that got through. A host that is down or a timer that is disabled runs nothing, so it doesn't fail: a watcher should treat a `checked_at` more than two days old as the alert. A day with no file in the lake (no searches that day, or one the API lost) is not a missing copy and isn't reported.

**When it reports a day.**

- _Missing_: look at the journal for why the copy failed (the identity's grant, the lake's firewall, the dataset's quota, or a link or directory named `{day}.jsonl.partial` in `days/`, which the tool leaves for an operator to remove), fix that and run `sudo systemctl start usnm-searchlog-copy`.
- _Mismatched_: the copy is not replaced. Check it against a snapshot, and against the lake with `--verify`. When the lake's file is the right one, move the bad copy aside by hand (`mv`, as an operator; the tool never removes one) and run the copy again.
- _Local only_: the lake has lost a day file (soft delete keeps it for 3 days: restore it there first). The copy is then the only one: snapshot the dataset (`zfs snapshot`) before anything else.

**Restoring.** A damaged copy comes back from a snapshot (`.zfs/snapshot/<name>/days/`, or `zfs clone`) or, for a day still in the lake, from the next run. A day lost from the lake is written back from the copy, create-only, by an operator with Storage Blob Data Contributor on the container and their own sign-in (no SAS, no key): `az storage blob upload --auth-mode login --account-name <account> --container-name searches --name days/<day>.jsonl --file <copy> --overwrite false`. A lost dataset is rebuilt by running the copy into an empty one (`--verify` afterwards), as long as the lake still has the files.
