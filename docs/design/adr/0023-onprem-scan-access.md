# ADR-0023: Authenticated, read-only on-prem scan access for the BOINC bridge

- **Status:** Proposed (design only; nothing implemented or deployed; needs decisions P1 to P4 in [design 16](../16-tank-replacement-and-scan-access.md#167-decisions-needing-the-maintainers-approval))
- **Date:** 2026-10
- **Relates to:** ADR-0008, ADR-0009, ADR-0015, ADR-0018, ADR-0019, ADR-0022

## Context

[ADR-0022](0022-onprem-cold-storage.md) moves cold data to `tank`. The scan keep set is already there. The BOINC bridge reads scans from the Azure archive with its managed identity and falls back to LoC (ADR-0018). Retiring the Azure scan copy (about $16 a month) means the bridge would read from `pve-big`, which has no VPN to Azure (ADR-0015, ADR-0019) and whose existing `serve-scans` server is LAN-only plain HTTP that also accepts `DELETE` and `PUT`.

## Decision

1. **Never expose `serve-scans` or any general file, S3 or NFS server.** Any reader reachable from Azure is a separate, read-only, authenticated service.
2. **Default route: no inbound exposure.** Stage the pages a run needs to the Azure exchange container from on-prem with the Arc identity (as the scan fetcher already does), and keep the Azure archive as fallback. Choose this unless the maintainer wants the Azure scan copy retired.
3. **If a reader is approved (`usnm-scan-gateway`):** it serves only `GET`/`HEAD /v1/scans/keep/<batch>/<lccn>/<date>/<edition>/<seq>.<jp2|jpg>`, with strict segment validation, sidecar sha256, md5, `iiif_base` and `loc_url` returned as headers from an on-prem manifest, a restricted filesystem namespace that exposes only a read-only `keep/` mount, no symlink or `..` traversal, no listing and no write verbs. Every request carries an Entra bearer token validated for signature (tenant JWKS), issuer, `tid`, the gateway's **own audience** (pinned with the token version: the client ID GUID for v2.0 tokens), expiry, and the bridge managed identity's `oid` (plus an app role). No secret or certificate is created. It reuses `usnm-scan-reader`'s router and integrity design with a filesystem reader and an Entra authenticator; tickets remain for volunteer clients.
4. **Transport:** TLS, preferably an outbound-only tunnel (no inbound port); IP allowlists only as defence in depth (consumption outbound IPs change, ADR-0008). No Azure VPN gateway (cost exceeds the saving).
5. **Failure behavior:** the bridge's chain becomes gateway, then the Azure archive copy (while it exists), then LoC. Any 401/403, 404, 429, 5xx, timeout or checksum mismatch falls through. Hand-off and coordination stay in Azure.
6. **Rollout:** shadow reads, then primary with Azure fallback, then retire the Azure copy only after sha256 verification of every blob against `tank`, a clean scrub, the Cold-tier and soft-delete retention, and the maintainer's approval of that specific deletion. Rollback before retirement is switching the source setting back; after retirement it is a restore of scans and sidecar metadata to Azure with verification, because the setting alone would point at deleted blobs.

## Alternatives

- **VPN gateway:** about $139 a month, more than the saving; contradicts ADR-0015.
- **Reuse `serve-scans`:** unauthenticated, plain HTTP, has write verbs.
- **SAS or shared-key style signed links:** ADR-0009 forbids them for the bridge; Entra only.
- **Garage or MinIO S3 on-prem:** a larger surface than needed; revisit only if several Azure readers appear.

## Consequences

- A new internet-reachable service and new JWT-validation code to build and review; both are risks listed in design 16.
- The Azure scan copy stays until D1, P4 and the checksum gates are met, so there is no saving from this ADR until then.
- A new Entra app registration and app role would be needed (an Azure change, not made here).
