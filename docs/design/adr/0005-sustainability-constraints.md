# ADR-0005: Sustainability constraints

- **Status:** Accepted
- **Date:** 2026-09

## Context

The original site worked, won awards and was used, then **went offline for lack of organizational support**. The root causes: institution-internal servers, hand-configured services, no IaC, unclear cost ownership, and knowledge held by individuals ([01 §1.4.3](../01-background-and-legacy-analysis.md#143-why-it-failed-root-cause-analysis)).

## Decision

These are **hard constraints** on every design choice:

1. **Cost ceiling:** at most **$500/month** steady state in production, and at most $40/month in dev, enforced by Azure Budgets and alerts.
2. **No VMs or self-managed clusters.** Only PaaS services (Container Apps, Static Web Apps, Storage, Front Door) and containerized OSS where it is clearly cheaper.
3. **Everything as code:** Bicep + azd, GitHub Actions with OIDC, runbooks in the repo. A new maintainer can stand up the whole system with `azd up` plus a documented backfill.
4. **Handover-ready:** the subscription, domain and repo can each be transferred to a sponsoring institution. At least 2 admins at all times. A handover runbook exists.
5. **Graceful degradation:** if the search engine is unavailable, cached results and a status banner still serve users. If LoC changes its APIs, the site keeps serving from the lake.

## Consequences

- Some managed-service conveniences (Azure AI Search, private networking, Front Door Premium) are deferred until a sponsor funds them. The design keeps each one a configuration switch, not a rewrite.
- Cost and operability are reviewed in every PR that touches `infra/` (a what-if plus a cost note).
