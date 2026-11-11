# Local BOINC pipeline documents

`usnm-state` supports concurrent pipeline documents in PostgreSQL, alongside the
existing Cosmos backend. BOINC's `--cosmos` argument and ingest's
`USNM_COSMOS_ENDPOINT` accept a `postgres://` URL with the same certificate
parameters as the existing pgmq backend. The legacy names are retained to keep
current controller and wrapper call sites compatible.

Apply `scripts/onprem/queue/documents.sql` as the owner of the document database.
The document database must use `UTF8`; the schema refuses other encodings before
making changes. If an existing queue database uses `SQL_ASCII`, create a separate
`UTF8` document database from `template0` and configure its certificate-authenticated
access instead of changing the queue database in place. JSONB must preserve the
corpus's non-ASCII text.
The schema grants BOINC and ingest read/write access, and dashboard readers
read-only access. Authentication requires a matching client certificate, TLS 1.3,
and server-name verification. Password URLs are refused. Each operation uses a
fresh connection, a bounded statement/lock timeout and synchronous commits.
Unique keys protect create; ETag-conditional updates protect concurrent leases.
There is no automatic cloud fallback.

This implementation does not switch any live settings. Before cutover, stop all
writers and reconcile the final document snapshot, queue leases and journals.
Import the original container, partition key, document and ETag into
`usnm_state.documents`; retain the cloud snapshot read-only. Move producers,
claimers and collectors together. A local handoff root alone does not migrate
the lake, queue or document backend.

Acceptance requires actual claim, export, submission, returned-result collection,
process restart and duplicate-free replay, plus successful off-host backup and
isolated restore. Keep Azure Key Vault and managed-identity credential access.
Do not delete cloud state or alter production web routing as part of this change.

## Moving documents and queue messages

Two `usnm-boinc` commands do the copies at the cutover. Their code is in
`crates/usnm-boinc/src/cli/migrate.rs`, `crates/usnm-state/src/migrate.rs` and
`crates/usnm-store/src/queue_move.rs`. Both read Azure with the ambient Entra credential
(`usnm_store::credential`) and write PostgreSQL with the client-certificate connection from
`usnm_store::pgmq::connection_options`. Neither takes a password, SAS token or shared key.
Progress goes to stderr. The receipt, one JSON object, goes to stdout.

```text
usnm-boinc docs-migrate --from https://<account>.documents.azure.com/ \
    --to 'postgres://<host>/usnm_state?user=<role>&sslcert=…&sslkey=…&sslrootcert=…' \
    [--container batches]... [--verify] [--page-size 100] [--max-ru-per-sec 200]
```

`docs-migrate` copies every document of each container in `migrate::CONTAINERS` (`batches`,
`index_runs`, `issues`, `ops` and `titles`, as `infra/modules/cosmos.bicep` creates them) into
`usnm_state.documents`. Each row keeps its container, partition key value, id, document and
Cosmos ETag. Before reading, `migrate::plan` checks each container's partition key path
against the account. Without `--container`, it refuses an account that holds a container the
list doesn't name. Reads are paged (`CosmosDocs::query_page`) and paced to `--max-ru-per-sec`.
The account's throughput is never changed. The import (`Target::import` for `PostgresDocs`)
inserts missing rows and rewrites an existing row only when its ETag differs, so a rerun after
a partial copy adds no duplicates and leaves unchanged rows alone. A document deleted from
Cosmos after an earlier copy stays in PostgreSQL. `--verify` reports it.

`--verify` writes nothing. It reads both sides and compares, per container, the count and a
SHA-256 over the entries sorted by partition key and id. Each entry is the length-prefixed
partition key, id and ETag, then the SHA-256 of the document's canonical JSON (`migrate::canonical`:
keys sorted, no whitespace, and a float with no fraction written as an integer, as JSONB
returns it). The command prints the first ten differences per container
(`missing_in_target`, `missing_in_source`, `etag`, `document`) and exits non-zero if any
container differs. The receipt carries each container's counts, digests and request charge,
and the start and end times.

```text
usnm-boinc queue-move --from https://<account>.queue.core.windows.net/ocr-bulk \
    --to 'postgres://<host>/usnm_queue?user=<role>&sslcert=…&sslkey=…&sslrootcert=…&queue=ocr-bulk' \
    [--max N] [--dry-run] [--visibility-secs 300] [--concurrency 4] [--hash-log moved.jsonl]
```

`queue-move` (`queue_move::move_messages`) receives each message with the visibility timeout,
sends its text unchanged to the pgmq queue the `queue=` parameter names, and deletes it from
Azure only after the send has committed. A failed send deletes nothing and stops the move;
the hash log records the message as `send_failed`, since the send may have committed before
its reply was lost. A
failed delete after a committed send also stops it; the hash log records that message as
`sent_delete_failed`. An empty receive only means no message is visible. Unless `--max` ended
the move, the command exits non-zero while the source still counts messages, for example ones
hidden under another receiver's lease, so run it again once they are visible.
Delivery is at least once: a message whose lease ran out before the delete is counted under
`sent_lease_lost` and will be sent again. Duplicates can be found from the body hashes.
`--hash-log` appends one line per message with its Azure id, dequeue count, body SHA-256 and
outcome. The receipt counts messages received, moved and previously received (dequeue count
above one), distinct and duplicate bodies, and the approximate counts of both queues before
and after. Its `body_digest` is the sum of every moved body's SHA-256 modulo 2^256. The sum
does not depend on order, and the digests of partial runs add up to the digest of the whole
move. `--dry-run` (`queue_move::peek_messages`) only counts both queues and hashes up to 32
messages from a peek, which neither takes nor hides them.

Run both with the writers stopped. Move each of `ocr-priority`, `ocr-bulk` and
`ocr-deadletter` with its own `queue-move`. Run `docs-migrate --verify` after the last copy.
The integration tests run against the queue fixture (`scripts/onprem/queue/ci-fixture.sh`):
`postgres_documents_import_keeps_etags_and_reruns_cleanly` with `USNM_TEST_DOCUMENTS`, and
`queue_move_into_pgmq_live` with `USNM_TEST_PGMQ`.

## Local lake mounts

The on-prem handoff runner accepts `CURATED_URL=/lake-state/curated`,
`REFERENCE_URL=/lake-state/reference`, and `OUT_URL=/lake-state/ocr-out`.
Each uses an existing child directory of `/srv/usnm-boinc/lake-state`, or the
host-owned `USNM_BOINC_LAKE_ROOT` override. The root must be canonical, owned by
the runner user and mode `0700`. Symlinked or absent stores fail before Podman
starts. Caller-selected container paths are refused; HTTPS stores retain their
existing behavior. Each configured local store is mounted read-write at its
fixed path.

This only exposes staged files; it does not switch settings or establish
authority. Reconcile existing on-prem content using source checksums and retained
transfer receipts before downloading missing or changed objects. Qualify atomic
writes and crash durability on the actual backing filesystem, and preserve
single-writer coordination during the final delta.
