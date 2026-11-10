# BOINC server on-prem

The BOINC server, its database, the bridge steps and the hand-off steps run on an on-prem container host, CT 230, behind a Cloudflare Tunnel. The server and bridge steps moved there from Azure Container Apps on 21 October 2026 (#942, part of #917; [the cutover](../../notes/2026-10-21-boinc-server-onprem-cutover.md)), and the hand-off steps followed when the Azure hand-off job was retired on 6 November 2026 (#308). The scripts are in `boinc/scripts/onprem/`, and the step runner is `boinc/scripts/onprem-step.sh` ([the bridge steps](bridge-steps.md)). The Azure code that remains is a fallback ([the Azure server](azure.md)).

## What runs on the host

| Unit                               | What                                                                                                                                                                | Notes                                                                                                                     |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| `usnm-boinc-mysql.service`         | MySQL 8.4, from `mysql-setup.sh`                                                                                                                                    | Host network, `bind_address=127.0.0.1`, `mysqlx=OFF`, `require_secure_transport=ON`                                       |
| `usnm-boinc-server.service`        | The bridge image running `entrypoint.sh --serve`: Apache and the BOINC daemons                                                                                      | `--entrypoint` set, because the bridge image's own entrypoint is `bridge-job`                                             |
| `usnm-boinc-step`                  | `onprem-step.sh`: one bridge step at a time, in place of the Azure bridge job                                                                                       | The controller reaches it with `step_command` over ssh, using a forced command and `--from-ssh`                           |
| `usnm-boinc-step --target publish` | Publishes an app version from a local appver image, in place of the Azure publish job                                                                               | Root on the host only; `boinc/scripts/boinc-app-release.sh --onprem-host --onprem-ct` builds the image and runs it (#961) |
| `usnm-boinc-step --target handoff` | The hand-off job's `claim`, `release`, `collect` and `export`, in place of the Azure hand-off job; `--target scans --step receive` takes the controller's scan push | The controller's `--onprem-handoff`; settings in `/etc/usnm-boinc/handoff.env` (#967)                                     |
| `cloudflared`                      | A Cloudflare Tunnel with ingress `boinc.usnewsmap.com` → `http://localhost:80`                                                                                      | Public ingress with no inbound port                                                                                       |
| `usnm-image-security-check.timer`  | A daily check of the server image against `bookworm-security`, and of the MySQL pin                                                                                 | A failed `usnm-image-security-check.service` is the alert ([Security updates](#security-updates), #969)                   |
| `usnm-feeder-slot-guard.timer`     | A read-only check for a stale or unknown `--allapps` allocation every five minutes                                                                                  | A failed `usnm-feeder-slot-guard.service` is the local alert ([Feeder allocation guard](#feeder-allocation-guard-1462))   |
| `caddy`                            | TLS for `boinc.usnewsmap.com` on the LAN, on 443, in front of `http://localhost:80`                                                                                 | [LAN access](#lan-access-1357)                                                                                            |
| `usnm-boinc-lan-tls.timer`         | Daily pull of the LAN certificate from the TLS vault                                                                                                                | Fails closed: a bad certificate leaves the installed one                                                                  |
| `usnm-boinc-lan-guard.service`     | nftables: port 80 answers loopback only                                                                                                                             | Apache stays reachable to cloudflared, not the LAN                                                                        |

All containers use the host network. Podman 4.3's CNI backend has no DNS for container names, so
`usnm-boinc-mysql` is mapped to 127.0.0.1 in `/etc/hosts` and with `--add-host`.

**Identity.** The host is an Azure Arc machine in the on-prem group. `lib.sh`'s `kv_token` and `usnm-store`'s credential both follow the Arc challenge flow when `IDENTITY_ENDPOINT` and
`IMDS_ENDPOINT` are set with no `IDENTITY_HEADER` (#943). The containers mount `/var/opt/azcmagent/tokens`
read-only for the challenge file.

**Database TLS.** `mysql-setup.sh` makes a private CA under `/srv/usnm-boinc/mysql-tls`. The
server certificate's SANs are `usnm-boinc-mysql`, `localhost` and `127.0.0.1`. The containers mount `ca.pem` over
`/usr/local/share/usnm/mysql-ca.pem`, so the TLS patch (`boinc/scripts/patch-server-db-tls.py`)
verifies the server unchanged. The CI fixtures for that patch are in [database TLS checks](db-tls-acceptance.md).

**Credentials.** The admin and runtime passwords come from the BOINC Key Vault and are kept in `/etc/usnm-boinc/secrets` (0600). The root password is local. A provision still gives the vault's `mysql-admin-password` a new value; the host uses its own copy in `/etc/usnm-boinc/secrets/db-admin`.

**The controller's `step_command`.** The controller reaches `usnm-boinc-step` over ssh. End `step_command` with `--` after the host (`["ssh", "-i", "<key>", "-o", "BatchMode=yes", "root@<host>", "--"]`); without it, ssh reads the step's `--env` as one of its own options.

### Work that ran as Azure jobs

| Work                                                         | On-prem                                                                                                                                                                                                |
| ------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Bridge steps (the controller's submit, spool-out, inventory) | `usnm-boinc-step`                                                                                                                                                                                      |
| Hand-off steps                                               | `usnm-boinc-step --target handoff` ([below](#hand-off-steps-on-prem))                                                                                                                                  |
| Gold bundle                                                  | `usnm-boinc-step --env prod --step gold-bundle --var GOLD_CAMPAIGN=<campaign> --wait-mins 360`, run as root on the host                                                                                |
| App version publish                                          | `boinc/scripts/boinc-app-release.sh --onprem-host <Proxmox host> --onprem-ct <id>`: builds the `appver` image in the container and runs `usnm-boinc-step --target publish` (#961)                      |
| Quality wave                                                 | the on-prem feeder on pve-laptop (#962, [OCR quality](../apps/quality.md#on-prem-feeder-v1-wave10-on)): staged inputs reach the server through `usnm-boinc-step`; the server reads no scan data itself |

### What stays in Azure

With `"server": "onprem"`, `provision-boinc.sh` deploys only what the on-prem server still uses: the two identities (the main environment's Bicep reads `usnm-boinc-id-<env>`), the Key Vault the host reads, the VNet and the static site. The stack deletes the rest: the server app, its jobs and environment, MySQL, both project storage accounts, Log Analytics, the workbook and the private endpoints. The provision then checks that the on-prem server answers at the master URL.

The work queues (`ocr-priority`, `ocr-bulk`, `ocr-deadletter`), `ocr-out`, the `boinc-handoff` container, Cosmos and the lake are still in Azure, reached with the host's Arc identity. Moving the hand-off state onto the host is [#1367](https://github.com/tgoodyear/usnewsmap.com/issues/1367) ([local hand-off state](local-state.md)).

## Feeder allocation guard (#1462)

After a reviewed image containing the guard has been deployed and its instrumented
feeder has started at a coordinated boundary, install the host check from that
reviewed checkout:

```sh
install -m 0644 boinc/scripts/onprem/usnm-feeder-slot-guard.service boinc/scripts/onprem/usnm-feeder-slot-guard.timer /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now usnm-feeder-slot-guard.timer
systemctl start usnm-feeder-slot-guard.service
```

The oneshot runs the image's read-only check as `boincadm` in the running server
container. `drift` exits 1 and `unknown` exits 2, so both fail the unit and appear
in `systemctl --failed` and `journalctl -u usnm-feeder-slot-guard.service`.
`maintenance` is distinct from a match and does not fail the unit. The unit sends
no external notification; monitor failed units and missing timer runs separately,
or enable the feed monitor's opt-in
[`feeder_allocation` check](monitoring.md#live-feed-health-1441),
which runs the same command and alerts through the BOINC feed alerts.
An unreachable container, old feeder, stale snapshot or failed verified-TLS query
is unknown, not success. So is `feeder_restart_hold`: the server's supervisor loop
stopped restarting a feeder that kept stopping
([The loop's restart bound](feeder-refresh.md#the-loops-restart-bound)). The image's independent five-minute JSON event is useful
for diagnosis. See [Allocation drift detection](../apps/app-release.md#allocation-drift-detection)
for the scope and limitations, and [Feeder-only refresh](feeder-refresh.md) for the
recovery procedure.
Neither check restarts a process or edits work.

## Security updates

The images start from pinned digests, so their Debian packages don't change after a build, and
the host's own automatic updates never reach them. Each image that serves Apache and PHP takes
Debian's security updates when it is built: the server and bridge Containerfiles, the overlays
and `security-overlay.Containerfile` run `apt-get upgrade` from the image's sources, which
include `bookworm-security`. The build then fails if `php8.2-common` or `apache2` is older than
`PHP_FLOOR` or `APACHE_FLOOR`, now 8.2.35-1~deb12u1 from DLA-4848-1 (21 October 2026) and
2.4.67-1~deb12u3 from DSA-6323-1 (6 June 2026). Each floor is the version of bookworm's last
advisory for that package, so a newer build from bookworm's own suite passes it too.
`test_security_updates.py` in `boinc/images/server` checks that every one of them
upgrades and enforces both floors.

After a Debian security advisory for a package in the image (PHP, Apache, OpenSSL, curl):

1. For PHP or Apache, raise `PHP_FLOOR` or `APACHE_FLOOR` in all of those Containerfiles to the fixed version, and `--php-floor` or `--apache-floor` in `usnm-image-security-check.service`, through a pull request. `test_image_security_check.py` fails when the unit and the Containerfiles disagree.
2. On the on-prem BOINC host, build `security-overlay.Containerfile` on the running image: `RUNTIME_IMAGE` is `SERVER_IMAGE` from `/etc/usnm-boinc/server.env`, and `SECURITY_REFRESH` is today's date, so a cached build runs the upgrade again. The file's header has the command. Only packages change; the project's code stays as it was.
3. Set `SERVER_IMAGE` in `server.env` to the new image and restart `usnm-boinc-server.service`. The step runner reads the same file, so the next step uses it too.
4. Check the running image: `podman exec usnm-boinc-server php -v`, and `podman exec usnm-boinc-server dpkg-query -W php8.2-common apache2 openssl libcurl3-gnutls` against the advisory's fixed versions. `systemctl start usnm-image-security-check.service` runs the scheduled check now and exits clean when nothing is pending. It reads `SERVER_IMAGE`, not the container, so it only vouches for what runs after step 3's restart.

A full rebuild from `main` (base, then server, then bridge, as `boinc/scripts/build-boinc-images.sh`
orders them) also takes the updates, since the server and bridge stages upgrade.

### Scheduled check

`usnm-image-security-check.timer` runs `image-security-check.sh` on the host once a day (about 06:23
host time, with up to 15 minutes of random delay; `Persistent=true` makes up for a missed run). It
starts a throwaway container from `SERVER_IMAGE` (`podman run --rm --pull=never`, host network, so
`apt-get update` works) and runs `apt-get update` and a simulated `apt-get upgrade` in it, then
compares `php8.2-common` and `apache2` with the two floors, which the unit passes as `--php-floor`
and `--apache-floor`. Nothing in the image changes and nothing restarts. It fails the unit (exit 1) for:

- An upgrade the image's own apt sources offer. The journal line says `SECURITY` when it comes from the security archive and `UPDATE` otherwise: a security fix can also arrive in a point release, so both fail. Building `security-overlay.Containerfile` on the image clears them.
- `KEPT-BACK`: a package that `apt-get upgrade` leaves alone because upgrading it needs a new package or a removal. apt prints no upgrade line for it, so the check reads the "not upgraded" count. The overlay and the Containerfiles run the same `apt-get upgrade`, so building the overlay does not clear it; install the package by name in their upgrade step, through a pull request.
- `BELOW-FLOOR`: `php8.2-common` or `apache2` older than its floor.
- `MYSQL-DRIFT`, below.

It exits 3, which also fails the unit, when it can't answer. Examples are a failed `apt-get update`, no security archive in the image's apt
sources, no digest in the MySQL unit, and a failed `podman pull` or `podman image inspect` of the MySQL image. A check that couldn't run never
reads as clean. The alert is the failed unit, in `systemctl --failed` and in
`journalctl -u usnm-image-security-check.service`; nothing routes it anywhere else yet. The check
sees what bookworm publishes: a CVE the Debian security tracker still lists as open for bookworm has
no package to take, in the build or here.

Install, as root on the host, from a checkout of `main`. The floors are in the unit, so run the
same lines again after a floor changes:

```sh
install -m 0755 boinc/scripts/onprem/image-security-check.sh /usr/local/bin/usnm-image-security-check
install -m 0644 boinc/scripts/onprem/usnm-image-security-check.service boinc/scripts/onprem/usnm-image-security-check.timer /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now usnm-image-security-check.timer
```

Uninstall:

```sh
systemctl disable --now usnm-image-security-check.timer
rm /etc/systemd/system/usnm-image-security-check.service /etc/systemd/system/usnm-image-security-check.timer /usr/local/bin/usnm-image-security-check
systemctl daemon-reload
```

Run by hand with `usnm-image-security-check --php-floor <version> --apache-floor <version>`; add
`--image <image>` to check another image (for example the overlay before step 3), `--mysql-image <name:tag>`
to check another tag of the MySQL image (default `docker.io/library/mysql:8.4`), and `--skip-mysql`
to leave the MySQL part out. `--mysql-image` must be a name with a tag; a registry port is fine.
The check exits 2 for a name with no tag or with an `@digest`,
because it asks what the tag resolves to now and neither can show a moved tag.

### The MySQL image

The official MySQL image is Oracle Linux based, so `bookworm-security` doesn't apply to it. Its only
signal is a new digest for the `8.4` tag. `mysql-setup.sh` resolves `docker.io/library/mysql:8.4` to a
digest once and writes it into `usnm-boinc-mysql.service`, so the database stays on that image until
someone moves it. The check pulls the tag (`podman pull -q`, then `podman image inspect` for its
`.Digest`, as `mysql-setup.sh` does) and reports `MYSQL-DRIFT` when the digest differs from the
unit's. It only reports, and a new digest is not always a security fix, so read the 8.4 release
notes first.

To roll the database onto the new digest, when no bridge step is running (the restart takes
`usnm-boinc-server.service` down with it, because the server requires the database):

1. Run `mysql-setup.sh` again on the host. It is safe to re-run: it keeps the CA, certificates, data and secrets, pulls the tag, and writes the new digest into the unit.
2. `systemctl restart usnm-boinc-mysql.service`. MySQL gets up to 110 seconds to stop cleanly (`--stop-timeout 110`).
3. `systemctl start usnm-image-security-check.service` no longer reports drift.

## Hand-off steps on-prem

`claim`, `release`, `collect` and `export` run on the host (#967 phase 3); prod's Azure hand-off job was retired on 6 November 2026 (#308). `dead-letter` runs here too, and as root on the host it also takes an operator's
`SUPERSEDES_FAILED_JOB` and `CONFIRM_MESSAGE_SHA256`
([batch controller](batch-scheduler.md#a-retried-page-claimed-after-its-dead-letter)).
`deadline-retry` (#1417, the same section) runs as root on the host only, never over ssh;
the controller's export takes its `HANDOFF_DEADLINE_RETRIES`. `usnm-boinc-step --target handoff --step <step>` runs the bridge
image's `handoff-job.sh` from `SERVER_IMAGE`, the image the other
bridge steps use. It runs as the host's own Arc machine identity, which ADR-0008's Cosmos
amendment gives the hand-off job's grants. The container mounts only the Arc agent's challenge
directory, with no project tree, database or Key Vault. It takes `HANDOFF_STEP` and only that
step's inputs, over ssh or as root.

`/etc/usnm-boinc/handoff.env` holds the hand-off settings, the former Azure job's without its Container Apps
`AZURE_CLIENT_ID`, plus the Arc endpoints, and nothing else (`usnm-boinc-step` refuses any
other name):

```sh
HANDOFF_URL=https://<exchange account>.blob.core.windows.net/boinc-handoff
COSMOS_ENDPOINT=https://<cosmos account>.documents.azure.com:443/
CURATED_URL=https://<lake account>.blob.core.windows.net/curated
REFERENCE_URL=https://<lake account>.blob.core.windows.net/reference
OUT_URL=https://<exchange account>.blob.core.windows.net/ocr-out
QUEUE_URL=https://<exchange account>.queue.core.windows.net
IDENTITY_ENDPOINT=http://localhost:40342/metadata/identity/oauth2/token
IMDS_ENDPOINT=http://localhost:40342
```

The controller sends the steps here with `--onprem-handoff claim --onprem-handoff collect`
([batch controller](batch-scheduler.md#on-prem-claim-collect-and-export-967)), and
`export` too with `--onprem-handoff export`: the controller then pushes the scans straight to this host (`--target scans --step receive`, into `/srv/usnm-boinc/project/handoff-scans`), export mounts them read-only, and `submit-remote` takes each page from them, so no scan goes through Azure (#1116).
`texts-export` runs on the host with the local copy of curated (`boinc/scripts/texts-export-onprem.sh`, [OCR quality](../apps/quality.md)).

The hand-off steps take `/run/usnm-boinc-handoff.lock`, not the step lock: they touch only Azure
storage, Cosmos and the queues, and the controller runs one at a time. `spool-out` and
`batch-inventory` take it too: they only read the project and the database. A collect of 100 jobs
doesn't keep publishing or submits waiting, and a publish doesn't fail a claim or a spool-out.

Every `usnm-boinc` process of a hand-off step needs access tokens from the Arc agent, and a
collect runs several processes for each job. Four collects at once drew HTTP 429 from the agent
(#1447). So `handoff-job.sh` gives an execution's processes one token cache (`USNM_TOKEN_CACHE`, a
private directory it removes when it ends): one process fetches a token and the others reuse it
until five minutes before it expires. With that image, a batched `collect` takes
`COLLECT_PARALLEL=<1 to 8>` (the controller's `--collect-parallel`) and collects that many jobs
at once. The default is still one at a time.

To ship a hand-off fix without a new server image, build the changed `usnm-boinc` or
`handoff-job.sh` over the running image, built on bookworm from the running image's source plus the
fix, and put the new image's `sha256:<id>` in `/etc/usnm-boinc/handoff-image`. Only hand-off steps
read that file. `/etc/usnm-boinc/stage-image` does the same for `quality-stage` alone, which
writes only its campaign's tree (#529). Delete either once `SERVER_IMAGE` carries the fix:

```dockerfile
FROM <SERVER_IMAGE>
COPY usnm-boinc /usr/local/bin/usnm-boinc
COPY handoff-job.sh /usr/local/lib/usnm-boinc/handoff-job.sh
```

## Cloudflare

`boinc.usnewsmap.com` is a proxied CNAME to the tunnel. Volunteer clients and the controller are scripts, so the zone's bot checks are set up not to block them. That configuration is kept outside this repository. [ADR-0024](../../design/adr/0024-cloudflare-tunnel-and-r2-credentials.md) records the ADR-0009 exception for the Cloudflare credentials.

## LAN access (#1357)

**The network-wide DNS records were rolled back on 30 October 2026; [project-run clients](#project-run-clients) use a hosts entry instead.** Everything else on the LAN goes through Cloudflare. With the gateway's local records in place, Firefox failed with `SSL_ERROR_INTERNAL_ERROR_ALERT`: the gateway still forwarded the public HTTPS record advertising Cloudflare's Encrypted Client Hello, and Caddy can't answer its outer name, `cloudflare-ech.com`. Don't restore the records until browsers are handled and tested as well as BOINC clients. Caddy, the certificate, its timer and the guard remain.

On CT 230, Debian's Caddy serves 443 with a public Let's Encrypt certificate for `boinc.usnewsmap.com` and
reverse-proxies to `localhost:80`, Apache's only allowed path, which cloudflared also uses. `origin-tls.yml` issues the certificate (`origin_tls.py --target
boinc-lan`, with its own ACME state) into the deploy-only secret `usnm-prod-boinc-lan-tls`. The
host's Arc identity can read only that secret, never the DNS token or the ACME account.
`usnm-boinc-lan-tls.timer` pulls it daily, checks name, expiry, key, chain and that it's not older
than the installed one, and restarts Caddy when it changes. Caddy's access log,
`/var/log/caddy/boinc-lan.log`, shows the LAN clients; Apache sees `::1` for both paths. The certificate in use on 30 October 2026 expires 27 January 2027.

```sh
python3 scripts/azure/origin_tls_bootstrap.py --boinc-lan   # operator, once: the secrets and grants
gh workflow run origin-tls.yml -f validate-dns=true         # then check the run
tar -C boinc/scripts/onprem/lan-tls -cf - . | ssh root@<Proxmox host> 'cat > /root/lan-tls.tar'
ssh root@<Proxmox host> 'pct push 230 /root/lan-tls.tar /root/lan-tls.tar && pct exec 230 -- sh -c "mkdir -p /root/lan-tls && tar -C /root/lan-tls -xf /root/lan-tls.tar && /root/lan-tls/lan-tls-setup.sh"'
```

`usnm-boinc-lan-guard.service` makes port 80 answer loopback only; to remove it, `systemctl disable --now usnm-boinc-lan-guard`.

### Project-run clients

Clients the project runs on the LAN reach CT230 through one entry in their own
`/etc/hosts`, so the network's DNS and every browser stay as they are. This covers hosts 2, 9, 10
and 12. The master URL, scheduler, download and upload URLs don't change. Certificate validation
stays on. The client checks Caddy's public Let's Encrypt certificate against the roots it already
trusts, with nothing disabled and no new root added. The client resolves the name for each new
connection, so running tasks and the client itself need no restart. A connection already open to
Cloudflare stays in use until it's been idle for about two minutes, so the Caddy log can lag the
change by a few minutes.

In the client's container, as root, using CT230's current LAN IPv4 address:

```sh
cp -p /etc/hosts "/etc/hosts.pre-boinc-lan-$(date -u +%Y%m%dT%H%M%SZ)"
printf '%s boinc.usnewsmap.com\n' "$CT230_LAN_IPV4" >> /etc/hosts
getent ahosts boinc.usnewsmap.com    # only that address
curl -sS -o /dev/null -w '%{http_code} %{ssl_verify_result}\n' https://boinc.usnewsmap.com/server_status.php?xml=1
```

Append in place, keeping the file's inode, because a client in its own mount namespace may hold
the same file. Keep the entry outside the `# --- BEGIN PVE ---` block. Proxmox rewrites only that block,
so the entry survives a restart. Check the result in `/var/log/caddy/boinc-lan.log`, where
the client's LAN address appears, and in the validator log.

**There's no automatic fallback to Cloudflare.** If Caddy or its certificate fails, these clients
retry with BOINC's usual backoff and keep their finished work. The tunnel runs on the same container,
so losing the container stops both paths anyway. If CT230's address changes, update each entry.

**Rollback:** delete the entry, or restore the backup, on that client. Its next connection goes
through Cloudflare. No restart is needed.

Accepted on 1 November 2026 (all four hosts, no client restarted): real scheduler RPCs on
each host and real uploads on hosts 2, 9 and 10 returned 200 in the Caddy log from each client's LAN address, and
the validators accepted the results those uploads carried. Host 12 runs no tasks, so it proved the
scheduler path only. In Cloudflare's analytics the LAN's public address went from 367 POSTs and
62 MB of uploads in the hour before, to 1 POST and 0.1 MB in a 30-minute window after. Caddy logged
about the same upload rate over that period.

## Qwen model files on R2

The four Qwen model files are too large for Cloudflare's cache (512 MB on the plans the zone can use) and are the largest repeat downloads, so they're also hosted in the public R2 bucket `usnm-boinc-models`, at `https://models.usnewsmap.com`. Workunits don't change. The Single Redirect `usnm_boinc_models_r2` sends those exact `/usnmocr_download/<dir>/<name>` paths to the object of the same name with a `302`. The BOINC client follows it. The rule runs at Cloudflare, so a client that reaches the server directly downloads from the server. [Project-run clients](#project-run-clients) do that, so all four files must stay on the server while they use the LAN path. Other clients follow the redirect.

The rule lists exact paths on purpose, so a file that isn't in the bucket keeps coming from the server. The server's copies stay in place as the source of truth.

When `weights.lock` adds a model file:

1. Upload it to the bucket under its physical name, with `Cache-Control: public, max-age=31536000, immutable`, and check that its `ETag` equals the `.md5` file's checksum. The object must fit one `PUT` (under 5 GiB). A multipart upload's `ETag` isn't the file's MD5, so hash a full download of it and compare that with the `.md5` file before adding the path.
2. Check `https://models.usnewsmap.com/<name>` returns the same `Content-Length`.
3. Add the path to the rule. Remove paths of revisions no workunit uses.

To undo it, delete the rule. Downloads go back to the server at once.
