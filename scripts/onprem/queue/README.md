# OCR work queues in PostgreSQL + pgmq

The OCR work queues move from Azure Storage Queues to PostgreSQL with the
[pgmq](https://github.com/pgmq/pgmq) extension on-prem (#967). The database
runs in its own container, created by home-infra's
`scripts/lab/queue-ct.sh`: PostgreSQL 17, pgmq 1.13.0, TLS 1.3, and client
certificates only.

## Schema and roles

`setup.sql` creates the three queues and the client roles. Run it as
`postgres` in the queue container. It's safe to run again.

```sh
psql -v ON_ERROR_STOP=1 -d usnm_queue -f setup.sql
```

pgmq names its tables after its queues, so the Azure names map with `-` turned
into `_`: `ocr-priority`, `ocr-bulk` and `ocr-deadletter` are `ocr_priority`,
`ocr_bulk` and `ocr_deadletter`.

| Role              | May                                                                                   |
| ----------------- | ------------------------------------------------------------------------------------- |
| `boinc_claim`     | Read (hide for a lease), send and delete on the work queues; send to `ocr_deadletter` |
| `ingest_producer` | Send to every queue; read and delete `ocr_deadletter`                                 |
| `dash_reader`     | Metrics and peek                                                                      |

Each role logs in only with a certificate whose CN is the role
(`queue-ct.sh --issue-client --role <name>`), and none has a password. Tested
on 2026-10-22: each role did what its row says and was refused the rest. A
producer can't read, and the dashboard can't read.

## Client side

`usnm_store::queue::open` takes a `postgres://` location and returns
`usnm_store::pgmq::PgmqQueue`:

```text
postgres://usnm-queue.tjg.fyi/usnm_queue?user=boinc_claim&sslcert=<client.crt>&sslkey=<client.key>&sslrootcert=<ca.pem>
```

- **Leases:** a received message's pop receipt is its visibility time.
  `delete` acts only while that lease is the current one, and otherwise fails
  with `LeaseLost`.
- **Release:** the trait's default: a copy is sent, then the original is
  deleted. pgmq reads visible messages in `msg_id` order, so a message given
  back in place would stay at the front and stop a scan there each time. The
  copy goes to the back, as on Azure.
- **Message text:** stored as a JSON string, so it comes back byte for byte.
- **Testing:** set `USNM_TEST_PGMQ` to a location whose role may send, read,
  update and delete on `ocr_deadletter` to run the live round trip in
  `cargo test -p usnm-store pgmq`.

## BOINC handoff containers

For a PostgreSQL `QUEUE_URL`, the on-prem step runner requires the role's
`client.crt`, `client.key` and `ca.pem` under `/etc/usnm-boinc/queue`.
It mounts only those three regular, non-symlink files read-only at
`/etc/usnm/queue/` in handoff containers. Use those container paths in the
URL. Keep the host directory root-owned and mode 0700, and the private key
mode 0600. Transfer keys directly between the queue host and client host,
never through a workstation. Azure queue URLs do not add certificate mounts.
`--what-if` checks the files without opening a connection or consuming work.
Rust's queue client still verifies TLS, server identity and client credentials.

This mount support is a prerequisite, **not an authorized backend cutover**.
Existing claim-v1 records carry backend-specific message IDs/pop receipts
without recording the backend. Do not change `QUEUE_URL` while unresolved
Azure claims exist: finish/release would send their receipts to PostgreSQL.
Coordinate one producer/consumer freeze, reconcile outstanding claims and
retain a rollback/transfer journal before changing any backend setting.
Certificate provisioning, message migration, producer/dashboard settings,
backup/restore and real lease/restart/ack acceptance remain separate gates
in #967. Never run Azure and PostgreSQL consumers on copied work concurrently.
