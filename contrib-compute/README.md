# contrib-compute

Hosting for the stock BOINC server that PR #441 (the BOINC batch OCR pilot, `usnm-boinc`) submits to. Prod's server runs on an on-prem host since 21 October 2026 ([docs/boinc-onprem.md](../docs/boinc-onprem.md)); the Bicep below, which hosts it in Azure, is the fallback, and with `"server": "onprem"` in the settings it deploys only the identities, Key Vault, VNet and static site. #441 is the client base and the bridge: a Docker-wrapper app, one cached page per task, a bridge that reserves and journals pages and publishes native output. It does not provision a server. This directory does. The decision is in [ADR-0019](../docs/design/adr/0019-boinc-server-hosting.md); the on-prem workers it sits beside are in [ADR-0015](../docs/design/adr/0015-onprem-ocr-workers.md).

The production BOINC stack was deployed in Azure and checked as recorded in #470, before the move on-prem. The committed settings and Bicep are the declarative source of truth; this repository change does not re-check the live service. Each document keeps remaining unverified behavior explicit.

Volunteers should start with [docs/boinc-volunteer.md](../docs/boinc-volunteer.md).

## Contents

- `infra/boinc/`: Bicep for the stock server: a Container App (Apache, PHP and the stock daemons, one replica, no scale to zero), a MySQL flexible server, an Azure Files share, Key Vault, Log Analytics, a static site for the human pages, and a manual-trigger Container Apps Job that runs `usnm-boinc submit`. Lint and compile only.
- `boinc/`: `config.xml.template` (daemons come from #441's `ocr/boinc/daemons.xml`), `project.xml` and `plan_class_spec.xml` (the `docker` plan class).
- `images/`: the server image (stock BOINC, Apache, PHP and the daemons in one container) and the bridge job image; see [docs/images.md](docs/images.md).
- `scripts/provision-boinc.sh` and `scripts/build-boinc-images.sh`: provision the stack and build and pin its server and bridge images. Secrets and project initialization run as in-VNet Container Apps Jobs. The signing and app-version release workflow remains in `scripts/boinc-app-release.sh`; its private signing key stays offline.
- `docs/`: [design](docs/design.md), [interface](docs/interface.md) (what is assumed of #441), and [verified facts](docs/verified-facts.md) (each with its source and fetch date), and [images and deployment steps](docs/images.md).

This directory has no Rust, app or work generator; those are #441's. Later options for anonymous volunteers, and a deferred fetch idea, are one short section each in the design.

## Checks

From the repository root:

```sh
for f in contrib-compute/boinc/project.xml contrib-compute/boinc/plan_class_spec.xml contrib-compute/boinc/config.xml.template; do
  python3 -c 'import sys, xml.dom.minidom as m; m.parse(sys.argv[1])' "$f"
done
shellcheck -x -S warning scripts/boinc/*.sh contrib-compute/images/server/*.sh
for f in $(git ls-files 'contrib-compute/infra/*.bicep'); do bicep build "$f" --stdout > /dev/null; done
```

CI runs the same when a pull request touches this directory (`scripts/ci/changes.sh`). `bicep build` only compiles on the local machine; nothing in this repository is deployed by these commands.

The repository's rules in [CONTRIBUTING](../CONTRIBUTING.md) apply here too: no private addresses, no secrets, and scripts take `--flags`, never positional arguments. The server template has three exceptions to the Entra-only rule, listed in ADR-0019, including the database password in its Key Vault.
