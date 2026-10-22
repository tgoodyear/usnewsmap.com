# ADR-0025: Client certificates for on-prem databases

- **Status:** Proposed (2026-10). In use by the OCR queue database (#967).
- **Date:** 2026-10
- **Relates to:** ADR-0009, ADR-0024; #967, #972, #955; home-infra `scripts/lab/queue-ct.sh`

## Context

[ADR-0009](0009-entra-identity-only.md) allows Entra identities only: no keys, SAS, connection
strings or stored secrets. It was written for clients of Azure services.

The OCR work queues are moving from Azure Storage Queues to PostgreSQL with pgmq in an on-prem
container, `usnm-queue` (#967). Its clients are on-prem too: the BOINC claim and collect steps,
ingest's producers and the OCR dashboard. On-prem PostgreSQL can't check an Entra token without
an Azure-hosted login flow, and nothing on-prem accepts inbound connections from Azure. Passwords
would be stored secrets with no scope beyond the role.

## Decision

On-prem services authenticate to on-prem PostgreSQL with TLS client certificates, and nothing
else:

1. **One CA per database container**, created inside it (`/etc/postgresql/usnm-ca`, root only,
   ten years). It signs only the server certificate and the client certificates. Its key never
   leaves the container.
2. **One role per client, named by the certificate's CN.** `pg_hba.conf` allows network clients
   only as `hostssl … cert` from the container's subnet, so the CN must equal the role.
   Locally, only peer authentication is allowed. No role has a password. Each role gets only what
   its row in `scripts/onprem/queue/README.md` lists; `setup.sql` grants it and
   `test_roles.sql` checks it in CI. The queue has three roles: `boinc_claim`,
   `ingest_producer` and `dash_reader`.
3. **TLS 1.3 only.** Clients verify the server certificate against the CA (`sslrootcert`;
   `usnm-store` verifies the host name).
4. **Client keys stay on the host that uses them**, with the key file root-only (mode 0600). They
   are copied from the database container to that host through the Proxmox host (`pct pull`,
   `pct push`), never through a workstation, the repository or Key Vault. Certificates last 825
   days.

This adds no Azure credential, and Azure workloads never connect to these databases. A future
on-prem database for serving (#955) uses the same model with its own CA and roles.

## Consequences

- There's no revocation list, so a compromised client's role name is retired for good:
  1. `ALTER ROLE … NOLOGIN`, then end its sessions, which `NOLOGIN` doesn't touch (`usnm-store`
     keeps its connection open):
     `SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE usename = '<role>'`.
  2. In the same change, take the name out of `setup.sql` and `test_roles.sql`. `setup.sql`
     sets every role it manages to `LOGIN`, so a rerun that still lists the name would let the
     stolen certificate back in.
  3. Give the client a new role name with the same grants, and issue its certificate for that
     name.

  If the CA key is exposed, make a new CA, reissue every certificate, and restart PostgreSQL to
  load the new CA (a restart ends every session).

- Renewal is manual, before the 825 days run out: `queue-ct.sh --issue-client --role <name>`, then
  copy the new certificate and key to the client and restart it.
- The CA key is in the container's nightly Proxmox Backup Server backups (home-infra #55). Anyone
  who can restore those can issue certificates, but they can already open the container as
  root.
- Every new client needs a role, a grant in `setup.sql` and a row in `test_roles.sql`. CI fails if
  a role can do more or less than its row says.
