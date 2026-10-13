# BOINC facts, checked against sources

Every fact here has a source and a fetch date. All sources were read on 2026-10-13 (US Eastern).
A fact marked UNVERIFIED was not confirmed from a source and must be tested before design
depends on it. A fact marked CORRECTED differs from what the request assumed.

Source snapshots:

- BOINC wiki, cloned from `https://github.com/BOINC/boinc.wiki.git`, commit `7e16379`
  (2026-10-10). Pages are cited as `wiki:<Page>`, meaning
  `https://github.com/BOINC/boinc/wiki/<Page>`.
- BOINC source, `https://github.com/BOINC/boinc`, master at commit `0f611c6` (2026-10-12).
  Files are cited as `src:<path>`, meaning `https://github.com/BOINC/boinc/blob/master/<path>`.
- boinc-server-docker, `https://github.com/BOINC/boinc-server-docker`, cited as `bsd:<path>`.
- Latest server release tag: `server_release/1.6/1.6.2`, 2026-06-12
  (`https://api.github.com/repos/BOINC/boinc/releases/latest`; also `wiki:Server-release-notes`).

Summary of the ten claims:

| Claim                                           | Result                                                                                 |
| ----------------------------------------------- | -------------------------------------------------------------------------------------- |
| (a) No central service, you run your own server | CONFIRMED                                                                              |
| (b) Server stack and daemons                    | CONFIRMED, with four CORRECTIONS below (shared memory, shared filesystem, MyISAM, TLS) |
| (c) Client auth is a per-project account key    | CONFIRMED, one detail added (weak key)                                                 |
| (d) App types                                   | CONFIRMED, with limits on Docker (client 9.0+, WSL on Windows)                         |
| (e) Workunit sizing guidance                    | CONFIRMED in outline; numbers for OCR are ours and UNVERIFIED                          |
| (f) Validation                                  | CONFIRMED; CORRECTED on what a script validator can see                                |
| (g) Credit, leaderboards, teams                 | CONFIRMED                                                                              |
| (h) File serving and signing                    | CONFIRMED for download; CORRECTED for upload (cannot be blob storage)                  |
| (i) Server container images                     | CONFIRMED, but the published images are stale                                          |
| (j) Official list                               | CONFIRMED optional; attach by URL CONFIRMED                                            |

## (a) There is no central service

- CONFIRMED. A project is a server you run. The cookbook builds a project with
  `make_project`, which creates the DB, the project directory, the Apache config and the
  daemon config. Source: `wiki:Create-a-BOINC-server-(cookbook)`, `wiki:The-make_project-script`,
  `wiki:ServerIntro`.
- The client learns of a project from its master URL. Source: `wiki:Master-URL`.
- Account managers (BOINC Manager's "Add project" list, Science United) are optional indexes,
  not hosts for the project. See (j).

## (b) Server stack and daemons

Components, all CONFIRMED from `wiki:ServerIntro`, `wiki:Daemons`, `wiki:ProjectDaemons`,
`wiki:Create-a-BOINC-server-(cookbook)`:

| Component      | What it is                                                                   |
| -------------- | ---------------------------------------------------------------------------- |
| Apache + PHP   | Web UI, account RPCs, `get_project_config.php`, forums, leaderboards         |
| Scheduler      | CGI binary `cgi-bin/cgi` (fcgi supported), called by clients on each RPC     |
| feeder         | Fills a shared-memory cache of sendable jobs for the scheduler               |
| transitioner   | Moves workunits and results between states, creates replicas, sets deadlines |
| validator      | One per app. Checks results, picks the canonical one, grants credit          |
| assimilator    | One per app. Handles the canonical result of each finished workunit          |
| file_deleter   | Deletes input and output files after assimilation                            |
| db_purge       | Archives and removes old workunit and result rows                            |
| work generator | Project code that creates workunits (`bin/create_work` or `create_work2`)    |
| periodic tasks | Stats, `update_stats`, team/user cleanup, via `bin/start --cron`             |

Version and platform facts:

- Database: MySQL or MariaDB. The cookbook lists MySQL 5.x/8 or MariaDB;
  `bsd:images/mysql` uses `mariadb:lts`. Source: `wiki:SoftwarePrereqsUnix`,
  `bsd:images/mysql/Dockerfile`. A specific supported-version matrix is not published on the
  wiki: UNVERIFIED whether MySQL 8.4 is exercised by BOINC's own CI.
- Charset: `utf8mb4` / `utf8mb4_unicode_ci` in `src:py/Boinc/database.py` (`create_database`).
- DB privileges: `create_database` issues `CREATE DATABASE` and then DDL and DML on it, so
  the DB user needs `CREATE`, `ALTER`, `INDEX`, `DROP` during setup and `SELECT`,
  `INSERT`, `UPDATE`, `DELETE` at run time. No triggers, stored procedures or events are
  created (`src:db/schema.sql`). Sessions use `READ UNCOMMITTED`
  (`src:db/db_base.cpp`, `src:html/inc/db_conn.inc`). Splitting setup and run-time users is
  our inference: UNVERIFIED.
- Password handling: the DB password is in plain text in `config.xml`
  (`<db_passwd>`). Source: `wiki:Project-options`. This is the reason the design keeps
  `config.xml` out of the repo and renders it at container start from Key Vault.

CORRECTIONS and gaps, in order of how much they affect the design:

1. CORRECTED, shared memory. The feeder and the scheduler share a System V shared-memory
   segment (the job cache, default at least 32 MB). They must run on the same host, and so in
   the same container group. A scheduler in one replica and a feeder in another does not work.
   Source: `wiki:MultiHost`, `wiki:Daemons`. This is what rules out scale-to-zero and
   multiple replicas for the scheduler plus daemons (design.md section 2).
2. CORRECTED, shared filesystem. Scheduler, daemons, upload handler, file_deleter and the
   validators all read and write one project tree: `upload/`, `download/`, `keys/`, `log_*`,
   with a fan-out directory hierarchy (`uldl_dir_fanout`). The web user and the daemon user
   share a group; directories are setgid and umask is 0002. Source: `wiki:DirHierarchy`,
   `wiki:Create-a-BOINC-server-(cookbook)`, `wiki:Server-security`. On Container Apps the
   container disk is ephemeral and Azure Files is the persistent mount, and blob storage
   cannot be mounted (see the Azure notes below).
3. CORRECTED, MyISAM. Most tables are InnoDB, but `post`, `thread` and `profile` are MyISAM
   because of full-text indexes. Source: `src:db/schema.sql` lines 33 to 35. Azure Database
   for MySQL "uses the InnoDB storage engine"
   (`https://learn.microsoft.com/en-us/azure/mysql/concepts-version-policy`, ms.date 2026-05-13).
   UNVERIFIED: whether `ENGINE=MyISAM` is rejected or silently converted on the flexible
   server. Plan: disable forums and profiles (they are not needed), and test `schema.sql`
   against a throwaway server before any real deployment.
4. UNVERIFIED, TLS to the DB. The C++ daemons connect with `mysql_real_connect`
   (`CLIENT_FOUND_ROWS`) and set only `MYSQL_OPT_RECONNECT`; PHP uses `new mysqli(...)`. No
   SSL option is configured anywhere (`src:db/db_base.cpp`, `src:html/inc/db_conn.inc`).
   Whether they negotiate TLS depends on the client library's default (Oracle libmysqlclient
   defaults to "preferred"; `bsd:images/apache/Dockerfile` installs Debian's
   `default-mysql-client` and PHP `mysqli`, and MariaDB Connector defaults differ). The
   flexible server's default is `require_secure_transport = ON` with TLS 1.2
   (`https://learn.microsoft.com/en-us/azure/mysql/flexible-server/how-to-connect-tls-ssl`).
   If the BOINC clients do not negotiate TLS, set the server parameter to `OFF` inside a
   private-access VNet. This must be tested.

Other notes for (b):

- Azure auth to MySQL with Entra ID: the access token is the password, the client needs the
  cleartext plugin, and tokens live 5 to 60 minutes. Source:
  `https://learn.microsoft.com/en-us/azure/mysql/security/security-how-to-entra`
  (page dated 2026-07-17). BOINC reads a fixed password from `config.xml` and has no
  refresh path, so Entra auth is not usable for BOINC daemons. The password goes in Key
  Vault. UNVERIFIED: that no wrapper (sidecar token refresher) is worth the complexity.
- The flexible server burstable tier: `Standard_B1ms` has 1 vCore, 2 GiB, 341 max
  connections, 640 IOPS; base CPU 20%. Storage 20 GiB minimum. Source:
  `https://learn.microsoft.com/en-us/azure/mysql/flexible-server/concepts-compute-storage`.
- MySQL major versions on Azure: 8.0 (standard support ends 2027-01-31) and 8.4 are GA;
  5.7 is retired; 9.5 is a preview innovation release. Source: version-policy page above. Use 8.4.
- Private access (VNet integration) cannot be switched to or from public access after
  creation. Source: `https://learn.microsoft.com/en-us/azure/mysql/flexible-server/concepts-networking`.

## (c) Client authentication is a per-project account key

- CONFIRMED. The client stores an `authenticator` (account key) per project in
  `account_<project>.xml`, sent on each scheduler request. Account creation goes through
  `create_account.php` / `lookup_account.php`. Source: `wiki:AccountManagement`,
  `wiki:Weak account key`.
- Detail: BOINC has a "weak account key" (the key without the password hash) that can be used
  for read-only and attach actions. Source: `wiki:Weak account key`.
- There is no per-project credential for the cloud. Volunteers never hold an Azure credential.

## (d) App types

- Native executable per platform, signed. Source: `wiki:Code-signing`, `wiki:Anonymous-platform`.
- Wrapper for legacy apps: the generic `wrapper` runs one or more programs described by
  `job.xml`. Source: `wiki:WrapperApp`, `wiki:Wrapper-release-notes`.
- VirtualBox: `vboxwrapper` runs a VM image. Latest release noted: v26212. Source:
  `wiki:VboxApps`, `wiki:Vboxwrapper-release-notes`.
- Docker: `docker_wrapper` (release v22 noted) runs a container described by `job.toml`
  and built from a `Dockerfile` at the host. Source: `wiki:Docker-apps`,
  `wiki:Docker-app-cookbook`, `wiki:Docker-app-implementation`,
  `wiki:Docker-wrapper-release-notes`. Constraints from those pages:
  - needs BOINC client 9.0 or later;
  - needs Docker or Podman on the host; on Windows it needs WSL;
  - the image must contain `ps` (the Debian image does not by default);
  - the host builds the image from the Dockerfile, so it needs network access;
  - app versions need a plan class from `plan_class_spec.xml` (detects Docker availability).
- The Docker page says it supports "volunteer hosts (Win, Mac and Linux)" (line 21 of
  `wiki:Docker-apps`). The Mac detail (Docker Desktop or Podman machine) is UNVERIFIED.
- BUDA (BOINC User-Defined Apps) exists and runs Docker or VM apps under a generic wrapper
  for submitters. Not needed here. Source: `wiki:BUDA-overview`.

## (e) Workunit sizing guidance

- CONFIRMED in outline. Each job has `rsc_fpops_est` (estimated FLOPs), `rsc_fpops_bound`
  (kill limit), `rsc_memory_bound`, `rsc_disk_bound`, `delay_bound` (deadline). The scheduler
  estimates run time as fpops over host speed. Source: `wiki:Job-templates`,
  `wiki:Local-job-submission`, `wiki:Work-generators`.
- The wiki advises runtimes of minutes to hours and avoiding jobs that run for days; very short
  jobs overload the server (scheduler contact per job). The exact wording is in
  `wiki:ProjectLaunch` and `wiki:Work-generators`; no hard number is given. A specific target
  of 15 to 60 minutes is our choice: UNVERIFIED as BOINC guidance.
- Input template field limit: the workunit's `xml_doc` is 64 KB (`src:db/schema.sql`), so
  the manifest per workunit must stay small.

## (f) Validation

- Replication: `min_quorum`, `target_nresults`, `max_error_results`, `max_total_results`.
  Source: `wiki:Job-replication`, `wiki:Validators`.
- Custom validators: the C++ plug-in API has `init_result`, `compare_results`,
  `cleanup_result`; credit is granted by the framework (`src:sched/validate_util2.cpp`).
  Source: `wiki:Validators`, `wiki:Standard-validators`.
- Fuzzy comparison is supported through `compare_results` (the sample bitwise and "trivial"
  validators are in `src:sched/sample_bitwise_validator.cpp`, `src:sched/sample_trivial_validator.cpp`).
- CORRECTED, script validator: `script_validator` lets a validator be any executable in the
  project's `bin/` directory, but it only passes tokens `files`, `result_id`, `runtime` (and
  `files2`, `result_id2`, `runtime2` for compare). It runs the command with `system()`. Exit
  0 means valid or equivalent, exit 3 (`VAL_RESULT_TRANSIENT_ERROR`) means retry later, any
  other value means invalid. Source: `src:sched/script_validator.cpp`. So it cannot see
  host id or app version. A Rust validator is therefore possible without C++, but anything
  needing the host must read the DB.
- Adaptive replication: set `app.target_nresults = 2`; jobs get `min_quorum = 1`; the first
  result from an untrusted host is replicated, from a trusted host it is not (unless sampled).
  A host becomes trusted by consecutive valid results; the scheduler trusts with probability
  `1 - 1/CV` once `consecutive_valid` is 10 or more. Source: `wiki:Adaptive-Replication`,
  `src:sched/sched_send.cpp` lines 297 to 312 and 1138 to 1158.
- The validator can mark a result suspicious to reset trust (`wiki:Adaptive-Replication`).
- `assimilate` is a separate daemon; `script_assimilator` is given `--script "name wu_id
files result_id runtime"` and, when no canonical result exists, `--error N wu_name wu_id
batch_id`. Source: `src:sched/script_assimilator.cpp`, `wiki:Standard-assimilators`.

## (g) Credit, leaderboards, teams

- CONFIRMED built in. Credit: `src:sched/credit.cpp`, `wiki:CreditNew`,
  `wiki:Credit-options`. Leaderboards: `src:html/user/top_users.php`, `top_hosts.php`,
  `top_teams.php`. Teams: `team_create_form.php`, `team_display.php`. Disable-able with
  `DISABLE_TEAMS` in `html/project/project.inc` (`wiki:Web-configuration-file`).
- For this project, credit is cosmetic. Leaderboards are public by default, which is a
  privacy matter: `enable_privacy_by_default` exists (`wiki:GdprCompliance`).

## (h) File serving and signing

- Download: input files are described by `<file_info>` with one or more `<url>`; clients do
  plain HTTP GET (with Range). URLs can point to any HTTP server. `bin/create_work` takes
  `--remote_file URL nbytes MD5`, and remote files get the physical name `jf_<md5>`. Source:
  `wiki:Remote-input-file-management`, `wiki:Staging-input-files`, `wiki:Input-file-handling`.
  CONFIRMED: downloads can be served from blob storage.
- BOINC has no per-workunit token. The URL is stored in the workunit and sent to every host
  that gets a replica. A URL with an expiring SAS therefore works only until the SAS expires;
  retries after that fail. A user delegation SAS key can live at most seven days
  (`https://learn.microsoft.com/en-us/rest/api/storageservices/create-user-delegation-sas`).
- CORRECTED, upload: upload goes through the `file_upload_handler` CGI program, which checks
  an RSA-signed upload certificate in the job's file_info and writes to the local `upload/`
  tree. It cannot be redirected to blob storage. Source: `wiki:FileUpload`,
  `src:sched/file_upload_handler.cpp`. Persistent storage for it on Container Apps is an
  Azure Files mount (Azure NetApp Files and Blob mounts are not supported). Source:
  `https://learn.microsoft.com/en-us/azure/container-apps/storage-mounts`.
- Code signing: executables and app files are signed with the project's code signing key
  (`crypt_prog -genkey`, `sign_executable`, and `update_versions` signs when it installs app
  versions). The private key is meant to be kept offline on a separate machine; the public key
  ships in the client at attach time. Source: `wiki:Code-signing`,
  `wiki:File-signing-utilities`. Key rotation uses `old_key_i` with signatures from the old
  key over the new one.
- Offline signing is supported: if `update_versions` finds no private key in `keys/`, you
  produce `FILENAME.sig` for each app file on the separate machine and place them beside the
  files before running `update_versions`. Source: `wiki:Adding-app-versions` (section "Code
  signing"), `wiki:Code-signing` ("Using a separate code signing machine").
- Upload signing is a separate key pair (`upload_private`, `upload_public`) that stays on the
  server (`wiki:Code-signing`). `dont_generate_upload_certificates` exists for jobs whose
  uploads are not certified (`wiki:Project-options`); we do not use it.
- Files for Docker apps: the `Dockerfile` and `job.toml` are app files, signed like any other.

## (i) Server container images

- `boinc-server-docker` has three images: `mysql` (MariaDB LTS), `makeproject` (runs
  `make_project` once) and `apache` (PHP on Apache, `supervisord` running cron, rsyslog,
  Apache, and `bin/start` as user `boincadm`). All BOINC daemons and the scheduler run in the
  `apache` container, which is what the shared-memory constraint needs. Source:
  `bsd:docker-compose.yml`, `bsd:images/*/Dockerfile`.
- CORRECTED: the published Docker Hub tags (`boinc/server_apache`) are old (about 2019)
  (`https://hub.docker.com/v2/repositories/boinc/server_apache/tags`, read 2026-10-13). We
  would build our own image from the Dockerfiles, pinned to the `server_release/1.6/1.6.2`
  tag.

## (j) Official list, and attaching to an unlisted project

- CONFIRMED optional. The project list in BOINC Manager and the site's list are maintained
  by the BOINC team; a project asks to be listed through BOINC's contact process. Science
  United is a separate service that matches volunteers to projects. Source: `wiki:Going-public`,
  `wiki:ProjectLaunch`, `wiki:Choosing and joining projects`. We do not contact anyone.
- CONFIRMED attach by URL. BOINC Manager's "Add project" accepts a project URL; the client
  fetches `<master URL>get_project_config.php` (`src:client/acct_setup.cpp` line 72). It
  follows HTTP redirects (`CURLOPT_FOLLOWLOCATION` in `src:client/http_curl.cpp` line 527), but
  that redirects on `get_project_config.php` work end to end is UNVERIFIED.
- Master URL stability: changing the master URL forces volunteers to reattach. The master
  page must carry the scheduler URLs in an HTML comment (`<scheduler>...</scheduler>`) and
  `<link rel="boinc_scheduler" href="...">`. Source: `wiki:Master-URL`. Because the client
  requests `get_project_config.php` at the master URL, a purely static master host is not
  enough (design.md section 3).
- Privacy gates: `INVITE_CODES`, `INVITE_CODES_RPC` (regex for account-creation codes) and
  `disable_account_creation` keep the project effectively private. Source:
  `wiki:Web-configuration-file`, `wiki:Project-options`.
- GDPR: `enable_privacy_by_default`, `enable_delete_account`, terms-of-use consent,
  `delete_expired_*` tasks. BOINC stores volunteer email addresses and IP addresses. Source:
  `wiki:GdprCompliance`, `wiki:RightToErasure`.

## (k) Which app type works on each host route

Added after the scope change: BOINC is the work layer for the Proxmox LXC template, the
docker/podman container and the Azure Spot VM or ACI routes.

- On Unix, `docker_wrapper` needs Docker with the daemon always running, and the user the BOINC
  client runs as must be able to run the Docker CLI. BOINC says it cannot use Podman on Unix,
  because pausing containers needs root, which the client does not use. Source:
  `wiki:Docker-app-implementation`, read 2026-10-13.
- Azure Container Instances "doesn't expose direct access to the underlying infrastructure ...
  the container runtime, orchestration technology, and running privileged container operations".
  So a Docker-wrapper app cannot run inside an ACI group. CONFIRMED. Source:
  `https://learn.microsoft.com/en-us/azure/container-instances/container-instances-troubleshooting`,
  read 2026-10-13.
- A Docker app inside an unprivileged Proxmox LXC container, or inside a docker/podman container
  without mounting the host's Docker socket, needs nested containers. UNVERIFIED by test; the
  BOINC docs say nothing about it. Treated as unreliable.
- A native app (a signed executable per platform, optionally run by the `wrapper`) needs only the
  BOINC client, so it runs in all of these. Platform names: `x86_64-pc-linux-gnu`,
  `aarch64-unknown-linux-gnu`, `windows_x86_64`, `x86_64-apple-darwin`, `arm64-apple-darwin`
  (`wiki:BOINC-platforms`).
- `boinccmd` supports `--project_attach URL account_key`, `--create_account URL email password
name`, `--lookup_account URL email password`, `--project URL detach`, and
  `--read_global_prefs_override` (`src:client/boinc_cmd.cpp`, `wiki:Boinccmd-tool`). Local limits
  go in `global_prefs_override.xml` in the BOINC data directory (`wiki:Global prefs override.xml`);
  memory limits are percentages (`ram_max_used_busy_pct`, `ram_max_used_idle_pct`), CPU limits
  are `max_ncpus_pct` and `cpu_usage_limit`.
- Debian and Ubuntu install the client with `apt install boinc-client`; Fedora with
  `yum install boinc-client` (`wiki:Installing BOINC on Debian or Ubuntu`,
  `wiki:Installing BOINC on Fedora`). Whether RHEL-family distributions other than Fedora carry
  the package (for example through EPEL) is UNVERIFIED; the volunteer script prints guidance
  instead of adding repositories on its own.

## Azure facts used in the design

| Fact                                                                                                                | Source and date                                                                                                                                                 |
| ------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Container Apps free grant per subscription per month: 180,000 vCPU-seconds, 360,000 GiB-seconds, 2 million requests | `https://learn.microsoft.com/en-us/azure/container-apps/billing`, read 2026-10-13                                                                               |
| Multiple containers per replica share a replica-scoped volume; `minReplicas` 0 or higher                            | `.../container-apps/storage-mounts`, `.../container-apps/scale-app`, read 2026-10-13                                                                            |
| Azure Files can be mounted; Blob mount is not supported                                                             | `.../container-apps/storage-mounts`                                                                                                                             |
| Ephemeral storage 1 GiB at 0.25 vCPU up to 8 GiB over 1 vCPU                                                        | `.../container-apps/storage-mounts`                                                                                                                             |
| Static Web Apps linking a Container Apps backend needs the Standard plan                                            | `https://learn.microsoft.com/en-us/azure/static-web-apps/apis-container-apps`, read 2026-10-13. Proxying is for `/api` routes; root-path proxying is UNVERIFIED |
| Retail prices                                                                                                       | `https://prices.azure.com/api/retail/prices`, queried 2026-10-13 (see design.md section 6)                                                                      |

UNVERIFIED items that gate real work, in one list:

1. Whether `ENGINE=MyISAM` tables are accepted on the flexible server.
2. Whether BOINC's C++ and PHP clients connect when `require_secure_transport = ON`.
3. Whether Azure Files (SMB) preserves BOINC's setgid, umask and file-lock assumptions under a
   Container Apps mount, and its latency under the upload handler.
4. Whether the BOINC client follows a redirect for `get_project_config.php`.
5. Whether Docker apps work acceptably on volunteers' Macs.
6. The per-page CPU time on volunteer hardware (the 160 to 330 s figure is from the project's own nodes).
