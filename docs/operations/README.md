# Operations

Runbooks for the pipeline and the site: settings and deploys, releases and full rebuilds, search serving, the on-prem OCR pickup and the one-off fetches and measurements. How it runs as of November 2026 is drawn in [03 §3.3](../design/03-architecture-overview.md#33-deployment-today-november-2026):

- **Serving** runs on-prem behind a Cloudflare Tunnel (`usnm-serving`) since 31 October 2026, with the Azure API and searcher at zero replicas as a spare ([the Azure cold spare](search-serving.md#the-azure-cold-spare), [`scripts/onprem/serving/`](../../scripts/onprem/serving/README.md)).
- **Ingest** has no Azure jobs in prod (`USNM_INGEST_JOBS` is `false`). The `usnm-ingest` commands, the hourly OCR pickup and full releases run on on-prem hosts as their own Arc identities ([environments and deploys](environments-and-deploys.md#deploy-the-jobs), [full rebuild on-prem](rebuild-and-index-storage.md#full-rebuild-on-prem)). Commands that start an Azure job (`start-job.sh`, `az containerapp job start`, `start-quality.sh`) work only where the jobs exist, which as of November 2026 means a dev experiment.
- **OCR** runs only through BOINC ([BOINC pages](../boinc/README.md)).

The data stays in Azure: the Blob lake, the index account, Cosmos DB and the OCR exchange account. How it is deployed is in [`infra/README.md`](../../infra/README.md); the design is in [08](../design/08-azure-infrastructure.md) and [04](../design/04-data-sources-and-ingestion.md). Every `scripts/azure/` command uses your own `az` sign-in. CI only publishes images and rolls the spare on request, so a person starts jobs and changes settings by hand.

## Pages

- [Environments and deploys](environments-and-deploys.md): settings and where they live, the pinned API image, the Azure jobs and the on-prem backfill, the archival account, the dev environment and the search cluster experiment that runs in it.
- [The title catalog and releases](catalog-and-releases.md): place corrections, the backfill and weekly updates where the ingest job exists, and checking a release's splits.
- [Full rebuilds and index storage](rebuild-and-index-storage.md): the full rebuild on-prem and on the ingest job, duplicate-safe ingest, finishing and restoring a release, the index account, and pruning old versions.
- [Search serving](search-serving.md): the Cloudflare schema, sitemap checks, the searcher's caches and threads, the Azure cold spare and its drill, the Quickwit image, cache warm-up and the search log with its copy on `tank`.
- [On-prem serving: Phase 2 measurements and gates](onprem-serving-phase2.md): the open checklist for #1101 (ADR-0027's measurements and the gates moved from #955).
- [Production origin TLS](origin-tls.md): the DNS-01 certificate for the apex, `www` and `api`, and the BOINC LAN certificate.
- [On-prem OCR](onprem-ocr.md): the `ocr-sync` pickup, our OCR in releases, the exchange account, the scan archive, the OCR dashboard and the offline reading-order tools.
- [Japanese OCR and the OCR quality audit](japanese-ocr.md): the native Japanese OCR runtime, mixed pages and English text, checking Japanese search, the quality audit and the OCR damage tables.
- [American Stories](american-stories.md): American Stories' text in the index, its comparison with LoC's text, and writing it for our pages.
- [Congress pages and the Serial Set](congress-pages.md): the Congressional Globe, the Serial Set fetch and its damage scores, and the Annals and Register.
- [Pictures](pictures.md): Newspaper Navigator's picture boxes, the join report and the Beyond Words baseline.
- BOINC science-pipeline runbooks: [shadow inventory](boinc-shadow-inventory.md), [retained original-source selection](boinc-source-availability.md) and [single-input source requests](boinc-source-requests.md).
