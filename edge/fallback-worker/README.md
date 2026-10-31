# Fallback Worker

A Cloudflare Worker that sits in front of the `usnm-serving` tunnel during Phase 3 of the on-prem move ([#955](https://github.com/tgoodyear/usnewsmap.com/issues/955), [ADR-0027](../../docs/design/adr/0027-onprem-serving-behind-cloudflare.md) decision 10). It passes every request to the tunnel. When the tunnel cannot answer, it asks `ca-usnm-prod` at its default `azurecontainerapps.io` name once, so a home-side outage costs visitors a slower answer and not an error page. This is [#1098](https://github.com/tgoodyear/usnewsmap.com/issues/1098)'s Phase 3 work.

Deployment and drills are separate live Cloudflare operations. Production routing and the Phase 4 transition remain gated; an isolated drill does not authorize either.

The Worker has no dependencies and no `package.json`. Its code is `worker.mjs`, its production configuration `wrangler.jsonc`, its hand-deploy script `deploy.sh`.

## Legacy behavior (default)

| The tunnel's result                                                                         | The Worker                               |
| ------------------------------------------------------------------------------------------- | ---------------------------------------- |
| Any answer but 502 and 530: 2xx, 3xx, 4xx, 500, 503 (the API's problem+json), 504, 522, 524 | Returns it as it is. Azure is not asked. |
| 502 (the connector could not reach Caddy or the API)                                        | Asks Azure once.                         |
| 530 (error 1033 in the body: the tunnel has no connector)                                   | Asks Azure once.                         |
| The fetch throws (the connection failed)                                                    | Asks Azure once.                         |
| The fetch throws an `AbortError` or `TimeoutError`, or the visitor has gone                 | Throws it on. Azure is not asked.        |

It never retries a timeout (504, 524, a `TimeoutError`), because the origin may have done the work, and a search it did twice would be in the search log twice. The API's own 503s are answers, not outages: a searcher restart or a failed readiness check reaches the visitor as the API says it. The Phase 2 outage drills record what a Worker receives in each failure; `RETRY_STATUSES` in `worker.mjs` is the one set to change with their results.

What Azure gets:

- The same method, path, query and body, at the name in `AZURE_FALLBACK_ORIGIN`. The Worker sends a request there only if the name is `https://<something>.azurecontainerapps.io`, with no port, path or credentials. Anything else, including an empty variable, turns the fallback off, so a typing error cannot send visitors' requests to a host outside `azurecontainerapps.io`.
- Every header as it arrived, `Origin` and `Sec-Fetch-Site` among them. The search log records a search only when one of the two says it came from the site itself (`admit()` in `crates/usnm-api/src/searchlog.rs`), so a search Azure answers reaches the log only if they arrive unchanged. The `Host` header is the one change: the runtime sets it to Azure's name.
- The client IP headers (`CF-Connecting-IP`, `X-Forwarded-For`) as received, and no header of the Worker's own. Azure's ingress sees a Cloudflare address as the peer, so during a fallback all visitors share one rate-limit bucket and their page views geolocate wrongly (ADR-0027, Consequences). A header the Worker set for the API to trust would be spoofable through the default name, which stays publicly reachable. The Phase 2 drill measures what reaches Azure.
- Redirects are not followed. A redirect from Azure goes to the visitor's browser.

What comes back: Azure's answer, whatever its status, so there is no third request. If Azure cannot be reached at all, the visitor gets the tunnel's own answer when the tunnel gave one, and the thrown error when it did not.

A request with a body is the page-view beacon (`POST /v1/beacon`, at most 2 KiB), the only POST the API has. The Worker clones the request before the first fetch, which uses the body up, and sends the copy to Azure. `worker.test.mjs` checks that Azure gets the whole body after a 502, a 530 and a thrown fetch, and that a beacon the tunnel took is sent once. A 502 or 530 means the tunnel could not reach the API, so in the usual case no work was done. A failure after the API had started would still send the request twice, and could count a page view or a search twice. ADR-0027 retries these cases and leaves out timeouts.

## Opt-in health-driven production failover

`--health-failover` sets `HEALTH_FAILOVER:true`. Legacy defaults and the cached-shell error layer are unchanged; `--error-layer` and `--health-failover` are mutually exclusive. Both require the installed API's exact `--allowed-origins CSV` policy.

Every incoming request, including assets and mutations, first needs a recent ready answer from **the configured `HEALTH_HOSTNAME`'s** `/readyz`. A Cloudflare location remembers a ready answer for 10 seconds in its own cache, because a probe per request would send every edge cache hit to the origin and back; an unready answer isn't remembered, and a 5xx, network failure or timeout on delivery forgets it. When nothing is remembered, the Worker probes it through an origin fetch (Cloudflare routes bypass the current Worker on this origin subrequest). Production deploys default this to `usnewsmap.com`, so apex readiness controls apex/www/api requests even when www readiness would be redirected to the apex by an edge Single Redirect. The original request still uses its incoming hostname when delivered onprem. With the variable omitted (direct runtime use), preflight uses the incoming hostname; an explicitly invalid variable returns `health-config-invalid` 503 without sending any requests. The probe contains no visitor query, headers or body: only `DNT: 1` and `Cache-Control: no-cache, no-store`. It sets no `cf` cache options, because `cacheTtl: 0` stores the answer, so an origin 503 came back as the last 200 with `cf-cache-status: STALE`. It uses manual redirects, and requires status 200 with the exact five-byte body `ready` and EOF. The 3-second deadline covers both headers and body. Thus API/searcher readiness failure, tunnel failure, malformed readiness, network loss and stalled health all select Azure **before** the original request is sent, whenever a probe runs. During the 10 seconds a location remembers a ready answer, a request can reach a primary that has just failed: GET and HEAD still fail over, but a mutation gets a 503, because it can't be replayed.

Primary probes additionally **reject `X-USNM-Origin: azure`**, even with 200/exact `ready`. Live scheduled probes demonstrated that a public gateway response can report a warm Azure fallback while the primary connector remains stopped; that response must not masquerade as onprem readiness. This guard applies to request preflight, custom health-domain primary status and heartbeat classification. Unlabelled direct-origin readiness and explicit onprem-labelled readiness remain supported; Azure readiness probes may accept Azure-labelled responses. No DNS/route exception or mutation replay is introduced.

Health mode gates assets too: even an asset already eligible for the subrequest edge cache must pass origin selection before its payload is fetched. With primary unready, it can wait for cold Azure readiness or receive 503 without the cached payload being requested. Existing immutable cache headers/manual Cache API remain unchanged, but cached assets are **not independently available through this gateway during an origin outage**; handler caching remains disabled.

If primary is ready, the original request goes onprem. GET/HEAD may additionally fail over after any origin 5xx, connection failure or 30-second response-header deadline. **This can duplicate search-log entries if primary did the work before failing.** Readiness does not eliminate this race. After delivery starts, POST and all other non-GET/HEAD methods are never replayed, including ambiguous timeouts, network failures, 502 and 530. Their received responses pass through; an unanswered delivery produces explicit 503. Client aborts are propagated without retries. Methods, bodies, `Origin`, `Sec-Fetch-Site` and streamed response bodies are preserved; all received 4xx and redirects pass through without failover.

Azure is only probed when selected, at its validated default ACA hostname's `/readyz`. This anonymous DNT probe is intended to wake the API from scale zero and let API readiness wake its nested searcher. The original user request is held until Azure is ready; it is **not** a wake-up request. Polling uses 5-second probes, 3-second gaps, a 180-second readiness deadline and at most 44 probes. Fast negative responses exhaust that count in approximately 132 seconds; stalled probes exhaust the time limit instead. Including primary probe/delivery and Azure delivery, and the three cache operations that count against the quota, at most 50 subrequests are used (the limit). Apart from the 10-second memory of a ready primary, there is no shared isolate circuit or health cache. Azure delivery has one 30-second response-header deadline and no retry. Once headers arrive the response streams without a body deadline or buffering. Worst-case total time to headers is approximately 243 seconds (3 + 30 + 180 + 30); clients/proxies may give up sooner. Poll timers spend wall time, not busy-loop CPU.

If neither preflight is ready, Azure configuration is invalid, or a selected delivery cannot answer, the Worker returns generic `application/problem+json` 503, `Cache-Control: no-store`, `Retry-After: 10`, and exact allow-list CORS/Vary: Origin (no wildcard/credentials or visitor data in the body). HEAD has no body. This mode never serves success-shaped fallback content or the cached shell.

Every health-mode response includes `X-USNM-Origin: onprem|azure|unavailable` and a fixed `X-USNM-Failover-Reason`: `primary-ready`, `primary-unhealthy`, `primary-5xx`, `primary-network`, `primary-timeout`, `health-config-invalid`, `azure-config-invalid`, `azure-unready`, `azure-network`, or `azure-timeout`. Analytics Engine records one fixed origin/outcome, scope, count and response status per completed request, including onprem/unavailable. It never records visitor data or probe URLs. Aborted requests and heartbeat probes are not counted. Existing missing/write-failed metric indicators still apply.

### Opt-in minute heartbeat and deployment

Add `--health-heartbeat` to select **`wrangler.health.jsonc`**, whose only schedule is `"triggers": {"crons": ["* * * * *"]}` and whose mode/heartbeat variables are enabled. Without this flag, deployment uses the unchanged `wrangler.jsonc`, with no cron. Redeploying legacy mode uses that unscheduled configuration again.

The scheduled handler registers bounded readiness polling with `waitUntil`. It probes the fixed onprem hostname independently of visitors; it never probes/wakes Azure when primary is healthy. On unhealthy/stalled primary it polls Azure with the same bounded wake-up policy. Overlapping minute invocations are possible during long cold starts, so load-test this before production. Cron propagation is a Cloudflare live-deployment check, not established by local tests.

`--health-hostname` requires health mode, **not** heartbeat, and accepts exactly `usnewsmap.com`, `dev.usnewsmap.com`, `drill.usnewsmap.com`, or `failover-drill.usnewsmap.com`. It controls both request-time and scheduled probes. Production defaults to the apex. A drill on one of these allowed drill hosts defaults to that exact drill host; any other drill hostname must explicitly choose an allowed health target (never silently the production apex).

Exact production opt-in (placeholders are operator-supplied, not secrets committed here):

```bash
edge/fallback-worker/deploy.sh --account-id <32-lowercase-hex-id> \
  --azure-origin https://<default-ACA-name>.azurecontainerapps.io \
  --wrangler-version 4.154.0 --production \
  --health-failover --health-heartbeat --health-hostname usnewsmap.com \
  --allowed-origins https://usnewsmap.com --dry-run
```

Copy the real API allow-list instead of the example if different. For a live approved deployment remove `--dry-run` and add `--vault <vault> --token-secret <secret>`. For an isolated drill replace `--production` with `--drill-hostname failover-drill.usnewsmap.com` and `--health-hostname usnewsmap.com` with `--health-hostname failover-drill.usnewsmap.com` (drill requests also get DNT). No local test deploys or authorizes production routing.

Local pinned workerd tests prove **actual outbound TCP cancellation**, not only a JavaScript timeout race: stalled readiness headers and stalled readiness body sockets both close at the 3-second deadline. This does not prove Cloudflare route bypass, client-disconnect propagation, ACA scaling/searcher wake, maximum cold-start latency or production CPU/capacity; those remain live drill acceptance. Fail-open routes also bypass the Worker on platform exhaustion, so automatic fallback cannot protect that case.

### Optional Worker-owned health custom domain

Production health deployments can add **`--healthcheck-hostname healthcheck.usnewsmap.com`**. This exact, explicit flag generates the complete private deployment config with the three existing public routes **plus** `{ "pattern": "healthcheck.usnewsmap.com", "custom_domain": true }` and `HEALTHCHECK_HOSTNAME: healthcheck.usnewsmap.com`. The custom domain belongs to this Worker, not Caddy or the tunnel; do not point it at origin/tunnel DNS. It requires `--production --health-failover`; drill deployments cannot acquire the production health domain. Without the option all existing endpoint behavior/config remains unchanged. Generation preserves Analytics Engine unless `--native-metrics` explicitly removes it, and preserves the minute cron when requested.

`healthcheck.usnewsmap.com` is also reserved in `--drill-hostname` validation, even without `--healthcheck-hostname`, so an isolated drill route cannot intercept the production custom domain.

On that hostname only, GET/HEAD `/readyz` returns **200 exact `ready`** when either primary or standby is ready, otherwise **503 `unavailable`**. GET `/` returns compact JSON `{ "available": true|false, "origin": "onprem"|"azure"|"unavailable", "primary_ready": true|false }` with matching 200/503 status. HEAD has no body; both endpoints use no-store, origin/reason/metric headers, and unavailable responses include `Retry-After: 10`. Other paths return 404; non-GET/HEAD methods on these two paths return 405 without delivering bodies anywhere. Query parameters are ignored, never forwarded.

Endpoint requests probe only the allow-listed **`HEALTH_HOSTNAME`** (production apex by default), never the custom domain itself. Only anonymous DNT readiness probes leave the Worker; visitor body/query/IP/headers are not forwarded. Healthy primary means **no Azure probe or wake**. Otherwise the existing bounded ACA readiness polling checks/wakes standby; the endpoint can therefore take up to ~183 seconds and measures end-to-end failover availability rather than primary alone. `primary_ready` distinguishes that state. Invalid/self-referential health targets fail unavailable without outgoing fetches. This endpoint does not alter public-request mutation no-replay or cron behavior and uses at most 46 probes, below the 50 subrequest limit.

`wrangler.health.jsonc` and all private generated configs explicitly set **`"cache": {"enabled": false}`**. [Workers Cache](https://developers.cloudflare.com/workers/cache/configuration/) can check cached responses **before invoking the Worker** when enabled; disable it here so gateway responses never bypass request-time health decisions. This handler-level setting is supported by pinned Wrangler 4.154.0 (minimum 4.69) and is distinct from subrequest caching and the manual Cache API. Keep **Default placement** (no smart/host/region placement), compatibility date **2025-10-01**, and no compatibility flags. Health endpoints and synthesized outage/error-layer responses explicitly use `Cache-Control: no-store`; readiness subrequests additionally send no-cache/no-store request headers and set no `cf` cache options. Original origin answers retain their existing cache headers (including immutable static assets); no cache-everything override is applied to API queries/status/mutations. Native Cache API storage remains restricted to the legacy opt-in error layer's anonymous successful query-free home shell, never searches or mutation responses.

Exact production options, in addition to account/Azure/version/vault/token-secret:

```bash
--production --health-failover --health-hostname usnewsmap.com \
--healthcheck-hostname healthcheck.usnewsmap.com --health-heartbeat \
--allowed-origins https://usnewsmap.com --native-metrics
```

Use the installed API policy and `--dry-run` first. Bind deployment to the correct account using existing `--account-id` (sets `CLOUDFLARE_ACCOUNT_ID`); the token needs Workers Scripts/Routes write plus the custom-domain provisioning permissions required by Cloudflare for that account/zone. The domain must be in the account's active usnewsmap.com zone. This patch does not grant permissions, provision a live domain or validate certificates.

CI should run `node --test edge/fallback-worker/*.test.mjs`, `shellcheck edge/fallback-worker/deploy.sh`, the pinned workerd integration command below, and a **Wrangler dry-run** against generated config (absolute main, all four routes, no unintended bindings, cron retained). These tests verify anonymous apex-only routing/no recursion, endpoint rejection, standby readiness and config cleanup. Actual live acceptance must then verify custom-domain binding/certificate, 200/root JSON/apex health, no Azure wake while primary healthy, outage/cold Azure timing, no-store and fixed headers, CPU/outcome headroom and explicit privacy settings evidence. Null observability readback remains the qualified evidence described below, not an invented disabled-object assertion. Parent-owned CI/live provisioning is separate from these edge tests.

### Canonical production CI and reported live acceptance

The production entrypoint is **`scripts/ci/deploy-edge-worker.sh`**, invoked by **`.github/workflows/edge-worker-deploy.yml`** after successful **`ci`** on trusted main, with a current-head guard. That wrapper owns fixed production options and acceptance; use it rather than duplicating deployment logic in another workflow. Azure OIDC accesses the network-reachable vault **`usnmtlskvprodffapzy`**: Workers token secret **`usnm-prod-workers-ci-token`**, account-ID secret **`usnm-prod-cloudflare-account-id`**. The edge deploy script itself accepts the resolved account ID via `--account-id` and token source via `--vault usnmtlskvprodffapzy --token-secret usnm-prod-workers-ci-token`; it reads the token with the authenticated `az` process and passes it only in Wrangler's environment. The narrow secret access grant is defined by **`infra/modules/edge-worker-ci.bicep`**; this path does not require widening the home Key Vault firewall or a direct GitHub token secret.

Operator-reported acceptance: public resolver **1.1.1.1** resolves the custom domain, and CT243 validated TLS/200 primary root JSON plus 404 for an unknown path. A Mac-local resolver returned NOERROR with an empty answer and curl error 6; that local resolver symptom is not evidence of public-domain failure. Proper GET script-settings now reports observability/logs/invocation logging enabled, query redaction true, traces disabled, no Tail Consumers and Logpush false. This matches the latest user-approved invocation metadata policy, rather than the earlier manual disable/null-readback state; it does not claim zero URLs/path metadata captured. Both Azure apps were reported at zero replicas before the true cold public drill; **cold failover success/timing remains pending until that drill completes**. These live observations are operator reports, not deployments or external validation performed by the edge implementation agent.

Subsequent operator-reported **true production cold drill** started with both Azure apps at zero: **269 synthetic requests, zero failures**, first Azure response **68.331 seconds** after connector stop, maximum request **68.236 seconds**, and automatic return to onprem **1.021 seconds** after recovery. These synthetic clients allowed **215 seconds**; this is **not zero loss for all visitors**. Parent review clarified that initial `getJson()` uses ordinary fetch with its caller signal, **not a 12-second initial GET timeout**; `REQUEST_WAIT_MS` reserves one request at the end of the 150-second compute budget after busy/202 handling. Thus the observed cold wait can complete initial GET/HTML requests. Limited basemap preloads with 10-second abort signals, client-specific deadlines and visitor cancellation remain caveats. Browser behavior is outside this edge change; independent completed heartbeat logs and CPU headroom still require live observation, with no invented CPU measurement or all-client availability guarantee.

## Privacy

The Worker code emits no visitor-data logs. Committed/default configs turn Workers Logs off (`observability.enabled` is `false`); an explicitly approved `--worker-logs` deployment enables platform invocation logging with query redaction and fixed outcome records as described below. The Worker has no `workers.dev` name or preview URL. `config.test.mjs` checks default settings. Cloudflare still sees every request, which ADR-0027 settled decision 1 accepts.

### Explicit approved invocation logging

The latest operator-approved logging policy is available as **`--worker-logs`**, independently of metrics/custom-domain/heartbeat flags. This generates:

```json
{
  "observability": {
    "enabled": true,
    "logs": { "enabled": true, "invocation_logs": true },
    "traces": { "enabled": false },
    "redact_query_string": true
  }
}
```

Unrelated/drill deployments without this flag retain disabled defaults. Do not apply the manual disable PATCH below after an intentionally logging-enabled deploy; verify the **selected** policy through script-settings/dashboard instead. Invocation logging is not custom origin accounting and does not change `native-aggregate` limitations.

`--worker-logs` also sets **`LOG_OUTCOMES: "true"`** in the generated config. Only this explicit flag enables the single structured console record `{ origin, reason, status }` for each completed health-mode selection (public requests and supported health endpoints); default/absent/false emits none. Values are the fixed origin/reason enumerations and response status, never visitor URL/query/path/headers/body/IP. Completed scheduled health checks additionally emit `{ origin, reason, status, trigger: "heartbeat" }` only with that flag: onprem/primary-ready/200, azure/primary-unhealthy/200, or unavailable/azure-unready/503. This establishes the completed heartbeat result separately from cron invocation metadata; intermediate probes and aborted/unfinished invocations emit no outcome. Filter heartbeat records separately from fetch outcome records—not invocation records—to count observed origins in Workers Logs. Native analytics still have no origin dimension; logs are subject to retention/sampling/write limits and cannot count lost/aborted invocations as completed outcomes. Health endpoint traffic is included without a visitor-identifying scope field, so fetch counts are not exclusively public-origin deliveries.

The [API documents query redaction](https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/settings/methods/edit/) as removing query strings from request URLs in logs/traces. The [Workers Logs docs](https://developers.cloudflare.com/workers/observability/logs/workers-logs/) describe invocation metadata and method/URL messages: **hostname/path/method/status and timing metadata remain**, so this is not “no URLs logged.” Words embedded in URL paths rather than query strings are not protected by query redaction. **Live synthetic inspection confirmed platform invocation logs include client-IP headers and geolocation. Query redaction does not anonymize those fields.** Requested invocation logging deliberately remains enabled; no arbitrary IP/header redaction control or IP-free platform logging guarantee is claimed. Application fixed outcome records contain only fixed origin/reason/status (and the heartbeat trigger), with no bodies, headers, IPs or search terms. No request/body/header/IP console logging is added.

Documented retention is **3 days Free / 7 days Paid**, with included log-write allowances of **200,000/day Free / 20 million/month Paid** and Paid overage **$0.60/million**, under current pricing; Cloudflare announces an observability pricing change from December 1, 2026. Default sampling is 100% unless configured otherwise. Confirm subscription/pricing and capture live redaction/field evidence rather than assuming logs are cost-free or fully anonymized.

The drill Worker sends `DNT: 1` on both original requests (`ADD_DNT` is `true`), so a drill's searches are left out of the search log whichever origin answers (`admit()` excludes `DNT: 1`; ADR-0012). Production original requests keep the visitor's DNT unchanged, and `config.test.mjs` fails if the committed variables turn the drill flag on. Health-mode readiness probes always send DNT independently.

The `FALLBACK_METRICS` Analytics Engine binding records only fixed outcome (`azure` or `degraded` in legacy/error-layer modes; `onprem`, `azure`, or `unavailable` in health mode), fixed deployment scope (`production` or `drill`), a count of one and response status. No URL, path, search words, visitor header, address or request body enters the dataset. A direct request to Azure cannot write this Worker binding and is not counted. Failed Azure fetches are not counted as fallback answers (health mode counts the resulting explicit unavailable response). Missing/failed metric writes are surfaced as `X-USNM-Fallback-Metric: unavailable|write-failed`; monitoring/drill acceptance must reject incomplete accounting rather than treating it as zero downtime.

The dataset is `usnm_fallback`; Cloudflare creates it on first write. There is at most one datapoint per routed request, bounded by Workers Free's own 100,000/day ceiling, matching [Analytics Engine's Free allowance](https://developers.cloudflare.com/analytics/analytics-engine/pricing/). Read through an account-scoped **Account Analytics Read** token, not a visitor endpoint or the deployment credential. For example, count production fallback answers with `SELECT SUM(_sample_interval * double1) AS fallback_answers FROM usnm_fallback WHERE blob1 = 'azure' AND blob2 = 'production' AND timestamp >= NOW() - INTERVAL '1' DAY`. Account for sampling as shown; this is a fallback count, not a replacement for the existing availability-test SLI.

### Explicit native metrics alternative

If Cloudflare rejects deployment with error 10089 (Analytics Engine not enabled), operators may explicitly choose **`--native-metrics`** instead of enabling an unapproved account feature. Defaults still include the Analytics Engine binding. This flag generates a private, exclusively created 0600 JSON configuration inside the owned edge directory, preserving the entire selected source configuration (including heartbeat cron, routes and privacy), resolving `main` to its absolute path, explicitly setting `analytics_engine_datasets: []`, and setting `METRICS_MODE: native`. The deploy command also supplies `--var METRICS_MODE:native`. No account/token secrets enter this file; EXIT/INT/TERM traps remove it after dry runs, successful deployments and failures. SIGKILL/process loss can leave the ignored `.native-metrics-*.json` file for manual removal. The shared JSONC parser preserves URL strings and fails on malformed input.

In this intentional alternative, completed counted responses visibly carry **`X-USNM-Fallback-Metric: native-aggregate`**, never `unavailable` or `write-failed` merely because the binding is absent. Health-mode `X-USNM-Origin` and fixed failover reasons remain intact, including explicit unavailable 503 responses. Native mode never writes Analytics Engine or custom visitor logs; platform Workers Logs remain off unless independently opted in with `--worker-logs`.

**Native Workers analytics provide invocation/error/status/CPU aggregates, not origin-specific aggregate fallback counts.** This marker does not claim fallback accounting is complete. Measure synthetic request loss and fallback timing, and count onprem/Azure/unavailable outcomes from synthetic responses' fixed headers; do not invent per-origin native metrics or treat missing outcome counts as zero failures. Response headers measure only observed completed requests. Changing metrics mode does not change failover, cron overlap, CPU/request quotas or the ≤48 subrequest budget.

For the isolated drill, use the existing account/Azure/version/vault arguments plus:

```bash
--drill-hostname failover-drill.usnewsmap.com --health-failover \
--health-hostname failover-drill.usnewsmap.com --health-heartbeat \
--allowed-origins https://usnewsmap.com --native-metrics --worker-logs
```

Use `--dry-run` first and copy the API's actual allow-list. Production uses `--production --health-hostname usnewsmap.com` instead. Omitting `--native-metrics` on a later deploy restores the selected committed config's Analytics Engine binding and removes the native mode variable. Cron is still independently opt-in.

## Explicit Phase 4 error layer

`ERROR_LAYER` is committed as `false`; normal deploys retain the existing Phase 3 fallback behavior. Only an explicit `deploy.sh --error-layer` enables the error layer. It must not be used in production before ADR-0027's Phase 4 gate and approval; it may be tested on the isolated drill hostname.

That flag also requires `--allowed-origins CSV`, copied exactly from the API's `USNM_ALLOWED_ORIGINS` setting (read only that named environment entry from the app). Generated outage problems grant CORS only to exact members of this list and vary on Origin, without credentials or a wildcard. This preserves readable 503/problem responses for allowed cross-origin browser clients instead of a CORS/network failure. The committed safe default mirrors the API's single-origin default, but an error-layer deploy cannot silently use it instead of the installed API policy.

In that mode, 502/530/non-abort network failures never ask Azure. API paths (both `/v1` and `/api/v1`), POSTs and cold assets receive a generic problem+json 503 with `Retry-After: 30` and `Cache-Control: no-store`; the response includes no request data. The API's own 503s, timeouts and aborts retain their original semantics. An errored tunnel body cannot prevent the degraded response: cancellation failure is best-effort and exposed as `X-USNM-Error-Layer-Cleanup: cancel-failed`.

Only a successful, anonymous, query-free `GET /` HTML response without private/no-store caching or Set-Cookie is saved in the host-qualified private Cache API slot, for at most 24 hours. Search pages/results and authenticated/cookie-bearing requests are never saved. On an HTML navigation outage, that generic home shell is returned as 503 with a fixed status banner, no-store, Retry-After and no stale validators. HEAD has no body. A cold/evicted shell yields the explicit problem response, not invented content. `X-USNM-Error-Layer: cache-write-failed|cache-read-failed` exposes storage failures; `cached-shell` identifies the degraded response. Warm and verify the actual shell before the outage drill; Cache API storage is evictable, not a durable availability guarantee.

## Configuration

`wrangler.jsonc` is the production configuration:

- The routes are `usnewsmap.com/*`, `www.usnewsmap.com/*` and `api.usnewsmap.com/*`. A route covers every path, so `/assets/*` is on the Worker's route: an asset the edge does not hold also falls back to Azure (ADR-0027 settled decision 10). `boinc.usnewsmap.com` is not routed. The Phase 2 test hostname replaces the routes at deploy time (`deploy.sh --drill-hostname`).
- `compatibility_date` is pinned, so a runtime change cannot alter the Worker unnoticed.
- `AZURE_FALLBACK_ORIGIN` is empty in the file. The name is the Container Apps environment's own domain, which the repository does not hold, so `deploy.sh --azure-origin` sets it. It is not a secret. With it empty, the Worker passes everything to the tunnel and falls back to nothing.

The route's failure mode is a setting on the route in Cloudflare, not part of `wrangler.jsonc`. The route has to fail open. When Workers Free's daily limit is spent, a route that fails open skips the Worker and handles the request as if none were configured, where a route that fails closed answers with Cloudflare's error 1027 page ([Workers limits](https://developers.cloudflare.com/workers/platform/limits/)). Set it on each of the three routes after deploying, and check it again after any change to them.

### Deployment acceptance: settings evidence and distinct failure modes

**Route-quota fail-open is not configured or verified by this script.** The documented route endpoints, `GET /zones/{zone_id}/workers/routes`, `GET /zones/{zone_id}/workers/routes/{route_id}` and `PUT /zones/{zone_id}/workers/routes/{route_id}`, expose route ID/pattern/script only; the update body supports only pattern/script. Neither the [public API schema](https://developers.cloudflare.com/api/resources/workers/subresources/routes/methods/update/) nor the [official TypeScript SDK](https://github.com/cloudflare/cloudflare-typescript/blob/main/src/resources/workers/routes.ts) provides a supported failure-mode setter/readback. Do not submit or claim verification of undocumented `request_limit_fail_open`. The [limits documentation](https://developers.cloudflare.com/workers/platform/limits/#daily-requests) says to toggle the route's fail mode; use the dashboard route controls (Workers & Pages → Worker → Settings → Domains & Routes), retaining explicit per-route evidence, or obtain Cloudflare support confirmation if the control is unavailable. Pattern/script readback alone is not evidence. Account `default_usage_model: standard` is not proof of an active Workers Paid subscription; verify the account's Workers subscription separately.

This quota behavior **bypasses** the Worker and uses the DNS-selected onprem origin; it cannot select Azure or attach failover headers. It is distinct from normal health-driven failover, where the Worker runs and chooses a ready origin. It also does not establish behavior on CPU/memory exhaustion.

**Unexpected exceptions do not currently enable runtime pass-through.** The code deliberately does not call `ctx.passThroughOnException()` (the source test enforces this). [Cloudflare documents](https://developers.cloudflare.com/workers/runtime-apis/context/#passthroughonexception) that this method handles uncaught code exceptions, not CPU/memory exhaustion, and cannot safely replay a consumed request body. Adding it indiscriminately would conflict with mutation no-replay and client-abort handling. Expected health/network/deadline failures are caught and handled explicitly; an unexpected uncaught exception may instead produce a platform error. No blanket exception fail-open guarantee is made.

**Workers Logs have a supported privacy setter and readback.** Both committed configs explicitly request `observability.enabled: false`, including the generated native-metrics config. The [edit script-settings API](https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/settings/methods/edit/) supports `PATCH /accounts/{account_id}/workers/scripts/{script_name}/script-settings` with JSON:

```json
{
  "observability": {
    "enabled": false,
    "logs": { "enabled": false, "invocation_logs": false },
    "traces": { "enabled": false },
    "redact_query_string": true
  }
}
```

Apply only through an authorized operator, then read back both production and drill with the [GET script-settings API](https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/settings/methods/get/): `GET /accounts/{account_id}/workers/scripts/{script_name}/script-settings`. This is distinct from legacy `/settings`; an omitted observability field there does not establish disabled logging. Verify explicit false values in the proper script-settings response (and query redaction true as defense in depth), or resolve missing/ambiguous values through dashboard/support. The API makes observability optional and documents no absent-field-means-disabled contract. `logpush: false` and `tail_consumers: []` verify those separate delivery channels only, not Workers Logs. [Workers Logs documentation](https://developers.cloudflare.com/workers/observability/logs/workers-logs/) says newly created Workers have observability enabled by default and invocation logs include request URLs. No console calls or an empty recent log view are not sufficient proof. This manual privacy operation does not configure route-quota fail-open and is not performed by `deploy.sh`.

**Null readback is a supported representation, not an enabled object.** The [official SDK's `ScriptSetting`](https://github.com/cloudflare/cloudflare-typescript/blob/main/src/resources/workers/scripts/scripts.ts) explicitly declares `observability?: ScriptSetting.Observability | null`; its [settings methods](https://github.com/cloudflare/cloudflare-typescript/blob/main/src/resources/workers/scripts/settings.ts) return this type. A successful explicit disable PATCH followed by `observability: null` is therefore valid API output, not a failed write merely because an assertion expected `.enabled === false`. Retain the successful PATCH payload/response and subsequent GET showing the explicit null, separately from a missing property; do not dereference null or silently coerce missing/null to a false object. Neither the docs nor these SDK comments defines **null means disabled** or proves the nested query-redaction setting persisted. Record this as “explicit disable accepted; GET returned nullable observability representation,” and obtain dashboard/support confirmation if acceptance requires definitive runtime-disabled or redaction readback. No general null/absence-based privacy guarantee is inferred.

**Cold readiness waiting does not raise CPU budgets.** No `limits.cpu_ms` override is set. [Current limits](https://developers.cloudflare.com/workers/platform/limits/#cpu-time) specify Free HTTP and cron CPU at **10 ms each**; Paid HTTP defaults to **30 seconds CPU**, and minute cron also has **30 seconds CPU**. Waiting on network is not CPU time, but each probe/response/header operation still consumes CPU; the ≤50 subrequests and 180-second readiness bound do not prove the Free CPU budget is met. Native CPU/outcome metrics and real cold-start drills must demonstrate headroom. Cron duration is separately capped at **15 minutes**; the bounded ~183-second heartbeat fits that wall limit, although minute invocations can overlap. HTTP requests have no fixed wall limit while the client stays connected; disconnects and resource errors can still interrupt them. Do not enable paid limits or change subscription based solely on `standard` usage-model metadata.

## Workers Free budget

Every request on a Worker route counts toward Workers Free's 100,000 requests a day, cached or not, and `/assets/*` is on the route. The Phase 0 baselines ([#955, 22 October 2026](https://github.com/tgoodyear/usnewsmap.com/issues/955#issuecomment-6355390978), scripts in #1006) counted the requests Azure answered on the 23 full UTC days from 30 September to 22 October:

| Requests a day | Count | Share of 100,000 |
| -------------- | ----- | ---------------- |
| Median day     | 3,193 | 3.2%             |
| Busiest day    | 5,628 | 5.6%             |

The busiest day could grow about 18 times before it reached the limit. Most of the requests were 404s that look like scanners; the site had about 23 page views a day. Workers Free also allows 10 ms of CPU a request, and this Worker only passes requests through. #1101 measures 30 days of traffic against Workers Free's limit, and settled decision 10 says to decide again if the figure does not fit.

## Deploying by hand

The deploy needs its own Cloudflare token (ADR-0027 decision 7): Workers scope, limited to the operator's IP address, with an expiry, kept in Key Vault under [ADR-0024](../../docs/design/adr/0024-cloudflare-tunnel-and-r2-credentials.md)'s rules and read only by `deploy.sh`. A leaked one could put a Worker in front of every page. This change does not create it. When it is created, check which permissions Wrangler needs for a Worker with routes (Workers Scripts edit on the account and Workers Routes edit on the `usnewsmap.com` zone, plus whatever Wrangler asks for) and grant no more.

1. Run the tests, with Node 24: `node --test edge/fallback-worker/*.test.mjs`. (`node --test edge/fallback-worker/` fails on Node 24, which reads the directory as a module.)
2. Read the app's default name: `az containerapp show --name ca-usnm-prod --resource-group rg-usnm-prod --query properties.configuration.ingress.fqdn -o tsv`.
3. Choose an exact Wrangler release, read its changelog, and look at the command first:

   ```bash
   edge/fallback-worker/deploy.sh --account-id <id> --azure-origin https://<default name> \
     --wrangler-version <X.Y.Z> --production --dry-run
   ```

   `--dry-run` prints the command and stops before it reads the token or calls Cloudflare.

4. Run it again without `--dry-run`, adding `--vault <vault> --token-secret <secret>`, from the address the token is limited to. The script reads the token with your `az` sign-in into the Wrangler process only: never printed, never on a command line, never written to disk. With `--production` it deploys `usnm-fallback` on the three routes. With `--drill-hostname <name>` it deploys the drill Worker `usnm-fallback-drill` on that one hostname, which must be a name under `usnewsmap.com` other than `www`, `api` and `boinc`.
5. In Cloudflare, set each route to fail open and check that Workers Logs are off for the Worker.

To undo the Phase 3 cutover, remove the Worker routes and set the three records back to DNS-only at their recorded targets (ADR-0027, Phases). Deleting the Worker is optional. `deploy.sh` does not remove routes.

## Drills

Phase 2 (#1101) deploys the drill Worker on the private test hostname, then stops `cloudflared`, the API and the container's network in turn, and records what the Worker receives each time. A page is loaded with a cold cache, to see that an asset the edge does not hold falls back with its HTML. The same drills show whether `Sec-Fetch-Site`, `Origin` and the client IP headers reach Azure unchanged. The drills have not been run. The Worker keeps no log, so how a drill reads the tunnel's result is part of planning them.

## Remaining live acceptance

- Deploy only the reviewed release to the isolated hostname with logs/previews off and fail-open readback. Retain the original DNS, route and Worker configuration for rollback.
- Demonstrate actual connector/API/network failures, cold assets, intact POST/header forwarding, no timeout/abort/API-503 retry, no duplicate search log, and fixed-only metric readback excluding direct Azure requests.
- On the drill hostname, explicitly enable the error layer, warm its anonymous root shell, then verify the real Cache API/HTMLRewriter banner, API 503/Retry-After, cold-cache response and recovery. No production mode change follows automatically.

## Tests

`node --test edge/fallback-worker/*.test.mjs` runs four files offline, with no network and no Cloudflare:

- `worker.test.mjs` runs the Worker against a stand-in for `fetch`: which results fall back and which do not, the beacon body, the headers, the drill flag, the Azure name check, the path with two slashes, no loop, and no logging.
- `config.test.mjs` reads `wrangler.jsonc`: logs off, no `workers.dev`, the three routes, the empty Azure name and no drill flag.
- `deploy.test.mjs` runs `deploy.sh` with `az` and `npx` replaced by stubs: argument checks, the dry run, and that the token reaches only Wrangler's environment.
- `health.test.mjs` checks readiness validation/isolation, stalled probes, 5xx/network/timeouts, mutation no-replay, cold Azure polling, aborts, budgets, unavailable CORS and cron opt-in/wake behavior with a fake clock.

`WRANGLER_SEND_METRICS=false npx --yes --package=wrangler@4.154.0 --call 'node edge/fallback-worker/native-integration.mjs'` additionally runs the actual workerd Cache API and HTMLRewriter. It warms a generic home shell, checks the GET outage banner, retained content, 503/no-store/Retry-After, removed validators and API CORS. The health tests additionally use real workerd TCP fetches to an owned loopback HTTP server, proving deadline socket cancellation for headers/body and streamed healthy responses with isolated probe headers. No public origins are contacted; owned runtimes/server are disposed even on failure. Fetching the pinned development tool requires npm access, but this test needs no account, token or Cloudflare deployment.

CI runs both suites in the `edge (fallback worker tests)` job when something under `edge/` changes, and shellchecks `deploy.sh`.
