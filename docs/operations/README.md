# Operations

Running the pipeline and the site: the backfill, releases, full rebuilds, search serving and how to watch them. How it runs today is drawn in [03 §3.3](../design/03-architecture-overview.md#33-deployment-today-november-2026):

- **Serving** runs on-prem behind Cloudflare, with an Azure copy at zero replicas for failover ([search serving](search-serving.md#the-azure-cold-spare), [`scripts/onprem/serving/`](../../scripts/onprem/serving/README.md)).
- **Ingest** has no Azure jobs in prod (`USNM_INGEST_JOBS` is `false`). The `usnm-ingest` commands, the hourly OCR pickup and the releases run on on-prem hosts with their own Arc identities ([environments and deploys](environments-and-deploys.md), [full rebuild on-prem](rebuild-and-index-storage.md#full-rebuild-on-prem)). The Container Apps jobs, `caj-usnm-ingest-{env}` (`usnm-ingest run`: new LoC batches, the title catalog and a release) and `caj-usnm-backfill-{env}` (parallel `curate` workers), exist only where the setting is on, as in dev's settings. Commands below that start them apply only there.
- **OCR** runs only through BOINC ([BOINC pages](../../boinc/docs/README.md)).

The data stays in Azure: the Blob lake, Cosmos DB and the OCR exchange account. How it is deployed is in [`infra/README.md`](../../infra/README.md); the design is in [08](../design/08-azure-infrastructure.md) and [04](../design/04-data-sources-and-ingestion.md).

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
- [Search serving](search-serving.md): the Quickwit searcher's caches, threads, capacity, app and image, cache warm-up, the search log and its daily copy on `tank`.
- [On-prem serving: Phase 2 measurements and gates](onprem-serving-phase2.md): the checklist for ADR-0027's measurements and the gates moved from #955, with methods, evidence and status.
- [American Stories](american-stories.md): American Stories' text in the index, its comparison against LoC's text, and writing the text for our pages.
- [Congress pages and the Serial Set](congress-pages.md): the Congressional Globe and Serial Set fetches, and the Serial Set on an OCR node.
- [Pictures](pictures.md): Newspaper Navigator's picture boxes in the curated store.
- [Japanese OCR and the OCR quality audit](japanese-ocr.md): mixed pages and English text, checking Japanese search, the OCR quality audit, the text that search can't reach, and the Japanese OCR on an OCR node.
- [On-prem OCR](onprem-ocr.md): the ocr-sync pickup, our OCR in releases, the OCR dashboard, and the Arc lifecycle of the retired OCR nodes (OCR itself runs through BOINC).
