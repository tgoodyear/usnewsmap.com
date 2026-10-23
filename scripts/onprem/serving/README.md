# On-prem serving

These files run usnewsmap.com from one unprivileged container on an on-prem Proxmox host, behind a Cloudflare Tunnel ([#955](https://github.com/tgoodyear/usnewsmap.com/issues/955); the plan is in [its architecture comment](https://github.com/tgoodyear/usnewsmap.com/issues/955#issuecomment-6331553520)). They follow the BOINC server's pattern ([`scripts/boinc/onprem/`](../../boinc/onprem/), [docs/boinc-onprem.md](../../../docs/boinc-onprem.md)).

This is Phase 0. The files are written and tested, but nothing runs them yet. Phase 2 builds the container and runs them on a private test hostname. Azure serves the public until Phase 3.

The container itself, its VLAN and its firewall are in home-infra's `scripts/lab/serving-ct.sh`. Host names and addresses stay there, out of this repository.

## What runs in the container

Everything listens on loopback only. The only way in is the tunnel, which cloudflared opens outbound.

| Unit                                              | What                                                                                                                                                         | Listens on                            |
| ------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------- |
| `cloudflared-usnm-serving.service`                | The connector for the remotely managed tunnel `usnm-serving`. Its token comes from Key Vault into a root-only file (ADR-0024).                               | 127.0.0.1:20241 (metrics)             |
| `caddy.service`                                   | Debian's Caddy with this directory's `Caddyfile`. It forwards every request to the API color that serves.                                                    | 127.0.0.1:8080, and :2019 (admin API) |
| `usnm-api@blue.service`, `usnm-api@green.service` | The API and site, the same `usnewsmap-api` image Azure runs, pulled by digest under podman. One color serves; the other is stopped, or starting a new image. | 127.0.0.1:8081, :8082                 |
| `usnm-searcher.service`                           | The pinned Quickwit binary (`scripts/onprem/install-quickwit.sh`), searcher and metastore roles, reading the index from a read-only `file://` metastore.     | 127.0.0.1:7280, and :7281 (gRPC)      |
| `usnm-serving-update.timer`                       | Runs `update-api.sh` 2 minutes after the last run ends, so the API follows CI's prod deployment.                                                             |                                       |

The API reaches Azure (the lake's `reference/`, Cosmos and App Insights) with the container's own Arc identity. The search log and OCR requests stay off until Phase 3, because the API records same-origin searches on any hostname, the test hostname included.

## Files

| File                                                  | Runs where                                                                      | What it does                                                                                                                                                                                                                                                                                     |
| ----------------------------------------------------- | ------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `gen-env.sh`                                          | The operator's machine, from a checkout, signed in to `az`                      | Writes `api.env`, `searcher.env` and `searcher.yaml` from the sources Bicep uses: `infra/env/<env>.json`, the stack's outputs, `infra/quickwit/searcher.yaml` and `containerapp.bicep`. `--check-app` compares `api.env` with the Azure app and fails on any difference the script doesn't list. |
| `install.sh`                                          | The container, as root, from a checkout                                         | Installs the units, configs and update script, the journald and tmpfiles rules and the Quickwit binary, then enables and starts Caddy, the searcher and the timer. Safe to run again. It restarts the searcher only when its config, binary or unit changed.                                     |
| `update-api.sh`                                       | The container, from the timer                                                   | Blue/green deploys (below).                                                                                                                                                                                                                                                                      |
| `apply-tunnel-ingress.sh`                             | The operator's machine, from the address the Cloudflare API token is limited to | Sends `tunnel-ingress.json` to Cloudflare, with the token read from Key Vault. `--hostname` routes a test hostname only.                                                                                                                                                                         |
| `Caddyfile`, `*.service`, `*.timer`                   | Installed by `install.sh`                                                       |                                                                                                                                                                                                                                                                                                  |
| `tunnel-ingress.json`                                 | Applied by `apply-tunnel-ingress.sh`                                            | The tunnel's ingress, kept in the repository (ADR-0005 decision 3).                                                                                                                                                                                                                              |
| `journald-usnm-serving.conf`, `usnm-serving.tmpfiles` | Installed by `install.sh`                                                       | The journal is kept 30 days. Response cache entries are deleted 14 days after they're written.                                                                                                                                                                                                   |
| `test_serving.py`                                     | CI and locally                                                                  | Runs every script against stub tools. The deploy and install tests need Linux.                                                                                                                                                                                                                   |

Every script takes named flags and has a `--dry-run`.

## Setup (Phase 2)

1. home-infra's `serving-ct.sh` creates the container, its VLAN tag, its firewall and its packages. Then the container joins Arc under its own identity, outside the on-prem group, and Bicep grants that identity its roles with `onPremServing` on.
2. The host mounts the index read-only at the path the release wrote into its metastore (the release's run directory plus `/qw`), and the NVMe pool's cache dataset at `/srv/usnm-serving/cache`.
3. On the operator's machine:

   ```bash
   scripts/onprem/serving/gen-env.sh --env prod --index-dir <run directory>/qw --out <new directory> --check-app
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

6. The timer's first run deploys the newest prod deployment to blue.

## Deploys

CI's publish job records a GitHub deployment for each prod run. Two minutes after its last run ends, `update-api.sh` reads the newest one whose latest status is `success`, through the public API with no token. Its requests are conditional, so a run that finds nothing new doesn't count against GitHub's 60 requests an hour per address.

When that commit isn't the one serving, the script:

1. reads the `usnewsmap-api:<commit>` manifest from the registry with the Arc identity's AcrPull;
2. checks that its build provenance attestation is signed by `ci.yml` on main, for that commit;
3. pulls the image by digest and starts it on the idle color;
4. once the new color's `/readyz` passes, points Caddy at it, reloads Caddy, and checks that Caddy's admin API names the new port and that `/readyz` answers through Caddy;
5. waits 130 s for requests in flight and stops the old color.

If the new color doesn't get ready, or Caddy doesn't switch, the old color keeps serving and the commit is listed in `/var/lib/usnm-serving/rejected`. If the serving color isn't ready either, the host is the likely cause (a missing index, no lake access), so the commit isn't rejected. A commit with no valid attestation is rejected before anything is pulled.

The new commit serves within 10 minutes of CI's `success`: up to 2 minutes until the next run, then the pull and the new color's start and warm-up (its readiness cap is 360 s).

To roll back by hand, deploy an earlier commit:

```bash
usnm-serving-update --registry crusnmprodffapzy --commit <sha>
```

That creates `/etc/usnm-serving/hold`, and the timer leaves the API alone while it exists. Delete the file to follow CI again. To apply a changed `api.env` without downtime, run `usnm-serving-update --registry crusnmprodffapzy --redeploy`.

## Logs

Nothing here logs a client address or a query string (09 §9.4.2):

- cloudflared runs at `--loglevel info`, since debug logs every URI;
- Caddy has no access log;
- Quickwit keeps the `RUST_LOG` filter from `containerapp.bicep`;
- the API logs no query strings.

The journal keeps 30 days.

## Open items for Phase 2

- **How a new index version reaches the running container.** The searcher reads one `file://` metastore, and each release writes its own under its run directory. So a new version needs a new `--index-dir`, a new `searcher.yaml` and a searcher restart, which with one searcher is a short outage with cold caches. The choice is between a mount per version, added ahead of the switch, and one mount whose contents the host swaps.
- **Searcher upgrades.** An upgrade restarts the only searcher, so it is an outage too. `install.sh` restarts it only when its config, binary or unit changed.
- **The client address.** `api.env` sets `USNM_TRUSTED_PROXY_HOPS=0` for now, so the API sees loopback for every visitor, and all visitors share one rate-limit bucket. The API's loopback-only `CF-Connecting-IP` setting (a Phase 0 code PR) goes into `gen-env.sh` once it merges.
- **The API image's attestation** (a Phase 0 code PR). Until CI attests the image, `update-api.sh` rejects every commit.
- **`USNM_READY_REQUIRES_PUBLISHED_INDEXES`.** Whether to turn it on here.
- **Limits and hardening to confirm on the host:**
  - the 2 GiB memory limit for each API color;
  - the API container's read-only root;
  - cloudflared under a dynamic user;
  - the systemd sandboxing in an unprivileged container.
- **Monitoring.** No alert covers failed units, OOM kills, a full disk or NVMe wear in the container yet. The plan lists what to replace.
