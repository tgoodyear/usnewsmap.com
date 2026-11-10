# Production origin TLS

Part of the [operations guide](README.md).

The apex, `www` and `api` share one public Let's Encrypt DNS-01 certificate, `usnm-public-dns01` ([ADR-0028](../design/adr/0028-cloudflare-with-azure-origin.md)). The weekly `origin-tls` workflow renews it in Key Vault. Since #308 no Azure app carries the custom domains: on-prem serves through the Cloudflare Tunnel and the spare is reached by its default hostname, so the renewed certificate stays in Key Vault and isn't uploaded or bound (`AZURE_UPLOAD = False` in `scripts/azure/origin_tls.py`). The [spare runbook](search-serving.md#the-spares-environment-without-a-vnet) has the steps to bind the domains to Azure again. Cloudflare must use **Full (strict)**. DNS-01 works with proxied DNS; Azure managed-certificate direct-DNS renewal doesn't. Keep `letsencrypt.org` authorized in the zone's CAA records.

## Provision and validate

Run from the repository root as the authorized operator, signed in with `az login`. It needs `python3`, `certbot`, `openssl` and `dig`. Certbot's manual hooks use only Python's standard library; no Cloudflare plugin or token file is needed.

```sh
python3 scripts/azure/origin_tls_bootstrap.py
python3 scripts/azure/origin_tls.py renew --staging --no-bind
python3 scripts/azure/origin_tls.py renew --no-bind
```

**Bootstrap** creates the 90-day zone-DNS-only token with the IP-restricted operator token, in memory, saves it straight to Key Vault, and grants the production OIDC identity secret-level access plus environment certificate read and write (no certificate delete). It is idempotent and rotates tokens with fewer than 30 days left, saving the replacement before it revokes the old token. It grants no access to the operator token or other vault secrets. After provisioning, it retries a first secret read's HTTP 403 for up to five minutes while the operator's role propagates; a 404 means the vault is reachable but the token doesn't exist yet; other errors and a persistent 403 fail without creating or replacing credentials.

**The vault.** The two workload secrets live in the dedicated production vault `usnmtlskvprodffapzy`, not the home-infrastructure vault. Bootstrap provisions it with RBAC, purge protection and 90-day soft deletion, and enables its `AuditEvent` diagnostics in `log-usnm-prod` (secret access, changes and denied requests, without values). GitHub-hosted runners have changing egress IPs, so this vault has a public endpoint, no trusted-service bypass and secret-level Entra authorization, under a resource-scoped `Mitigated` policy exemption (`usnm-origin-tls-github`) for this vault alone. The production deployment identity has no vault-management or firewall-write permission, only the two secret-scoped roles; only the operator gets vault-level Secrets Officer. The IP-restricted operator token stays in the home-infrastructure vault, read from there only by bootstrap on the authorized operator network. Every Azure CLI call names the production subscription.

**Staging** exercises every DNS challenge without changing stored production state, uploading an untrusted certificate or binding hostnames. Production state is saved before upload, so a failed deployment retries the same certificate rather than issuing another. Losing the state is a recovery event: restore a previous Key Vault secret version; don't issue again.

**Binding to Azure** (needs `AZURE_UPLOAD` set): save the app's current `customDomains` so you can roll back to them, then run `python3 scripts/azure/origin_tls.py renew`. That patches only ingress custom domains and checks all three bindings; it never changes public DNS, Cloudflare settings, app secrets or revisions. Then set the private prod settings `USNM_API_CERT_ID`, `USNM_SITE_CERT_ID` and `USNM_WWW_CERT_ID` to the uploaded certificate's ID with `scripts/azure/settings.sh`, so later provisions keep the bindings.

### BOINC LAN certificate (#1357)

`origin_tls.py --target boinc-lan` issues a separate certificate for `boinc.usnewsmap.com` over the same DNS-01 path, with its own ACME state (`usnm-prod-boinc-lan-acme-state`). It never touches Azure ingress: it writes the private key and full chain only to the deploy secret `usnm-prod-boinc-lan-tls`, which CT 230's Arc identity can read (Secrets User on that secret alone). The weekly workflow renews it after the origin certificate. Grants: `origin_tls_bootstrap.py --boinc-lan`. The host side is in [the on-prem BOINC server page](../boinc/server/onprem.md#lan-access-1357).

## Renewal and credential maintenance

`.github/workflows/origin-tls.yml` runs weekly on `main` with the prod environment and its Azure OIDC identity. It reuses the saved ACME account and issues when the certificate has fewer than 30 days left (normally about every 60 days). There are no private-key caches, artifacts, long-lived Azure credentials or operator-machine dependencies. Scheduled workflows don't run from an unmerged branch, so after a change merges, dispatch the workflow once and check it succeeds. For a real DNS-01 issuance test under the production OIDC identity, dispatch it with `validate-dns=true`: that exercises the staging CA, then reuses or renews the production certificate normally; staging state is never saved.

**Every 60 days**, run `origin_tls_bootstrap.py` as the operator to rotate the 90-day DNS token; rotation needs the authorized operator IP, and the operator token never goes to CI. Subscribe to failed Actions notifications for `origin-tls`: once the token has under 14 days left, failures name the rotation. The token's expiry is also in its Key Vault secret attributes. To rotate before the automatic 30-day threshold:

```sh
python3 scripts/azure/origin_tls_bootstrap.py --rotate-dns-token
```

This needs a tracked predecessor, replaces only the zone-DNS credential in the TLS vault, then revokes the predecessor once the replacement is saved. It doesn't provision resources, change Entra grants or touch ACME state. Afterwards dispatch `origin-tls` with `validate-dns=true` and require a successful staging DNS-01 and reuse of the production certificate. Record the new token's expiry and next rotation date; never log either token.

Certificate and account keys exist on disk only under `.origin-tls-work` (mode 0700, files 0600) while the script runs. A normal exit removes them, and the workflow's `always` step cleans up after a cancellation. Never enable shell tracing, upload this directory or cache it. An interrupted local run may leave it: check no renewal is running before removing it and retrying.

A renewal failure doesn't replace the current certificate. The certificate is publicly trusted, not Cloudflare Origin CA, so bypassing Cloudflare still passes normal TLS validation; the application's trust of proxy headers must not depend on it.

## Live acceptance (27 October 2026)

Staging and production DNS-01 issuance succeeded. The production certificate expires **25 January 2027, 20:32:33 UTC**, and direct connections to the Azure origin, then bound, passed system trust and SNI hostname validation for all three names. The DNS token also expires 25 January 2027, so the first operator rotation is due **26 December 2026**.

The first GitHub OIDC run (`40592972233`) failed because the home-infrastructure vault allows only the operator IP. The DNS token and ACME state were then copied in memory to the dedicated vault, without logging their values, which avoided reissuing or a new account.
