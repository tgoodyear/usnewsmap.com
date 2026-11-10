# Server images

The BOINC server, bridge and app-version images: what each holds, how they are built and pinned, and the stock BOINC facts that shaped them. The image volunteers pull is separate: see [The worker image](worker-image.md).

Prod's server runs on-prem since 21 October 2026 ([BOINC server on-prem](../server/onprem.md)). The host runs the image named by `SERVER_IMAGE` in `/etc/usnm-boinc/server.env`, and the step runner uses the same image. The Azure Container Apps server these images were first built for, with its jobs, MySQL, project share and workbook, was deleted when `"server": "onprem"` was set ([onprem.md](../server/onprem.md)); `provision-boinc.sh` now deploys only the identities, Key Vault, VNet and static site the on-prem server still uses.

## What is built

| Image      | Source                                  | Contents                                                                                                                                                                            |
| ---------- | --------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| base       | `boinc/images/base/Containerfile`       | Pinned BOINC source, runtime dependencies and `docker_wrapper`; cached in the shared ACR as `usnm-boinc-base:<input-key>`.                                                          |
| bridge-bin | `boinc/images/bridge-bin/Containerfile` | Release-built `usnm-boinc` binary; cached in the shared ACR as `usnm-boinc-bridge-bin:<input-key>`.                                                                                 |
| server     | `boinc/images/server/Containerfile`     | The base plus project configuration, Apache/PHP, scheduler and upload CGIs, and all daemons under `supervisord`; one container.                                                     |
| bridge     | `boinc/images/bridge/Containerfile`     | The server image plus the bridge binary; runs the bridge steps and gold-bundle builds.                                                                                              |
| appver     | `boinc/images/appver/Containerfile`     | The server image plus one signed app version, built by `boinc/scripts/boinc-app-release.sh` for the publish step ([Releasing an app version](app-release.md)).                      |
| worker     | `boinc/apps/Containerfile`              | Tesseract and `usnm-boinc work`; volunteers pull it from the public GHCR package. Built and published by `.github/workflows/boinc-worker.yml` ([worker-image.md](worker-image.md)). |

`boinc/images/server/` also holds overlay Containerfiles built on a running image rather than from `main`: `security-overlay.Containerfile` for package updates ([onprem.md](../server/onprem.md)), and `live-overlay.Containerfile`, `batch-overlay.Containerfile` and `amd-885.Containerfile`, each for one scoped change (the last is recorded in [the first AMD Vulkan release](../../notes/2026-10-19-amd-vulkan-release.md)).

One container holds Apache and the daemons because the feeder and the scheduler share a System V shared-memory segment ([design](../design.md), section 2.1).

## Build

`boinc/scripts/build-boinc-images.sh --env prod` builds inside Azure Container Registry Tasks in the app's shared registry, tags the server and bridge with the committed Git revision, and writes their registry digests into `boinc/infra/boinc/env/prod.json`. It refuses a dirty tree for production; `--what-if` prints the builds. Submit the updated settings through a pull request.

The slow layers are reused when their inputs haven't changed:

- `usnm-boinc-base` is keyed on the base Containerfile, BOINC source patches and MySQL TLS options.
- `usnm-boinc-bridge-bin` is keyed on the Rust sources, embedded BOINC templates, lockfiles, `.dockerignore`, toolchain (when present) and its Containerfile.

When either key changes, only that layer is rebuilt. The server image is then built on the base digest with the repository root as its context (so it can copy the `.well-known` files from `web/public`, which the API image also uses), and the bridge on the server and bridge-bin digests. Each image is pinned by the registry-reported digest, not the local image ID. The registry has no anonymous pull or admin access.

## Configuration in the image

- The entrypoint renders the project's `config.xml` from `boinc/project/config.xml.template` at container startup, so a template change needs a new image. It also changes the `config.xml` SHA-256 the Qwen refill controller pins: disable its timer before the restart and re-pin it after ([Intel GPUs](intel-gpu.md#persistent-finite-nvidiaintel-refill)).
- `db_purge` runs continuously with `--min_age_days 30`, archiving and removing workunit and result records after 30 days. Don't add a bare `--max`: it needs a number, and a limit makes the daemon exit once it has purged that many. With no eligible work it sleeps.
- `maintenance_delay` is 1200 seconds, so a client told the project is down (no feeder or one not yet ready, `stop_sched`, no database) asks again after about 20 minutes, not BOINC's default hour ([client retry behaviour](../server/feeder-refresh.md#client-retry-behaviour)).
- The bridge stages public download inputs with mode `0644` before publication and before `create_work`, so Apache can read them at once. The hand-off journal and original input snapshots stay owner-only.

### BOINC source revision

`batch_collect_assimilator`, which `boinc/apps/daemons.xml` runs, is not in the `server_release/1.6/1.6.2` tree (searched on 13 October 2026) and is in master. `boinc/images/base/Containerfile` pins master at `22bbebe08e2eaac5d5e88a6d93b88e800cb6341c` (13 October 2026). Move to the first release tag that contains it when one exists. The image is unmodified upstream source plus the patches the base key covers.

### Stock BOINC tools and the entry point

- `make_project` creates the schema, the project tree and key pairs in its key directory (`py/Boinc/setup_project.py`). Init mode uses tmpfs and removes the generated signing private key. The code-signing private key stays offline on the signing host and is never made by a deployment.
- `make_project` refuses an existing project root, so the project lives in a subdirectory of the project volume: `/project/usnmocr`, with `/project/handoff` beside it.
- BOINC's key generator is `crypt_prog -genkey 1024`, built with the server.
- The `docker_wrapper` that `install.py` puts in a container app version is built from the same source (`samples/docker_wrapper`) in the base image and left at `/usr/local/lib/usnm-boinc/docker_wrapper`. The Linux container versions of `usnm_ocr` and `usnm_gold` run with it on volunteers. Mac wrappers are built separately ([Mac app versions](app-release.md#mac-app-versions)).
