# ADR-0024: Cloudflare Tunnel and R2 credentials for the on-prem BOINC server

- **Status:** Accepted (2026-10-21, maintainer). In use since the #942 cutover. Amended 2026-10-25 (maintainer): the `usnm-ocrdash` tunnel (item 4).
- **Amendment 2026-10-27 (maintainer):** renewable public DNS-01 TLS for
  Cloudflare-proxied Azure Container Apps (item 5), explicitly authorized with
  the Cloudflare/Azure cutover.
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
4. **The tunnel token** for the `usnm-ocrdash` tunnel (added 25 October 2026). `cloudflared` in the
   LAN OCR dashboard's container (CT 212) runs the remotely managed tunnel with it. Its ingress is
   `ocr.usnewsmap.com` → the dashboard's public read-only listener on loopback, which serves only
   the page and its JSON, with lab host names, container numbers, paths and Azure IDs left out.
   Key Vault home-kv, secret `cloudflare-tunnel-usnm-ocrdash-token`; on the host,
   `/etc/cloudflared/usnm-ocrdash.env` (mode 0600). The LAN view and the hosts' pushes stay on the
   LAN listener.
5. **The DNS-01 validation token** for the production public origin certificate.
   It has DNS Write on the `usnewsmap.com` zone only, no account administration,
   token administration or tunnel access. Unlike the operator token, this workload
   token permits GitHub-hosted runner IPs. It expires after 90 days and is stored
   as `usnm-prod-acme-cloudflare-token` in the dedicated production vault
   `usnmtlskvprodffapzy`. The existing production GitHub
   OIDC identity has Secrets User on this secret alone and Secrets Officer on
   `usnm-prod-acme-state` alone (the ACME account and certificate private keys).
   No workload receives the operator token. The operator rotates this token
   every 60 days; the weekly renewal job fails with an explicit maintenance
   instruction when fewer than 14 days remain. See the
   [certificate runbook](../../operations/origin-tls.md).
   The vault's public endpoint supports GitHub-hosted runner egress with
   secret-level Entra RBAC; its resource-scoped public-network policy exception
   covers only the DNS token and ACME state. The home vault retains its
   operator-only IP firewall, and no workload may read its operator token.

Rules for all applicable credentials:

- Kept in Azure Key Vault, read through Entra RBAC; never in the repository or on a command line,
  and never printed. The one copy outside Key Vault of each tunnel token is on its host, in a
  root-only environment file (`/etc/cloudflared/<tunnel>.env`, mode 0600) that `cloudflared`
  reads at start.
- Scoped as narrowly as Cloudflare allows (one tunnel; one bucket; one client IP), with an expiry
  where Cloudflare supports one.
- This adds no Azure credential. Everything on the Azure side stays Entra-only.

## Consequences

- Rotation is manual: a new token in Cloudflare, then the Key Vault secret. For the tunnel
  token, the BOINC host's copy changes too. Run home-infra's `boinc-ct.sh --tunnel` (in
  `scripts/lab/`, on the Proxmox host, with the new token in a root-only file): it rewrites the
  container's environment file and restarts `cloudflared`. Revoke the old token only after
  that, or the restarted connector can't reconnect. Expired or revoked tokens fail closed (the
  tunnel stops serving; the relay gets 403).
- The R2 relay token was temporary. On 22 October 2026, after the project storage accounts
  were gone (#942, #960), the bucket `usnm-boinc-project` was emptied and deleted, the relay
  token was revoked, and its key pair was deleted from both vaults. Exception 2 no longer
  applies. Exceptions 1 and 3 remain.
- From 31 October 2026 the public bucket `usnm-boinc-models` serves the Qwen model files
  ([docs/boinc-onprem.md](../../boinc-onprem.md)). Clients read it without credentials. This
  amends the decision above with one more exception: an account API token limited to writing
  objects in that bucket, expiring 31 October 2027, whose S3 form is stored in the home Key Vault
  as `cloudflare-r2-*`. It's used only from an operator's run to upload objects. No workload holds
  it, and it stays in memory or `/dev/shm` for that run.
- Moving serving on-prem (#955) would add tunnels under the same rules, with no new kind of credential.
- The production origin certificate now uses Let's Encrypt DNS-01 rather than
  Azure's direct-DNS managed-certificate renewal. Its private ACME state is
  encrypted in Key Vault and materialized only in a mode-0700 working directory
  while Certbot runs, removed on exit; never in Actions artifacts or caches.
