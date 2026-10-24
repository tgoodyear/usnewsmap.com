# BOINC server on-prem

The BOINC server, its database and the bridge steps move from Azure Container Apps to an on-prem
container host (#942, part of #917). This page covers what runs there and the cutover order.
The scripts are in `scripts/boinc/onprem/`, and the step runner is `scripts/boinc/onprem-step.sh`.

## What runs on the host

| Unit                               | What                                                                                  | Notes                                                                                                                     |
| ---------------------------------- | ------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| `usnm-boinc-mysql.service`         | MySQL 8.4 (the same major and minor as the Azure server), from `mysql-setup.sh`       | Host network, `bind_address=127.0.0.1`, `mysqlx=OFF`, `require_secure_transport=ON`                                       |
| `usnm-boinc-server.service`        | The bridge image running `entrypoint.sh --serve`: Apache and the BOINC daemons        | `--entrypoint` set, because the bridge image's own entrypoint is `bridge-job`                                             |
| `usnm-boinc-step`                  | `onprem-step.sh`: one bridge step at a time, in place of the Azure bridge job         | The controller reaches it with `step_command` over ssh, using a forced command and `--from-ssh`                           |
| `usnm-boinc-step --target publish` | Publishes an app version from a local appver image, in place of the Azure publish job | Root on the host only; `scripts/boinc/boinc-app-release.sh --onprem-host --onprem-ct` builds the image and runs it (#961) |
| `cloudflared`                      | A Cloudflare Tunnel with ingress `boinc.usnewsmap.com` → `http://localhost:80`        | Public ingress with no inbound port                                                                                       |

All containers use the host network. Podman 4.3's CNI backend has no DNS for container names, so
`usnm-boinc-mysql` is mapped to 127.0.0.1 in `/etc/hosts` and with `--add-host`.

**Identity.** The host is an Azure Arc machine in the on-prem group. `lib.sh`'s `kv_token` (its Arc
path is #943, which this needs) and `usnm-store`'s credential both follow the Arc challenge flow when `IDENTITY_ENDPOINT` and
`IMDS_ENDPOINT` are set with no `IDENTITY_HEADER`. The containers mount `/var/opt/azcmagent/tokens`
read-only for the challenge file.

**Database TLS.** `mysql-setup.sh` makes a private CA under `/srv/usnm-boinc/mysql-tls`. The
server certificate's SANs are `usnm-boinc-mysql`, `localhost` and `127.0.0.1`. The containers mount `ca.pem` over
`/usr/local/share/usnm/mysql-ca.pem`, so the TLS patch (`scripts/boinc/patch-server-db-tls.py`)
verifies the server unchanged.

**Credentials.** The admin and runtime passwords come from the BOINC Key Vault, the same ones the
Azure server uses. They are kept in `/etc/usnm-boinc/secrets` (0600). The root password is local.

## Security updates

The images start from pinned digests, so their Debian packages don't change after a build, and
the host's own automatic updates never reach them. Each image that serves Apache and PHP takes
Debian's security updates when it is built: the server and bridge Containerfiles, the overlays
and `security-overlay.Containerfile` run `apt-get upgrade` from the image's sources, which
include `bookworm-security`. The build then fails if `php8.2-common` is older than `PHP_FLOOR`,
now 8.2.35-1~deb12u1 from DSA-6255-1 (21 October 2026). `test_security_updates.py` in
`contrib-compute/images/server` checks that every one of them does both.

After a Debian security advisory for a package in the image (PHP, Apache, OpenSSL, curl):

1. For PHP, raise `PHP_FLOOR` in all of those Containerfiles to the fixed version, through a pull request.
2. On the on-prem BOINC host, build `security-overlay.Containerfile` on the running image: `RUNTIME_IMAGE` is `SERVER_IMAGE` from `/etc/usnm-boinc/server.env`, and `SECURITY_REFRESH` is today's date, so a cached build runs the upgrade again. The file's header has the command. Only packages change; the project's code stays as it was.
3. Set `SERVER_IMAGE` in `server.env` to the new image and restart `usnm-boinc-server.service`. The step runner reads the same file, so the next step uses it too.
4. Check the running image: `podman exec usnm-boinc-server php -v`, and `podman exec usnm-boinc-server dpkg-query -W php8.2-common apache2 openssl libcurl3-gnutls` against the advisory's fixed versions.

A full rebuild from `main` (base, then server, then bridge, as `scripts/boinc/build-boinc-images.sh`
orders them) also takes the updates, since the server and bridge stages upgrade.

## Moving the data

The project share's storage account allows no public access, so data leaves Azure from inside the
BOINC environment and goes through R2 (bucket `usnm-boinc-project`, private):

- `r2-relay-job.sh` copies the share as a bridge-job execution with its command overridden. It skips the html cache, logs, the `tls464-*` backups, and every upload the database no longer needs. Each pass fits in the job's timeout and resumes on the next run. `RELAY_PART` (`download`, `quality`, `project-quality`, `uploads`, `rest`) splits a full pass across executions run side by side.
- `db-dump-job.sh` dumps the database (`--single-transaction`) to `db/<name>.sql.gz`. It also writes `db/<name>.uploads-filter`, which names the upload files of every result whose workunit isn't assimilated yet. With `RELAY_UPLOADS_FILTER` set to it, the uploads part copies exactly those.
- On the host, `r2-pull.sh` copies the tree into `/srv/usnm-boinc/project`, and `db-import.sh --name <dump> --replace` loads a dump.

The R2 token is limited to this bucket and expires once the move is done.

A trial (2026-10-21) took 21 s to dump, with a 37 MB file, and 77 s to import.

## The project tree after a copy

The copy doesn't bring the share's ownership or modes, and the share's runtime code is older
than the images. After each pull, before the server starts:

1. `chown -R 1000:1000 /srv/usnm-boinc/project` (boincadm), `chmod -R g+rwX` on the project, and `chmod 755` on the files in `bin/` and `cgi-bin/`. The share gave every file the same permissive mode, and the web pages write `html/cache` as `www-data`, which is in the boincadm group. The daemons create a `log_<host>` directory when they start, and the scheduler and upload handler log there as `www-data`, so it needs mode 2775. The relay skips `html/cache`, but a pass run before that exclusion copies Azure's cache, so delete those files: a cached `server_status` page shows the old host's daemons as Not Running.
2. `refresh-project-code.py apply --ack-server-stopped`, run in the bridge image with the server stopped. The share's `html/inc`, `py` and `bin` copies predate the TLS patch. The Azure database doesn't enforce TLS, so this never failed there. The on-prem database does.

## Cutover order

1. On Azure, make the tree still, as for the share move (`docs/boinc/server-and-apps.md`, "Moving the project share"): stop the priority controller and wait for running bridge executions. Then, as `boincadm` in `/project/usnmocr`, run `touch stop_maintenance`, `bin/stop`, then `touch stop_sched stop_upload`, and wait a minute for requests in flight. The scheduler and the upload handler then refuse with errors that clients retry, so no result lands on Azure after the final copy. Keep these files on Azure through the cutover.
2. Final `db-dump-job.sh`, then the five `RELAY_PART`s side by side, with `RELAY_UPLOADS_FILTER` set to that dump's filter. Each relay execution must log `RELAY complete`.
3. On the host: `r2-pull.sh`, `db-import.sh --replace`, then the project tree steps above.
4. On the host, remove `stop_maintenance`, `stop_sched` and `stop_upload` (they came over in the copy) and start the server. Point `boinc.usnewsmap.com` at the tunnel (proxied CNAME).
5. Start the controller under a new plan whose `step_command` reaches `usnm-boinc-step` (plans are immutable, so the change needs a drain). End `step_command` with `--` after the host (`["ssh", "-i", "<key>", "-o", "BatchMode=yes", "root@<host>", "--"]`). Without it, ssh reads the step's `--env` as one of its own options.
6. After it is verified, set `"server": "onprem"` in `contrib-compute/infra/boinc/env/<env>.json` and run `scripts/boinc/provision-boinc.sh`. It deletes the project storage accounts, their private endpoints and the rest of the Azure server (see below).

The bridge steps read and write the hand-off container in the main environment's storage, so
the on-prem group needs a role there before step 5. The hand-off job itself has no project tree
and stays in Azure.

## Azure after the cutover

With `"server": "onprem"`, `provision-boinc.sh` deploys only what the on-prem server still uses: the two identities (the main environment's Bicep reads `usnm-boinc-id-<env>`), the Key Vault the host reads, the VNet (its `aca` subnet is in the main environment's storage network rules) and the static site. The stack deletes the rest: the server app, its jobs and environment, MySQL, both project storage accounts, Log Analytics, the workbook and the private endpoints. The provision then checks that the on-prem server answers at the master URL. Prod moved on 2026-10-21.

`mysql-admin-password` in the vault still gets a new value on every provision. It no longer matters: the on-prem host keeps its own copy in `/etc/usnm-boinc/secrets/db-admin`.

Work that ran as Azure jobs, and where it runs now:

| Work                                                         | On-prem                                                                                                                                                                                                   |
| ------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Bridge steps (the controller's submit, spool-out, inventory) | `usnm-boinc-step`                                                                                                                                                                                         |
| Gold bundle                                                  | `usnm-boinc-step --env prod --step gold-bundle --var GOLD_CAMPAIGN=<campaign> --wait-mins 360`, run as root on the host                                                                                   |
| App version publish                                          | `scripts/boinc/boinc-app-release.sh --onprem-host <Proxmox host> --onprem-ct <id>`: builds the `appver` image in the container and runs `usnm-boinc-step --target publish` (#961)                         |
| Quality wave                                                 | the on-prem feeder on pve-laptop (#962, [boinc-quality.md](boinc-quality.md#on-prem-feeder-v1-wave10-on)): staged inputs reach the server through `usnm-boinc-step`; the server reads no scan data itself |

## Cloudflare

`boinc.usnewsmap.com` is a proxied CNAME to the tunnel. Volunteer clients and the controller are scripts, so the zone has Bot Fight Mode off. A configuration rule skips Browser Integrity Check for requests from the on-prem public IP to `boinc.usnewsmap.com`, because Browser Integrity Check answers `Python-urllib` with 403. An IP Access allow rule covers the same IP. Both rules name that IP, so update them if it changes. [ADR-0024](design/adr/0024-cloudflare-tunnel-and-r2-credentials.md) records the ADR-0009 exception for the Cloudflare credentials.
