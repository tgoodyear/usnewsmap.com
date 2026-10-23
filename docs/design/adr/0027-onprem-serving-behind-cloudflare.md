# ADR-0027: Serving moves on-prem behind a Cloudflare Tunnel; Azure keeps a cold off-site copy

- **Status:** Proposed (2026-10-22). The maintainer decided on 22 October 2026 that serving moves on-prem ([#955, decision](https://github.com/tgoodyear/usnewsmap.com/issues/955#issuecomment-6354758689)). This record sets out how, from the [architecture comment](https://github.com/tgoodyear/usnewsmap.com/issues/955#issuecomment-6331553520) on the same issue. It stays Proposed until the maintainer accepts it and settles the open decisions below.
- **Date:** 2026-10
- **Amends:** ADR-0023 decision 2; ADR-0005 decisions 1 to 5; ADR-0006's availability trade-off and 09 §9.1; ADR-0008; ADR-0010; ADR-0012; ADR-0015; ADR-0021; ADR-0022 (its title and decision 1)
- **Extends:** ADR-0024 (a second tunnel token and a Workers-scoped API token under its rules)
- **Supersedes:** the end state of #917, in which Azure kept the searcher, the API and the web app
- **Relates to:** #955, #283, #917, #942, #944, #967, #969

## Context

Public serving runs in Azure Container Apps. `ca-usnm-prod` serves the site and the API from one image ([ADR-0010](0010-site-served-by-the-api.md)), searcher apps run Quickwit, and the index is in Blob Storage. Serving cost about $250 a month at 16 to 20 October 2026 rates (Cost Management by resource and meter, times 30), against [ADR-0005](0005-sustainability-constraints.md)'s ceiling of $80 for everything. All of Azure came to about $427 a month on 22 October.

Since that ceiling was set:

- The BOINC server moved to an on-prem container behind a Cloudflare Tunnel (#942, [ADR-0024](0024-cloudflare-tunnel-and-r2-credentials.md)). The `usnewsmap.com` zone is on Cloudflare. Universal SSL is issued for the apex and a wildcard, and the CAA records list Cloudflare's CAs alongside DigiCert (checked 21 October 2026).
- The #283 rebuild runs on pve-big and writes its index to `tank`.
- On 22 October the maintainer decided that serving moves on-prem once pve-big has an NVMe drive and a UPS, and that Azure ends as a cold off-site copy. That copy stays the second site that [ADR-0022](0022-onprem-cold-storage.md) decision 5 asks for, until another exists.

Two records say otherwise. ADR-0022's title keeps the live index and public serving in Azure. [ADR-0023](0023-onprem-scan-access.md) decision 2 says nothing on pve-big is reachable from Azure or the internet.

## Decision

### 1. One serving container on pve-big

- An unprivileged LXC container that does nothing but serve. It has nesting and keyctl for podman, as the BOINC container has. It runs:
  - the API and site as two podman units, the blue and green colors, from the same image as today (`usnewsmap-api:<sha>`), pulled by digest from ACR with AcrPull;
  - the pinned Quickwit binary from ACR (`install-quickwit.sh`), with the searcher and metastore roles, a read-only `file://` metastore and the caches in `infra/quickwit/searcher.yaml`, on loopback;
  - Caddy on loopback, forwarding to one color;
  - `cloudflared`.
- It starts at boot, takes unattended-upgrades, and gets about 4 cores at a high CPU weight and about 12 GiB.
- The only state on it is the response cache, so it can be rebuilt from scripts. The units, Caddy and `cloudflared` configuration, env-file generation and the update timer live in `scripts/onprem/serving/`. The container, its VLAN and its firewall live in the lab repository, next to the BOINC container's script, so no internal addresses or hostnames land here.
- A container shares the host's kernel with `tank`, the backup server and `serve-scans`. Unprivileged ID mapping, Proxmox's default AppArmor profile, no device passthrough, read-only bind mounts and the firewall in decision 2 reduce that risk but don't remove it. A KVM VM instead is open decision 7.
- pve and pve-laptop can't hold the 1.2 TB index.

### 2. Exception to ADR-0023 decision 2

ADR-0023 decision 2 now allows one exception: the serving container, which the internet reaches only through its own Cloudflare Tunnel. The container dials out to Cloudflare, and no inbound port is opened. Everything else on pve-big stays unreachable. Decision 1 (never expose `serve-scans` or any general file, S3 or NFS server) is unchanged.

The container:

- has its own VLAN, behind pve-firewall;
- connects out only to a written allowlist:
  - Cloudflare's tunnel port, 7844 over TCP and UDP;
  - HTTPS to Entra, the Arc agent's endpoints (Microsoft's list), the lake, Cosmos, Application Insights ingestion, ACR and `management.azure.com` (the registry token exchange), the GitHub API and Sigstore (attestation checks), the OS mirrors and pkg.cloudflare.com;
  - NTP;
  - DNS to a public resolver, not the LAN gateway, whose local DNS answers for internal names;
- reaches nothing else on the LAN and none of the host's management ports;
- never mounts a `tank` dataset. The host bind-mounts index datasets into it read-only.

### 3. Cloudflare edge on the Free plan

- **A new tunnel, `usnm-serving`,** remotely managed and separate from `usnm-boinc`. `cloudflared` comes from pkg.cloudflare.com through apt, and unattended-upgrades gets an origin pattern for that repository (by default it installs only the distribution's security updates). Log level info, because debug logs every URI.
- **The public records move at the cutover (Phase 3) and not before.** Apex, www and api become proxied records to the tunnel. Proxied while they still point at Container Apps, they would block the managed certificates' renewal, and Free Origin Rules can't override Host or SNI.
- **Redirects.** www→apex becomes a Single Redirect, and the API's own redirect stays as a fallback. Always Use HTTPS is on, because nothing behind `cloudflared` redirects HTTP.
- **Cache Rules.** On Free, a Cache Rule can override the origin's TTL only at 2 hours or more, so short TTLs come from the origin's `Cache-Control`. Free rules have no regex, so they match with `starts_with`, `in` and `contains`.

  | Path                                                                                                       | At the edge                                                                                                              |
  | ---------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
  | `/assets/*` (this also covers `states.geojson` and `.map` files)                                           | Cached for the origin's TTL: a year, `immutable`, hashed names, no purge needed                                          |
  | `/v1/places`, `/v1/titles*`, `/v1/coverage`, `/v1/days` and their `/api/v1` twins, when the query has `v=` | Cached under the origin's `Cache-Control` (24 hours), bypassed without one. The 307, the 202 and problems are `no-store` |
  | `robots.txt`, `favicon.svg`, `og-image.png`                                                                | Cloudflare's defaults (1 hour from the origin)                                                                           |
  | `/`, `/datasets`, `/privacy`, `/credits` and the 404 shell                                                 | Not cached. `/` revalidates with its ETag; the `app_route` pages get an ETag in Phase 0                                  |
  | `/v1/aggregate`, `/v1/hits`, `/v1/meta`, `/v1/status`, `/v1/versions`, `/v1/beacon`, `/healthz`, `/readyz` | Bypass. The aggregate and hits bypass is the last rule in the list                                                       |

  By default Cloudflare ignores `Vary: Origin`. A Cache Rule could key on `Origin`, but that is untested, so the rules apply on the apex only, and researchers' cross-origin calls to `api.usnewsmap.com` aren't edge-cached. Smart Tiered Cache is on. Nothing is purged on deploy, so the release path holds no Cloudflare token. Asset 404s get `no-store` (Phase 0), so the edge can't cache a 404 served in the gap during a deploy.

- **Zone settings.** They are zone-wide, so they also change the proxied `boinc.usnewsmap.com`. Each old value is read and recorded before it changes. The values to set:
  - Bot Fight Mode off. It sets a cookie (`__cf_bm`) where `/privacy` says the site sets none, challenges API scripts, and can't be skipped per path. It was turned off at the BOINC cutover (#942).
  - Browser Integrity Check skipped for apex, www and api. It answers `Python-urllib` with a 403 (`docs/boinc-onprem.md`), which would block API scripts.
  - Web Analytics automatic setup off. The CSP's `script-src 'self'` would block it anyway.
  - Speed Brain off, unless the maintainer chooses it. It adds a `Speculation-Rules` header, so browsers prefetch same-site links, each one another HTML fetch through the tunnel.
  - Rocket Loader, Email Obfuscation, Automatic HTTPS Rewrites, Replace insecure JavaScript libraries and managed robots.txt off. They rewrite HTML or `robots.txt`, and three of them make Cloudflare drop the origin's ETags.
  - Always Online off. It turns off `stale-if-error` and doesn't act on error 1033.

### 4. One origin, no Static Web App

The site stays in the API's image, on one origin.

- **No Static Web App.** The search log and the OCR-request queue admit only requests from the site's own origin (`Origin: https://usnewsmap.com` or `Sec-Fetch-Site: same-origin`, `crates/usnm-api/src/searchlog.rs`). A site on a Static Web App calling `api.usnewsmap.com` sends `same-site`, so every search would be dropped. Every Static Web App deploy path uses the shared deployment token, which ADR-0009 forbids. Linking an API backend needs Standard ($9 a month, new Azure cost), and network-isolated backends aren't supported.
- **No Workers Static Assets or Pages.** They split one release into two deploys that must ship the same build. With the site on its own hostname, the same origin check drops every search. With `/v1` routed back to the origin on the same hostname, it is today's single origin plus a second deploy path, through Cloudflare's GitHub App.
- Cloudflare in front of the one origin takes the role the growth profile gave Front Door ([ADR-0003](0003-stateless-aggregate-first-api.md), [ADR-0006](0006-lean-hosting-profile.md)), at $0. Hashed assets and versioned lookups come from the edge. HTML, searches and status still reach the origin, now over the home uplink instead of from East US 2, so their first byte may get slower. Phases 0 and 2 measure it.
- The last Static Web App, `usnm-boinc-swa-prod`, has no custom domain and no deploy workflow. It is retired separately (Phase 5).

### 5. Index on NVMe, previous version on `tank`

- A host-managed ZFS pool on a 4 TB NVMe drive in pve-big's empty M.2 slot holds two versions, so a release is a switch, not an overwrite. The host copies each version from `tank` (`zfs send` and `zfs recv`) into a dataset of its own and bind-mounts it into the container read-only, at the path the release wrote into its `file://` URIs. How each new version reaches the running container is open decision 6. ADR-0022 decision 4 already allows NVMe for rebuildable data.
- The previous version stays on `tank` in a dataset of its own, with `sync=standard` and snapshots, outside the rebuild's scratch quota. If the NVMe fails, the host attaches that copy read-only and the site serves from the HDDs, more slowly.
- The index doesn't come from the rebuild's scratch dataset. That dataset has `sync=off` and a 2 TiB quota, its container doesn't start at boot, and it sits on the HDD pool, where Kafka stalled for 2 to 9 seconds.
- The response cache is a writable dataset on the NVMe pool. A 14-day tmpfiles rule replaces the lifecycle rule.

### 6. Search log, OCR requests, reference data and Cosmos in Azure

The container reaches them with its own Arc identity.

- **ADR-0015 is amended.** The container is onboarded to Arc with decision 2's lockdown (an allow list of one extension, guest configuration and incoming connections off). It is not added to the OCR nodes' group, so `onboard.sh` isn't used as it is. That group includes the BOINC host, which ran a PHP build with two unauthenticated remote code execution bugs until it was patched by hand on 21 October 2026 (#969). The container's principal id goes in a setting of its own, as `onPremReleaseHost` does.
- **Its grants**, in a module like `infra/modules/onprem-release.bicep`, provisioned after onboarding because they need the principal id: Storage Blob Data Reader on `reference`; the search-log writer role (list, read and create, no delete) on `searches` and `ocr-requests`; Cosmos DB Data Reader on `batches`, `index_runs` and `ops`; Monitoring Metrics Publisher; AcrPull. They cost $0. No SAS, no keys.
- **ADR-0008 is amended**, behind an `onPremServing` setting that stays off until Phase 2:
  - The lake's opening to the home IP becomes permanent while serving is on-prem. Today it is open only while another on-prem term is on (`lakeOpen(...)` in `infra/main.bicep`), and closing the Serial Set run would close it. The new term goes into `infra/main.bicep` and `infra/policy-exemptions.bicep` together; with only one changed, provisioning is denied.
  - Cosmos allows public network access with IP rules for the home IP, and its account joins `publicAccountIds` so the guardrail policy exempts it. Today `infra/modules/cosmos.bicep` hard-codes `Disabled`. Cosmos opens only when Phase 2 first needs it. Without it, `/datasets` shows "not connected" and `/v1/versions` loses its pipeline-derived list; search is unaffected. Opening Cosmos to the home IP is also an option for #967's claim and collect move, so the two are decided together.
  - An IP-change runbook. A new home IP cuts lake and Cosmos access and invalidates IP-pinned Cloudflare tokens; the tunnel is unaffected. No restarts or deploys until lake access is back, because a color started without the lake never becomes ready.
- **Azure calls on the request path.** For `/v1/aggregate` and `/v1/hits`, the only Azure call is the persistent-cache read after an in-process miss, capped at 2 seconds. `/v1/status` and `/v1/versions` still refresh on a request once a minute per replica (three Cosmos queries and the lake's `reference` reads), until Phase 5 moves reference on-prem or a background refresh replaces it.

### 7. Credentials

The ADR-0009 rules stay. Everything on the Azure side is Entra-only, through the container's Arc identity.

- The `usnm-serving` tunnel token follows ADR-0024's rules: kept in Key Vault and read through Entra RBAC, with one root-only environment file in the container, and no `cert.pem` anywhere. ADR-0024 already expects a serving tunnel under the same rules.
- The tunnel's ingress is mirrored in the repository (ADR-0005 decision 3) and applied by an operator script with ADR-0024's IP-limited API token.
- The fallback Worker in decision 10 needs a token with Workers scope. A leaked one could put a Worker in front of every page, so it is a separate IP-pinned token, used only by hand, and revoked in Phase 4 unless the Worker stays.
- No R2 or S3 keys for serving data, and no public image registry.

### 8. Deploys and releases

- **Blue/green.** The two API colors listen on loopback ports under host networking, as the BOINC host's containers do. Caddy forwards to one, and a switch is a `caddy reload` once the new color's `/readyz` passes. This replaces Container Apps' revision overlap. Each unit's stop timeout is at least 35 seconds, because the search-log and OCR flushes take up to 10 seconds each and podman's default is 10.
- **The update timer.** A timer in the container, modelled on `scripts/ocr-node/update-worker.sh`, reads the commit of the latest prod deployment from GitHub's public API, verifies the image's build attestation without a token, and switches colors only after `/readyz` passes, rolling back otherwise. CI attests only the OCR worker today, so the API image gets an attestation step in Phase 0. No workstation and no inbound SSH are in the deploy path.
- **Settings.** The units' environment files are generated from `infra/env/prod.json` and the settings store, the sources Bicep uses, and checked against the Azure app's settings.
- **Releases.** The on-prem flow builds a version, verifies it with `compare-versions`, copies it to the NVMe, publishes it to the index account while Azure still serves, and switches `current.json` once both origins hold it. Both origins read the same `current.json`, so they serve the same version. The previous version is kept, and each run is recorded. `release.sh` needs a delta mode, since it runs only full releases today. The Azure weekly job moves to curate and titles only.

### 9. The search log and privacy

This amends ADR-0012 and 09 §9.4.2.

- **The edge never caches `/v1/aggregate` or `/v1/hits`.** Every search still reaches the API, so the search log and the OCR-priority stager see it. A Phase 2 check asserts `cf-cache-status` for each path class.
- **The search log doesn't change.** It stays permanent, anonymous and in Azure, with the same record. During Phase 3 both origins write the one log. The fallback Worker forwards `Sec-Fetch-Site`, so searches it sends to Azure are still admitted.
- **The on-prem search-log and OCR-request URLs stay unset until the cutover,** because `admit()` records same-origin requests on any hostname, including the Phase 2 test hostname.
- **The client IP.** A new setting makes the API take it from `CF-Connecting-IP` only when the peer is loopback, instead of counting proxy hops. It's used as today: the rate limiter's in-memory salted hash and a page view's location lookup.
- **No client IPs or query strings in host logs.** `cloudflared` runs at info, Caddy's access logs are off (its default), Quickwit keeps the `RUST_LOG` filter that stops it logging queries (now set in `infra/modules/containerapp.bicep`), and the API's console logs go to journald with neither. Error logs carry request data too: `cloudflared` can log a failed request's destination URL at error level, and Caddy's error logger records the request's URI and headers. Both are set to drop or redact the query string, the headers and the client IP in those records before they reach journald. A Phase 2 drill forces an upstream failure at each proxy with a marked search and checks that neither the marked words nor the client IP appear in any retained log.
- **Every request passes through Cloudflare.** It terminates TLS, so it sees each client's IP address with the full URL, including the search words in `q`, and the request headers. Cloudflare's [privacy policy](https://www.cloudflare.com/privacypolicy/) (section 6) makes it a data controller for the personal information it collects about visitors, with two exceptions where it is a processor acting on the customer's instructions: Customer Logs (the visitor data it shows the customer in its dashboard) and the content that transits its network. Its [customer DPA](https://www.cloudflare.com/cloudflare-customer-dpa/) applies under the Self-Serve Subscription Agreement. [Security Analytics](https://developers.cloudflare.com/waf/analytics/security-analytics/) keeps sampled request logs with the client IP and path for up to 31 days on the Free plan. Whether Security Analytics or Security Events store the query string is measured in Phase 2. What to do about it is open decision 1, settled before Phase 3. Until then, no Cloudflare rule acts on the search paths: no rate-limit rule there, and the managed rules skipped there if Free allows it. During Phase 2, Security Events must hold no search-path entries.
- **`/privacy` changes at the cutover, not before.** Its draft is a separate pull request, deployed to both origins in Phase 3.

### 10. Availability and outages

This amends ADR-0005 decision 5, ADR-0006's availability trade-off and 09 §9.1.

- **The target stays at 99.0%** for `/v1/aggregate` and `/v1/hits` (about 7.3 hours a month). ADR-0006 accepted that target with "Stateless app restarts in seconds" as the mitigation, which one host doesn't give. Edge errors never reach the API's logs, so the SLI becomes the existing availability test (one standard test, every 15 minutes, from 3 locations), pointed at an example `/v1/aggregate` search, plus a count of fallback answers. A second test would be new Azure cost.
- **Phase 3 has an automatic fallback.** A Worker passes every request to the tunnel. Only on a 502, a 530 or 1033, or a thrown fetch does it retry at `ca-usnm-prod`'s default `azurecontainerapps.io` name, which serves the same index version. It doesn't retry on a timeout, which could log a search twice, and the API's own problem+json 503s pass through. Workers Logs are off, because URLs carry `q`. The route fails open. Every request on a Worker route counts toward Workers Free's 100,000 a day, cached or not, and the architecture comment gave `/assets/*` a route without the Worker for that reason. But a year-long TTL doesn't keep an asset in every edge cache. During an outage, an asset miss on that route would go to the failed tunnel, and a visitor would get the HTML from Azure without its JavaScript and CSS. Which route `/assets/*` takes is open decision 10, and the Phase 2 outage drill includes a cold-cache browser load. Fallback answers can be counted in `AppRequests` (Container Apps tags each with its replica, and only the Worker reaches that app), which measures on-prem downtime.
- **After Phase 4, ADR-0005 decision 5 is restated.** In a searcher outage the API's own cache still serves cached results with a banner, as today. In a whole-origin outage, hashed assets and edge-cached lookups still serve, but HTML is never edge-cached, so visitors would see Cloudflare's error page, API clients would get HTML instead of problem+json, and search would be down. The degraded mode serves the last good shell with a status banner, and no cached search results. How to provide it is open decision 2, and it is a Phase 4 gate.
- **Monitoring.** The `api-errors`, `searcher-health` and `warm-up` queries and the API workbook read Container Apps tables, and the #238 CPU alert reads Container Apps metrics, so on-prem they see no data. Before Phase 4 they're replaced, with the OCR nodes' pattern (the Azure Monitor Agent into their workspace, after a cost check; ADR-0015) or local alerting. Either must cover the container's heartbeat, OOM kills, NVMe wear, ZFS and SMART errors, a full disk, and a failed unit or `cloudflared`. Cloudflare's free Tunnel Health Alert is added.
- **One host, one uplink.** Phase 3 measures whether that is acceptable. A second tunnel connector wouldn't help without a second origin: replicas go to the nearest connector without checking the origin, and no other home host can hold the index. Load Balancing (from $5 a month) helps only with a second origin too.
- **The Phase 4 gate.** All of these, before any Azure serving resource is deleted: the replaced `tank` disks; the NVMe; a UPS for pve-big and the network gear; a plug-in Intel NIC (the onboard one reset every 30 seconds for 6 minutes on 21 October 2026); blue/green deploys; the monitoring replacements; the degraded mode; 30 days in which the on-prem origin alone meets 99.0% (fallback answers count as downtime) with p95 within the SLO; a rehearsed reboot; this ADR accepted; and the maintainer's approval of each deletion, as ADR-0022 decision 3 requires. If the gate fails, Azure keeps serving, or the Worker keeps falling back to `ca-usnm-prod` at a minimum of one replica, with the searcher's minimum at zero (about $40 to $45 a month of existing resources), with every release published to Azure as well. A searcher at zero doesn't stay idle today: the API's readiness probe calls `/readyz`, which checks the searcher's health, every 10 seconds, and the API reads the searcher's metrics every 60 seconds (`USNM_SEARCHER_METRICS_SECS`). The fallback option needs a rehearsed standby mode in which neither of these wakes the searcher but the first fallback request does. Without one, the searcher keeps running and costs about $90 a month more.

### 11. Cost and patching

This amends ADR-0005 decisions 1 and 2.

- **Decision 1:** the ceiling covers Cloudflare and on-prem items as well as Azure. Cloudflare costs $0 in every phase: the Free plan covers the tunnel, Universal SSL, Cache Rules, Tiered Cache, Workers Free, the free managed WAF, one rate-limit rule and DDoS protection. Azure serving falls from about $250 a month to about $135 after Phase 1 and about $4 after Phase 4 (the availability test). All of Azure would then be about $105 to $110 a month, and about $5 to $15 at the end state, once the remaining jobs leave too (estimates in the maintainer's decision). The one-off purchases (the NVMe, the replacement drives, the UPS and the NIC) aren't priced, and power isn't measured; pve-big already runs all day.
- **Decision 2** (no VMs or other compute whose OS we patch) becomes a patching duty for the serving host: unattended-upgrades in the container, a scheduled Proxmox update window, and image rebuilds when a security advisory lands. ADR-0005's context names hand-configured, institution-internal servers among the old site's root causes, and #969 shows a hand-run server falling behind on patches.
- No new Azure cost: no Static Web App, no Front Door, no second availability test.

### 12. Governance

This amends ADR-0005 decisions 3 and 4.

- **Decision 3:** the tunnel's ingress is mirrored in the repository, and the zone settings are applied by an operator-run script that reads its token from Key Vault, never from GitHub. Until that script exists, the zone settings live in the dashboard, and their recorded values are the reference.
- **Decision 4:** the Cloudflare account gets a second member, and a second person can operate the host. Today the account has one admin.

### 13. Cold off-site copy in Azure

- **ADR-0022** is amended. Its title and decision 1 kept live and public-serving data in Azure; that no longer holds for serving. Decisions 2, 3 and 5 stand. Decision 4 now also covers the serving index on NVMe, and decision 5 covers the search log, which has one copy today: a Standard_LRS lake with 3-day soft delete. Its second copy is open decision 3.
- **ADR-0021** is amended. After Phase 4 the separate index account, its endpoint and `publish-staged` retire with the API and searcher apps.
- **What stays in Azure after Phase 4:** the Container Apps environment and ACR, kept for the ingest jobs, `ocr-sync` and the BOINC hand-off job; the blob and Cosmos endpoints; and the availability test.
- **The end state** is the maintainer's 22 October decision. Azure keeps one Cold-tier copy of what can't be re-derived: the search log, our OCR output, BOINC results and Cosmos exports, for about $0.15 a month (about $5.60 if it also keeps the optional copies of `curated` and the scans). Nothing reads it in normal operation once Phase 5 has moved reference, the search log, `ocr-requests` and Cosmos on-prem.

### 14. Phases

Each phase ends in a working state. The [architecture comment](https://github.com/tgoodyear/usnewsmap.com/issues/955#issuecomment-6331553520) lists each phase's steps.

| Phase                                                                                                  | Ends with                                                                   | Rollback                                                                                                                                                 |
| ------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 0: code, the Bicep setting, scripts, docs and baselines; no serving change                             | Azure serving as today                                                      | Revert any single pull request                                                                                                                           |
| 1: #283's publish and promotion                                                                        | Azure serving run e, with an identical copy on `tank`                       | The previous `current.json`                                                                                                                              |
| 2: hardware, the container, a private test hostname, the on-prem release flow, drills and measurements | The public still on Azure                                                   | Delete the test hostname and the container; each hardware step has its own                                                                               |
| 3: public cutover with the fallback Worker, for 30 days                                                | On-prem serving the public; Azure answering when the home side fails        | Remove the Worker route and set the three records back to DNS-only at their recorded targets: about 5 minutes, plus resolver caches of up to 300 seconds |
| 4: retire Azure serving, behind the gate in decision 10                                                | Nothing in Azure on the request path                                        | Hours: provision the apps, run `release.sh publish` (about 1.2 TB, about 2.7 hours at 1 Gbps), and point a Worker at the new app's default name          |
| 5: reference, `ocr-requests`, pipeline state and `curated` move on-prem, one at a time                 | Each with its backup, off-site copy and restore point and time stated first | Point the setting back at the Azure container, kept until the step has proved itself                                                                     |

- **The Phase 1 freeze.** From the promotion until the first on-prem release, nothing writes `current.json` or the index account's `qw-index`: no weekly ingest or backfill, no manual, OCR or Congress release and no prune. Otherwise the next weekly run would write a delta into Azure that the on-prem copy never gets. New batches aren't searchable during the freeze. That is an exception to 09 §9.1's 7-day freshness target, bounded by the freeze's maximum (open decision 4). If the maximum passes, the freeze lifts, Azure releases resume, and the copy is taken again from the next full rebuild before Phase 3.
- **Phase 3's hard end.** Container Apps certificates can't renew while the names are proxied. All three (apex, www and api) expire on 28 March 2027, so Phase 3 ends before Azure would start renewing them.

## Open decisions

These are the maintainer's. Each lists the options; none is chosen here.

1. **Cloudflare's analytics and the search words.** Cloudflare sees `q` on every search. Phase 2 measures whether Security Analytics or Security Events store the query string, and whether that can be turned off. 09 §9.4.2 allows raw client IPs only in access logs kept at most 30 days with the query string dropped, and Security Analytics keeps sampled IPs and paths for up to 31 days on the Free plan. The options:
   - (a) accept it and say so in 09 §9.4.2, ADR-0012 and `/privacy`;
   - (b) keep `q` out of what Cloudflare stores, by a setting if one exists or by taking `q` out of the URL;
   - (c) keep serving in Azure.
2. **The degraded mode after Phase 4.**
   - (a) Keep the Worker as the error layer ($0). On a 502 or 530 it serves the last good HTML shell from the Cache API with a status banner, and answers `/v1` with a problem+json 503 and `Retry-After`. The Workers token and the daily request limit stay.
   - (b) `stale-if-error` on the HTML. That needs a Cache Rule making the HTML paths eligible and a short max-age in place of `no-cache`, because Cloudflare ignores the directive next to `no-cache`. Cloudflare documents 500, 502, 503 and 504 as triggers, so with the tunnel down (530 or 1033) it probably won't serve stale content (an inference from the documentation, tested in Phase 2 if chosen).
3. **The search log's second copy** (ADR-0022 decision 5). One option is a daily copy of the day files to a restricted dataset on `tank`, weighed against keeping search text on the home host.
4. **The freeze's maximum length,** set on the promotion date. Proposed: 4 weeks.
5. **How on-prem releases update Cosmos `index_runs`,** so `/datasets` and `/v1/versions` stay right. `release.sh` keeps a local state file today.
6. **How a new version reaches the running container:** a mount point per version, added before the switch, or one mount whose contents the host swaps.
7. **Container or VM.** An unprivileged LXC container (proposed, following the maintainer's direction of containers over VMs on the pve hosts), or, as an exception to that direction, a KVM VM for kernel isolation from `tank`, the backup server and `serve-scans`.
8. **Searcher upgrades.** With one searcher, a restart is an outage with cold caches. The procedure, and how `searcher.yaml` changes reach the container, since `roll-api.sh` applies them only on Azure.
9. **The flood rule:** its path, threshold and action, for a runbook that names the rule to turn on, with its privacy cost from open decision 1. Under Attack Mode would break API scripts. A challenge action sets a `cf_clearance` cookie, and a rate-limit rule that counts by `cf.unique_visitor_id` sets `_cfuvid` ([Cloudflare's cookies](https://developers.cloudflare.com/fundamentals/reference/policies-compliances/cloudflare-cookies/)), so either would change what `/privacy` says about cookies. The API's per-client limit (120 a minute, burst 40) stays.
10. **Which route `/assets/*` takes during Phase 3.**
    - (a) The Worker's route. An asset the edge doesn't hold also falls back to Azure during an outage, and every asset request counts toward Workers Free's 100,000 a day, which the Phase 0 traffic baseline checks.
    - (b) A route without the Worker, as the architecture comment has it. Asset requests don't count toward the limit, but during an outage an asset miss goes to the failed tunnel and the page loads without its JavaScript and CSS.

## Measurements

Before the gates that need them:

- cold-search latency on the NVMe, idle, during a rebuild and during a publish, against p95 ≤ 2 seconds (09 §9.1), with the same scripts run against Azure on the same day. The evidence that `file://` beats Blob comes from SSD-backed NFS, not from this host;
- time to first byte, asset load and LCP, today and through the tunnel, from several regions;
- 30 days of traffic (requests a day, peak requests a second, bytes) against the Workers Free limit and the uplink;
- a cold-cache browser load through the fallback Worker with the tunnel down, what a Worker receives when the tunnel has no connectors, and the `X-Forwarded-For` order at Container Apps ingress behind a Worker;
- whether `cloudflared` adds an `X-Forwarded-For` entry;
- whether a forced upstream failure at `cloudflared` or Caddy puts a marked search or the client IP in any retained log;
- sustained upstream throughput (ADR-0022 records the uplink as 1 Gbps symmetric), and whether the home IP is static;
- the current zone settings;
- whether the M.2 slot shares lanes with SATA ports on pve-big's board (all six SATA ports are in use);
- Azure's renewal lead time for the three certificates;
- whether Security Analytics or Security Events store the query string;
- the share of the lake's Hot reads that is the index;
- Cloudflare Load Balancing's prices;
- on-prem power draw.

## Alternatives

| Option                                                  | Why not                                                                                                                                                     |
| ------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Keep serving in Azure (#917's end state)                | About $250 a month for serving, against an $80 ceiling for everything                                                                                       |
| A Static Web App for the site                           | Breaks the same-origin admission of searches, needs the shared deployment token, and can't link a network-isolated backend (decision 4)                     |
| Workers Static Assets or Cloudflare Pages               | Two deploys per release that must ship one build, and a second deploy path through Cloudflare's GitHub App                                                  |
| Serve from the rebuild container's dataset              | Scratch: `sync=off`, a quota, not started at boot, and on the HDD pool                                                                                      |
| Put the serving container in the OCR nodes' Arc group   | The group includes the BOINC host (#969)                                                                                                                    |
| A second tunnel connector, or Cloudflare Load Balancing | No second copy of the origin to send traffic to                                                                                                             |
| Front Door                                              | New Azure cost; Cloudflare's Free plan covers the edge                                                                                                      |
| An index copy in R2                                     | About $18 a month, and S3 keys                                                                                                                              |
| Move the search log, reference and Cosmos on-prem now   | The search log can't be rebuilt and has one copy, in Azure; moving it now leaves none off site. Each moves in Phase 5 with its own backup and restore drill |

## Consequences

- Azure serving falls to about $4 a month after Phase 4. Cloudflare adds $0.
- **Risks:**
  - Disks. Both `tank` mirrors have a degrading drive, the NVMe is one disk, and losing both copies of the index means a 15 to 20 hour rebuild.
  - Single points of failure after Phase 4: power, the ISP, the gateway and switches, one connector, and Cloudflare. Detection takes 15 to 25 minutes, and alerts go to one address.
  - Contention. A rebuild uses 94 to 100% of the host's 8 threads for 15 to 20 hours, and the current memory ceilings add up to more than the host has. Serving shares one 1 GbE NIC and one uplink with BOINC, the OCR nodes, rebuild downloads and publishes. Phase 2 lowers the rebuild container's memory ceiling and caps publishes and rebuild downloads on the uplink (ADR-0023 decision 4 already asks for an uplink cap).
  - Startup depends on Azure until Phase 5 moves reference on-prem. After a reboot with the lake unreachable, the site stays down until the lake is back. Search-log records still queued are lost on a restart.
  - Exposure. A public workload on the release host, and the API's identity on a host the internet reaches. Application Insights ingestion has no IP firewall, so a stolen token with Monitoring Metrics Publisher could write fake `usnm-api` rows or fill the 1 GB daily cap. The daily-cap alert has to cover it.
  - Cache misconfiguration. A rule that catches aggregate or hits undercounts the search log and starves the OCR queue; one that catches meta pins clients to an old version. The Phase 2 cache check guards against both.
  - The fallback window. Behind the Worker, Container Apps sees a Cloudflare address as the peer, so all fallback visitors share one rate-limit bucket and their page views geolocate wrongly; trusting a Worker-set header there would be spoofable through the default name. In the timer's lag after a merge (up to 10 minutes), a fallback page can name an `/assets` file the other origin lacks.
  - Freshness. New batches wait during the Phase 1 freeze. After Phase 3, releases depend on one host.
- **When this is accepted**, the amended records get a note pointing here: ADR-0005, ADR-0006, ADR-0008, ADR-0010, ADR-0012, ADR-0015, ADR-0021, ADR-0022 and ADR-0023.
- **Docs that follow** in their own pull requests: the "serving stays in Azure" lines in #917, design 15 and `docs/operations.md`; 09 §9.3 (runbooks for a host reboot, a home IP change, tunnel token loss, NVMe failure, a flood and a searcher upgrade), §9.4.1 (the stale `connect-src` line), §9.5 (costs) and §9.6 (the Phase 4 rollback as the DR runbook); 08 §8.8 and #944 (DNS in Cloudflare, and splitting `dnsZoneName` so retiring the zone doesn't touch serving); `docs/configuration.md` (the `CF-Connecting-IP` setting, and `USNM_TRUSTED_PROXY_HOPS`); and the alert descriptions and saved queries that read Container Apps tables. ADR-0019 needs an update for #942, and its status in the ADR index needs fixing.
