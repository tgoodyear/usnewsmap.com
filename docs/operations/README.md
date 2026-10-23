# Operations

Running the ingest pipeline in Azure: the backfill, releases, full rebuilds, and how to watch them. The pipeline is two Container Apps jobs, `caj-usnm-ingest-{env}` (`usnm-ingest run`: new LoC batches, the title catalog and a release) and `caj-usnm-backfill-{env}` (parallel `curate` workers for the initial corpus). Both run inside the VNet as `id-usnm-ingest`; the storage and Cosmos accounts stay private throughout. How they are deployed is in [`infra/README.md`](../../infra/README.md); the design is in [08](../design/08-azure-infrastructure.md) and [04](../design/04-data-sources-and-ingestion.md).

Every command below uses your own `az` sign-in. CI can only roll the API onto new images, so starting jobs and changing settings is always a person's action.

**Resource naming (#309).** Prod pins the job and identity names above; dev and
experiments use ADR-0017. Read names from stack outputs.
The [owner rollout](../../infra/README.md#naming-guard-and-phase-12-rollout)
covers stateless OCR alerts and role labels, plus Phase 2 monitoring, CI and title
renames: reconcile inventory, reset the budget, provision, immediately bootstrap
the new CI identity, then check alerts and workbooks. A merge does not migrate
live resources.

The rename guard requires explicit authorization for each known durable-resource
removal; missing or failed plans cannot be overridden. OCR exchange and Phase 3
names remain pinned until their migration and rebuild windows.

## Pages

- [Environments and deploys](environments-and-deploys.md): settings and where they live, deploying the jobs, the archival storage account, the dev environment lifecycle and the search cluster experiment that runs in dev.
- [The title catalog and releases](catalog-and-releases.md): the title catalog, the backfill and the first version, switching the API to the published indexes, weekly updates and checking a release's splits.
- [Full rebuilds and index storage](rebuild-and-index-storage.md): the full rebuild (with duplicate-safe ingest and the on-prem rebuild), the index storage cutover and pruning old index versions.
- [Search serving](search-serving.md): the Quickwit searcher's caches, threads, capacity, app and image, cache warm-up, and the search log.
- [American Stories](american-stories.md): American Stories' text in the index, its comparison against LoC's text, and writing the text for our pages.
- [Congress pages and the Serial Set](congress-pages.md): the Congressional Globe and Serial Set fetches, the Serial Set on an OCR node, and the Japanese OCR on an OCR node.
- [Japanese OCR and the OCR quality audit](japanese-ocr.md): mixed pages and English text, checking Japanese search, the OCR quality audit, and the text that search can't reach.
- [On-prem OCR](onprem-ocr.md): the on-prem OCR nodes, the ocr-sync pickup, our OCR in releases and the OCR dashboard on the LAN.
