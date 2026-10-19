# ADR-0023: On-prem scans reach the BOINC bridge by outbound-only staging into Azure

- **Status:** Accepted as direction (2026-10-19, maintainer); design only, nothing implemented. The gateway, Entra app registration and tunnel are unselected alternatives. See [design 16](../16-tank-replacement-and-scan-access.md#166-scan-access-for-the-boinc-bridge-outbound-only-staging-chosen-direction)
- **Date:** 2026-10
- **Relates to:** ADR-0008, ADR-0009, ADR-0015, ADR-0018, ADR-0019, ADR-0022

## Context

[ADR-0022](0022-onprem-cold-storage.md) moves cold data to `tank`. The scan keep set is already there. The BOINC bridge reads scans from the Azure archive with its managed identity and falls back to LoC (ADR-0018). Retiring the Azure scan copy (about $16 a month) means the bridge would read from `pve-big`, which has no VPN to Azure (ADR-0015, ADR-0019) and whose existing `serve-scans` server is LAN-only plain HTTP that also accepts `DELETE` and `PUT`.

## Decision

1. **Never expose `serve-scans` or any general file, S3 or NFS server.** No public gateway is built.
2. **Outbound-only staging.** On-prem stages the pages a run needs into the Azure exchange account with the Arc identity (as the scan fetcher already does). The bridge reads them with its managed identity. Hand-off and coordination stay in Azure; nothing on `pve-big` is reachable from Azure or the internet.
3. **Bridge read order:** staged copy, then the Azure archive (while it exists), then LoC. A missing staged page, a 404, 429, 5xx, timeout or checksum mismatch falls through; the run never waits on staging.
4. **Implementation requirements** (future, each separately reviewed; design 16 §16.6): a durable per-run page list (today the hand-off job only consumes a manifest, and assess, Surya and quality select pages themselves); an on-prem staging writer that verifies sha256 locally and after upload, carries sha256/md5/`iiif_base`/`loc_url`, is idempotent and never deletes; a staging prefix in the exchange account (writer for Arc, read for the bridge only; no new account, key or SAS); lifecycle expiry for staging only; bridge changes in each stage; lag alerts; an uplink cap.
5. **Rollout:** shadow (stage and compare), then staging primary with archive fallback, then retire the Azure scan copy only after sha256 of every blob equals `tank`, a clean scrub, Cold-tier accounting and the maintainer's specific approval. Rollback before retirement is pointing the bridge back at the archive; after retirement it is a restore of scans and sidecar metadata to Azure with verification (blob soft delete is off on the archive, so prefix deletion is irreversible).

## Alternatives

Unselected; kept for the record, none approved:

- **Authenticated read-only gateway on `pve-big`** (reusing `usnm-scan-reader`; Entra token with issuer, tenant, own audience, `oid` and `Scans.Read` checks; restricted filesystem namespace; sidecar headers from a manifest of verified local sidecars). Needs a new Entra app registration and new internet-facing code. Specified in design 16 §16.6 as an alternative only.
- **Outbound tunnel or port forward** for such a gateway. A tunnel connector carries a non-Entra credential, so it would need an explicit ADR-0009 exception.
- **VPN gateway:** about $139 a month, more than the saving; contradicts ADR-0015.
- **Reuse `serve-scans`:** unauthenticated, plain HTTP, has write verbs.
- **SAS or shared-key links:** forbidden by ADR-0009.
- **Garage or MinIO S3 on-prem:** a larger surface than needed.

## Consequences

- No inbound exposure of the home network and no new Entra registration.
- New work: page list, staging writer and bridge read-order changes. Staging adds Azure transactions and short-lived blobs, and staging lag falls back to the archive or LoC.
- The Azure scan copy stays until D1, the retirement approval and the checksum gates are met, so this ADR saves nothing by itself.
