# Moving the BOINC server on-prem

**Status:** done; prod's server has run on-prem since 21 October 2026 · **Date:** 21 October 2026 · **Measured on:** a trial dump and import on 21 October 2026, and the cutover that day · **Code:** `boinc/scripts/onprem/` (`r2-relay-job.sh`, `db-dump-job.sh`, `r2-pull.sh`, `db-import.sh`), `boinc/images/server/refresh-project-code.py` · **Issues:** #942, #917

How the project share and the database left Azure for the on-prem host. The server as it runs now is in [BOINC server on-prem](../boinc/server/onprem.md). Moved here from that page on 10 November 2026.

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

1. On Azure, make the tree still, as for the share move ([Moving the project share](../boinc/server/azure.md#moving-the-project-share)): stop the priority controller and wait for running bridge executions. Then, as `boincadm` in `/project/usnmocr`, run `touch stop_maintenance`, `bin/stop`, then `touch stop_sched stop_upload`, and wait a minute for requests in flight. The scheduler and the upload handler then refuse with errors that clients retry, so no result lands on Azure after the final copy. Keep these files on Azure through the cutover.
2. Final `db-dump-job.sh`, then the five `RELAY_PART`s side by side, with `RELAY_UPLOADS_FILTER` set to that dump's filter. Each relay execution must log `RELAY complete`.
3. On the host: `r2-pull.sh`, `db-import.sh --replace`, then the project tree steps above.
4. On the host, remove `stop_maintenance`, `stop_sched` and `stop_upload` (they came over in the copy) and start the server. Point `boinc.usnewsmap.com` at the tunnel (proxied CNAME).
5. Start the controller under a new plan whose `step_command` reaches `usnm-boinc-step` (plans are immutable, so the change needs a drain). End `step_command` with `--` after the host (`["ssh", "-i", "<key>", "-o", "BatchMode=yes", "root@<host>", "--"]`). Without it, ssh reads the step's `--env` as one of its own options.
6. After it is verified, set `"server": "onprem"` in `boinc/infra/boinc/env/<env>.json` and run `boinc/scripts/provision-boinc.sh`. It deletes the project storage accounts, their private endpoints and the rest of the Azure server.

The bridge steps read and write the hand-off container in the main environment's storage, so
the on-prem group needs a role there before step 5.
