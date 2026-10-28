# BOINC server on-prem

The BOINC server, its database and the bridge steps move from Azure Container Apps to an on-prem
container host (#942, part of #917). This page covers what runs there and the cutover order.
The scripts are in `scripts/boinc/onprem/`, and the step runner is `scripts/boinc/onprem-step.sh`.

## Reviewed resumable release pipeline (#551)

`bootstrap_pipeline.py --onprem-host HOST --onprem-ct CT` selects the local Podman
lifecycle; omitting these arguments retains the Azure-only lifecycle. It never uses the
environment's historical Azure image pins as on-prem deployment or signing identities.
This is an **operator-invoked release**, not permission to run it during a protected #529
window. Code review/CI do not authorize a live restart, signing, dispatch or acceptance.

Prerequisites: the existing on-prem MySQL/project setup, configured `server.env` and
`bridge.env`, server unit, Podman, Python 3 and installed `usnm-boinc-step`; root SSH to
the Proxmox host/CT; an already unlocked reachable isolated signing host; GitHub workflow
and reviewed-PR permissions; and independent, dedicated existing-client acceptance inputs
from #548. The pipeline does not install a runtime, initialize/reset a database, unlock a
key, copy private keys, or change topology. A missing prerequisite fails explicitly.

Place the operator configuration and #548 existing-client configuration below ignored
`target/` in a clean checkout of reviewed main. Container configuration:

```json
{
  "env": "dev",
  "app": "usnm_ocr",
  "version": "1.0",
  "platform": "x86_64-pc-linux-gnu",
  "plan_class": "docker",
  "signing_host": "signing.example",
  "acceptance_config": "existing-client.json"
}
```

For a native Linux x86_64 release, add `"kind": "native"`, `"binary": "worker"`,
`"binary_sha256": "<64 lowercase hex>"`, and `"binary_source": "<40 hex source commit>"`.
Supported native identities are `usnm_quality` with empty `plan_class`, or `usnm_qwen`
with `mt`, `nvidia_cuda_2b`, `nvidia_cuda_8b`, `intel_vulkan_8b`,
`nvidia_vulkan_8b`, or `amd_vulkan_8b`. Supply an already built binary; additional
platforms are not claimed. A native release reviews the
binary checksum/source instead of dispatching a container worker workflow. Native
auxiliary payloads use `"extra_files": {"llama-server": {"path": "llama-server",
"sha256": "<64 hex>", "source_commit": "<40 hex>"}}`; every payload checksum/source
is included in the reviewed manifest and checked before signing. Existing native
plan-specific required-file checks still apply. Container
support remains OCR `docker` and Qwen `docker_mt_2b` / `docker_nvidia_cuda_8b`; do not
select a deprecated production plan just to exercise the lifecycle.

Offline parser check (no SSH, builds, signing, installation or publication):

```sh
python3 scripts/boinc/bootstrap_pipeline.py --config target/release/config.json \
  --state-dir target/release/state --onprem-host pve.example --onprem-ct 230 --dry-run
```

Only in a separately authorized serialized window, invoke/resume the same command:

```sh
python3 scripts/boinc/bootstrap_pipeline.py --config target/release/config.json \
  --state-dir target/release/state --onprem-host pve.example --onprem-ct 230 \
  --authorize-runtime-install \
  --coordination-claim https://github.com/tgoodyear/usnewsmap.com/issues/529#issuecomment-CLAIM
```

The explicit install flag authorizes installation of the **reviewed** `SERVER_IMAGE`
pin and a server-unit restart. The runner shares the installed bridge runner's exclusive
lock. Existing runtime prerequisites are read first; base/server/bridge-bin/bridge are
built locally from an archived reviewed source, using the existing Containerfiles.
Each local image's immutable ID and source label is read back. Partial builds retain
private per-stage image-ID receipts on the CT and reuse only source-verified IDs.
An independent per-operation supervisor inherits that lock and passes it to child
commands, so a lost SSH/helper cannot release exclusion while an operation continues.
Installation waits for the expected container to be running. A submitted systemd job
must finish before the supervisor releases its lock; if systemd cannot confirm this,
exclusion remains held for operator reconciliation rather than permitting overlap.
The rollback receipt and replacement env pin are file- and directory-fsynced before
restart submission; a failed artifact/source check cannot modify the privileged config.

Two independent review checkpoints remain mandatory:

1. A version-1 manifest at
   `contrib-compute/infra/boinc/releases/ENV-onprem-APP-PLAN.json` (`default` for an empty
   plan) records target host/CT, environment, exact release, source commit, local
   `base`/`bin`/`server`/`bridge` IDs, and the public worker digest with successful
   run/attempt/model/source receipt provenance (or native checksum/source).
   Only this manifest may change. The pipeline opens a PR and stops. A maintainer
   must review and merge it with successful CI and no unresolved findings.
   The pipeline never approves or merges its own candidate.
2. After reviewing the first manifest, installation writes `images.bridge` (the bridge
   contains both server and bridge tools) into `/etc/usnm-boinc/server.env`, preserving
   other host settings and a private old-pin rollback receipt. Both the server unit
   and installed step runner consume that file. The running image is checked, not
   just a tag. Signing transfers only this public image to the isolated signing host;
   no Azure pull/pin fallback is allowed. `boinc-app-release.sh --prepare-only
--onprem-manifest PATH --artifact-receipt FILE` emits a versioned signed local
   appver-ID receipt. Its source and runtime must match the candidate.
   A second manifest-only PR adds `images.appver` and `signing_source`.
   Publication waits for its reviewed merge and successful CI too.

Publication uses the installed `usnm-boinc-step --target publish --step publish
--image sha256:...`, never a moving tag. Runtime/env/source identities are rechecked
before publication and acceptance. The pipeline binds #1246's root-local acceptance
transport to the reviewed running bridge ID and the staged reviewed adapter source;
the operator's existing-client command and expected outputs are retained. The final
durable #548 receipt must prove canonical acceptance **and cleanup**, and is checksummed
on subsequent resumes. Fake tests are not live #548/#551 closure evidence.

Exit 3 means a workflow/review checkpoint is pending; rerun the identical command
after it completes. Interrupted/failed phases persist an ambiguous intent and require
operator reconciliation followed by `--approve-retry PHASE`. Completed phases never
repeat automatically. A crash after `server.env` replacement can be reconciled by the
runtime retry, which restarts and reads back the already-reviewed pin. There is no
automatic rollback, key unlock or destructive recovery. Do not delete state to evade
an ambiguous publication or acceptance reservation.
Native binary/auxiliary inputs are rehashed after copying into private staging;
only those verified copies are transferred for signing and appver construction.

## Deliberate terminal OCR retry (#1159)

`usnm-boinc retry` prepares **one explicitly reviewed single-page retry**, not an
automatic recovery loop. Attempt 0 keeps its existing v1 JSON and job/workunit ID.
Retry contract v2 binds a new attempt to its parent ID. Never edit an old request,
signed app version, journal, assimilator marker or accepted result to force a replay.

Before activation, deploy the reviewed reader to **every** strict journal consumer
(server, bridge and native sync), and sign/install a matching `usnm_ocr` worker
supporting contract v2. Old workers reject v2; a new CLI alone is insufficient.
Record the supporting worker's full configuration digest (`TESSERACT_IMAGE`) and
BOINC app version number. Existing trust, routing, source budgets and validation
still apply. Do not retry a bundle or bypass its reconciliation.

After confirming BOINC's actual terminal disposition and that native sync has
settled the exact previous native row as a permanent failure:

```sh
usnm-boinc retry --root /private/jobs --spool /private/assimilated \
  --job PARENT_JOB_ID --expected-error-mask 2 \
  --image sha256:SUPPORTING_WORKER_DIGEST --app-version-num SUPPORTING_APP_VERSION \
  --reviewed-reason 'Reviewed terminal 0x2; supporting worker release recorded' \
  --state-file /private/state.json \
  --curated-url /private/curated --reference-url /private/reference
```

Use `--expected-no-output-files` instead of `--expected-error-mask` only for an
actual `_no_output_files` marker. Unknown and zero masks are refused, not treated
as missing output. An error mask is decimal on the CLI (`2` reviews `0x2`).
Accepted/canonical/partial output, changed terminal proof, nonterminal work and a
newer native reservation all require reconciliation, not retry.

The command prints the new child ID and reserves it without submission. It
preserves the original scan, failed journal and request-day quota attribution.
An identical invocation resumes its durable prepared/reserved child; it never
creates another workunit for that parent. Submit that child through the existing
single-page `submit` path, which pins `--app_version_num` from the retry journal.
Uncertain submission still requires authoritative BOINC lookup and explicit
`reconcile`; never delete its journal and repeat. A retry of a failed child needs
a fresh terminal review naming that child.

If that child fails, the existing handoff `claim dead-letter` step publishes the
child's terminal mask and new message timestamp without touching the old claim.
Its private durable payload/sent receipt makes a completed repeat a no-op; lost
send acknowledgement retains at-least-once delivery. Native sync fences duplicate
old-attempt events from subsequent retries.

First acceptance is one reviewed failed-page canary: preserve its original real
mask through collect/journal/dead letter, verify the one new workunit and supporting
app version, collect/import via ordinary validation, verify native settlement,
and record all identities. Code tests or a merged PR do not prove this live gate.

## What runs on the host

| Unit                               | What                                                                                                                                                                | Notes                                                                                                                     |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| `usnm-boinc-mysql.service`         | MySQL 8.4 (the same major and minor as the Azure server), from `mysql-setup.sh`                                                                                     | Host network, `bind_address=127.0.0.1`, `mysqlx=OFF`, `require_secure_transport=ON`                                       |
| `usnm-boinc-server.service`        | The bridge image running `entrypoint.sh --serve`: Apache and the BOINC daemons                                                                                      | `--entrypoint` set, because the bridge image's own entrypoint is `bridge-job`                                             |
| `usnm-boinc-step`                  | `onprem-step.sh`: one bridge step at a time, in place of the Azure bridge job                                                                                       | The controller reaches it with `step_command` over ssh, using a forced command and `--from-ssh`                           |
| `usnm-boinc-step --target publish` | Publishes an app version from a local appver image, in place of the Azure publish job                                                                               | Root on the host only; `scripts/boinc/boinc-app-release.sh --onprem-host --onprem-ct` builds the image and runs it (#961) |
| `usnm-boinc-step --target handoff` | The hand-off job's `claim`, `release`, `collect` and `export`, in place of the Azure hand-off job; `--target scans --step receive` takes the controller's scan push | The controller's `--onprem-handoff`; settings in `/etc/usnm-boinc/handoff.env` (#967)                                     |
| `cloudflared`                      | A Cloudflare Tunnel with ingress `boinc.usnewsmap.com` → `http://localhost:80`                                                                                      | Public ingress with no inbound port                                                                                       |
| `usnm-image-security-check.timer`  | A daily check of the server image against `bookworm-security`, and of the MySQL pin                                                                                 | A failed `usnm-image-security-check.service` is the alert ([Security updates](#security-updates), #969)                   |

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
include `bookworm-security`. The build then fails if `php8.2-common` or `apache2` is older than
`PHP_FLOOR` or `APACHE_FLOOR`, now 8.2.35-1~deb12u1 from DLA-4848-1 (21 October 2026) and
2.4.67-1~deb12u3 from DSA-6323-1 (6 June 2026). Each floor is the version of bookworm's last
advisory for that package, so a newer build from bookworm's own suite passes it too.
`test_security_updates.py` in `contrib-compute/images/server` checks that every one of them
upgrades and enforces both floors.

After a Debian security advisory for a package in the image (PHP, Apache, OpenSSL, curl):

1. For PHP or Apache, raise `PHP_FLOOR` or `APACHE_FLOOR` in all of those Containerfiles to the fixed version, and `--php-floor` or `--apache-floor` in `usnm-image-security-check.service`, through a pull request. `test_image_security_check.py` fails when the unit and the Containerfiles disagree.
2. On the on-prem BOINC host, build `security-overlay.Containerfile` on the running image: `RUNTIME_IMAGE` is `SERVER_IMAGE` from `/etc/usnm-boinc/server.env`, and `SECURITY_REFRESH` is today's date, so a cached build runs the upgrade again. The file's header has the command. Only packages change; the project's code stays as it was.
3. Set `SERVER_IMAGE` in `server.env` to the new image and restart `usnm-boinc-server.service`. The step runner reads the same file, so the next step uses it too.
4. Check the running image: `podman exec usnm-boinc-server php -v`, and `podman exec usnm-boinc-server dpkg-query -W php8.2-common apache2 openssl libcurl3-gnutls` against the advisory's fixed versions. `systemctl start usnm-image-security-check.service` runs the scheduled check now and exits clean when nothing is pending. It reads `SERVER_IMAGE`, not the container, so it only vouches for what runs after step 3's restart.

A full rebuild from `main` (base, then server, then bridge, as `scripts/boinc/build-boinc-images.sh`
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
install -m 0755 scripts/boinc/onprem/image-security-check.sh /usr/local/bin/usnm-image-security-check
install -m 0644 scripts/boinc/onprem/usnm-image-security-check.service scripts/boinc/onprem/usnm-image-security-check.timer /etc/systemd/system/
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
the on-prem group needs a role there before step 5.

## Hand-off steps on-prem

`claim`, `release` and `collect` can run on the host instead of the Azure hand-off job (#967
phase 3). `usnm-boinc-step --target handoff --step claim|release|collect` runs the bridge
image's `handoff-job.sh`, the Azure job's own command, from `SERVER_IMAGE`, the image the other
bridge steps use. It runs as the host's own Arc machine identity, which ADR-0008's Cosmos
amendment gives the hand-off job's grants. The container mounts only the Arc agent's challenge
directory, with no project tree, database or Key Vault. It takes `HANDOFF_STEP` and only that
step's inputs, over ssh or as root.

`/etc/usnm-boinc/handoff.env` holds the Azure job's settings, without its Container Apps
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

The values are the job's environment: `az containerapp job show --name usnm-boinc-handoff-caj-<env>
--resource-group rg-usnm-<env> --query "properties.template.containers[0].env"`. The
controller then sends the steps here with `--onprem-handoff claim --onprem-handoff collect`
([boinc-batch-scheduler.md](boinc-batch-scheduler.md#on-prem-claim-collect-and-export-967)), and
`export` too with `--onprem-handoff export`: the controller then pushes the scans straight to this host (`--target scans --step receive`, into `/srv/usnm-boinc/project/handoff-scans`), export mounts them read-only, and `submit-remote` takes each page from them, so no scan goes through Azure (#1116).
`texts-export` runs on the host with the local copy of curated (`scripts/boinc/texts-export-onprem.sh`, [boinc-quality.md](boinc-quality.md)). Once claim and collect have run cleanly here
for a day, a separate Bicep change retires the job's claim and collect use.

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
