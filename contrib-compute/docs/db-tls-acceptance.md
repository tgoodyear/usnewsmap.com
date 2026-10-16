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

## Full project on an existing server image

With an already reviewed, pulled immutable server image:

```sh
scripts/boinc/check-full-db-tls.sh --server-image "$REVIEWED_SERVER_IMAGE_DIGEST"
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

- Stock `tools/make_project`, including database/schema installation (47 tables),
  then the real runtime config renderer and `bin/xadd` (8 registered apps).
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
- CA/hostname restoration followed by all public-page checks, healthy CGI and
  four encrypted daemon sessions again.
- A database-container restart with the same database volume, followed by a
  **fresh application container** with the persisted project. It rehydrates
  disposable upload keys and the synthetic code-signing public key, repeats the
  complete matrix, and checks unchanged hashes of the sampled Python/two PHP
  connector files and feeder binary. Server observation logs are cleared at
  database start so reused connection IDs cannot mix evidence across boots.

HTTP/CGI connections are short-lived. Their transport evidence is the
TLS-required server's authenticated `using SSL/TLS` connection log plus the
actual connector enforcement and rejection cases; the fixture does not claim
a captured per-request CGI cipher. Direct ciphers are measured for the copied
Python/PHP APIs and all four live daemons.

Application and DB containers are limited to 1 GiB/512 MiB RAM and 128/64
processes; application/DB CPU is capped at two/one. The application has a read-only
root, no privilege escalation, and private writable state/keys on uniquely
named disposable Docker volumes. Standard container capabilities needed by
Apache's identity transition remain available; neither container is privileged.
The wrapper removes only its own containers, volumes and temporary source tag.
Image/build caches remain. CI bounds the full integration step to 15 minutes.
`test_full_db_tls.py` checks evidence classification separately from execution.

All keys and credentials are synthetic. The generated code-signing **private**
key is deleted before services start. Upload keys live outside the project
volume and are linked from private runtime memory, with access limited to
root/`boincadm`. This local key-store substitute does not test Key Vault,
managed identity, `--ensure-secrets`, or the secret-fetching `--init-project`
entrypoint.

## Remaining release gates

These fixtures demonstrate actual local MariaDB connector enforcement, full
project creation, PHP/CGI operation, four daemon connections and persistent
restart. They do not demonstrate Azure MySQL 8.4/rotating Azure-chain acceptance,
a public remote dev service, authenticated client/task dispatch and returned-task
validation/assimilation, or the bridge's additional application validators.
Starting validators on an empty queue is not completed-result acceptance.

Persistent restart is not an upgrade/rollback: existing completed projects
retain copied Python/PHP/binaries. Complete file inventory, safe replacement,
backward-compatible migration and tested rollback remain necessary before
changing production enforcement. The four sampled unchanged-file hashes are
not an all-files migration inventory.

The optional OCR-node pilot additionally needs the actual supported BOINC
client build/ABI, dedicated account/subordinate IDs, effective rootless
Podman/SELinux/cgroup/startup isolation, a tiny dev cohort through collection,
and comparable OCR throughput against an agreed non-disruption rule. Local
server success does not authorize account installation or node activation, and
does not make paid Azure resources the only possible dev path.

### Existing-resource node readiness

Build the actual pinned stable client away from a production OCR node:

```sh
scripts/boinc/build-node-client-container.sh --out "$PWD/target/boinc-node-client"
```

The pinned AlmaLinux 9 builder uses the existing `build-node-client.sh`
(`ae68f3290ac82d08b4d9767acfe0daaccaeeb883`, stable 8.2.13), one make job, and
an amd64 container bounded to 1.5 GiB build memory. It exports binaries, source
commit, build image ID and relative SHA256SUMS; it never installs on a node.
Actual configure requires `zlib-devel` as well as curl/OpenSSL development
packages. CI builds the real client and retains its bundle for seven days.
An exported bundle still requires actual RHEL ABI/version verification before
any installation; a successful builder is not host/client acceptance.

Take a genuine read-only worker snapshot **before** installing or starting a
canary, and again during/after any separately approved bounded cohort:

```sh
python3 scripts/boinc/observe_node.py \
  --host claude@usnm-ocr-node4-arcs-prod.tjg.fyi \
  --window-seconds 900 --out target/node4-before.json
```

The identical checked-in observer runs through SSH stdin under read-only sudo
commands, installs no host files and never starts/stops a service. It counts
actual successful `page done` events in the complete window, not an extrapolated
gap between selected page samples. It reports pages/hour, median page duration,
last completion age, errors, engine/language/queue composition, unchanged worker
PID/start time, CPU/memory cgroup counters, host memory/load and available pressure
metrics. No page text, credentials or account keys are returned. Output refuses
to overwrite an existing evidence file.

`baseline_ready` requires an active worker, at least ten successful pages in the
window, no errors and a completion within five minutes. These are conservative
**pre-pilot readiness** checks, not calibrated regression/throughput thresholds.
They must not be used to claim that two windows have equivalent workloads or
that an after-window passed a non-disruption contract. Choose the bounded
cohort, observation windows and rollback rule before starting; preserve worker
PID, queue/engine mix and actual resource deltas in the evidence.

An actual Node 4 pre-pilot snapshot at 2026-10-16 11:22 UTC had one successful
page in 900 seconds (4/hour), last completion 891 seconds earlier, while three
existing Tesseract containers each consumed about 96% CPU for 14–15 minutes.
The production worker was still active, unchanged since October 14, with no
OOM events. No BOINC account, subordinate IDs, client or service was installed;
adding pilot load was withheld. An active process alone is not a comparable
healthy throughput baseline. Let the owner resolve the existing workload or
obtain a naturally valid fresh baseline rather than restarting/replacing the
worker, attributing the existing slowdown to BOINC, or purchasing dev resources.

Production `require_secure_transport` remains OFF. Keep #464 open until these
remaining gates and a reviewed migration plan are satisfied. #631 remains draft;
preserve current production pins. The paid dev/DNS proposal in #634 remains
unapproved; this fixture does not authorize it, trust calibration or node rollout.
