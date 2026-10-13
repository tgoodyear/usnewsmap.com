# ADR-0018: BOINC as the contributor compute layer

- **Status:** Proposed
- **Date:** 2026-10
- **Extends:** ADR-0015 (on-prem OCR workers) and ADR-0016 (pluggable OCR engines)
- **Amends:** ADR-0009 for the BOINC project server only, in four places (decision 5)
- **Code and docs:** [`contrib-compute/`](../../../contrib-compute/README.md)

## Context

[ADR-0015](0015-onprem-ocr-workers.md) runs OCR on the maintainer's own machines. The same work, mostly pre-1920s German pages (Fraktur is the hypothesis, not checked) and the worst-read English batches, as ADR-0015 lists, could also use spare machines that other people offer. Those machines can't have a managed identity or a role on the project's storage, and the project should not hold a credential for them.

A first design for this was a custom enrollment and work broker: an Ed25519 key per contributor, an admin app to approve keys, and signed lease requests. It would be a protocol, a service and a worker image for the maintainer to build, host and keep secure. BOINC already provides accounts, a scheduler, file transfer with signed uploads, adaptive replication of results and signed app files, and its client is packaged for Debian, Ubuntu and Fedora and published as an image.

## Decision

1. **Contributors run the BOINC client, attached to one project server run by the maintainer.** The client needs a project URL and an account key. It holds no Azure credential and no storage key. The only access it receives is the read-only SAS URLs in each workunit's downloads and the signed upload certificates BOINC issues.
2. **There are three ways to host the client, plus a script for a plain machine** (`contrib-compute/scripts/`):
   - a stock Debian container on Proxmox with Debian's `boinc-client`;
   - the official `boinc/client` image on Docker or Podman;
   - a Spot VM scale set or a container group in the contributor's own Azure subscription, with the account key in a Key Vault in that subscription.

   No custom LXC template, worker image or registry is built or hosted. The project's no-IaaS policy (ADR-0005) covers the project's subscription; the contributor's VMs are in theirs.

3. **The OCR app is native.** The app is a static Tesseract plus `usnm-ocr-run`, started by BOINC's generic wrapper. Docker-wrapper apps cannot run in ACI, are unreliable in an unprivileged container (untested), and BOINC cannot use Podman on Unix for them, so they are not used.
4. **The server runs in Azure as one Container App with a MySQL flexible server**, in Bicep (`contrib-compute/infra/boinc/`). It is one replica, because the feeder and the scheduler share memory. A work generator stages pages and writes workunits, with a daily page cap. A validator compares replicas: new hosts have every job replicated, trusted hosts are sampled. An assimilator writes results to `ocr-out` with provenance, where `ocr-sync` checks them as it checks the project's own nodes' output.
5. **Secrets.** Everything on the server uses Entra identities except four things, each forced by the platform or by BOINC, and each an exception to ADR-0009:
   - the MySQL password, held in Key Vault and read by the app's managed identity, because BOINC reads a fixed password from `config.xml` and cannot use Entra tokens;
   - the key of the Azure Files account that holds BOINC's upload and download directories, because the Container Apps Azure Files mount takes an account key (shared-key access stays off on the staging blob account);
   - the Log Analytics shared key that the Container Apps environment configuration takes;
   - user-delegation SAS URLs in workunit rows, because the clients are outside Azure.

   The upload-signing key pair is kept apart from the code-signing key, which stays offline. `scripts/ci/no-shared-keys.sh` doesn't scan `contrib-compute/`; these are the places it would flag.

6. **The job type where a contributor's machine downloads scans from the Library of Congress (`fetch-and-ocr`) is written and tested but off by default.** Each client would run at a low request rate, and the type is not a way around the source's limits; a better place to get the images is preferred if one exists. The source has not approved it. Volunteers' addresses would be seen by the source and a block would affect only that volunteer; this is an accepted risk.
7. **The earlier Ed25519 enrollment path is not part of this change.** BOINC accounts, the scheduler and the validator replace it.

## Alternatives

- **The custom broker and admin app.** Rejected: more to build and secure than the problem needs, and BOINC already provides these pieces and is maintained (latest server release 1.6.2, 2026-06-12).
- **More Azure compute in the project's subscription.** Rejected for the volume: ADR-0015's context puts cloud OCR far above the project's budget.
- **Docker-wrapper BOINC apps.** Rejected: they need a container runtime inside the client's host.
- **Scale-to-zero for the server.** Not viable: the daemons and the shared memory segment run continuously. A pause switch (`minReplicas = 0`) saves the replica's cost, while the database and storage continue.

## Consequences

- The project gains a public scheduler, an upload handler and a web UI, and the work to keep them patched. The design's section 5.5 covers the controls.
- Results come from untrusted hosts, so quality depends on replication and the sampled re-OCR; the sample rate and thresholds are not chosen yet.
- Egress of scans from the project's storage is the one cost that grows with volume. A cap in the work generator sets the ceiling.
- Nothing is deployed. The design lists what is unverified: the MySQL flexible server's fit with BOINC's tables, Azure Files semantics under Container Apps, and a static Tesseract build giving the same text as the pinned image.
