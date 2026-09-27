# ADR-0005: Sustainability constraints

- **Status:** Accepted
- **Date:** 2026-09

## Context

The original site worked, won awards and was used, then **went offline for lack of organizational support**. The root causes: institution-internal servers, hand-configured services, no IaC, unclear cost ownership, and knowledge held by individuals ([01 §1.4.3](../01-background-and-legacy-analysis.md#143-why-it-failed-root-cause-analysis)).

## Decision

These are **hard constraints** on every design choice:

1. **Cost ceiling:** under **$80/month** all-in for production at steady state (typical ~$67 including private networking), and under $10/month for dev, **bounded** by replica caps (`maxReplicas`) and the Log Analytics daily cap, and **monitored** by Azure Budgets and alerts. Budgets only notify or trigger automation; they don't hard-stop spending, so the caps are the real limit. The one-time backfill (about $20–50 on Spot) is budgeted separately. See [ADR-0006](0006-lean-hosting-profile.md).
2. **No IaaS virtual machines.** We don't provision or manage Azure VMs, VM Scale Sets, AKS node pools, Azure Batch pools or any other compute whose OS we patch. Allowed: PaaS services (Container Apps, including Microsoft-managed Dedicated workload profiles; Container Apps Jobs; Static Web Apps; Storage; Front Door; Azure AI Search), and open-source software only when it runs as a container on Container Apps. If a proposed component needs a VM, it is rejected or redesigned. **Interruptible batch compute is acceptable for offline work:** ACI Spot containers (preview) are preferred, and Azure Batch Spot pools that scale to zero are the fallback, used only for backfill and re-index jobs. Nothing on the request path runs on it. **Preview features are acceptable** where they materially reduce cost or complexity, provided a GA fallback is documented.
3. **Everything as code:** Bicep + azd, GitHub Actions with OIDC, runbooks in the repo. A new maintainer can stand up the whole system with `azd up` plus a documented backfill.
4. **Handover-ready:** the subscription, domain and repo can each be transferred to a sponsoring institution. At least 2 admins at all times. A handover runbook exists.
5. **Graceful degradation:** if the search engine is unavailable, cached results and a status banner still serve users. If LoC changes its APIs, the site keeps serving from the lake.

## Consequences

- Some managed-service conveniences (Front Door, Azure AI Search, a larger always-on search node, private networking, zone-redundant storage) are deferred until a sponsor funds them. They are described as the **growth profile** in [09 §9.5](../09-operations-security-cost.md#95-cost-model-monthly-usd-list-prices-confirm-with-the-azure-pricing-calculator). The design keeps each one a configuration switch, not a rewrite.
- Cost and operability are reviewed in every PR that touches `infra/` (a what-if plus a cost note).
