# ADR-0008: Private networking for app → data paths

- **Status:** Accepted
- **Date:** 2026-09
- **Amends:** ADR-0006, which had deferred all network isolation to the growth profile

## Context

Every app/job → storage/Cosmos path already used managed identities, with keys disabled. But the data services' public endpoints were open (Entra-only) because the ACI Spot backfill workers can't join a VNet and have no stable outbound IP. The maintainer asked for private networking on those paths and chose "private in steady state".

## Decision

- **VNet-integrated Container Apps environment** (`vnet-usnm`, `/27` delegated subnet; workload-profiles env with the Consumption profile). The public API keeps external ingress at `api.usnewsmap.com`.
- **Private endpoints** for `stusnmdata` (Blob) and `cosmos-usnm` (Sql), with linked private DNS zones. **`publicNetworkAccess: Disabled`** on both in steady state.
- **Managed identity on every path** (see the table in [08 §8.3](../08-azure-infrastructure.md#83-networking)). Keys stay disabled, and Azure Policy **denies** re-enabling them.
- **Guarded public-access window** only while a Spot backfill or full re-index runs:
  - the launcher records a window item in Cosmos with an expiry, then enables public access (still Entra-only, no IP rules);
  - it disables public access again when the work completes;
  - an hourly in-VNet `network-guard` job closes any window that is expired or wasn't recorded;
  - every change to `publicNetworkAccess` raises an alert.
- **Drop the Storage Queue.** Cosmos leases already claim work, so the queue was only a wake-up signal, and removing it avoids a third private endpoint (~$8/month).

## Alternatives

| Option | Why not |
|--------|---------|
| Identity-only (previous lean design) | Cheapest (−$17/mo), but data services reachable from the internet (with a valid token) at all times |
| Always private, backfill on VNet-integrated Container Apps Jobs | No public window ever, but the backfill costs ~$150–250 instead of ~$25–50, and full re-indexes cost more each time. Kept as the growth-profile option |
| IP allow-lists | ACI Spot egress IPs are unknowable, and a Consumption environment's outbound IPs can change; brittle |
| Azure Monitor Private Link Scope for telemetry | Adds cost and complexity for low-sensitivity telemetry; growth profile |

## Consequences

- About **+$17/month** (2 endpoints, 2 DNS zones, per-GB processing). The typical month is ~$72, and a press-spike month ~$114 (see [09 §9.5](../09-operations-security-cost.md#95-cost-model-monthly-usd-list-prices-confirm-with-the-azure-pricing-calculator)).
- The **2 vCPU search lever no longer fits under $80** without dropping private endpoints or raising the ceiling. This is flagged as a maintainer decision in [10 §10.4](../10-roadmap-and-risks.md#104-open-questions-for-the-maintainer).
- The Azure portal's Data Explorer and Storage Browser can't reach the data from the internet in steady state. Maintainers use the in-VNet `usnm-ingest admin` job, or open a short recorded window.
- The environment must be created with VNet integration from the start.
