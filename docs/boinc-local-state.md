# Local BOINC pipeline documents

`usnm-state` supports concurrent pipeline documents in PostgreSQL, alongside the
existing Cosmos backend. BOINC's `--cosmos` argument and ingest's
`USNM_COSMOS_ENDPOINT` accept a `postgres://` URL with the same certificate
parameters as the existing pgmq backend. The legacy names are retained to keep
current controller and wrapper call sites compatible.

Apply `scripts/onprem/queue/documents.sql` as the owner of the queue database.
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
