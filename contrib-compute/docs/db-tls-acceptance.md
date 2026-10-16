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
an amd64 compilation container capped at one CPU, 1.5 GiB memory with no extra
swap, and 256 tasks. The dependency-image build is separate; a Docker BuildKit
`build --memory` flag is not relied upon. The compiler container checks actual
cgroup limits and exports those readings alongside binaries, source commit,
dependency image ID and relative SHA256SUMS; it never installs on a node.
Actual configure requires `zlib-devel` as well as curl/OpenSSL development
packages. CI builds the real client and retains a tar archive for seven days,
preserving executable modes. It re-extracts that archive, verifies executable
bits/checksums/actual cgroup limits, and runs both loaders plus `boinc --version`
inside pinned EL10 with no network. This native CI check passed for stable
8.2.13; local ARM emulation instead fails at EL10 glibc's x86-64-v3 requirement
before BOINC starts. An exported bundle still requires actual RHEL-host
ABI/version verification before installation; successful CI is not host/client
acceptance.

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
At 11:44:50 UTC it naturally recovered: 23 successful pages in 900 seconds
(92/hour), median 115.50 seconds, zero failure events and last completion
45.8 seconds earlier, with the same worker PID/start. This meets readiness,
not the still-unexecuted during/after pilot's no-regression acceptance.

### Disabled existing-node client prerequisites

`scripts/boinc/prepare_node.py` is a separate explicit opt-in for the user-approved
existing-node pilot. It does not install packages, adopt existing accounts, start
or enable a service, create a project attachment, or modify worker/timer units.
It refuses an existing `boinc` account/home/install/unit, verifies both exported
binary hashes and the stable source commit, requires a ready baseline no more
than 120 seconds old with the same live worker PID, then previews the exact plan:

```sh
sudo python3 scripts/boinc/prepare_node.py \
  --bundle /path/to/extracted/boinc-node-client \
  --baseline /path/to/fresh-node4-before.json
```

Only an explicit `--apply` creates the locked, nologin `boinc` identity, fresh
non-overlapping 65536-entry subuid/subgid ranges, owned home, client binaries,
and disabled `usnm-boinc-canary.service`. It executes `--version` as that actual
unprivileged host UID, then verifies the worker PID stayed unchanged. The base
unit cannot start until the reviewed volunteer-profile drop-in exists. Apply
`setup_volunteer.py --ocr-node --service usnm-boinc-canary.service` separately
to configure and verify whole-UID limits, SELinux and rootless runtime; that
helper also leaves the client stopped/disabled. Neither step authorizes an
attachment by itself. Production attachment requires explicit account/master
authorization, a fresh healthy baseline and the verified capped profile; do
not infer consent or enable unreviewed apps/GPU work.

Rollback an explicitly started pilot with:

```sh
sudo python3 scripts/boinc/prepare_node.py --stop
```

This stops/disables only the managed canary client and its dedicated user
manager, disables lingering, and retains account/files/task evidence for
inspection. It does not delete subordinate-ID allocations or kill/restart OCR.
If preparation fails before the unit is written, no service was started;
inspect retained identity/files instead of retrying over an existing account.
The source/tests alone are not evidence that this host setup or a returned
cohort has executed.

Production `require_secure_transport` remains OFF. Keep #464 open until these
remaining gates and a reviewed migration plan are satisfied. #631 remains draft;
preserve current production pins. The paid dev/DNS proposal in #634 remains
unapproved; this fixture does not authorize it, trust calibration or node rollout.

## Private managed-client account attachment

`scripts/boinc/client_account.py` uses the stock BOINC GUI-RPC protocol on loopback.
Preview with `--data-dir`, `--port`, `--expected-user-id`, and
`--expected-user-name`; it prints only whitelisted attribution/status fields, not
the account name, authenticators, or raw RPC replies. Explicit `--apply --key-file`
accepts only a regular, nonsymlink, private weak-key file matching the expected
user ID. Never pass the key itself as an argument. The canonical production URL is
`https://boinc.usnewsmap.com/`; legacy/duplicate attachments require reconciliation
instead of silently creating a second project.

Credential-bearing debug flags are checked against the latest retained
**effective runtime** core log-flag dump as well as the on-disk configuration;
missing or potentially truncated runtime evidence refuses attachment. Stock
startup and supported config reread emit this dump. Project messages cannot
substitute for core runtime evidence. The versioned private drain journal is
bound to this machine, real data path, RPC port, and GUI-auth-file identity;
legacy/cross-client journals are refused. A successful late-attribution retry
releases only the validated helper-owned hold and removes its journal without
detaching again. Network policy is restored after the **last** attach/update RPC,
including initially unattached clients.

Stock BOINC 8.2 GUI-RPC authentication requires `MD5(nonce + GUI password)`.
This is a transient, loopback-only protocol challenge, not a stored password
hash. CodeQL's password-hashing warning requires an explicit audited decision
on this mandatory interoperability line; inline LGTM comments are not honored
by GitHub code scanning. This does not claim MD5 is suitable for password storage
or permit remote, plaintext GUI-RPC exposure.

The supported client rejects attaching an already attached URL. For an explicitly
authorized managed-client rejoin, the helper records the original work-enabled
preference in a private durable journal before requesting no new tasks. It refuses
to detach while any project result or scheduler RPC remains; reinvoke after
draining. A temporary client-wide network guard closes the final detach race and
restores the previous permanent/temporary network mode even on errors. Other
projects' transfers may pause briefly during this guard; their work is not reset.
The supplied key is used only in the private RPC payload. Attribution waits a
bounded time for the actual scheduler's expected user/name and nonzero host ID.

Stock `project_attach` re-enables networking and cannot atomically preserve a
pre-existing user no-new-tasks hold. Therefore user-held and account-manager
projects are refused **without changing them**, rather than claiming a safe hold
that the client cannot provide. A helper-owned drain hold is distinguished by
its journal and resumes the original work-enabled policy after rejoining.
An independent GUI project suspension is also refused, including on reinvocation;
`disallow_attach` is checked before journaling or any project mutation so a valid
attachment is never detached from a client that prohibits reattachment.

Before attachment, capture effective global resource preferences and protect
them using the supported local-override RPC, preventing a new account's scheduler
reply from silently changing CPU/memory and other limits. The original override
and protected effective preferences are backed up privately beside the journal;
the explicit local resource policy remains installed and should be inspected
before subsequently changing account-wide preferences. Historical server records
are not rewritten. A standard detach can remove unused project downloads;
inventory caches and obtain explicit rejoin authorization first. This does not
provision resources, activate the optional OCR-node profile, grant GPU consent,
or change server pins/TLS policy.

For a dedicated, otherwise empty OCR-node client whose throughput comparison
remains uncertain, preserve its original `boinccmd --get_cc_status` first.
Use the installed supported `boinccmd --set_run_mode never` and
`--set_gpu_mode never` **before** attachment, using its data directory and
loopback RPC port (no credential arguments). After verified attachment,
immediately request project `nomorework` and `suspend`. The client stays
responsive and credit attribution can be checked without executing any new
BOINC tasks. Stock attach cannot atomically prevent an initial assignment;
report and preserve any queued results rather than claiming none were assigned.
Resume only this owned project and restore the recorded computation modes when
the bounded workload/CPU/memory comparison permits real work. Do not apply this
empty-client operational hold blindly to a client running other projects.
