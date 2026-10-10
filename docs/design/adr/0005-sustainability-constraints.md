# ADR-0005: Sustainability constraints

- **Status:** Accepted
- **Date:** 2026-09

## Context

The original site worked, won awards and was used, then **went offline for lack of organizational support**. The root causes: institution-internal servers, hand-configured services, no IaC, unclear cost ownership, and knowledge held by individuals ([01 §1.4.3](../01-background-and-legacy-analysis.md#143-why-it-failed-root-cause-analysis)).

## Decision

These are **hard constraints** on every design choice:

1. **Cost ceiling:** under **$80/month** all-in for production at steady state (typical ~$72 including private networking and the registry), and under $10/month for dev, **bounded** by replica caps (`maxReplicas`) and the Log Analytics daily cap, and **monitored** by Azure Budgets and alerts. Budgets only notify or trigger automation; they don't hard-stop spending, so the caps are the real limit. The one-time backfill (about $20–30 on Container Apps Jobs, paced to LoC's download limit) is budgeted separately. See [ADR-0006](0006-lean-hosting-profile.md).
2. **No IaaS virtual machines.** We don't provision or manage Azure VMs, VM Scale Sets, AKS node pools, Azure Batch pools or any other compute whose OS we patch. Allowed: PaaS services (Container Apps, including Microsoft-managed Dedicated workload profiles; Container Apps Jobs; Static Web Apps; Storage; Front Door; Azure AI Search), and open-source software only when it runs as a container on Container Apps. If a proposed component needs a VM, it is rejected or redesigned. **Interruptible batch compute is acceptable for offline work:** ACI Spot containers (preview) are preferred, and Azure Batch Spot pools that scale to zero are the fallback, used only for backfill and re-index jobs. Nothing on the request path runs on it. **Preview features are acceptable** where they materially reduce cost or complexity, provided a GA fallback is documented.
3. **Everything as code:** Bicep, GitHub Actions with OIDC, runbooks in the repo. A new maintainer can stand up the whole system with one command plus a documented backfill. _Amended by [ADR-0011](0011-deployment-stacks.md): azd is replaced by one deployment stack per environment; the command is `scripts/bootstrap.sh`._
4. **At least 2 admins:** the subscription, domain and repo each have at least 2 admins at all times.
5. **Graceful degradation:** if the search engine is unavailable, cached results and a status banner still serve users. If LoC changes its APIs, the site keeps serving from the lake.

## Consequences

- Some managed-service conveniences (Front Door, Azure AI Search, a larger always-on search node, zone-redundant storage) are left out to stay under the cost ceiling. They are described as the **growth profile** in [09 §9.5](../09-operations-security-cost.md#95-cost-model-monthly-usd-list-prices-confirm-with-the-azure-pricing-calculator). The design keeps each one a configuration switch, not a rewrite.
- Cost and operability are reviewed in every PR that touches `infra/` (a cost note, and the resources the change adds or removes; deployment stacks have no what-if preview, [ADR-0011](0011-deployment-stacks.md)).
