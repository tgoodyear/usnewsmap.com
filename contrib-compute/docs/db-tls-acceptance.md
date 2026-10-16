# Local BOINC database TLS acceptance

Run from the repository root with an existing Docker-compatible runtime:

```sh
scripts/boinc/check-db-tls.sh
```

This creates **no Azure resources or DNS records**, uses no production settings
or credentials, publishes no image, and never changes production enforcement.
The image is a test fixture, not a replacement server release or a dependency on
the closed staging-relocation PR #610.

## What actually runs

The pinned Debian Bookworm image installs MariaDB server and the same connector
families used by the BOINC runtime: Python MySQLdb/MariaDB Connector/C, PHP 8.2
mysqlnd, and C++ MariaDB Connector/C. The fixture fetches and verifies BOINC
`22bbebe08e2eaac5d5e88a6d93b88e800cb6341c` and applies the checked-in
`patch-server-db-tls.py` without substituting connection implementations.

- Python imports the actual `Boinc.db_base` and calls `do_connect`.
- PHP invokes both `_mysql_connect` and `DbConn::init_conn` in the actual includes.
- C++ compiles the actual `db/db_base.cpp` translation unit and invokes
  `DB_CONN::open` and `do_query`. Minimal feature declarations plus libbsd supply
  string compatibility; unused database methods are linker-discarded. This is
  not a full configured scheduler/daemon build.
- CLI uses the production scripts' `--ssl --ssl-verify-server-cert --ssl-ca`
  flags. The Python connector's `ssl-enforce` option file is **not** passed to
  the CLI, which does not support that option.

The database listens only on container loopback; Docker networking is disabled.
The root filesystem is read-only, capabilities are dropped, privilege escalation
is disabled, and runtime limits are 2 CPUs, 1 GiB RAM and 128 processes. Its data,
one-day synthetic private CA/key and option files live in bounded disposable
memory filesystems. No host files or sockets are mounted. Public Debian/GitHub
downloads occur only during the local image build (up to 1.5 GiB build memory).
The fixture has subprocess/readiness timeouts, and the wrapper removes only its
uniquely named container on exit. The local image/cache remains reusable.

## Matrix and evidence

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

Output is categorical JSON lines including consumer/case and successful session
cipher. Negative cases require a nonzero exit **and** connector-specific TLS
error codes/messages. Python and PHP probes emit structured errors without
tracebacks; C++/CLI diagnostics must match anchored TLS-error lines, so TLS
function names or hostnames in an authentication traceback cannot pass.
mysqlnd reports no-SSL handshakes as error 2006, "MySQL server has gone away";
this exact failure is accepted only for the independently verified
reachable/authenticated SSL-disabled peer, never for certificate tests.
unreachable DNS/TCP, missing modules or authentication-only errors are not passes.
Raw client errors, keys and passwords are not printed.

The initial real run exposed a defect hidden by connector mocks:
`mysqli_options(MYSQLI_OPT_SSL_VERIFY_SERVER_CERT, true)` returns false under
Bookworm mysqlnd, causing the old helper to refuse valid connections before the
handshake. The patch now uses
`MYSQLI_CLIENT_SSL | MYSQLI_CLIENT_SSL_VERIFY_SERVER_CERT`, with actual positive,
wrong-host/CA and plaintext rejection coverage. The lightweight PHP mock rejects
the unsupported option rather than pretending it succeeds.

CI runs this exact disposable matrix in its own bounded job when BOINC changes.
`python3 -m unittest scripts/boinc/test_db_tls_fixture.py` tests evidence
fail-closed behavior; it is not a substitute for the Docker integration run.

## Remaining release gates

This demonstrates actual **local MariaDB connector** enforcement, stock database
schema creation and verified transport. It is not Azure MySQL 8.4/rotating
Azure-chain acceptance, full project installation, public PHP/scheduler RPC or
every daemon's runtime session.
Database restart/reconnection is not a persisted BOINC project migration:
existing completed projects retain copied Python/PHP/binaries, so replacement,
hash inventory, full lifecycle and rollback still need separate acceptance.

Production `require_secure_transport` remains OFF. Keep #464 open until these
remaining gates and a reviewed migration plan are satisfied. #631 remains draft;
preserve current production pins. The paid dev/DNS proposal in #634 remains
unapproved; this fixture does not authorize it, trust calibration or node rollout.
