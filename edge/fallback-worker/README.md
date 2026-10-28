# Fallback Worker

A Cloudflare Worker that sits in front of the `usnm-serving` tunnel during Phase 3 of the on-prem move ([#955](https://github.com/tgoodyear/usnewsmap.com/issues/955), [ADR-0027](../../docs/design/adr/0027-onprem-serving-behind-cloudflare.md) decision 10). It passes every request to the tunnel. When the tunnel cannot answer, it asks `ca-usnm-prod` at its default `azurecontainerapps.io` name once, so a home-side outage costs visitors a slower answer and not an error page. This is [#1098](https://github.com/tgoodyear/usnewsmap.com/issues/1098)'s Phase 3 work.

Deployment and drills are separate live Cloudflare operations. Production routing and the Phase 4 transition remain gated; an isolated drill does not authorize either.

The Worker has no dependencies and no `package.json`. Its code is `worker.mjs`, its production configuration `wrangler.jsonc`, its hand-deploy script `deploy.sh`.

## What it does

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

## Privacy

The Worker logs nothing. URLs carry the search words in `q`, so the code has no `console` call (`worker.test.mjs` fails on one), `wrangler.jsonc` turns Workers Logs off (`observability.enabled` is `false`), and the Worker has no `workers.dev` name or preview URL. `config.test.mjs` checks the settings. Cloudflare still sees every request, which ADR-0027 settled decision 1 accepts.

The drill Worker sends `DNT: 1` on both requests (`ADD_DNT` is `true`), so a drill's searches are left out of the search log whichever origin answers (`admit()` excludes `DNT: 1`; ADR-0012). The production Worker never sets it, and `config.test.mjs` fails if the committed variables turn it on.

The `FALLBACK_METRICS` Analytics Engine binding records only fixed outcome (`azure` or `degraded`), fixed deployment scope (`production` or `drill`), a count of one and response status. No URL, path, search words, visitor header, address or request body enters the dataset. A direct request to Azure cannot write this Worker binding and is not counted. Failed Azure fetches are not counted as fallback answers. Missing/failed metric writes are surfaced as `X-USNM-Fallback-Metric: unavailable|write-failed`; monitoring/drill acceptance must reject incomplete accounting rather than treating it as zero downtime.

The dataset is `usnm_fallback`; Cloudflare creates it on first write. There is at most one datapoint per routed request, bounded by Workers Free's own 100,000/day ceiling, matching [Analytics Engine's Free allowance](https://developers.cloudflare.com/analytics/analytics-engine/pricing/). Read through an account-scoped **Account Analytics Read** token, not a visitor endpoint or the deployment credential. For example, count production fallback answers with `SELECT SUM(_sample_interval * double1) AS fallback_answers FROM usnm_fallback WHERE blob1 = 'azure' AND blob2 = 'production' AND timestamp >= NOW() - INTERVAL '1' DAY`. Account for sampling as shown; this is a fallback count, not a replacement for the existing availability-test SLI.

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

`node --test edge/fallback-worker/*.test.mjs` runs three files offline, with no network and no Cloudflare:

- `worker.test.mjs` runs the Worker against a stand-in for `fetch`: which results fall back and which do not, the beacon body, the headers, the drill flag, the Azure name check, the path with two slashes, no loop, and no logging.
- `config.test.mjs` reads `wrangler.jsonc`: logs off, no `workers.dev`, the three routes, the empty Azure name and no drill flag.
- `deploy.test.mjs` runs `deploy.sh` with `az` and `npx` replaced by stubs: argument checks, the dry run, and that the token reaches only Wrangler's environment.

CI runs them in the `edge (fallback worker tests)` job when something under `edge/` changes, and shellchecks `deploy.sh`.
