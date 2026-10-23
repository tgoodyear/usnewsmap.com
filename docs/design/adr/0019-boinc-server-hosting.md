# ADR-0019: Hosting for the stock BOINC server

- **Status:** Accepted; production deployment recorded in #470
- **Date:** 2026-10
- **Extends:** ADR-0015 (on-prem OCR workers) and ADR-0016 (pluggable OCR engines)
- **Amends:** ADR-0009 for the BOINC project server only, in three places (decision 6)
- **Code and docs:** [`contrib-compute/`](../../../contrib-compute/README.md)
- **Follow-up:** [ADR-0020](0020-boinc-buda-evaluation.md) evaluates BUDA; separate science apps remain the proposed choice.

## Context

PR #441 adds a BOINC batch pilot to the OCR pipeline. Its worker is a Docker-wrapper app, `usnm_ocr`, that runs Tesseract on one cached page per task. Its bridge, `usnm-boinc`, reserves pages, submits them to a stock BOINC server with `create_work`, reads the stock `batch_collect_assimilator` spool, and publishes native ALTO and Parquet. The pilot leaves the server itself out: it says to create a stock Linux BOINC server and does not provision one.

This ADR covers that missing piece, the Azure hosting of a stock server, so the pilot runs on a reproducible server instead of a hand-built machine. It adds no BOINC code and does not replace anything in #441.

## Decision

1. **The stock BOINC server runs as one Container App with a private MySQL flexible server in `westus2`**, defined in `contrib-compute/infra/boinc/`. The server has one replica and does not scale to zero because the feeder and scheduler share memory and the daemons poll continuously. The daemons are those in #441's `ocr/boinc/daemons.xml` plus the stock feeder, transitioner, file_deleter and db_purge.
2. **The project tree and hand-off folders live on a private Azure Files share.** The app and its jobs mount the share over SMB with `mfsymlinks` and explicit uid/gid and mode options; `mfsymlinks` provides the symlink behavior BOINC's project tree needs. The account is private-endpoint only.
3. **Submission, reconciliation, secrets creation, project initialization and app-version publication run in Container Apps Jobs inside the VNet.** The bridge job runs `usnm-boinc submit` and `reconcile`; the secrets job creates runtime and upload secrets only when absent; the init job runs the server entrypoint's safe ensure-project mode; and the publish job installs the selected signed app version. Each job has the share, identity and settings it needs. `export` and `collect` remain on the maintainer side; the bridge jobs have no access to `ocr-out` or `curated`.
4. **The project uses the app's shared Azure Container Registry.** The stack does not create a second registry. It grants `AcrPull` on the existing shared registry to the BOINC runtime and secrets-job identities; registry admin and anonymous pull remain disabled. Server and bridge images are built and pinned by digest using ACR Tasks; input-keyed `usnm-boinc-base` and `usnm-boinc-bridge-bin` images avoid repeating the slow build layers.
5. **The master URL is a custom domain on the Container App.** Production is `https://boinc.usnewsmap.com/`; dev is `https://boinc-dev.usnewsmap.com/` and is empty unless an experiment is running. Bicep creates the CNAME and `asuid` TXT records in the existing public DNS zone. `scripts/boinc/provision-boinc.sh` advances the app binding and managed certificate through deployment stages; the production endpoint and DigiCert managed certificate were reported working in #470. Static Web Apps (Free) hosts only human-readable pages. The master URL must not change after volunteers attach.
6. **Secrets.** Everything uses Entra identities except three things, each forced by the platform or by BOINC, and each an exception to ADR-0009:
   - the MySQL password, held in Key Vault and read by the app's managed identity, because BOINC reads a fixed password from `config.xml` and cannot use Entra tokens;
   - the key of the Azure Files account, because the Container Apps Azure Files mount takes an account key;
   - the Log Analytics shared key that the Container Apps environment configuration takes.

   **Exemptions.** The guardrail definitions (`infra/guardrails.bicep`) deny a storage account with shared key access and a workspace with local auth, and both stay unchanged. The BOINC stack deploys into its own resource group, `usnm-boinc-rg-<env>`, named by `infra/naming.bicep`. `infra/main.bicep` takes `deployBoincGroup` (setting `USNM_BOINC_GROUP`, false by default, so existing deployments do not change); when it is true, `infra/main.bicep` creates that group, which holds only BOINC resources. It also assigns the same guardrail definitions to the group, and `infra/modules/boinc-exemptions.bicep` writes two Waiver exemptions on it: one for `usnm-deny-storage-shared-key` and one for `usnm-deny-monitor-local-auth`. Nothing else is waived. Each waiver is group-wide: it covers every resource in the group for that one policy, which is why the group must hold only BOINC resources. The scope is the group, not the two resources, because a resource-scoped exemption cannot exist before its target and the policy denies the target without it. So provision first, then deploy the BOINC stack. The keys these two features require live only in the platform configuration (the Container Apps environment's storage and log definitions), never in app code, the repo or a script. The Key Vault and the Files account need no public-access exception: both are private-endpoint only.

   The upload-signing key pair is kept apart from the code-signing key, which stays offline. `scripts/ci/no-shared-keys.sh` doesn't scan `contrib-compute/`; these three exceptions are what it would flag.

7. **Network and database settings match the deployed stack.** The VNet contains the Key Vault private DNS zone `privatelink.vaultcore.azure.net` and its link. MySQL `require_secure_transport` is explicitly OFF because BOINC's clients do not request TLS; #464 tracks the migration to required TLS. The MySQL endpoint is private in the VNet, with no public endpoint.
8. **The images are built from `contrib-compute/images/`** (the server and bridge job; [docs/images.md](../../../contrib-compute/docs/images.md)), from pinned BOINC source. `scripts/boinc/build-boinc-images.sh` uses ACR Tasks and caches the input-keyed `usnm-boinc-base` and `usnm-boinc-bridge-bin` images. The worker image's registry is in the addendum.
9. **The places where #441 and this hosting may not fit are recorded, not worked around.** The Docker wrapper on the client side needs Docker only on volunteers' machines, not on the server (`docs/verified-facts.md`). The bridge's single `--root`, the permissions of the Docker job under `--cap-drop=ALL`, and Azure Files permission and locking semantics are open (`docs/design.md`, section 7).

## Alternatives

- **A virtual machine.** Rejected: ADR-0005 keeps IaaS out of the project's subscription.
- **Running `submit` inside the web app container.** Rejected: it would give the public-facing replica an exec path and a second writer to the project tree.
- **Running `usnm-boinc` end to end on-prem.** Not possible as written: there is no VPN from on-prem to the VNet (ADR-0015), and the database is private.
- **Scale-to-zero for the server.** Not viable: the daemons and the shared memory segment run continuously. A pause switch (`minReplicas = 0`) saves the replica's cost, while the database and storage continue.

## Consequences

- The project gains a public scheduler, an upload handler and a web UI, and the work to keep them patched. The design's section 5 covers the controls.
- #441's pilot trusts its friends: the default validator checks nothing. Anything open to anonymous volunteers needs more validation, which is out of scope here.
- PR #470 records a successful production deployment, project initialization, public health checks, running daemons and the managed certificate. This ADR and the source configuration describe that deployment; they are not a new live acceptance test. The design keeps remaining unverified behavior explicit.

## Addendum (2026-10-13): where the worker image is published

Implemented by `.github/workflows/boinc-worker.yml` (docs/boinc.md, "Publish the worker image").

**Problem.** Each volunteer's Docker pulls the worker image: the app version's `Dockerfile` is `FROM <image>` (`scripts/boinc/install.py` in PR #441). Volunteers have no credentials for us. The project's Azure Container Registry has no anonymous pull and no admin user (design.md, 5.2), and that is right for the server images, so it cannot serve volunteers.

**Decision.** Publish the worker image as a public GHCR package under this repository: `ghcr.io/tgoodyear/usnm-boinc-worker`, linked to the repository. It is built the way #441 describes (`docs/boinc.md`, "Build the worker image"): first the engine image from `ocr/engines/tesseract/Containerfile`, then `ocr/boinc/Containerfile` with `--build-arg TESSERACT_IMAGE=sha256:<engine image id>`. The app version records the registry digest of the pushed worker image (`ghcr.io/tgoodyear/usnm-boinc-worker@sha256:<64 hex>`), not the local image id and not the engine's pin.

- The image holds Tesseract, its pinned models, and the `usnm-boinc` binary. It holds no credential, no data and no scan, so a public package discloses nothing that the repository does not already publish.
- A GHCR package is private by default. The maintainer sets its visibility once in the package settings; a workflow cannot do it. Before the first task is sent, verify an anonymous pull from a logged-out Docker on a host that is not the maintainer's (#441 asks for the same check from the volunteer runtime).
- Publishing is a workflow started by hand (`workflow_dispatch`), using the repository token, with permission to write packages. A pull request that changes the image's inputs builds it without pushing. A push to main publishes nothing: each image needs its own signed app version. Rolling the image means a new app version and a drained cohort (#441, "Build the worker image").

**Alternatives.**

- _Azure Container Registry with anonymous pull._ A registry-wide setting that exposes every repository in it, and its availability depends on the registry's SKU (UNVERIFIED). Rejected: the server images share that registry.
- _A second registry in Azure just for the worker._ Adds a fixed monthly cost for one image, against a free public package.
- _Docker Hub._ Anonymous pulls are rate limited per client address (UNVERIFIED for current limits), which fails for volunteers behind shared addresses.
- _Ship the image from the project server as a file._ BOINC's Docker wrapper does not do that.

**Consequences.** The registry digest exists only after the first push. GHCR's terms and retention for public packages are the maintainer's to check (UNVERIFIED).

## Addendum: native worker index and Mac wrapper candidates (#476)

The worker publisher builds and tests amd64 and arm64 on native runners, then
publishes an OCI index under the commit tag. App versions and cohorts pin the
index digest. Engine-base configuration IDs remain architecture-specific;
assessment and gold expectations must match the chosen native engine.

Mac wrappers are built separately on macOS from the same upstream commit as the
server, checked for the intended Mach-O slice and checksum, then passed to the
existing offline BOINC signing path. Publishing adds a platform directory without
changing siblings and registers the reviewed project platform metadata before
`update_versions`. [Release steps](../../../contrib-compute/docs/app-release.md#mac-app-versions)
remain maintainer operations. End-to-end Mac execution, lifecycle recovery and
gold agreement are still unverified; this addendum does not change the BUDA
decision.
