# BOINC database backups

The BOINC MySQL on the BOINC host (CT 230 on pve, [onprem.md](onprem.md)) has three copies
besides its own data directory:

| Copy                               | Where                                              | How far behind  | Kept                               |
| ---------------------------------- | -------------------------------------------------- | --------------- | ---------------------------------- |
| Nightly Proxmox snapshot of CT 230 | the backup server on pve-big                       | up to a day     | as the backup job keeps it         |
| Live replica                       | CT 237 on pve-big ([db-replica.md](db-replica.md)) | seconds         | current only                       |
| Dump and binlogs                   | R2 bucket `usnm-boinc-db-backup` (off site)        | about 5 minutes | 30 nightly dumps, binlogs to match |

The replica protects against losing CT 230 or pve, and the R2 copy against losing the house.
Neither protects against a bad change that replicates: for that, restore from R2 to just before
it (below).

The scripts are in `boinc/scripts/onprem/db-backup/`.

## What runs

On CT 230 (installed by `install.sh`):

- `usnm-boinc-db-dump.timer`, daily at 08:20 UTC (03:20 US Eastern in winter):
  `db-backup.sh --dump` streams `mysqldump` of `usnmocr` (`--single-transaction`, `--routines`,
  `--triggers`, `--events`, `--hex-blob`, `--source-data=2`, `--set-gtid-purged=AUTO`) through gzip to
  `dump/<year>/<UTC time>.sql.gz`, then writes `<same name>.json`: server UUID, `gtid_mode`, the
  binlog file and position the dump starts from, the image digest, the dump's sha256 and size.
  mysqldump holds `FLUSH TABLES WITH READ LOCK` for a moment to read those coordinates; a query
  that runs long at that moment makes everything wait behind it, hence the quiet hour. A dump is
  about 95 MB and takes about 2 minutes.
- `usnm-boinc-db-binlog-ship.timer`, 5 minutes after each run ends:
  `db-backup.sh --ship-binlogs` runs `FLUSH BINARY LOGS` and copies every closed binlog not yet in the bucket to
  `binlog/<server UUID>/<file>.gz`. The primary keeps binlogs for 3 days
  (`binlog_expire_logs_seconds`, `mysql-setup.sh`), so shipping can be down for up to 3 days
  before a gap opens; a run that finds a gap fails.

Each run logs one `boinc_db_backup` JSON line, and a failed run fails its unit.

The MySQL clients run from the server's own image (`podman run --log-driver none`, so the dump
never lands in the journal), as the user `usnmbackup@127.0.0.1` (TLS only; `SELECT`, `SHOW VIEW`,
`TRIGGER`, `EVENT`, `LOCK TABLES` on `usnmocr`, and `RELOAD`, `PROCESS`, `REPLICATION CLIENT`,
`BACKUP_ADMIN`). Its password is `/etc/usnm-boinc/secrets/db-backup`, made by `install.sh`.

## The bucket and its keys

- Bucket `usnm-boinc-db-backup`, location hint ENAM, Standard storage.
- Lifecycle: `dump/` objects are deleted after 31 days, `binlog/` after 32, so the oldest dump
  kept still has every binlog after it. Unfinished multipart uploads are aborted after a day.
- Bucket lock `no-delete-7d`: no object can be deleted or overwritten in its first 7 days, by any
  key. A stolen writer key can't erase the last week.
- Two account API tokens, each limited to this one bucket. Their S3 keys are in the home Key
  Vault (`home-kv-bmfr6iwvyzs52`):
  - writer (object read and write), for CT 230: `usnm-boinc-db-backup-r2-writer-access-key-id`,
    `usnm-boinc-db-backup-r2-writer-secret-access-key`
  - reader (object read), for restores and the drill: `usnm-boinc-db-backup-r2-reader-access-key-id`,
    `usnm-boinc-db-backup-r2-reader-secret-access-key`
  - endpoint: `usnm-boinc-db-backup-r2-endpoint`

At about 100 MB of dump a night and 150 MB of compressed binlogs a day, the bucket holds about
8 GB, inside R2's free 10 GB, and its operations stay inside the free tier.

## Install or reinstall on CT 230

From a checkout of reviewed main on a host with the Key Vault and root ssh to pve:

```sh
V=home-kv-bmfr6iwvyzs52
s() { az keyvault secret show --vault-name $V --name "usnm-boinc-db-backup-r2-$1" --query value -o tsv; }
{ printf 'R2_ACCESS_KEY_ID=%s\n' "$(s writer-access-key-id)"
  printf 'R2_SECRET_ACCESS_KEY=%s\n' "$(s writer-secret-access-key)"
  printf 'R2_ENDPOINT=%s\n' "$(s endpoint)"; } |
  ssh root@pve.example 'pct exec 230 -- <checkout>/boinc/scripts/onprem/db-backup/install.sh --r2-env-stdin'
```

`install.sh` installs the pinned rclone, `db-backup.sh`, the four units and the `usnmbackup`
user, and enables the timers. It changes no MySQL setting and restarts nothing. Check with
`journalctl -u usnm-boinc-db-dump -u usnm-boinc-db-binlog-ship -o cat | grep boinc_db_backup`.

## Restore

`restore.sh` restores into a MySQL container of its own (the image the dump's manifest names),
on any host with podman, and never touches another server. It:

1. picks the newest dump (or `--dump dump/<year>/<time>.sql.gz`) and checks its sha256 against
   the manifest;
2. loads it;
3. rolls it forward with the server's own replication applier, because the image has no
   `mysqlbinlog`: every shipped binlog from the dump's file on goes into the data directory as a
   relay log, and the SQL thread applies them from the dump's position, with
   `REPLICATE_DO_DB = (usnmocr)`. It fails if a binlog in the run is missing;
4. writes row counts per table (`--counts-out`).

The host also needs rclone at `/usr/local/lib/usnm-db-backup/rclone` (CT 230 has it; elsewhere,
install the release `install.sh` pins, or pass `--rclone`). Put the reader key on the host where
you restore (mode 600), with the same three lines as
above, using the reader names. Then:

```sh
restore.sh --r2-env /etc/usnm-db-backup/r2-reader.env --workdir /srv/restore --keep
```

`--keep` leaves the restored server on 127.0.0.1:3306 (`--port` to change it). Its root password
is in `<workdir>/dl/root`.

### Undo a bad change

Restore with `--keep` to a scratch host first and find where the change starts. A kept restore
leaves the relay logs (the binlogs under another name) and its stopped replay channel, so:

```sql
SHOW RELAYLOG EVENTS IN 'restore-relay.000123' LIMIT 200;
```

(the `Pos` column is the binlog position, and `restore-relay.000123` holds `binlog.000123`). Then
restore again with `--until binlog.000123:<that Pos>`: the applier stops before that event.

### Replace the primary's database

Use this when CT 230's data is lost and the replica can't be promoted. Promoting the replica
([db-replica.md](db-replica.md)) is faster and loses less.

1. Stop the BOINC daemons and `usnm-boinc-mysql` on CT 230.
2. Restore with `--keep` on CT 230 itself, to another port and work directory, and check the
   counts.
3. On the restored server, run `RESET REPLICA ALL` (a kept restore leaves its replay channel
   and relay logs in place). Stop the restored container, move its data directory to
   `/srv/usnm-boinc/mysql-data` (`chown -R 999:999`), copy `<workdir>/dl/root` (the restored
   server's root password) over `/etc/usnm-boinc/secrets/db-root`, and start
   `usnm-boinc-mysql`.
4. Run `mysql-setup.sh` again: with that root password it recreates the `usnmboincadmin` and
   `usnmboincrun` users with their Key Vault passwords. Run `install.sh` again for `usnmbackup`, and
   `replica-source.sh --users` for the replica's user.
5. Start the daemons, then rebuild the replica from the new primary.

## Monthly drill

`restore-drill.sh` runs on pve-big from `usnm-boinc-db-restore-drill.timer` (the 3rd of each
month, 14:40 UTC). It makes scratch CT 9237, installs podman and the pinned rclone there, runs
`restore.sh` with the reader key, compares each table's row count with the live replica (CT 237),
and destroys the CT. The replica runs a few minutes ahead of the newest shipped binlog, so a
table passes within 500 rows or 2%, whichever is larger. It logs one `boinc_db_restore_drill`
line with `"pass"`; a failed drill fails the unit.

Each drill also leaves `{"pass", "finished_at"}` in `/var/lib/usnm-db-backup/last-drill.json` in
the replica CT. The feed monitor alerts on it (`db_drill`), and on the newest dump and binlog in
R2 (`db_dump`, `db_binlog`); see
[boundaries-and-monitoring.md](boundaries-and-monitoring.md#optional-checks).

Install on pve-big, from a checkout of reviewed main:

```sh
install -d -m 755 /usr/local/lib/usnm-db-backup-drill
install -m 755 boinc/scripts/onprem/db-backup/{restore.sh,restore-drill.sh,install.sh} /usr/local/lib/usnm-db-backup-drill/
install -m 644 boinc/scripts/onprem/db-backup/usnm-boinc-db-restore-drill.{service,timer} /etc/systemd/system/
install -d -m 700 /etc/usnm-db-backup   # then the reader key as /etc/usnm-db-backup/r2-reader.env, mode 600
systemctl daemon-reload && systemctl enable --now usnm-boinc-db-restore-drill.timer
```

(`install.sh` is there because the drill reads the rclone pin from it.) Run one by hand with
`systemctl start usnm-boinc-db-restore-drill` and read `journalctl -u usnm-boinc-db-restore-drill`.
