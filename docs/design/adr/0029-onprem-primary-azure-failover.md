# ADR-0029: On-prem primary serving with health-driven Azure cold standby

- **Status:** Accepted by maintainer, 31 October 2026 UTC; rollout and measurements must be evidenced separately.
- **Amends:** [ADR-0027](0027-onprem-serving-behind-cloudflare.md), especially decisions 6, 10 and 13 and Phases 3 to 5; [ADR-0028](0028-cloudflare-with-azure-origin.md) remains the pre-cutover routing record.
- **Authorization:** The maintainer authorized production downtime and routing changes and requires all public serving requests to use on-prem when healthy and Azure when on-prem fails. Related work: [#955](https://github.com/tgoodyear/usnewsmap.com/issues/955), [#1098](https://github.com/tgoodyear/usnewsmap.com/issues/1098), [#245](https://github.com/tgoodyear/usnewsmap.com/issues/245).

## Context

Before this rollout, the public apex, `www` and `api` used Cloudflare in front
of Azure; the isolated `dev` origin served the same Oct26 index from read-only
local NVMe. Production switched to on-prem at 02:01:07 UTC on 31 October 2026.
A healthy homepage is not proof that the searcher, published indexes or
reference state are usable. The previous Worker handles connector failure but
deliberately passes through application 503s and timeouts. That is insufficient
for the requested automatic failover.

Keeping Azure compute permanently warm is simpler but expensive. Conversely,
deleting it as ADR-0027 Phase 4 proposed would require provisioning and copying
an index during an outage. DNS changes alone neither detect an unhealthy
searcher nor wake an internal searcher reliably, and cached DNS makes recovery
time difficult to bound.

## Decision

1. Keep Cloudflare as the public edge. An explicit health-driven Worker mode
   routes the apex, `www` and `api` to the on-prem tunnel while `/readyz` is
   healthy, and otherwise to the retained Azure API's default hostname. Edge
   cache hits require no origin; BOINC hostnames are not covered by these routes.
2. Use a bounded, uncached readiness check before sending a visitor request.
   A heartbeat scheduled on Cloudflare, not on the home host, also checks
   readiness every minute. It wakes Azure only on an on-prem failure. A home
   heartbeat alone cannot detect its own power/ISP failure from outside.
3. Preserve method, body, query, same-origin admission headers and CORS.
   Readiness probes contain no visitor data and opt out of visitor logging.
   Mutating requests are selected before delivery and are never replayed after
   an ambiguous failure once sent. GET/HEAD may retry the other origin on
   failures; exactly-once anonymous search-log recording cannot be guaranteed
   if the first origin finishes after its response is lost. Do not replay
   beacons to pretend that no request was lost.
4. Keep Azure API and searcher provisioned and HTTP-scalable from zero.
   `USNM_API_MIN_REPLICAS=0` controls both through Bicep. A failed primary
   triggers an Azure readiness request; bounded readiness polling precedes
   sending the visitor request. If neither origin becomes ready within the
   budget, return an explicit uncached 503 with `Retry-After`. Cold standby
   therefore trades money for longer first-request latency and cannot promise
   zero errors during an arbitrary outage.
5. Keep **one complete current published search release** in Azure (main and
   Japanese indexes are one release, not redundant generations). Keep local
   rollback outside scratch, qualify it through actual Quickwit restart/query
   tests and never download bytes already present locally. New releases must
   be installed and qualified on both origins before changing the shared
   serving pointer. Old Azure releases may retire only after that acceptance;
   Azure is not an unlimited historical-index archive.
6. The approved retirement of the legacy lake indexes and the immediate
   version-specific permanent purge are recorded in [#245](https://github.com/tgoodyear/usnewsmap.com/issues/245#issuecomment-6536934940):
   657 versions / 1,218,881,115,715 bytes removed, zero retired versions left,
   current production response unchanged. Permanent-delete permission was
   temporary and restored to disabled; lake blob/container retention remains
   3/14 days. This does **not** authorize blanket permanent deletion of corpus
   data or a global one-day recovery window. Settled billing is separate from
   physical inventory.
7. Both serving origins initially use the same authoritative Azure reference,
   Cosmos status, anonymous log and OCR-request stores. The on-prem identity
   is separate from producer/BOINC identities, with existing Bicep least-
   privilege serving grants, and the index/cache remain local. This interim
   path moves the public API/web/search computation, not every state service.
   A cached local reference snapshot alone must not silently replace current
   metadata/status or drop search logging.
8. All-local BOINC/state authority remains separately gated: writer freeze,
   incremental document/ETag and deletion reconciliation, verified local
   corpus/handoff bytes, logged UTF8 PostgreSQL state, nonempty backup restore,
   and useful job/returned-result acceptance. Stateful BOINC failover needs
   replication and a single fenced writer; this Worker does not provide it.
   Do not switch BOINC backend authority merely because a public web cutover
   succeeded.

## Acceptance and operations

Before public routing, verify both origins against the same release, readiness,
real search/lookup responses, complete site assets, proxy privacy and identity
permissions. Preserve the previous DNS/tunnel/Worker settings for rollback.

Run continuous DNT synthetic requests through the public hostname across
cutover, connector stop, API stop, searcher stop, network failure and recovery.
Record timestamped origin, status, duration, completed and failed request counts;
separate interrupted in-flight requests from new requests. Stop drills and
restore persistent services even if measurement fails. A sample with no errors
does not establish zero loss for all visitor methods, regions or failures.

Measure a true Azure zero-replica start, not a revision restart or warm request.
Record API and searcher replica transitions, first ready response and first
successful real search. Keep readiness polling and cold-start time distinct
from network/search latency. Verify automatic return to on-prem and subsequent
Azure scale-down without an operator or workstation process.

The deployment receipt/runbook must report actual failover, recovery and cold
start times and exact probe/request budgets. Do not call these measured until
the live drills finish. The maintainer subsequently requested Workers invocation logs. Enable them
explicitly through Wrangler with query-string redaction; they retain
path/method/status and platform invocation metadata, unlike the fixed
origin/reason/status outcome records. A live synthetic inspection confirmed
that this metadata includes client-IP headers and geolocation: query
redaction does **not** anonymize those fields. Enabling the requested
invocation logs therefore changes the earlier no-IP edge-log policy.
Do not add request bodies, headers,
search words or client IPs in application code. No Logpush, Tail consumer or
tracing export is enabled. Retention/sampling and platform-field inspection
must be reported rather than claiming no URLs exist. Existing external
availability monitoring stays on the public edge.

Rollback restores recorded Azure origin DNS and removes only these Worker
routes. Keep DNS-01 certificate renewal and Full (strict) TLS working in either
mode. Worker limits/fail-open behavior and Cloudflare itself remain availability
dependencies; failover is not a second independent public edge.

### Production evidence

The [cutover receipt](https://github.com/tgoodyear/usnewsmap.com/issues/955#issuecomment-6537903917)
records DNS preservation, public responses, replica inventories and the
following continuous DNT readiness/Lindbergh measurements. Queries retained
79,332 hits on `pages-v20261026-1`. Synthetic clients allowed 215 seconds,
approximately two requests/second and at most eight in flight.

| Scenario                                   | Requests | Failed | First Azure response after completed injection | First on-prem response after recovery command |
| ------------------------------------------ | -------: | -----: | ---------------------------------------------: | --------------------------------------------: |
| Public DNS cutover                         |      179 |      0 |                                            n/a |                                           n/a |
| Connector failure, both Azure apps at zero |      269 |      0 |                                       68.331 s |                                       1.021 s |
| API failure, warm Azure                    |      119 |      0 |                                        0.490 s |                                       8.325 s |
| Searcher failure, warm Azure               |      119 |      0 |                                        0.019 s |                                       0.134 s |
| External-network packet loss, warm Azure   |      179 |      0 |                                        3.399 s |                                      20.465 s |

There were **0/865 measured public synthetic failures**; the longest cold
request took 68.236 seconds. These are observed samples, not universal
detection/recovery bounds or proof that every visitor succeeds. Initial
browser API GETs have no built-in 12-second deadline; the 12-second constant
reserves a server request within the subsequent bounded computation wait.
Basemap preloads and clients with shorter deadlines can still abort.
Searcher readiness sampling and tunnel reconnect backoff affect recovery.

The cold drill required empty API and searcher replica inventories before
failure. Both apps have explicit HTTP wakeup rules and zero minimum replicas.
Azure skips the API's example warm-up and keeps a 60-second readiness cap;
on-prem retains the 300/360-second blue/green policy. The generator checks
these intentional differences and rejects readiness drift.

The final heartbeat-only connector drill generated **zero synthetic visitor
requests** and verified both current Azure revisions had empty replica
inventories at 03:40:30 UTC. The connector was stopped from 03:40:32.126 to
03:43:38.840 UTC. All three outage heartbeats reported Azure / primary-unhealthy
/ HTTP 200; the first completed at 03:41:53.708 UTC, **81.583 seconds after
injection**, including 52.388 seconds waiting for the next scheduled heartbeat.
Its native execution wall time was 28.774 seconds; maximum scheduled CPU across
the three checks was **4 ms**, with no Worker errors. This is a heartbeat
availability measurement, not another visitor request-loss sample.

An earlier drill exposed a false-primary-health result: a scheduled/custom-
domain probe can receive the gateway's Azure fallback response through the apex
route. HTTP 200 with body `ready` alone is insufficient. Primary readiness now
rejects `X-USNM-Origin: azure`; public, custom-domain and scheduled regression
tests cover this case. The corrected Worker was deployed before the final drill.
Recovery is automatic; Azure scale-down is observed against the current
latest/ready revisions rather than a pinned obsolete API revision. Platform
cooldown and background work mean combined scale-down is not a ten-minute SLA.
After the guarded drill, the current API revision was observed empty at
03:53:01.951 UTC and both API/searcher inventories were empty by
04:02:14.273 UTC, **18 minutes 35.433 seconds after connector restoration**.
Subsequent samples remained empty while the minute heartbeat selected on-prem.
This required no scale-down command.

Healthy public acceptance also verified HTML and live metadata remained
Cloudflare DYNAMIC, versioned title lookup and JavaScript warmed MISS to HIT,
and two PMTiles requests returned exact 127-byte HTTP 206 ranges with no content
encoding. Only 254 archive bytes were read, not the index/corpus. Actual
on-prem reference, Cosmos, search-log and OCR-request settings matched Azure;
allowed-origin hosts and local Quickwit remain intentional differences. The
owned qualification hold was retired, and the updater verified trusted-main
CI provenance before deploying the current API image. An observed local
readiness drift was corrected to the generator's 300/360-second policy.

### Code-backed edge configuration and CI

`healthcheck.usnewsmap.com` is a Workers **custom domain**, not another tunnel
origin. `/` reports availability/selected origin/primary readiness; `/readyz`
is uncached, and other paths return 404. Its probes target the apex, never its
own custom domain. Healthy checks never poll or keep Azure warm.

The health Wrangler configuration and deployment generator set handler
`cache.enabled=false`, default placement, compatibility date `2025-10-01`,
no compatibility flags, disabled previews, minute cron and explicit
query-redacted invocation logging. Static origin headers/manual Cache API
behavior remain separate; gateway caching must not bypass readiness.

`.github/workflows/edge-worker-deploy.yml` deploys after successful trusted-main
`ci`, checks out that immutable SHA and refuses to replace a newer main
release. It validates Node tests, pinned Wrangler/workerd and shell scripts,
then uses production Azure OIDC and `scripts/ci/deploy-edge-worker.sh`.
Manual dispatch is restricted to main and repeats the edge checks.

`infra/modules/edge-worker-ci.bicep` grants the existing CI identity read-only
access to exactly two secrets in the existing production CI vault. It does
not change vault networking or expose the home vault to GitHub-hosted IPs.
The Cloudflare token permits Worker scripts for the account and Worker
routes/zone read for this zone, with no token-administration or DNS-write
permission. Secrets/account identifiers remain in Key Vault, not Git.
The provisioned token expires 31 October 2027 and must be rotated beforehand.

Native analytics expose aggregate errors/status, not origin-specific
fallback totals. Opt-in fixed outcome logs provide origin/reason/status
counts; heartbeat records are marked separately and health endpoint traffic
must not be confused with visitor traffic. Route-quota fail-open is
dashboard-only in the documented API and is not claimed configured.

The live deployment wrapper passed using the same scoped CI credentials and
ordinary DNS/TLS, including the custom-domain health assertion. The first
GitHub-hosted automatic deployment is a post-merge acceptance gate: this new
trusted-main workflow cannot register on the default branch before merging.
Do not weaken its branch/provenance guard to obtain a pre-merge demonstration.
The final edge suite passed 90 tests plus native workerd integration and the
pinned Wrangler dry-run; the integrated GNU/Linux serving suite passed 43 tests.

Conservatively budget the Free plan's 100,000 requests/day and 10 ms fetch CPU;
the account subscription response alone does not establish a paid entitlement.
Native aggregate analytics reported 2,832 account-wide requests (1,900 for the
production Worker) and zero errors from UTC midnight through 03:36 on the drill
day. Neither that small sample nor scheduled CPU establishes a fetch-CPU or
traffic headroom SLA. Invocation-log retention is currently three days on Free
and seven on Paid, separate from the previously disclosed security logs.
An aggregate-only query covering 155 unique fetch invocations from the guarded
fault through 04:01:40 UTC reported maximum fetch CPU of **8 ms**. That is only
2 ms below the conservative Free limit; retain CPU/error/traffic monitoring
rather than claiming ample headroom or a universal cold-request guarantee.
The visitor privacy page explicitly discloses on-prem/Azure routing and
IP-bearing operational invocation logs; query redaction does not imply anonymity.

## Alternatives and consequences

- **Warm Azure standby:** lower outage latency, recurring idle compute cost;
  use it if measured cold-start delay exceeds the accepted recovery budget.
- **Delete Azure compute/index:** rejected; incompatible with automatic,
  bounded failover without a large re-copy or provisioning step.
- **DNS heartbeat switching:** rejected as the primary mechanism because
  propagation and resolver caching complicate timing and in-flight continuity.
- **Cloudflare Load Balancing:** a managed alternative with recurring cost;
  the existing Worker keeps scope and cost smaller, but requires explicit
  readiness, cold-start and method-safety testing.

On-prem power, ISP, storage and the single searcher remain failure domains,
but Azure answers new serving requests when the home side fails. Shared Azure
state remains a dependency for both origins until a separately accepted
replication/fencing design replaces it.
