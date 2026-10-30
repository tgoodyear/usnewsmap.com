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
