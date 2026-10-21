# BOINC server on-prem

The BOINC server, its database and the bridge steps move from Azure Container Apps to an on-prem
container host (#942, part of #917). This page covers what runs there and the cutover order.
The scripts are in `scripts/boinc/onprem/`, and the step runner is `scripts/boinc/onprem-step.sh`.

## What runs on the host

| Unit | What | Notes |
|---|---|---|
| `usnm-boinc-mysql.service` | MySQL 8.4 (the same major and minor as the Azure server), from `mysql-setup.sh` | Host network, `bind_address=127.0.0.1`, `mysqlx=OFF`, `require_secure_transport=ON` |
| `usnm-boinc-server.service` | The bridge image running `entrypoint.sh --serve`: Apache and the BOINC daemons | `--entrypoint` set, because the bridge image's own entrypoint is `bridge-job` |
| `usnm-boinc-step` | `onprem-step.sh`: one bridge step at a time, in place of the Azure bridge job | The controller reaches it with `step_command` over ssh, using a forced command and `--from-ssh` |
| `cloudflared` | A Cloudflare Tunnel with ingress `boinc.usnewsmap.com` → `http://localhost:80` | Public ingress with no inbound port |

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

## Moving the data

The project share's storage account allows no public access, so data leaves Azure from inside the
BOINC environment and goes through R2 (bucket `usnm-boinc-project`, private):

- `r2-relay-job.sh` copies the share as a bridge-job execution with its command overridden. It skips old uploads, the html cache, logs and the `tls464-*` backups. Each pass fits in the job's timeout and resumes on the next run.
- `db-dump-job.sh` dumps the database (`--single-transaction`) to `db/<name>.sql.gz`.
- On the host, `r2-pull.sh` copies the tree into `/srv/usnm-boinc/project`, and `db-import.sh --name <dump> --replace` loads a dump.

The R2 token is limited to this bucket and expires once the move is done.

A trial (2026-10-21) took 21 s to dump, with a 37 MB file, and 77 s to import.

## The project tree after a copy

The copy doesn't bring the share's ownership or modes, and the share's runtime code is older
than the images. After each pull, before the server starts:

1. `chown -R 1000:1000 /srv/usnm-boinc/project` (boincadm), `chmod -R g+rwX` on the project, and `chmod 755` on the files in `bin/` and `cgi-bin/`. The share gave every file the same permissive mode, and the web pages write `html/cache` as `www-data`, which is in the boincadm group.
2. `refresh-project-code.py apply --ack-server-stopped`, run in the bridge image with the server stopped. The share's `html/inc`, `py` and `bin` copies predate the TLS patch. The Azure database doesn't enforce TLS, so this never failed there. The on-prem database does.

## Cutover order

1. On Azure, make the tree still, as for the share move (`docs/boinc.md`, "Moving the project share"): stop the priority controller and wait for running bridge executions. Then, as `boincadm` in `/project/usnmocr`, run `touch stop_maintenance`, `bin/stop`, then `touch stop_sched stop_upload`, and wait a minute for requests in flight. The scheduler and the upload handler then refuse with errors that clients retry, so no result lands on Azure after the final copy. Keep these files on Azure through the cutover.
2. Final `db-dump-job.sh` and a delta `r2-relay-job.sh` pass (both `RELAY_PART`s). Each relay execution must log `RELAY complete`.
3. On the host: `r2-pull.sh`, `db-import.sh --replace`, then the project tree steps above.
4. On the host, remove `stop_maintenance`, `stop_sched` and `stop_upload` (they came over in the copy) and start the server. Point `boinc.usnewsmap.com` at the tunnel (proxied CNAME).
5. Start the controller under a new plan whose `step_command` reaches `usnm-boinc-step` (plans are immutable, so the change needs a drain).
6. After it is verified: delete the project storage accounts and their private endpoints, and remove them from Bicep.

The bridge steps read and write the hand-off container in the main environment's storage, so
the on-prem group needs a role there before step 5. The hand-off job itself has no project tree
and stays in Azure.
