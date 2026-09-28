# ADR-0009: Entra identity only, no shared keys

- **Status:** Accepted (one open exception, below)
- **Date:** 2026-09
- **Strengthens:** ADR-0008 and 08 §8.2, which disabled keys on the data services only

## Context

The data services already refused keys: Blob (`allowSharedKeyAccess: false`) and Cosmos (`disableLocalAuth: true`), both enforced by policy. A review found shared keys elsewhere in the stack:

- the Container Apps environment sent logs to Log Analytics with the workspace's shared key;
- Log Analytics and Application Insights still accepted key-based ingestion;
- the Static Web App deploys with its deployment token.

The owner made the rule foundational: **every authentication is by an Entra identity authorized with Azure RBAC.**

## Decision

1. **Every caller is an Entra principal.**
   - Workloads use managed identities: user-assigned (`id-usnm-app`, `id-usnm-ingest`), or system-assigned where a component can't pick a user-assigned one (the Quickwit sidecar and writer, 08 §8.2).
   - CI uses a user-assigned identity with a GitHub OIDC federated credential (`id-usnm-ci-{env}`).
   - People use their own Entra accounts.
   - Every grant is an Azure RBAC role assignment (or a Cosmos data-plane role assignment), scoped as narrowly as Azure allows.
2. **No shared secrets of any kind.** That means no account keys, no connection strings with keys, no SAS (account, service or user-delegation), no workspace keys or instrumentation-key ingestion, no registry admin user or repository tokens, and no stored secrets in GitHub. If a service offers a switch to turn local auth off, it's off:

   | Service | Setting |
   |---|---|
   | Storage (data, tiles) | `allowSharedKeyAccess: false` |
   | Cosmos DB | `disableLocalAuth: true`, `disableKeyBasedMetadataWriteAccess: true` |
   | Log Analytics | `features.disableLocalAuth: true` |
   | Application Insights | `DisableLocalAuth: true` |
   | Container Registry | `adminUserEnabled: false`, no anonymous pull |
   | Container Apps environment | logs through a diagnostic setting (`appLogsConfiguration: azure-monitor`), not the workspace key |

   Short-lived tokens that Entra issues for a principal count as Entra authentication, because they carry that principal's RBAC. These are OAuth access tokens, and the registry refresh token that `az acr login` exchanges for an Entra token.
3. **Enforced, not just configured.**
   - Azure Policy **denies** each "local auth on" setting above on the project resource groups (`infra/modules/policy-definitions.bicep`).
   - CI fails any change that reintroduces a key pattern (`scripts/ci/no-shared-keys.sh`, in the `infra` job).
   - The Rust storage client refuses URLs that carry a SAS query.

## Open exception: the Static Web App deployment token

Static Web Apps has no Entra-only deploy path. Every deploy path ends in the site's deployment token, including the GitHub action, the SWA CLI and `az staticwebapp`. See [Azure/static-web-apps#1359](https://github.com/Azure/static-web-apps/issues/1359).

Today CI reads the token at deploy time. The read is authorized by RBAC (`Microsoft.Web/staticSites/listSecrets/action` in the "usnm deployer" role), and the token is never stored. It is still a shared secret, so it breaks rule 2. It's listed as the only exception in `scripts/ci/no-shared-keys.sh` until the owner picks a resolution:

- **Serve the site from Container Apps** instead of SWA: from the API app on the site's hostnames, or from a small separate app. Deploys become an image push plus a revision, both Entra-only.
  - Cost: none if the API serves the site; a few dollars a month for a separate warm replica.
  - Loses SWA's global static distribution.
  - The apex domain moves from an alias record to an A record on the environment's static IP, with a free managed certificate.
- **Keep SWA and rotate the token after every deploy** (`az staticwebapp secrets reset-api-key`), so each token is single-use. This narrows the exposure but keeps a shared secret.
- Wait for Entra deploys in SWA.

## Consequences

- Nothing in the stack can be reached with a leaked key, because no service here accepts one. The only exception is the SWA deploy token above.
- Portal features that rely on keys stop working. For example, the Log Analytics "agents" key blade, and the legacy Application Insights ingestion by instrumentation key. Telemetry, if the API ever sends it, must use the Azure Monitor OpenTelemetry exporter with a managed identity (Monitoring Metrics Publisher on the component).
- The Quickwit fallback in 08 §8.2 changes. If the sidecar can't authenticate with its managed identity, it can't fall back to a user-delegation SAS. The fallback is a localhost proxy in the `api` container that adds the app identity's bearer token.
