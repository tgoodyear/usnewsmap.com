# Local BOINC database TLS acceptance

Run from the repository root with an existing Docker-compatible runtime:

```sh
scripts/boinc/check-db-tls.sh
```

This local fixture creates **no Azure resources or DNS records**, uses no
production settings or credentials, publishes nothing and changes no production
enforcement. It neither replaces the server release nor depends on closed #610.

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
`python3 -m unittest scripts/boinc/test_db_tls_fixture.py` tests evidence
fail-closed behavior; it is not a substitute for the Docker integration run.

The checked-in patch stays image-specific because it hardcodes our CA and MariaDB
Connector/C API. Offer the fail-closed connection behavior and connector tests
upstream once generalized to configurable CA paths and supported client libraries;
do not submit Azure-specific paths as an upstream BOINC default.

## MySQL 8.4 connector parity

```sh
scripts/boinc/check-mysql84-db-tls.sh
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

This is **not** the Azure MySQL certificate chain, a full MySQL project lifecycle,
or a copied-project migration/rollback test. `requireSecureTransport` in the
BOINC root template and MySQL module deliberately defaults to **false**; `true`
selects ON only after the remaining gates below. The normal provisioner forwards
the committed environment setting of the same name, defaulting to false when
absent; `env/prod.json` explicitly remains false. Changing it to true requires a
separate reviewed activation PR after all acceptance and rollback gates pass.
Any future activation needs a reviewed, persisted configuration path and the
#529 lock/claim protocol, followed by the normal main-triggered release workflow
and live daemon/scheduler/PHP/bridge and TLS-policy checks. Do not manually toggle
production or dispatch a branch release to bypass those requirements.

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

- Stock `tools/make_project`, including database and schema installation (47
  tables), then the real runtime config renderer and `bin/xadd` (8 registered
  apps).
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

## Remaining release gates

The fixtures verify local MariaDB TLS, project creation, PHP, CGI, four daemons
and persistent restart, plus MySQL 8.4 connector parity. Remaining gates are the
actual Azure MySQL 8.4 rotating certificate chain and full-project acceptance, a public remote dev service, authenticated task
dispatch through validation and assimilation, and the bridge's extra validators.
Starting an empty-queue validator does not test returned results.

Restart does not test upgrade or rollback. Existing projects retain copied
Python, PHP and binaries; changing production enforcement requires a complete
file inventory, safe replacement, compatible migration and tested rollback.
Four sampled hashes cannot establish that inventory.

The optional OCR-node pilot also requires a supported client build and host ABI,
dedicated account and subordinate IDs, rootless Podman, SELinux, cgroup and
startup isolation, a tiny dev cohort through collection, and comparable OCR
throughput under an agreed non-disruption rule. Local success authorizes neither
node activation nor account installation; paid Azure resources are not required
for every dev approach.

### Existing-resource node readiness

Build the actual pinned stable client away from a production OCR node:

```sh
scripts/boinc/build-node-client-container.sh --out "$PWD/target/boinc-node-client"
```

The pinned AlmaLinux 9 builder uses the existing `build-node-client.sh`
(`ae68f3290ac82d08b4d9767acfe0daaccaeeb883`, stable 8.2.13), one make job, and an
amd64 compilation container capped at one CPU, 1.5 GiB memory with no extra swap,
and 256 tasks. The dependency-image build is separate; a Docker BuildKit
`build --memory` flag is not relied upon. The compiler container checks actual
cgroup limits and exports those readings alongside binaries, source commit,
dependency image ID and relative SHA256SUMS; it never installs on a node. Actual
configure requires `zlib-devel` as well as curl and OpenSSL development packages.
CI builds the real client and retains a tar archive for seven days, preserving
executable modes. It re-extracts that archive, verifies executable bits,
checksums and actual cgroup limits, and runs both loaders plus `boinc --version`
inside pinned EL10 with no network. This native CI check passed for stable
8.2.13; local ARM emulation instead fails at EL10 glibc's x86-64-v3 requirement
before BOINC starts. An exported bundle still requires actual verification of the
RHEL host's ABI and version before installation; successful CI is not host or
client acceptance.

Take a genuine read-only worker snapshot **before** installing or starting a
canary, and again during and after any separately approved bounded cohort:

```sh
python3 scripts/boinc/observe_node.py \
  --host claude@usnm-ocr-node4-arcs-prod.tjg.fyi \
  --window-seconds 900 --out target/node4-before.json
```

The identical checked-in observer runs through SSH stdin under read-only sudo
commands, installs no host files and never starts or stops a service. It counts
actual successful `page done` events in the complete window, not an extrapolated
gap between selected page samples. It reports pages/hour, median page duration,
last completion age, errors, engine, language and queue composition, unchanged
worker PID and start time, CPU and memory cgroup counters, host memory and load,
and available pressure metrics. No page text, credentials or account keys are
returned. Output refuses to overwrite an existing evidence file.

`baseline_ready` requires an active worker, at least ten successful pages in the
window, no errors and a completion within five minutes. These are conservative
**pre-pilot readiness** checks, not calibrated regression or throughput
thresholds. They must not be used to claim that two windows have equivalent
workloads or that an after-window passed a non-disruption contract. Choose the
bounded cohort, observation windows and rollback rule before starting; preserve
worker PID, queue and engine mix and actual resource deltas in the evidence.

An actual Node 4 pre-pilot snapshot at 2026-10-16 11:22 UTC had one successful
page in 900 seconds (4/hour), last completion 891 seconds earlier, while three
existing Tesseract containers each consumed about 96% CPU for 14–15 minutes. The
production worker was still active, unchanged since October 14, with no OOM
events. No BOINC account, subordinate IDs, client or service was installed;
adding pilot load was withheld. An active process alone is not a comparable
healthy throughput baseline. Let the owner resolve the existing workload or
obtain a naturally valid fresh baseline rather than restarting or replacing the
worker, attributing the existing slowdown to BOINC, or purchasing dev resources.
At 11:44:50 UTC it naturally recovered: 23 successful pages in 900 seconds
(92/hour), median 115.50 seconds, zero failure events and last completion 45.8
seconds earlier, with the same worker PID and start time. This meets readiness,
not the no-regression acceptance of the still-unexecuted during-pilot and
after-pilot windows.

### Disabled existing-node client prerequisites

`scripts/boinc/prepare_node.py` is a separate explicit opt-in for the user-approved
existing-node pilot. It does not install packages, adopt existing accounts, start
or enable a service, create a project attachment, or modify worker or timer units.
It refuses an existing `boinc` account, home, install or unit, verifies both
exported binary hashes and the stable source commit, requires a ready baseline no
more than 120 seconds old with the same live worker PID, then previews the exact
plan:

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
helper also leaves the client stopped and disabled. Neither step authorizes an
attachment by itself. Production attachment requires explicit account and master
authorization, a fresh healthy baseline and the verified capped profile; do not
infer consent or enable unreviewed apps or GPU work.

Rollback an explicitly started pilot with:

```sh
sudo python3 scripts/boinc/prepare_node.py --stop
```

This stops and disables only the managed canary client and its dedicated user
manager, disables lingering, and retains the account, files and task evidence
for inspection. It does not delete subordinate-ID allocations or kill or
restart OCR. If preparation fails before the unit is written, no service was
started; inspect the retained identity and files instead of retrying over an
existing account. The source and tests alone are not evidence that this host
setup or a returned cohort has executed.

Production `require_secure_transport` remains OFF. Keep #464 open until these
remaining gates and a reviewed migration plan are satisfied. #631 remains draft;
preserve current production pins. The paid dev and DNS proposal in #634 remains
unapproved; this fixture does not authorize it, trust calibration or node rollout.

## Private managed-client account attachment

`scripts/boinc/client_account.py` uses the stock BOINC GUI-RPC protocol on loopback.
Preview with `--data-dir`, `--port`, `--expected-user-id`, and
`--expected-user-name`; it prints only whitelisted attribution and status fields,
not the account name, authenticators, or raw RPC replies. Explicit
`--apply --key-file` accepts only a regular, nonsymlink, private weak-key file
matching the expected user ID. Never pass the key itself as an argument. The
canonical production URL is `https://boinc.usnewsmap.com/`; legacy or duplicate
attachments require reconciliation instead of silently creating a second project.

Credential-bearing debug flags are checked against the latest retained **effective
runtime** core log-flag dump as well as the on-disk configuration; missing or
potentially truncated runtime evidence refuses attachment. Stock startup and
supported config reread emit this dump. Project messages cannot substitute for
core runtime evidence. The versioned private drain journal is bound to this
machine, real data path, RPC port, and GUI-auth-file identity; legacy or
cross-client journals are refused. A successful late-attribution retry releases
only the validated helper-owned hold and removes its journal without detaching
again. Network policy is restored after the **last** attach or update RPC,
including initially unattached clients. Every apply holds a nonblocking exclusive
lock in the real client data directory from before RPC authentication through
final network restoration and journal cleanup. The lock is independent of key and
journal paths; its inode is never removed. Concurrent operators must retry instead
of racing detach against a new attachment.

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
restores the previous permanent or temporary network mode even on errors. Other
projects' transfers may pause briefly during this guard; their work is not reset.
The supplied key is used only in the private RPC payload. Attribution waits a
bounded time for the actual scheduler's expected user ID and name and nonzero host
ID.

Stock `project_attach` re-enables networking and cannot atomically preserve a
pre-existing user no-new-tasks hold. Therefore user-held and account-manager
projects are refused **without changing them**, rather than claiming a safe hold
that the client cannot provide. A helper-owned drain hold is distinguished by
its journal and resumes the original work-enabled policy after rejoining.
An independent GUI project suspension is also refused, including on reinvocation;
`disallow_attach` is checked before journaling or any project mutation so a valid
attachment is never detached from a client that prohibits reattachment.

Before attachment, capture effective global resource preferences and protect them
using the supported local-override RPC, preventing a new account's scheduler
reply from silently changing CPU, memory and other limits. The original override
and protected effective preferences are backed up privately beside the journal;
the explicit local resource policy remains installed and should be inspected
before subsequently changing account-wide preferences. Weekly CPU and network
schedules are materialized for all seven days so a later scheduler reply cannot
introduce an inherited day outside the protected limits. Unrestricted equal
endpoints become explicit 00:00–24:00 intervals; 24:00–00:00 remains never.
Stored and working schedules are compared semantically and must retain every
explicit day. Schedules that cannot round-trip the pinned client's two-decimal
weekly serialization are refused rather than silently rounded. For the stock
client's idle CPU-count percentage only, working value zero and parsed override
value 100 are equivalent (both leave the CPU count unlimited). Readback accepts
that documented representation change, not arbitrary limit changes. Weak-key
shape checks reject both 31- and 33-hex suffixes; valid shape alone cannot detect
a transcribed but different credential. Historical server records are not
rewritten. A standard detach can remove unused project downloads; inventory
caches and obtain explicit rejoin authorization first. This does not provision
resources, activate the optional OCR-node profile, grant GPU consent, or change
server pins or TLS policy.

For a dedicated, otherwise empty OCR-node client whose throughput comparison
remains uncertain, preserve its original `boinccmd --get_cc_status` first. Use
the installed supported `boinccmd --set_run_mode never` and
`--set_gpu_mode never` **before** attachment, using its data directory and
loopback RPC port (no credential arguments). After verified attachment,
immediately request project `nomorework` and `suspend`. The client stays
responsive and credit attribution can be checked without executing any new
BOINC tasks. Stock attach cannot atomically prevent an initial assignment;
report and preserve any queued results rather than claiming none were assigned.
Resume only this owned project and restore the recorded computation modes when
the bounded workload, CPU and memory comparison permits real work. Do not apply
this empty-client operational hold blindly to a client running other projects.

## Production drift reconciliation (2026-10-18, #464)

The source of the live lifecycle server/bridge (`9323a0d2…` / `b8fb643e…`)
was `09987e4732d631292031a4b82b82be8a194b72a7`, outside main. Its quality
metrics are now reconciled into main without importing unrelated resource-limit,
threading or alignment changes;
this does not adopt the separately proposed damage router (#777).
The additive web image `508a9256…` adds only the already-merged
`contrib-compute/boinc/html/user/work_summary.php`. Optional `images.web`
preserves this web/daemon binding independently of lifecycle `server`/`bridge`.
A normal image build replaces an existing web override with the new bridge image,
which includes that endpoint and the reconciled metrics. Existing main bundle and
upload limits, linear-space alignment and single-threaded execution are unchanged. Thus the override cannot
silently strand production on the old additive image after a rebuild.

The existing production bridge default is `BRIDGE_STEP=iris-bundle-collect`,
`IRIS_CAMPAIGN=overproof-100-v12`; root Bicep preserves these values and the actual
200-GiB project-share quota. Signed app-version and worker pins are not changed.
The independently operated quality job is **not managed by this root stack**.
Its observed configuration is 2 CPU / 4 GiB, 86400-second timeout, zero retries,
manual parallelism/completion count 1, live lifecycle bridge image, and the
managed-identity-fetched `quality/v1-wave9/run_wave9.py` runner. Its runtime shard
settings are NUM_SHARDS=8, BUNDLE_SIZE=200, MAX_IN_FLIGHT_BUNDLES=45,
MIN_FREE_GB=35.0. Its storage credentials remain private, never committed here.
Provisioning this stack neither starts nor adopts that job. An active quality
execution and its template must remain unchanged; record its deferred image
update in #529 and reconcile it separately after that execution ends naturally.

Per maintainer policy #856, do not impose a global all-idle gate for a scoped
server/TLS rollout. Take the #529 claim and controller lease; take the per-job
start lease and verify that job has no nonterminal execution before updating its
template. Defer only busy job templates, not the entire rollout. Complete copied
project backup/migration/rollback and every-consumer TLS gates still apply;
stock tools/upgrade also invokes database upgrade, so never run it blindly on
production or mistake an image rollout for migration of copied files.

Normal provisioning registers applications with an execution-only override of the
complete bridge template, protected by the existing per-job start lease. Only that
execution receives `BRIDGE_STEP=register-apps`; persisted Iris defaults, secret
references, resources, and project mounts remain unchanged. Legacy quality results
that lack the added image measurements export null, not fabricated numeric zero.

## Mixed-schema collector rollout gate

**Do not deploy the reconciled collector until its assessment-database consumer
supports mixed old and new campaign schemas.** The additional page metrics add
Parquet columns. `read_parquet(..., union_by_name=true)` within each campaign does
not make a positional `UNION ALL` across campaigns compatible: missing columns
must be filled with null by a name-based union across campaign queries as well.

The compatibility change and regression belong to the assessment-database owner
in PR #777, not this reconciliation. Before collector rollout, require a merged
fix for both page and crop campaign unions in
`scripts/build-quality-assessment-db.py` and a passing regression consolidating
old and new campaign files, with the older campaign's new measurements null.
Record the merged commit and test evidence in the #464/#529 rollout receipt.
Image builds may proceed, but neither the collector nor TLS activation may be
released while this compatibility gate is unmet. This is a code compatibility
gate, not a global all-idle requirement; #856 resource-specific deferrals still
apply to all later production operations.

## Code-only persisted-project migration and rollback

`/usr/local/lib/usnm-boinc/refresh-project-code.py` inventories every installed
file in `bin`, `cgi-bin`, `py`, and `html/inc` before writing anything. It maps
installed files to the immutable image source, rejects unknown/ambiguous files
and escaping links, preserves generated project paths and validator links, and
backs up changed code with hashes, modes and ownership. It does not run upstream
`tools/upgrade` (which also invokes schema upgrade), touch the database, copy
credentials, or restore queues, applications, uploads or scientific state.

Under the #529 claim and documented controller lease, stop only the server app
revisions during the short shared-code replacement (never stop a job execution).
Keep every active job template unchanged. Through a complete-template,
execution-only lifecycle-job override using the reviewed server image, run:

```sh
python3 /usr/local/lib/usnm-boinc/refresh-project-code.py inventory --project /project/usnmocr
python3 /usr/local/lib/usnm-boinc/refresh-project-code.py apply --project /project/usnmocr --backup /project/tls-code-backup-UNIQUE --ack-server-stopped
```

The acknowledgement records an operator prerequisite, not automatic proof that
other containers stopped. Verify all server revisions are inactive before it.
Never restore an entire project/database while jobs are active. A failed write
attempt rolls back already changed code; reconcile any failed rollback before
restarting. For a failed acceptance, first persistently restore TLS enforcement
OFF if it was enabled, stop server revisions, then run the same reviewed helper
with `rollback` and the same project/backup arguments and restore prior app/job
images (excluding busy job templates). The helper refuses concurrent code drift
or corrupt backup hashes. Start the prior app revision and verify baseline.

The full server-image CI fixture now copies its synthetic project, marks a copied
Python module and transitioner binary stale, applies the helper, verifies all
mapped hashes plus real copied Python/PHP, web/CGI and four daemon TLS sessions,
then rolls back, verifies every original code hash and unchanged configuration,
and repeats those real consumers. This is a copied-project code rehearsal,
not a production rehearsal, production schema migration, or proof of Azure Files
rename semantics; repeat safely on a production project copy before live apply.
