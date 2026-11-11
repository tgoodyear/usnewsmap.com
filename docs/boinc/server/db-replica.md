# BOINC database replica

A live replica of the BOINC MySQL runs in CT 237 (`usnm-boinc-db-replica`) on pve-big, a
different machine from the primary (CT 230 on pve). It is asynchronous and read-only, and
nothing fails over by itself. Backups to R2 are in [db-backup.md](db-backup.md).

## How it's built

- **Container:** an unprivileged Debian 12 LXC with podman (nesting, keyctl), 2 cores, 4 GB, 40
  GB on `local-lvm`, made by home-infra `scripts/lab/boinc-db-replica-ct.sh`. MySQL runs from
  the primary's own image digest (`mysql:8.4`, 8.4.13) as `usnm-boinc-mysql-replica.service`,
  with a 2 GB buffer pool for the ~1.6 GB working set.
- **Settings:** `server_id = 2`, `read_only` and `super_read_only` on, `log_replica_updates`,
  `relay_log_recovery`, `enforce_gtid_consistency = ON`, `gtid_mode = ON_PERMISSIVE` until the
  primary runs GTIDs (below), TLS only, listening on 127.0.0.1 only.
- **Transport:** the primary's MySQL listens on 127.0.0.1 only (`mysql-setup.sh`), so the replica
  reaches it through ssh. `usnm-boinc-db-tunnel.service` forwards the replica's 127.0.0.1:13306
  to the primary's 127.0.0.1:3306, as the system user `usnm-db-tunnel` on CT 230. That user's
  one key may only forward to 127.0.0.1:3306, or listen on 127.0.0.1:3306 for a failover. It has
  no shell and no other forwards (`replica-source.sh --tunnel`). The replica pins CT 230's host
  key.
- **TLS:** replication is TLS end to end inside the tunnel. The replica verifies the primary's
  certificate against the database's private CA (`SOURCE_SSL_VERIFY_SERVER_CERT=1`). The
  replica's own server certificate is signed by the same CA, with the same names
  (`usnm-boinc-mysql`, `localhost`, `127.0.0.1`), so after a failover the BOINC daemons trust it
  unchanged.
- **Users:** the replication user is `usnmrepl@127.0.0.1` (`REPLICATION SLAVE`, TLS only). Its
  password is in the home Key Vault as `usnm-boinc-mysql-repl-password` (MySQL caps it at 32
  characters).
- **Seeding:** the replica was seeded with `CLONE INSTANCE` from the primary. That needs the
  clone plugin, installed on the primary online, and a temporary `usnmclone@127.0.0.1` user,
  dropped afterwards. The clone brings the primary's users, and the root password with them, so
  the replica's `/etc/usnm-boinc/secrets/db-root` is the primary's.

## Check it

On pve-big:

```sh
<home-infra checkout>/scripts/lab/boinc-db-replica-ct.sh --status
```

prints `Replica_IO_Running`, `Replica_SQL_Running`, `Seconds_Behind_Source` and the last errors
as JSON. `--counts` prints row counts per table, which the monthly restore drill compares
against.

If the primary restarts or the tunnel drops, the replica retries every 10 s, for up to a million
attempts.

## Build or rebuild it

1. On pve-big:

   ```sh
   boinc-db-replica-ct.sh --create --image <CT 230: podman inspect usnm-boinc-mysql --format '{{.ImageName}}'> --dir /root/boinc-db-replica/xfer
   ```

2. On CT 230, with `server.csr` and `tunnel.pub` from that directory:

   ```sh
   replica-source.sh --sign-csr server.csr --out server-cert.pem
   replica-source.sh --tunnel --pubkey tunnel.pub > source-host-key
   ```

   Bring `server-cert.pem`, `source-host-key` and CT 230's `/srv/usnm-boinc/mysql-tls/ca.pem`
   back to the directory on pve-big.

3. On pve-big, run `boinc-db-replica-ct.sh --install-cert --dir ...`.
4. On CT 230, feed each password on stdin (32 hex characters, e.g. `openssl rand -hex 16`):

   ```sh
   replica-source.sh --users --password-stdin         # the Key Vault's repl password
   replica-source.sh --clone-donor on --password-stdin
   ```

5. On pve-big, with `repl-password`, `clone-password` and `db-root` (CT 230's
   `/etc/usnm-boinc/secrets/db-root`) in the directory, run
   `boinc-db-replica-ct.sh --clone --dir ...`. It moves them into the CT and deletes them from the directory. The clone of the
   ~4 GB data directory takes under a minute, capped at 40 MiB/s.
6. On CT 230, run `replica-source.sh --clone-donor off`.

A replica that fell behind past the primary's 3 days of binlogs can't catch up: rebuild it.
`--clone` refuses a server that already replicates, so first stop the CT's
`usnm-boinc-mysql-replica` and empty `/srv/usnm-boinc/mysql-data`.

## Turn on GTIDs

The primary started with `gtid_mode = OFF`, so replication runs from binlog file and position.
Moving the primary to GTIDs is online in MySQL 8.4, with no restart, one step at a time with
`SET PERSIST`:

1. `enforce_gtid_consistency = WARN`, then watch the primary's log
   (`podman logs usnm-boinc-mysql` on CT 230) for GTID-unsafe statements through at least one full day,
   including the 00:13 UTC maintenance runs.
2. `enforce_gtid_consistency = ON`.
3. `gtid_mode = OFF_PERMISSIVE`, then `ON_PERMISSIVE`.
4. Wait until `Ongoing_anonymous_transaction_count` is 0 on the primary and the replica has
   applied everything.
5. `gtid_mode = ON`.

Then run `boinc-db-replica-ct.sh --gtid-auto-position` on pve-big. It sets the replica's
`gtid_mode` to ON and replication to `SOURCE_AUTO_POSITION = 1`. Nightly dumps from then on
carry `gtid_purged`, and `restore.sh` starts its server with `gtid_mode = ON` for them.

## Failover

Fail over when the primary's MySQL is lost (its data, or CT 230's disk) and a BOINC host can
run the daemons: CT 230 itself, or a rebuilt BOINC host. If CT 230 or pve is down as a
whole, BOINC is down whatever the database does: rebuild the BOINC host first, then point it
here the same way.

Writes that hadn't reached the replica are lost: up to `Seconds_Behind_Source` (normally 0).
The R2 binlogs are no newer than the replica.

1. **Stop the writers.** Stop the BOINC daemons in `usnm-boinc-server` on CT 230 (the project's
   `bin/stop`) and the timers that write (feeds, bridge steps). Stop
   `usnm-boinc-mysql.service` if it still runs, so nothing writes to it and port 3306 is free.
2. **Promote.** On pve-big, run `boinc-db-replica-ct.sh --promote`. It waits for the applier to
   finish its relay log, runs `STOP REPLICA; RESET REPLICA ALL`, removes the read-only settings
   and stops the tunnel.
3. **Point the BOINC host at it.** On pve-big, run `boinc-db-replica-ct.sh --serve-primary-host`.
   It starts `usnm-boinc-db-failover-forward.service`, an `ssh -R` that makes 127.0.0.1:3306 on
   CT 230 reach the promoted server. The daemons' `config.xml` (`db_host` `usnm-boinc-mysql`,
   which is 127.0.0.1) and their CA stay as they are; the users and passwords came with the
   clone.
4. **Check it.** In `usnm-boinc-server` on CT 230, read a row through `/project/usnmocr/config.xml`
   (the pattern in `qwen-dispatch-fairness.py`, function `collect`). Start the daemons and the
   timers again, and check that `Com_update` rises on the promoted server.
5. **Back up the new primary.** The R2 dump and binlog timers on CT 230 read CT 230's own
   database, so they stop working. Until the database is home again, install `db-backup.sh` in
   CT 237 and run it there with `--container usnm-boinc-mysql-replica`,
   `--datadir /srv/usnm-boinc/mysql-data` and `--ca /srv/usnm-boinc/mysql-tls/ca.pem` (the `usnmbackup` user came
   with the clone).

To move back, rebuild the primary's database on CT 230 as a replica of the promoted server, or
from its newest R2 dump. Then stop the writers, stop `usnm-boinc-db-failover-forward`, start
`usnm-boinc-mysql`, and rebuild CT 237 as a replica ("Build or rebuild it").
