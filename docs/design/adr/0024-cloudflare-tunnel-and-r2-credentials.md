# ADR-0024: Cloudflare Tunnel and R2 credentials for the on-prem BOINC server

- **Status:** Accepted (2026-10-21, maintainer). In use since the #942 cutover.
- **Date:** 2026-10
- **Relates to:** ADR-0009, ADR-0023; #942, #955; `docs/boinc-onprem.md`

## Context

[ADR-0009](0009-entra-identity-only.md) allows Entra identities only: no keys, SAS, connection
strings or stored secrets. [ADR-0023](0023-onprem-scan-access.md) notes that a tunnel connector
"would need an explicit ADR-0009 exception".

The BOINC server moved to an on-prem host in #942. The host has no inbound port. Public ingress for
`boinc.usnewsmap.com` comes through a Cloudflare Tunnel, and the project tree came out of Azure
through a Cloudflare R2 bucket, because the project share's storage account allows no public
access. Neither Cloudflare service accepts an Entra identity.

## Decision

An exception to ADR-0009 covers exactly these Cloudflare credentials:

1. **The tunnel token** for the `usnm-boinc` tunnel. `cloudflared` on the BOINC host runs the
   remotely managed tunnel with it. The token can only run that tunnel; the tunnel's ingress is
   `boinc.usnewsmap.com` → `http://localhost:80`.
2. **The R2 relay key pair**: an S3 key pair for one account API token that may only read and
   write objects in bucket `usnm-boinc-project`, with an expiry date. The BOINC jobs read it from
   the BOINC Key Vault with their managed identity; the BOINC host reads it with its Arc identity.
   It lives only in process environments and `/dev/shm` while a job runs.
3. **The Cloudflare API token** used to manage the zone, the tunnel and the bucket. It is limited
   to one client IP and is never given to a workload.

Rules for all three:

- Kept in Azure Key Vault, read through Entra RBAC; never in the repository or on a command line,
  and never printed. The one copy outside Key Vault is the tunnel token on the BOINC host, in a
  root-only environment file (`/etc/cloudflared/usnm-boinc.env`, mode 0600) that `cloudflared`
  reads at start.
- Scoped as narrowly as Cloudflare allows (one tunnel; one bucket; one client IP), with an expiry
  where Cloudflare supports one.
- This adds no Azure credential. Everything on the Azure side stays Entra-only.

## Consequences

- Rotation is manual: a new token in Cloudflare, then the Key Vault secret. Expired or revoked
  tokens fail closed (the tunnel stops serving; the relay gets 403).
- The R2 relay token is temporary: delete it, the bucket's copy of the project and its secrets
  once the project storage accounts are gone (#942).
- Moving serving on-prem (#955) would add tunnels under the same rules, with no new kind of credential.
