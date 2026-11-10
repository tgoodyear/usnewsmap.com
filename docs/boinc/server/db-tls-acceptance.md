# Database TLS checks

The on-prem database requires TLS (`require_secure_transport=ON`, [BOINC server on-prem](onprem.md)), and every BOINC database client in the images verifies the server through `boinc/scripts/patch-server-db-tls.py` (#464). Two CI fixtures exercise that patch against real servers.

## Connector fixture

Run from the repository root with an existing Docker-compatible runtime:

```sh
boinc/scripts/check-db-tls.sh
```

CI runs it in `ci.yml`. It uses no production settings or credentials and publishes nothing.

### What runs

The pinned Debian Bookworm image installs MariaDB server and the same connector
families used by the BOINC runtime: Python MySQLdb/MariaDB Connector/C, PHP 8.2
mysqlnd, and C++ MariaDB Connector/C. The fixture fetches and verifies BOINC
`22bbebe08e2eaac5d5e88a6d93b88e800cb6341c` and applies the checked-in
`patch-server-db-tls.py` without substituting connection implementations.

- Python imports the actual `Boinc.db_base` and calls `do_connect`.
- PHP invokes both `_mysql_connect` and `DbConn::init_conn` in the actual includes.
- C++ compiles the actual `db/db_base.cpp` translation unit and invokes
  `DB_CONN::open` and `do_query`. Minimal feature declarations plus libbsd supply
  string compatibility; unused database methods are linker-discarded. This is not
  a full configured scheduler or daemon build.
- CLI uses the production scripts' `--ssl --ssl-verify-server-cert --ssl-ca`
  flags. The Python connector's `ssl-enforce` option file is **not** passed to
  the CLI, which does not support that option.

The database listens only on container loopback; Docker networking is disabled.
The root filesystem is read-only, capabilities are dropped, privilege escalation
is disabled, and runtime limits are 2 CPUs, 1 GiB RAM and 128 processes. Its data,
one-day synthetic private CA and key, and option files live in bounded disposable
memory filesystems. No host files or sockets are mounted. Public downloads from
Debian and GitHub occur only during the local image build (up to 1.5 GiB build
memory). The fixture has subprocess and readiness timeouts, and the wrapper
removes only its uniquely named container on exit. The local image and cache
remain reusable.

### Matrix and evidence

The server starts with `require_secure_transport=ON`. Each of the five consumers
must report a real nonempty `SHOW SESSION STATUS LIKE 'Ssl_cipher'` result.
The actual `Boinc.database.create_database` routine used by `make_project` also
creates a separate synthetic BOINC schema from the pinned `schema.sql`,
`constraints.sql` and `content.sql` over verified TLS. It checks the core tables
and initial consent rows, then verifies them again after the database restarts.
Only the upstream Python build-path template is rendered; database code and SQL
are not rewritten.

1. Trusted synthetic CA and SAN `tls-db.test`: all five connect encrypted.
2. Reachable `wrong-db.test` aliases the same server but is absent from the SAN:
   all five fail with TLS-related diagnostics.
3. Replace only the client CA with an unrelated CA: all five reject; restoring
   the correct CA must recover all five encrypted connections.
4. A deliberately unencrypted CLI connection must fail against the TLS-required
   server.
5. Restart the same disposable database with SSL disabled. Prove the credentials
   and TCP listener work using an unencrypted control connection; all five
   patched consumers must reject this reachable plaintext peer.
6. Restart that database with TLS required again: all five reconnect encrypted.

JSON lines identify the consumer, case and successful session cipher. Negative
cases require a nonzero exit **and** connector-specific TLS errors. Python and PHP
emit structured errors without tracebacks; C++ and CLI require anchored TLS-error
lines. A TLS name in an authentication traceback cannot pass. mysqlnd error 2006,
"MySQL server has gone away", counts only for the independently verified
reachable, authenticated SSL-disabled peer, never certificate tests.
DNS, TCP, missing-module and authentication-only failures do not pass.
Raw errors, keys and passwords are not printed.

The integration run caught a defect missed by mocks:
`mysqli_options(MYSQLI_OPT_SSL_VERIFY_SERVER_CERT, true)` returns false under
Bookworm mysqlnd, rejecting valid connections before the handshake. The patch uses
`MYSQLI_CLIENT_SSL | MYSQLI_CLIENT_SSL_VERIFY_SERVER_CERT`; tests cover trusted
TLS, wrong hosts, wrong CAs and plaintext rejection. The PHP mock also rejects
the unsupported option.

CI runs this exact disposable matrix in its own bounded job when BOINC changes.
`python3 -m unittest boinc/scripts/test_db_tls_fixture.py` tests evidence
fail-closed behavior; it is not a substitute for the Docker integration run.

The checked-in patch stays image-specific because it hardcodes our CA and MariaDB
Connector/C API. Offer the fail-closed connection behavior and connector tests
upstream once generalized to configurable CA paths and supported client libraries;
don't propose this deployment's CA paths as an upstream default.

### MySQL 8.4 connector parity

```sh
boinc/scripts/check-mysql84-db-tls.sh
```

This separate CI job reuses the same pinned MySQL 8.4 image as the existing SQL
query tests, plus the actual patched Bookworm BOINC consumers above. No production
source patch or replacement connector is substituted. It requires five nonempty
session ciphers for a trusted synthetic CA, five TLS-related hostname rejections,
five wrong-CA rejections, five encrypted recoveries, and five encrypted sessions
after restarting the same persisted database. An unpatched plaintext control must
fail with MySQL error **3159**, not an unrelated authentication or network error.
The observer checks `VERSION()` is 8.4 and `@@require_secure_transport` is 1 before
each application phase. The normal `caching_sha2_password` runtime account also
checks connector compatibility with MySQL's default authentication plugin.

The matrix phase then loads BOINC's `schema.sql` and `constraints.sql` and runs
`qwen-tiebreak-policy.py settle --apply` (#1477) on three synthetic disputed
`usnm_qwen` units. One unit's workunit row is locked by another session. Another
unit's unsent tie-breaker has been through the scheduler's exact `mark_as_sent`
`UPDATE` in an open transaction, which commits while the tool waits for the row.
The check requires one unit settled, one skipped as locked and one skipped with
its replica running. A second run must settle the locked unit. The scheduler's
`mark_as_sent` must then match no row for a cancelled replica.

Both database and client containers are network-disabled (clients share the
DB's loopback namespace), with no published ports, host files or Docker socket
mounts. The server runs as fixture-only root without capabilities or privilege
escalation on a read-only root, with one CPU, 768 MiB and 128 processes. Clients
are bounded to one CPU, 256 MiB and 32 processes. Unique disposable volumes hold
only synthetic keys and the test database and are removed by the exit trap;
image/build caches remain. Database initialization bypasses the official
entrypoint so no random admin password is emitted. Root has no TCP grant; the
observer uses the same known-good synthetic runtime account as the consumers.
The TLS-disabled-peer rejection remains covered by the MariaDB matrix.

This is not a full MySQL project lifecycle or a copied-project migration test;
the [full-project fixture](#full-project-fixture) covers those against MariaDB.

`requireSecureTransport` in the BOINC root template and MySQL module (the Azure
server's database) defaults to **false**. Two activations on the Azure server in
October 2026 were rolled back, and #464 was closed with Azure left OFF; the
on-prem database has required TLS since 21 October 2026. Turning it on for an
Azure server again needs a reviewed configuration change through #529 and public
Apache/mysqlnd sessions observed with non-empty ciphers, not CLI checks alone.

## Full-project fixture

With an already reviewed, pulled immutable server image:

```sh
boinc/scripts/check-full-db-tls.sh --server-image "$REVIEWED_SERVER_IMAGE_DIGEST"
```

The argument must include `@sha256:` and the complete digest. CI instead uses
`--local-server-image usnm-boinc-server:check` immediately after building the
actual server. This resolves the local image ID and pins a unique temporary tag
to that ID; it does not pull a mutable tag or publish another release.

The full fixture derives from that image without rebuilding or rewriting its
BOINC implementation. A separate native-architecture MariaDB fixture shares its
**network-disabled** namespace with the amd64 application container. This also
works under an existing ARM Docker runtime with amd64 emulation: emulating
InnoDB itself exhausted the local memory limit, whereas the native DB succeeds.
No ports, host directories or Docker sockets are mounted into either container.

It actually executes:

- Stock `tools/make_project`, including database and schema installation (47
  tables), then the real runtime config renderer and `bin/xadd` (every app in
  `project.xml`).
- Separate runtime credentials with only SELECT/INSERT/UPDATE/DELETE on the
  project schema; the fixture-only admin observes sessions separately.
- The image's non-destructive `--ensure-project` branch and `project_tree`
  refresh, including its real scheduler-refresh helper and project PHP pages.
- Apache/mod_php `get_project_config.php`, `server_status.php` and `index.php`,
  and an actual scheduler CGI POST. A healthy scheduler response must not be
  `project_is_down`; XML error replies do not count as positive acceptance.
  The synthetic request is unauthenticated and must dispatch no work.
- Actual feeder, transitioner, trivial validator and batch assimilator as
  `boincadm`. Each live DB session must have a nonempty server-side
  `performance_schema.status_by_thread` `Ssl_cipher`, correlated to its
  daemon-specific queries. All four distinct roles must be present.
- The Qwen tie-breaker stop (`boinc/scripts/qwen-tiebreak-policy.py`, #1477) on
  synthetic disputed `usnm_qwen` units, with the runtime credentials: `plan`,
  then `settle --apply` while a second session holds one unit's row lock, then
  `settle --apply` again. The locked unit must be skipped unchanged and settled
  by the second run; units outside the policy or with a replica running must be
  untouched. The live transitioner must then mark the settled units'
  inconclusive returns NO_CHECK and make them ready to assimilate.
- Both PHP APIs and Python from the **copied project**, not only the source
  tree, reporting actual session ciphers.
- Wrong hostname and unrelated client CA: all three copied consumers and four
  fresh daemon processes must reject with connector-specific TLS errors;
  actual CGI must report a database failure and no work. A previously trusted
  feeder stays alive while CGI reconnects: otherwise stock CGI short-circuits
  with "feeder not running", which is not TLS evidence. An attempted duplicate
  feeder can disturb shared-memory readiness, so CGI is checked first.
- A plaintext CLI connection with the same known-good runtime credentials must
  fail while the observer confirms `require_secure_transport=ON`.
- CA and hostname restoration followed by all public-page checks, healthy CGI
  and four encrypted daemon sessions again.
- A database-container restart with the same database volume, followed by a
  **fresh application container** with the persisted project. It rehydrates
  disposable upload keys and the synthetic code-signing public key, repeats the
  complete matrix, and checks unchanged hashes of the sampled Python connector
  file, the two PHP connector files and the feeder binary. Server observation
  logs are cleared at database start so reused connection IDs cannot mix
  evidence across boots.

HTTP and CGI connections are short-lived. Their transport evidence is the
TLS-required server's authenticated `using SSL/TLS` connection log plus the
actual connector enforcement and rejection cases; the fixture does not claim
a captured per-request CGI cipher. Direct ciphers are measured for the copied
Python and PHP APIs and all four live daemons.

The application container is limited to 1 GiB of RAM, 128 processes and two CPUs;
the DB container to 512 MiB, 64 processes and one CPU. The application has a
read-only root, no privilege escalation, and private writable state and keys on
uniquely named disposable Docker volumes. Standard container capabilities needed by
Apache's identity transition remain available; neither container is privileged. The
wrapper removes only its own containers, volumes and temporary source tag. Image
and build caches remain. CI bounds the full integration step to 15 minutes.
`test_full_db_tls.py` checks evidence classification separately from execution.

All keys and credentials are synthetic. The generated code-signing **private**
key is deleted before services start. Upload keys live outside the project
volume and are linked from private runtime memory, with access limited to root
and `boincadm`. This local key-store substitute does not test Key Vault,
managed identity, `--ensure-secrets`, or the secret-fetching `--init-project`
entrypoint.

## Project code refresh

`/usr/local/lib/usnm-boinc/refresh-project-code.py` inventories every installed
file in `bin`, `cgi-bin`, `py`, and `html/inc` before writing anything. It maps
installed files to the immutable image source, rejects unknown/ambiguous files
and escaping links, preserves generated project paths and validator links, and
backs up changed code with hashes, modes and ownership. It does not run upstream
`tools/upgrade` (which also invokes schema upgrade), touch the database, copy
credentials, or restore queues, applications, uploads or scientific state.

Run it in the bridge image (which carries both the helper and the validator
link targets) with the server stopped and no step running, as the on-prem
cutover did ([the cutover note](../../notes/2026-10-21-boinc-server-onprem-cutover.md)):

```sh
python3 /usr/local/lib/usnm-boinc/refresh-project-code.py inventory --project /project/usnmocr
python3 /usr/local/lib/usnm-boinc/refresh-project-code.py apply --project /project/usnmocr --backup /project/tls-code-backup-UNIQUE --ack-server-stopped
```

`--ack-server-stopped` records that the operator checked; it doesn't check.
A failed write rolls back the code it already changed; reconcile a failed
rollback before restarting. To undo an applied refresh, stop the server and run
the helper with `rollback` and the same project and backup arguments. The helper
refuses concurrent code drift or corrupt backup hashes.

The full server-image CI fixture now copies its synthetic project, marks a copied
Python module and transitioner binary stale, applies the helper, verifies all
mapped hashes plus real copied Python/PHP, web/CGI and four daemon TLS sessions,
then rolls back, verifies every original code hash and unchanged configuration,
and repeats those real consumers. This is a rehearsal on a synthetic copy, not
a schema migration; rehearse on a copy of the real project before applying it live.
