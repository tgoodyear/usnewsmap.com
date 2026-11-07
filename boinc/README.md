# BOINC

The BOINC operational tree: worker apps, hosting, scripts and operator documentation.
The Rust bridge remains in [`crates/usnm-boinc`](../crates/usnm-boinc).
Start with the [pilot documentation](docs/README.md).

Hosting for the stock BOINC server that PR #441 (the BOINC batch OCR pilot, `usnm-boinc`) submits to. Prod's server runs on an on-prem host since 21 October 2026 ([docs/boinc-onprem.md](../docs/boinc-onprem.md)); the Bicep below, which hosts it in Azure, is the fallback, and with `"server": "onprem"` in the settings it deploys only the identities, Key Vault, VNet and static site. #441 is the client base and the bridge: a Docker-wrapper app, one cached page per task, a bridge that reserves and journals pages and publishes native output. It does not provision a server. This directory does. The decision is in [ADR-0019](../docs/design/adr/0019-boinc-server-hosting.md); the on-prem workers it sits beside are in [ADR-0015](../docs/design/adr/0015-onprem-ocr-workers.md).

The production BOINC stack was deployed in Azure and checked as recorded in #470, before the move on-prem. The committed settings and Bicep are the declarative source of truth; this repository change does not re-check the live service. Each document keeps remaining unverified behavior explicit.

Volunteers should start with [docs/boinc-volunteer.md](../docs/boinc-volunteer.md).

## Contents

- `infra/boinc/`: Bicep for the stock server: a Container App (Apache, PHP and the stock daemons, one replica, no scale to zero), a MySQL flexible server, an Azure Files share, Key Vault, Log Analytics, a static site for the human pages, and a manual-trigger Container Apps Job that runs `usnm-boinc submit`. Lint and compile only.
- `project/`: `config.xml.template` (daemons come from `apps/daemons.xml`), `project.xml` and `plan_class_spec.xml` (the `docker` plan class).
- `apps/`: container recipes, app templates, engine locks and native app support.
- `images/`: the server image (stock BOINC, Apache, PHP and the daemons in one container) and the bridge job image; see [docs/images.md](docs/images.md).
- `boinc/scripts/provision-boinc.sh` and `boinc/scripts/build-boinc-images.sh`: provision the stack and build and pin its server and bridge images. Secrets and project initialization run as in-VNet Container Apps Jobs. The signing and app-version release workflow remains in `boinc/scripts/boinc-app-release.sh`; its private signing key stays offline.
- `docs/`: [design](docs/design.md), [interface](docs/interface.md) (what is assumed of #441), and [verified facts](docs/verified-facts.md) (each with its source and fetch date), and [images and deployment steps](docs/images.md).

The Rust apps and work generator remain in the workspace crates. Later options for anonymous volunteers, and a deferred fetch idea, are one short section each in the design.

## Tree migration

[#1208](https://github.com/tgoodyear/usnewsmap.com/issues/1208) brought the BOINC sources into this one tree.
Public URLs, image names, app identities, installed paths and artifact contracts did not change,
and there are no copies at the old source paths. The batch controller runs
`boinc/scripts/batch_scheduler.py` from its host's pinned checkout, and its bridge entry point is
`boinc/scripts/start-boinc-bridge.sh`; `scripts/ocr-node/push-boinc-scan.sh` did not move.
The server host's installed `/usr/local/sbin/usnm-boinc-step` and units keep their installed paths;
a later installation takes its files from the new source paths.

Source paths moved as follows:

| Before                                              | After                           |
| --------------------------------------------------- | ------------------------------- |
| `contrib-compute/boinc/`                            | `boinc/project/`                |
| `contrib-compute/images/`, `contrib-compute/infra/` | `boinc/images/`, `boinc/infra/` |
| `ocr/boinc/`                                        | `boinc/apps/`                   |
| `scripts/boinc/`                                    | `boinc/scripts/`                |
| `contrib-compute/docs/`, `docs/boinc/`              | `boinc/docs/`                   |

Build contexts stayed the repository root where they were the root;
only recipe and source paths changed. CI job IDs and display names stayed the same
(including the historical `contrib-compute` names). Dependabot has no BOINC
directory entries to migrate: Actions, root Cargo, `/web` npm and `/scripts/ci`
pip directories are unchanged.

## Checks

From the repository root:

```sh
for f in boinc/project/project.xml boinc/project/plan_class_spec.xml boinc/project/config.xml.template; do
  python3 -c 'import sys, xml.dom.minidom as m; m.parse(sys.argv[1])' "$f"
done
shellcheck -x -S warning boinc/scripts/*.sh boinc/images/server/*.sh
for f in $(git ls-files 'boinc/infra/*.bicep'); do bicep build "$f" --stdout > /dev/null; done
```

CI runs the same when a pull request touches this directory (`scripts/ci/changes.sh`). `bicep build` only compiles on the local machine; nothing in this repository is deployed by these commands.

The repository's rules in [CONTRIBUTING](../CONTRIBUTING.md) apply here too: no private addresses, no secrets, and scripts take `--flags`, never positional arguments. The server template has three exceptions to the Entra-only rule, listed in ADR-0019, including the database password in its Key Vault.
