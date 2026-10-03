# ADR-0001: Search engine

- **Status:** Proposed. Becomes final after Spike S-2 (see [10](../10-roadmap-and-risks.md#102-validation-spikes)). The correctness half of S-2 has passed on Quickwit 0.9.1 ([05 §5.5.1](../05-search-and-storage.md#551-quickwit-index-config-validated-in-s-2)), except fuzzy terms, which Quickwit doesn't support (R-15). The 1M-page benchmark is still to run.
- **Date:** 2026-09
- **Deciders:** Maintainer, lead engineer

## Context

We need phrase and proximity search over about 23M OCR'd newspaper pages (roughly 450–800 GB of text). The primary query is a **nested aggregation**, *place × time bucket*, over **all** matches, not a ranked top-k. The legacy system used self-managed Solr, which is now ruled out by the "no servers" constraint ([ADR-0005](0005-sustainability-constraints.md)). The maintainer asked us to lean on Azure PaaS for document storage and retrieval.

## Decision

Use **Quickwit** (Apache-2.0, Rust/Tantivy) running on **Azure Container Apps**, with its index splits and file-backed metastore on **Azure Blob Storage**. The Rust API accesses it through a `SearchBackend` trait. Keep a maintained **Azure AI Search** adapter and index definition as the **managed alternative**, to be switched on if S-2 fails or if the budget can carry about $3–6k/month.

## Alternatives

| Option | Why not chosen (now) |
|--------|----------------------|
| Azure AI Search (L1/S3) | Fits functionally and has the least operational work, but costs about **$2.8–5.6k/month** at our size (5–10× the budget), and nested facets (`place > year`) are **preview-only**. Kept as the alternative. |
| Elastic Cloud on Azure (managed SaaS, no VMs of ours) | Full-featured, but a hot tier of about 1 TB runs ~$1.5–3k/month, and the frozen tier is too slow for interactive aggregations. Fallback. |
| Cosmos DB full-text | GA BM25/phrase search, but RU cost for aggregating millions of matches is prohibitive. Reserved for future user data. |
| PostgreSQL tsvector | A `tsvector` records at most 16,383 positions, so phrases on dense pages fail. |
| Embedded Tantivy | Needs about 1 TB of local persistent disk, which means IaaS VMs or AKS, and that violates the no-VM constraint. Kept only for offline research builds. |

## Consequences

- Monthly search cost is about $25–65 (lean profile: a Quickwit sidecar plus index storage) instead of about $2,800+. With the $80 ceiling ([ADR-0006](0006-lean-hosting-profile.md)), this is the only viable option; AI Search and Elastic belong to the growth profile.
- We operate one open-source container (version pinning, upgrades). Mitigated by keeping the index disposable and rebuildable from the lake.
- The Azure-native SLA and support don't apply to the engine itself; they do apply to Container Apps and Storage.
- The team must keep both translators tested (golden tests) to keep the switch real.
