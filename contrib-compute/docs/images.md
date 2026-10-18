# Images and deployment steps

The production stack was deployed and checked as recorded in #470. CI builds and smoke-tests
the server image; this page does not claim a fresh live check. Remaining unverified behavior is
listed below.

## What is built

| Image      | Source                            | Contents                                                                                                                                                           |
| ---------- | --------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| base       | `images/base/Containerfile`       | Pinned BOINC source and runtime dependencies; cached in the shared ACR as `usnm-boinc-base:<input-key>`.                                                           |
| bridge-bin | `images/bridge-bin/Containerfile` | Release-built `usnm-boinc` binary; cached in the shared ACR as `usnm-boinc-bridge-bin:<input-key>`.                                                                |
| server     | `images/server/Containerfile`     | The base plus project configuration, Apache/PHP, scheduler and upload CGIs, and all daemons under `supervisord`; one container.                                    |
| bridge     | `images/bridge/Containerfile`     | The server image plus the bridge binary; used by the in-VNet bridge and optional gold-bundle jobs.                                                                 |
| worker     | `ocr/boinc/Containerfile` (#441)  | Tesseract and `usnm-boinc work`; volunteers pull it from the public GHCR package. Built and published by `.github/workflows/boinc-worker.yml` (ADR-0019 addendum). |

One container holds Apache and the daemons because the feeder and the scheduler share a System V shared-memory segment (design.md, section 2.1).

The `db_purge` daemon runs continuously with `--min_age_days 30`, archiving and
removing eligible workunit and result database records after 30 days. Do not add
a bare `--max`: it requires a numeric argument, and a limit makes the daemon exit
once it has purged that many workunits. With no eligible work, it sleeps rather
than exits. Changes to `config.xml.template` require an image rollout; the
entrypoint renders the project's configuration at container startup.

### Why the BOINC commit is master, not the 1.6.2 tag

`batch_collect_assimilator`, which PR #441's `daemons.xml` runs, is not in the `server_release/1.6/1.6.2` tree (searched on 2026-10-13) and is in master. The Containerfile pins master at `22bbebe08e2eaac5d5e88a6d93b88e800cb6341c` (2026-10-13). Move to the first release tag that contains it when one exists. The image is still unmodified upstream source.

### Facts about the stock tools that shaped the entry point

- `make_project` creates the schema, the project tree, and key pairs in its key directory (`py/Boinc/setup_project.py`). The init mode uses tmpfs and removes the generated signing private key. The in-VNet secrets job puts the upload keys in Key Vault; the in-VNet init job uses the mounted project share and the server entrypoint's safe ensure-project mode.
- `make_project` refuses an existing project root, so the project lives in a subdirectory of the share: `/project/usnmocr`. `/project/handoff` is its sibling.
- BOINC's key generator is `crypt_prog -genkey 1024`, built with the server. The secrets job runs the image's key-generation path inside the VNet and writes the upload keys directly to Key Vault. The code-signing private key remains offline and is never made by this deployment.
- The `docker_wrapper` that `install.py` copies into the app version is built from the same source (`samples/docker_wrapper`) and left at `/usr/local/lib/usnm-boinc/docker_wrapper`. UNVERIFIED that it builds in this configuration, and that a binary linked on Debian 12 runs on volunteers' older glibc.

## Build

The supported image builder is `scripts/build-boinc-images.sh --env <prod|dev>`. It builds
inside Azure Container Registry Tasks in the app's shared registry, tags the server and bridge
with the committed Git revision, and writes their registry digests into
`contrib-compute/infra/boinc/env/<env>.json`. The script refuses a dirty tree for production.
Submit the updated settings through a pull request before deploying them.

The slow layers are reused when their inputs have not changed:

- `usnm-boinc-base` is keyed on the base Containerfile, BOINC source patches, and MySQL TLS
  options.
- `usnm-boinc-bridge-bin` is keyed on the Rust sources, embedded BOINC templates, lockfiles,
  `.dockerignore`, toolchain (when present), and its Containerfile.

When either key changes, only that layer is rebuilt. The server image is then built on the base
digest, and the bridge on the server and bridge-bin digests. Each image is pinned in settings by
the registry-reported digest, not the local image ID. The shared registry is the app's existing
ACR; the BOINC stack grants its runtime and secrets-job identities `AcrPull` there. It has no
anonymous pull or registry admin access.

## Deploy, in order

The production BOINC regional resources use `westus2`; Static Web Apps retains its limited-region
`eastus2` setting. `scripts/provision-boinc.sh --env <prod|dev>`
uses committed settings, creates a retained stack-aware What-If preview, and deploys the Bicep.
The custom-domain CNAME and `asuid` TXT records are declared in Bicep; provisioning advances
the Container App binding and managed certificate in stages.

With images configured, provisioning starts the in-VNet secrets and project-init jobs, registers
applications, replays the configured signed app version using the publish job, and checks public
health. Project initialization is safe to rerun: completed init is skipped, and partial project
data or an existing database is not dropped automatically. The secrets job creates runtime and
upload secrets only when absent, without routing their values through the maintainer's machine.
`--what-if` creates only the retained preview, not managed resources.

The VNet includes the private Key Vault DNS zone `privatelink.vaultcore.azure.net` and its link.
The Azure Files share mounts over SMB with `mfsymlinks` plus explicit uid/gid, mode and cache
options. The MySQL server's `require_secure_transport` is OFF: BOINC's clients do not request TLS,
and #464 tracks the gated migration to required TLS. The database is private to the VNet.

To record a work-volume sample, run
`scripts/start-boinc-bridge.sh --env prod --step work-volume`. This emits one JSON record
containing aggregate workunit and result counts plus BOINC's recent-credit user and host counts;
the workbook reads it from console logs. Its KQL uses a fuzzy union of both Container Apps
resource-specific tables (`ContainerAppConsoleLogs`, `ContainerAppSystemLogs`) and legacy `_CL`
tables, mapping their unsuffixed and `_s` columns and removing identical rows when both sources
are present. The standard system-log schema has no execution-name column, so the workbook
reports job activity and events rather than claiming to count job executions. Workunit
validation, assimilation and file-deletion counts are separate, potentially overlapping flags.
The bridge job is manual-triggered, so the time chart gains a point only when this step is run.

#### Work-volume acceptance after #633

Release #633's successful lifecycle jobs are not exporter acceptance: execution
`usnm-boinc-bridge-caj-prod-yjzy92x` failed with the sanitized aggregate-query error.
The follow-up for #510 reproduces a defect in the checked-in work-volume option file:
the generated file had a literal masked line instead of a password option, which
MySQL rejects before executing SQL. The work-volume branch now quotes and escapes the
tmpfs option values. TLS and certificate verification remain mandatory. This
reproduction does not prove that the deployed failure had the same cause.

CI runs the unchanged aggregate query against MySQL 8.4 using synthetic temporary
tables with the referenced columns and types from BOINC commit `22bbebe0`,
including empty tables, all state buckets and the recent-credit threshold. It
also checks option-file permissions, parsing and authentication with a synthetic
password. To run these tests locally, point `USNM_WORK_VOLUME_MYSQL_PORT` at a
disposable, loopback-only MySQL 8.4 fixture with an empty-password root account,
then run
`python3 -m unittest -v contrib-compute/images/server/test_work_volume.py` from
the repository root. Never point this fixture test at a deployed database. These
tests do not establish the production schema or runtime grants.

Keep #510 acceptance open until an authorized operator can:

1. Build and release the reviewed server and bridge images through the normal PR
   and provisioning process; record the deployed digest and revision. Do not
   patch the running container or dispatch volunteer work.
2. From the bridge's VNet and runtime identity, verify the installed client
   version, readable CA bundle, certificate-verified TLS connection, effective
   SELECT access on `workunit`, `result`, `user` and `host`, and the referenced
   column names and types. Do not print the option file, credentials, raw rows or
   unrestricted database errors. If the query still fails, retain only an
   allowlisted numeric MySQL error code or SQLSTATE and the failure stage for
   diagnosis.
3. Run `scripts/start-boinc-bridge.sh --env prod --step work-volume`, record its
   execution ID and successful exit, and verify exactly one schema-version-1
   aggregate event in the BOINC workspace.
4. Deploy the workbook through the normal main-stack process with its required
   drift checks, and verify the latest tiles, state table and time chart consume
   that event.

Live verification requires a recent BOINC log sample and an in-VNet runtime check.
The intermittent `InsufficientAccessError` in #528 was isolated to a faulty Traffic
Manager query frontend, not missing Reader permissions. Resource-context constant
queries with no visible workspace sources did not prove absence of ingestion.
Use `scripts/validate-boinc-workbook.py --workspace <customer-GUID>` for the nine
panels; see [query routing and retries](../../docs/boinc.md#workbook-query-validation).
Successful empty panels do not establish status/filter or recent-sample acceptance.

`scripts/build-boinc-images.sh` builds and pins the server and bridge artifacts.
`scripts/boinc-app-release.sh` stages, signs and publishes a selected app version on the
designated signing host without exporting its private key. These are release inputs, not
steps printed after every deployment. See [app-release.md](app-release.md) for signing and
acceptance boundaries. The code-signing private key stays offline.

## Submission file permissions

The bridge CLI stages public download inputs with mode `0644` before publication
and before `create_work`, so Apache can read them immediately. The hand-off
journal and original input snapshots remain owner-only. The bridge wrapper
executes the CLI directly; it performs no chmod or chgrp scan after submission.
Rebuild and deploy the bridge image with the updated CLI and wrapper together.
Ensure the Apache identity can traverse the existing download fan-out directories
and read a staged scan and request on the deployed Azure Files mount. Local
POSIX-mode regressions do not verify Azure Files ACLs or mount behavior.

## Known blockers and risks

- **Guardrails.** ADR-0019 documents the dedicated BOINC resource group and the two required policy waivers for Azure Files shared-key mounting and the Log Analytics shared key.
- **Network path.** Key Vault and the Files share are private-endpoint only. The secrets and init jobs run inside the VNet; the maintainer still needs a VNet path for the manual share hand-off.
- **Remaining checks.** Full Azure Files POSIX permission and lock semantics; the MySQL runtime grants and MyISAM/8.4 compatibility; required-TLS migration (#464); job managed-identity behavior beyond the successful deployment checks; shared-memory and image-size bounds.
