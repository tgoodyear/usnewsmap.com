# Local hand-off state

The controller and the hand-off steps still keep their state in Azure: the dispatch journals, claims, scheduler inventory and controller lease in the `boinc-handoff` container, the `ocr-sync` lease in Cosmos, and the lake's `curated`, `reference` and `ocr-out`. Moving that state onto the BOINC host is [#1367](https://github.com/tgoodyear/usnewsmap.com/issues/1367), open on 10 November 2026. The code below is in place; **none of it is switched on in prod**.

Each piece only adds a local option. Switching one over is a cutover: stop every writer, copy and verify the state, move the producers, claimers and collectors together, and keep the Azure copy read-only for rollback. Never run one writer against each copy. Acceptance needs real claim, export, submission, returned-result collection, a process restart and a duplicate-free replay. Changing one transport doesn't move the others, and Key Vault and managed-identity access may stay in Azure.

## Hand-off journals

A queue-fed controller plan can set `handoff_root` to an existing local directory. It is mounted into the bridge and hand-off containers at `/handoff-state`. The controller then locks with the filesystem and reads inventory and journals directly; Azure hand-off reads are refused in this mode, and every hand-off step must run on-prem. The root must contain `dispatch/jobs`, even for an empty queue (a missing inventory is an error, not an empty backlog), and must be owned by the controller's service user with mode `0700`, because claims hold private queue receipts.

The on-prem runner mounts `/srv/usnm-boinc/handoff-state` read-write when present; an operator can choose the host path with `USNM_BOINC_HANDOFF_ROOT`, and ssh callers can't choose a mount. The runner checks the root the same way (canonical, not a symlink, owned by its service user, mode `0700`). At cutover, set `HANDOFF_URL=/handoff-state` in the host's `handoff.env`. The controller passes `EXPECTED_HANDOFF_URL` to every on-prem hand-off step, and the runner compares it with its own setting before launching anything; a mismatch fails without touching either backend.

For the first local cutover, run the controller on the BOINC host beside the hand-off root. Separate containers may bind the same host directory, but a filesystem shared across hosts is not covered by the locking tests, so no controller on CT 211 may keep writing. Before enabling the replacement timer, stop the old one, wait for its tick to exit, and check that no old controller or bridge writer remains.

Qualify the filesystem first, with networking disabled and against private copies: a second controller is refused while the first holds its lock; killing the lock holder lets a fresh process recover; and an isolated submitted job replays through separate CLI containers without changing its stage or bytes or creating another job.

At cutover, stop new controller starts and wait for the tick and bridge steps to finish. Copy and independently verify dispatch requests and journals, claims (with live pop receipts), spools, trust, scheduler inventory and any input objects still needed, keeping job IDs, scientific identities, stage history and queue leases. Then switch the controller and host settings together. `archive_history.py reconcile` reads only the Azure hand-off and refuses a plan with a local `handoff_root`.

## Pipeline documents

`usnm-state` can keep pipeline documents in PostgreSQL as well as Cosmos. BOINC's `--cosmos` argument and ingest's `USNM_COSMOS_ENDPOINT` accept a `postgres://` URL with the same certificate parameters as the pgmq backend; the old names keep the controller's and wrappers' call sites working.

Apply `scripts/onprem/queue/documents.sql` as the owner of the document database. The database must use `UTF8` (the schema refuses others before changing anything), because JSONB must keep the corpus's non-ASCII text; if the queue database is `SQL_ASCII`, create a separate `UTF8` database from `template0`. The schema grants BOINC and ingest read and write access and dashboard readers read-only access. Connections need a matching client certificate, TLS 1.3 and server-name verification; password URLs are refused. Each operation uses a fresh connection, bounded statement and lock timeouts and synchronous commits. Unique keys protect creation and ETag-conditional updates protect concurrent leases. There is no automatic cloud fallback.

At cutover, import each document's original container, partition key, document and ETag into `usnm_state.documents`, and keep the Cosmos snapshot read-only.

## Lake mounts

The on-prem hand-off runner accepts `CURATED_URL=/lake-state/curated`, `REFERENCE_URL=/lake-state/reference` and `OUT_URL=/lake-state/ocr-out`. Each must be an existing child directory of `/srv/usnm-boinc/lake-state`, or of the host's `USNM_BOINC_LAKE_ROOT`. The root must be canonical, owned by the runner's user and mode `0700`; a symlinked or missing store fails before Podman starts, and other container paths are refused. Each configured local store is mounted read-write at its fixed path. HTTPS stores work as before.

This only exposes staged files. Before switching, reconcile on-prem content against source checksums and transfer receipts, download only missing or changed objects, check atomic writes and crash durability on the real filesystem, and keep a single writer during the final delta.
