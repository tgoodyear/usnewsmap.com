# Architecture Decision Records

We use the [MADR](https://adr.github.io/madr/)-style lightweight format. Each record lists its status, the context, the decision, the alternatives we rejected, and the consequences.

| ADR | Title | Status |
|-----|-------|--------|
| [0001](0001-search-engine.md) | Search engine: Quickwit on Container Apps + Blob, with Azure AI Search as the managed alternative | Proposed; final after Spike S-2 |
| [0002](0002-blob-data-lake-system-of-record.md) | Azure Blob Storage curated Parquet lake as the system of record | Accepted |
| [0003](0003-stateless-aggregate-first-api.md) | Stateless, aggregate-first, cacheable API; playback in the browser | Accepted |
| [0004](0004-rust-api.md) | Rust (axum) for the API and ingest | Accepted |
| [0005](0005-sustainability-constraints.md) | Sustainability constraints: cost ceiling, no VMs, IaC, handover-ready | Accepted (ceiling amended by 0006) |
| [0006](0006-lean-hosting-profile.md) | Lean hosting profile under $80/month: no Front Door, SWA Free, single container app, Spot ingest | Accepted |
| [0007](0007-cosmos-document-state.md) | Cosmos DB (free tier) for document state (per LCCN, batch, issue, index run) | Accepted |
| [0008](0008-private-networking.md) | Private networking: VNet-integrated env, private endpoints for Blob + Cosmos, guarded public window for Spot backfills | Accepted |
