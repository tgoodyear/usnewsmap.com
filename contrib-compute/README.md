# contrib-compute

A BOINC project server and its Rust code for letting other people's computers re-read badly OCR'd newspaper pages for usnewsmap.com. The decision is in [ADR-0018](../docs/design/adr/0018-boinc-contributor-compute.md); the on-prem workers it sits beside are in [ADR-0015](../docs/design/adr/0015-onprem-ocr-workers.md).

Status: a draft. Nothing here is deployed, no project server exists and no volunteer is attached. Checked so far: `cargo fmt`, clippy and tests, shellcheck, and Bicep compile. Each document lists what was not tested, marked UNVERIFIED.

A contributor runs the BOINC client, attached to the project with an account key. They never hold an Azure credential. The scripts and guides for running the client (Proxmox, Docker or Podman, Azure, plain Linux) are in a separate follow-up pull request. This directory is the server side.

## Contents

- `boinc/`: the Rust crate `usnm-boinc`. It has the work generator, validator, assimilator and the client-side runner `usnm-ocr-run`, plus the project definition and job templates under `boinc/apps/usnm_ocr/`.
- `infra/boinc/`: Bicep for the project server (a Container App, MySQL, Key Vault, storage). Lint and compile only.
- `scripts/boinc/bootstrap.sh`: the operator's plan for standing the server up; it only prints and what-ifs.
- `docs/`: [design](docs/design.md), [interface](docs/interface.md) (the names the client scripts rely on), [verified facts](docs/verified-facts.md) (each with its source and fetch date) and [ocr-sync and BOINC results](docs/ocr-sync-provenance.md).

The job type that has the contributor's machine download scans from the Library of Congress itself (`fetch-and-ocr`) is written and tested but off by default. If it is ever enabled, each client runs at a low request rate and the design is not a way around the source's limits; a better place to get the images is preferred if one exists. The source has not approved it. See section 4.4 of the [design](docs/design.md).

## Checks

`contrib-compute/` is a Cargo workspace of its own and is not part of the main build. From this directory:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked

shellcheck -x -S warning scripts/boinc/*.sh
for f in $(git ls-files 'infra/*.bicep'); do bicep build "$f" --stdout > /dev/null; done
```

CI runs the same when a pull request touches this directory (`scripts/ci/changes.sh`). `bicep build` only compiles on the local machine; nothing in this repository is deployed by these commands.

The repository's rules in [CONTRIBUTING](../CONTRIBUTING.md) apply here too: no private addresses, no secrets, and scripts take `--flags`, never positional arguments. The server template has four exceptions to the Entra-only rule, listed in ADR-0018, including the database password in its Key Vault.
