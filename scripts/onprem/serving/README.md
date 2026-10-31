# On-prem serving

These files run usnewsmap.com from one unprivileged container on an on-prem Proxmox host, behind a Cloudflare Tunnel ([#955](https://github.com/tgoodyear/usnewsmap.com/issues/955); the plan is in [its architecture comment](https://github.com/tgoodyear/usnewsmap.com/issues/955#issuecomment-6331553520)). They follow the BOINC server's pattern ([`scripts/boinc/onprem/`](../../boinc/onprem/), [docs/boinc-onprem.md](../../../docs/boinc-onprem.md)).

Production cutover and automatic Azure fallback follow
[ADR-0029](../../../docs/design/adr/0029-onprem-primary-azure-failover.md).
Do not infer active routing from the existence of these files: retain the live
DNS/Worker/service and public-request receipts.

The container itself, its VLAN and its firewall are in home-infra's `scripts/lab/serving-ct.sh`. Host names and addresses stay there, out of this repository.

## What runs in the container

Everything listens on loopback only. The only way in is the tunnel, which cloudflared opens outbound.

| Unit                                              | What                                                                                                                                                         | Listens on                            |
| ------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------- |
| `cloudflared-usnm-serving.service`                | The connector for the remotely managed tunnel `usnm-serving`. Its token comes from Key Vault into a root-only file (ADR-0024).                               | 127.0.0.1:20241 (metrics)             |
| `caddy.service`                                   | Debian's Caddy with this directory's `Caddyfile`. It forwards every request to the API color that serves.                                                    | 127.0.0.1:8080, and :2019 (admin API) |
| `usnm-api@blue.service`, `usnm-api@green.service` | The API and site, the same `usnewsmap-api` image Azure runs, pulled by digest under podman. One color serves; the other is stopped, or starting a new image. | 127.0.0.1:8081, :8082                 |
| `usnm-searcher.service`                           | The pinned Quickwit binary (`scripts/onprem/install-quickwit.sh`), searcher and metastore roles, reading the index from a read-only `file://` metastore.     | 127.0.0.1:7280, and :7281 (gRPC)      |
| `usnm-serving-update.timer`                       | Runs `update-api.sh` 2 minutes after the last run ends, so the API follows the image prod runs.                                                              |                                       |

The API reaches Azure (the lake's `reference/`, Cosmos and App Insights) with the container's own Arc identity. Rehearsals leave search logging and OCR requests off, because the API records same-origin searches on any hostname, the test hostname included. The authorized production deployment enables their shared authoritative stores.

At the authorized public cutover, pass `--production` to both `gen-env.sh`
and `install.sh`. This enables the shared Azure search log without
introducing a second state authority. The OCR-request writer follows
Azure's infrastructure predicate: `USNM_OCR_NODES && USNM_USE_ACR` and
either `USNM_INGEST_JOBS` or `USNM_OCR_SYNC_ONPREM` with a nonblank
`USNM_BATCH_HOST`; `USNM_OCR_LOCAL` does not control the writer.
`--check-app --production` requires both writer settings to match Azure,
rather than accepting them as rehearsal-only differences. The default
rehearsal configuration still leaves both unset and the installer refuses
writer settings without that explicit flag.

Additional isolated listeners, such as the dev origin, may be installed under
`/etc/usnm-serving/extra/*.caddy`. The updater verifies only the production
listener on `127.0.0.1:8080`, not an unrelated dev upstream. Preserve the
qualified read-only index mount's UID/GID in a systemd drop-in when it differs
from the default searcher account; do not rewrite the source index permissions.

## Files

| File                                                  | Runs where                                                                      | What it does                                                                                                                                                                                                                                                                                     |
| ----------------------------------------------------- | ------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `gen-env.sh`                                          | The operator's machine, from a checkout, signed in to `az`                      | Writes `api.env`, `searcher.env` and `searcher.yaml` from the sources Bicep uses: `infra/env/<env>.json`, the stack's outputs, `infra/quickwit/searcher.yaml` and `containerapp.bicep`. `--check-app` compares `api.env` with the Azure app and fails on any difference the script doesn't list. |
| `install.sh`                                          | The container, as root, from a checkout                                         | Installs the units, configs and update script, the journald and tmpfiles rules and the Quickwit binary, then enables and starts Caddy, the searcher and the timer. Safe to run again. It restarts the searcher only when its config, binary or unit changed.                                     |
| `update-api.sh`                                       | The container, from the timer                                                   | Blue/green deploys (below).                                                                                                                                                                                                                                                                      |
| `apply-tunnel-ingress.sh`                             | The operator's machine, from the address the Cloudflare API token is limited to | Sends `tunnel-ingress.json` to Cloudflare, with the token read from Key Vault. `--hostname` routes a test hostname only.                                                                                                                                                                         |
| `apply-cache-rules.py`, `cache-rules.json`            | The operator's machine, from the address the Cloudflare API token is limited to | Stages apex static/versioned lookup caching and explicit dynamic/API bypasses. Edits individual rules, preserves existing basemap/OCR rules, and verifies read-back. Does not change DNS or certificates.                                                                                        |
| `Caddyfile`, `*.service`, `*.timer`                   | Installed by `install.sh`                                                       |                                                                                                                                                                                                                                                                                                  |
| `tunnel-ingress.json`                                 | Applied by `apply-tunnel-ingress.sh`                                            | The tunnel's ingress, kept in the repository (ADR-0005 decision 3).                                                                                                                                                                                                                              |
| `flood-rule.json`                                     | Recorded only; the operator makes the rule in the dashboard                     | The zone's one Free rate-limit rule for the four search routes, off until a flood (ADR-0027 settled decision 9), in the shape of the body for the zone's `http_ratelimit` entry point. Until the zone-settings script exists (ADR-0027 decision 12), the dashboard rule must match this file.    |
| `run-cloudflared.sh`                                  | Installed by `install.sh`, run by the cloudflared unit                          | Runs cloudflared and cuts query strings from its log lines.                                                                                                                                                                                                                                      |
| `journald-usnm-serving.conf`, `usnm-serving.tmpfiles` | Installed by `install.sh`                                                       | The journal is kept 30 days. Each color gets its own response cache directory, and entries are deleted 14 days after they're written.                                                                                                                                                            |
| `test_serving.py`                                     | CI and locally                                                                  | Runs every script against stub tools. The deploy and install tests need Linux.                                                                                                                                                                                                                   |

Every script takes named flags and has a `--dry-run`.

## Cloudflare cache policy

Staging Cache Rules does not enable proxying or prove a cache HIT.
[ADR-0028](../../../docs/design/adr/0028-cloudflare-with-azure-origin.md) allows
proxying the existing Azure origin independently of the on-prem migration.
Replace DNS-dependent managed certificates with renewable DNS-01 certificates,
deploy the privacy/proxy changes and verify strict TLS before enabling the
proxy. ADR-0027's hardware, rehearsal and fallback gates still apply to the
later on-prem cutover, not to the Azure-origin proxy.

Preview the policy without credentials or network calls:

```bash
python3 scripts/onprem/serving/apply-cache-rules.py --zone-id <zone id> --dry-run
```

Stage it with a Key Vault token authorized for the zone's Cache Rules:

```bash
python3 scripts/onprem/serving/apply-cache-rules.py --zone-id <zone id> --vault <vault> --token-secret <secret>
```

Keep the printed before/after receipt. The script updates only its three
stable rule refs, preserving existing rules (including `usnm_versioned_basemap`
and the OCR dashboard). It places the dynamic bypass last. A failed operation
is explicit and may leave a partially applied policy; inspect the receipt/zone
and rerun before proceeding with any cutover.

| Request                                                                                                                                       | Policy                                                                                                                             |
| --------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| Apex `/assets/*`, `robots.txt`, `favicon.svg`, `og-image.png`                                                                                 | Eligible only under origin Cache-Control; bypass when absent                                                                       |
| Apex versioned basemap                                                                                                                        | Existing `basemap-cache-rule.json` remains unchanged                                                                               |
| Apex `/v1/places`, `/v1/titles`, `/v1/titles/*`, `/v1/coverage`, `/v1/days` and `/api/v1` equivalents, with a nonempty `v` query argument     | Eligible only under origin Cache-Control; default cache key retains the full query                                                 |
| Apex `/v1/aggregate` and `/api/v1/aggregate` with `v` and `g`                                                                                 | Eligible; the API sends 14 days when both match what it serves and `no-store` otherwise. Deploy that API before applying this rule |
| Everything else on apex, including HTML/search previews, sitemap, other aggregate, hits, live metadata/status, health and unversioned lookups | Bypass                                                                                                                             |
| All requests on `api` and `www`                                                                                                               | Bypass; avoids cross-origin cache mixing, while `www` redirects to apex                                                            |

Origin `no-store` responses, including lookup 202/307/errors and missing assets,
must remain uncacheable. No forced TTL, query-string stripping, deployment purge
or cache-everything rule is introduced. After the approved cutover, fetch each
class twice and retain `CF-Cache-Status`/`Age` evidence: static/versioned successes
should warm to HIT, dynamic/API requests must not HIT, and PMTiles ranges must
retain `Content-Length`/`Content-Range` without `Content-Encoding`.

## Setup (Phase 2)

1. home-infra's `serving-ct.sh` creates the container, its VLAN tag, its firewall and its packages. Then the container joins Arc under its own identity, outside the on-prem group, and Bicep grants that identity its roles with `onPremServing` on.
2. The host mounts the index read-only at the path the release wrote into its metastore (the release's run directory plus `/qw`), and the NVMe pool's cache dataset at `/srv/usnm-serving/cache`. The installer also creates `/srv/usnm-serving/sitemap-state`, writable by the API image's user and bind-mounted into both API colors. `USNM_SITEMAP_STATE_URL` points there independently of visitor search logging, which remains disabled before cutover. Unlike the response cache, these durable sitemap checkpoints never expire; preserve this directory across upgrades and restores.
3. On the operator's machine:

   ```bash
   bash scripts/onprem/serving/gen-env.sh --env prod --index-dir <run directory>/qw --out <new directory> --check-app
   ```

   Then copy that directory into the container (`serving-ct.sh --config`).

4. In the container, from a checkout:

   ```bash
   scripts/onprem/serving/install.sh --registry crusnmprodffapzy --config-dir /root/usnm-serving-config
   ```

5. Create the tunnel `usnm-serving` in Cloudflare, put its token in Key Vault, and install it with `serving-ct.sh --tunnel`. Then, from the operator's machine, route the test hostname:

   ```bash
   scripts/onprem/serving/apply-tunnel-ingress.sh --account-id <id> --tunnel-id <id> --vault <vault> --token-secret <secret> --hostname <test hostname>
   ```

6. The timer's first run deploys the image prod runs to blue.

## Deploys

The edge has a separate automated release:
[`edge-worker-deploy.yml`](../../../.github/workflows/edge-worker-deploy.yml)
waits for successful trusted-main CI, checks the immutable SHA is still
current, validates the Worker and deploys
[`deploy-edge-worker.sh`](../../ci/deploy-edge-worker.sh).
It maintains the public routes, `healthcheck.usnewsmap.com` custom domain,
minute heartbeat and code-backed runtime/logging/cache settings in
[`edge/fallback-worker/`](../../../edge/fallback-worker/README.md).
It never reads the home-IP-only vault from GitHub-hosted runners.

One-time operator setup stores an account/zone-scoped Cloudflare Worker token
and account identifier as `usnm-prod-workers-ci-token` and
`usnm-prod-cloudflare-account-id` in the existing production CI vault.
Do not put token values in shell arguments, repository files or Actions logs.
Apply only the corresponding secret reader grants:

```bash
CI_PRINCIPAL=$(az identity show -g rg-usnm-prod -n usnm-ci-id-prod --query principalId -o tsv)
az deployment group what-if -g rg-usnm-prod \
  --template-file infra/modules/edge-worker-ci.bicep \
  --parameters vaultName=usnmtlskvprodffapzy ciPrincipalId="$CI_PRINCIPAL"
az deployment group create -g rg-usnm-prod --mode Incremental \
  --template-file infra/modules/edge-worker-ci.bicep \
  --parameters vaultName=usnmtlskvprodffapzy ciPrincipalId="$CI_PRINCIPAL"
```

These grants do not permit listing other secrets or modifying vault
networking. The token expires 31 October 2027; rotate it in the secret store
beforehand. Health-domain readiness validation permits DNS/certificate
propagation time. When diagnosing a local resolver failure, verify public
DNS and HTTPS from another resolver/client rather than changing origin DNS
or claiming the Worker failed.

CI's publish job pushes each prod image as `usnewsmap-api:main` and `usnewsmap-api:<commit>`. Its last step, after the Azure app rolls onto the image, attests it (a Phase 0 code PR adds this step), so an attested image is one prod runs.

Two minutes after its last run ends, `update-api.sh` reads the `main` manifest from the registry with the Arc identity's AcrPull. If that manifest is already serving, the run ends there, without asking GitHub anything. For a new manifest, the script:

1. reads its build provenance attestation from GitHub's public API, with no token, checks that `ci.yml` on main signed it, and checks that the commit it names has its own tag on the same manifest;
2. pulls the image by digest and starts it on the idle color;
3. once the new color's `/readyz` passes, points Caddy at it, reloads Caddy, and checks that Caddy's admin API names the new port and that `/readyz` answers through Caddy;
4. waits 130 s for requests in flight and stops the old color.

Until the attestation appears, the script asks for it on every run for 30 minutes after it first sees the manifest, then every 10 minutes, and rejects the manifest after 3 hours. GitHub allows 60 requests an hour from an address without a token, and only a new image costs any.

If the new color doesn't get ready, or Caddy doesn't switch, the old color keeps serving and the manifest's digest is listed in `/var/lib/usnm-serving/rejected`. If the serving color isn't ready either, the host is the likely cause (a missing index, no lake access), so nothing is rejected; nor on the first deploy, with no serving color to compare. If Caddy can't be shown to route back to the old color after a failed switch, both colors stay up and `/etc/usnm-serving/hold` stops the timer until someone checks the route by hand. A manifest whose attestation doesn't verify is rejected before anything is pulled.

Once CI attests a new image, the next run picks it up within about 2 minutes, as long as the attestation came within 30 minutes of `main` moving. Then come the pull and the new color's start, which takes at most 800 s: the API's own budget of 420 s to load the published version and a 360 s readiness cap.

To roll back by hand, deploy an earlier commit:

```bash
usnm-serving-update --registry crusnmprodffapzy --commit <sha>
```

The image must be attested for that commit. This creates `/etc/usnm-serving/hold`, also when the commit serves already, and the timer leaves the API alone while the file exists. Delete it to follow `main` again. To apply a changed `api.env` without downtime, run `usnm-serving-update --registry crusnmprodffapzy --redeploy`.

## Logs

The on-prem service logs do not record a client address or a query string (09 §9.4.2):

- cloudflared runs at `--loglevel info`, since debug logs every URI, and `run-cloudflared.sh` cuts the query string from the URL in its error lines;
- Caddy has no access log, and its error log drops each line's request field (the URI and the headers, `CF-Connecting-IP` among them);
- Quickwit keeps the `RUST_LOG` filter from `containerapp.bicep`;
- the API logs no query strings.

The journal keeps 30 days.

The separately requested Cloudflare Worker invocation logs are different:
platform request metadata includes IP-bearing headers and geolocation even
with query-string redaction. Fixed application outcome records omit those
fields. See [ADR-0029](../../../docs/design/adr/0029-onprem-primary-azure-failover.md)
and the visitor privacy page for this explicit policy amendment, retention and
measured heartbeat/cold-start acceptance.

## Searcher restarts

An upgrade or a new index version restarts the only searcher, so it is a short outage with cold caches. Do it at a scheduled off-peak time (ADR-0027 settled decision 8). Under the production [ADR-0029](../../../docs/design/adr/0029-onprem-primary-azure-failover.md) mode, failed readiness selects Azure and the minute heartbeat can wake its cold standby; the legacy connector-only Worker mode passes application 503s through. `install.sh` restarts the searcher only when its config, environment, binary or unit changed. The steps are in [09 §9.3.1](../../../docs/design/09-operations-security-cost.md#931-on-prem-serving).

## Open items for Phase 2

- **How a new index version reaches the running container.** It is decided after a Phase 2 test (ADR-0027 settled decision 6). The searcher reads one `file://` metastore, and each release writes its own under its run directory. So a new version needs a new `--index-dir`, a new `searcher.yaml` and a searcher restart. The test tries both ways: a mount per version, added ahead of the switch, and one mount whose contents the host swaps. It shows whether Proxmox can add a bind mount to the running container, and how ZFS datasets mounted under one bind mount show inside it.
- **Settings from the Phase 0 API PRs.** `api.env` sets `USNM_TRUST_CF_CONNECTING_IP=true`, so the API takes the visitor's address from `CF-Connecting-IP` when the peer is loopback. It also sets `USNM_READY_REQUIRES_PUBLISHED_INDEXES=true`, so `/readyz` fails while the searcher lacks an index `current.json` names. An image from before those PRs ignores both. It then keys rate limits on the loopback peer (`USNM_TRUSTED_PROXY_HOPS=0`), so all visitors share one bucket.
- **The API image's attestation** (a Phase 0 code PR). Until CI attests the image, `update-api.sh` deploys nothing. Each new image waits 3 hours for its attestation and is then rejected.
- **One response cache for both colors.** The API's local store names its temporary files from the process id (with a thread id or a per-process counter), and both colors run the API as process 1, so each color has its own cache directory. Temporary names unique across processes in `usnm-store` would let them share one again.
- **Limits and hardening to confirm on the host:**
  - the 2 GiB memory limit for each API color;
  - the API container's read-only root;
  - cloudflared under a dynamic user;
  - the systemd sandboxing in an unprivileged container.
- **Monitoring.** No alert covers failed units, OOM kills, a full disk or NVMe wear in the container yet. The plan lists what to replace.
