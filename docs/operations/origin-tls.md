# Production origin TLS

The apex, `www` and `api` use one public Let's Encrypt DNS-01 certificate in
`cae-usnm-prod`, named `usnm-public-dns01`. Its resource ID is stable across
renewal. Cloudflare must use **Full (strict)**. DNS-01 works with proxied DNS;
Azure managed-certificate direct-DNS renewal does not. Keep `letsencrypt.org`
authorized in the zone's CAA records (retain DigiCert until migration completes).

## Provision and validate

Run from the repository root as the authorized operator, signed in with `az
login`. Requires `python3`, `certbot`, `openssl` and `dig`. Certbot's manual hooks
use only Python's standard library; no Cloudflare plugin or token file is needed.

```sh
python3 scripts/azure/origin_tls_bootstrap.py
python3 scripts/azure/origin_tls.py renew --staging --no-bind
python3 scripts/azure/origin_tls.py renew --no-bind
```

Bootstrap creates the 90-day zone-DNS-only token with the IP-restricted operator
token **in memory**, saves it directly to Key Vault, and grants the existing
production OIDC identity secret-level access plus environment certificate
read/write (no certificate delete). It is idempotent and rotates tokens with
fewer than 30 days left, revoking the old token only after saving the replacement.
It does not grant access to the operator token or other vault secrets.

The two workload secrets live in the dedicated production vault
`usnmtlskvprodffapzy`, not the home-infrastructure vault. Bootstrap provisions
this vault with RBAC, purge protection and 90-day soft deletion. GitHub-hosted
runners have changing egress IPs, so this vault has a public endpoint, no
trusted-service bypass and secret-level Entra authorization. A tracked,
resource-scoped `Mitigated` policy exemption (`usnm-origin-tls-github`) covers
only this vault. It does not weaken the resource group's public-data policy,
home-kv's IP firewall, or any other vault. The production deployment identity
has no vault-management or firewall-write permission, only the two previously
described secret-scoped roles. Only the operator gets vault-level Secrets
Officer. The IP-restricted operator token remains in home-kv and is read from
there solely by bootstrap on the authorized operator network.
All Azure CLI resource operations explicitly target the production subscription, not
the operator's selected CLI default. Bootstrap idempotently enables the vault's
`AuditEvent` diagnostic category in `log-usnm-prod` for new and existing
vaults, recording secret access, mutations and denied requests without secret
values, consistent with the production diagnostic-settings convention.
After provisioning, bootstrap retries initial secret-read HTTP 403 responses
for up to five minutes while the operator RBAC grant propagates. HTTP 404
means the vault is accessible but the token has not been created; other errors
and a persistent 403 fail without creating or replacing credentials.

Staging exercises all DNS challenges without changing stored production state,
uploading an untrusted staging certificate, or binding hostnames. Production
state is persisted **before** upload; a failed deployment retries the same
certificate rather than issuing another one. Loss of state is a recovery event:
restore a previous Key Vault secret version rather than repeatedly issuing.

Once issued and approved, preserve the current `customDomains` as a rollback
receipt, then bind with:

```sh
python3 scripts/azure/origin_tls.py renew
```

This patches only ingress custom domains and verifies all three bindings; it
never changes public DNS, Cloudflare settings, app secrets or revisions. Set
the existing private prod `USNM_API_CERT_ID`, `USNM_SITE_CERT_ID`
and `USNM_WWW_CERT_ID` settings to the uploaded certificate ID using
`scripts/azure/settings.sh`, so subsequent provisioning preserves the bindings.
Do not delete the previous managed certificates until cutover is validated.

## Renewal and credential maintenance

`.github/workflows/origin-tls.yml` runs weekly on main using the existing prod
environment and Azure OIDC identity. It reuses the saved ACME account, issues
when the certificate has fewer than 30 days remaining (normally about every
60 days), and uploads to the same certificate ID. It deliberately does not
patch the application or race deployments. There are no private-key caches,
artifacts, long-lived Azure credentials or operator-machine dependencies.
After merging, dispatch the workflow once and verify it succeeds. Scheduled
workflows do not run from an unmerged feature branch.
For a real DNS-01 issuance test under the same production OIDC identity,
dispatch with `validate-dns=true`. This exercises the staging CA, then reuses or
renews the production certificate normally; staging state is never persisted.

**Every 60 days**, run `origin_tls_bootstrap.py` as the operator to rotate the
90-day DNS token. This is intentional human credential maintenance, not a
certificate-renewal dependency on a particular workstation. Subscribe to failed
Actions notifications for `origin-tls`: failures include the explicit rotation
instruction once token validity falls below 14 days. The token expiration is
also recorded in Key Vault secret attributes. Rotation needs the authorized
operator IP; do not broaden or give the operator token to CI.

Certificate/account keys exist on disk only under `.origin-tls-work` (0700,
files 0600) during execution. Normal exit removes them; the workflow's always
step also cleans up cancellation. Never enable shell tracing, upload this
directory, or cache it. An interrupted local invocation may leave this protected
directory: ensure no renewal is running before removing it and retrying.

Verify the certificate expiry and SANs at the Azure origin with SNI for all
three names, then verify the public names after proxying. A renewal failure
does not replace the currently serving certificate. The certificate is public
trust, not Cloudflare Origin CA: bypassing Cloudflare still passes normal TLS
validation, while application trust of proxy headers must remain independent.

## Live acceptance (27 October 2026)

Staging and production DNS-01 issuance succeeded. The production certificate
expires **25 January 2027, 20:32:33 UTC**, and direct connections to the Azure
origin passed system trust and SNI/hostname validation for all three names.
All three actual private settings (`USNM_API_CERT_ID`, `USNM_SITE_CERT_ID`,
`USNM_WWW_CERT_ID`, as consumed by `infra/main.bicepparam`) were repaired and
read back successfully during review. The mistakenly named
`USNM_*_CERTIFICATE_ID` entries were removed and verified absent.
The prior managed bindings are retained in the ignored operator receipt
`.azure/receipts/origin-tls-bindings-before-2026-10-27.json`; no managed
certificate was deleted. The DNS token expires 25 January 2027: first operator
rotation is due **26 December 2026**. The scheduled job must be dispatched and
checked after merge to main; local acceptance does not claim a GitHub OIDC run.

The first real GitHub OIDC run (`40592972233`) failed because home-kv permits
only the operator IP; its principal and secret-level grants were correct.
The DNS token and existing ACME state were subsequently copied **in memory**
to the dedicated vault and compared without logging their values, avoiding
reissuance or a new account. No home-kv network rule was changed.
