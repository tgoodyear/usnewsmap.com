# Boundaries and monitoring

Part of the [BOINC batch pilot guide](README.md).

## Public dataset-page counts

The public `/datasets` page reads `https://boinc.usnewsmap.com/work_summary.php`.
This credential-free, CORS-enabled endpoint caches an aggregate database query
for two minutes and exposes only application names and task counts: ready to
send, in progress, and completed successfully with BOINC validation. Errors
return HTTP 503, not zero counts; the page marks reports older than ten minutes
or failed refreshes as out of date.

These are **retained task records**, not lifetime totals, unique pages, or the
full backlog not yet submitted to BOINC. Eligible completed records are purged
after 30 days. Replicas and retries are separate tasks. BOINC validation does
not imply trusted collection, accepted OCR, or publication in the search index.
Application rows with no tasks in any displayed category are omitted.

The endpoint is installed with the other project PHP pages by the server
entrypoint. Roll out the reviewed server/bridge image before the frontend;
without the endpoint the page explicitly reports unavailable counts. No
scheduler, feeder, trust policy, work submission or collection is changed.
Before rollout, compare the live image's source revision and deployment preview
with main. Do not replace unmerged production code or apply unrelated resource,
signed-app or campaign-setting drift merely to publish these counts.

## Workbook query validation

The BOINC workbook and its workspace are part of the Azure server and are deleted by `"server": "onprem"`; this section applies to `"server": "azure"`.

Run `python3 scripts/boinc/validate-boinc-workbook.py --workspace <customer-GUID>`
with the Mac's existing Azure CLI user credential. On an authorized Arc host use
`--identity arc`; the standard localhost:40342 challenge and token stay on that
host, and its existing identity must already have workspace Reader access.
Arc membership alone does not grant query access. No login, grant, workload or
resource is created by validation. `--subscription <id>` selects the existing
Azure CLI account without changing global CLI configuration.

The shared `scripts/azure/log_analytics.py` client defaults to
`https://<workspace-location>.api.loganalytics.io/v1/workspaces/<customer-GUID>/query`
with the unchanged token audience `https://api.loganalytics.io`. It derives the
location from workspace metadata using the existing Azure CLI subscription, or
accepts `--location`, `LOG_ANALYTICS_WORKSPACE_LOCATION`, or an explicit
`--host` / `LOG_ANALYTICS_QUERY_HOST` hostname. Arc-only hosts must configure the
location or host; they do not need Azure CLI. For the BOINC workspace use
`--location westus2`. Host overrides accept only global/regional `.io` API hosts.

#528 subsequently isolated **workspace-specific** authorization failures on a
changing subset of Azure frontend pods: a pod can deny the BOINC workspace while
serving another workspace successfully. Deny-assignment A/B tests and the
resource-ID route did not explain or avoid the failure. Both global and regional
hosts can select affected pods. At 01:5x UTC on 2026-10-18 westus2, eastus2 and
northeurope failed, while westus, eastus, northcentralus and westcentralus served
successful queries. A single healthy regional host is therefore not durable.
No RBAC change is needed. The client never pins IPs, changes resolvers, or sends
tokens to arbitrary hosts. Portal Workbooks use Azure's own routing; their
queries do not need endpoint changes.

Live source-panel acceptance on 2026-10-18 at 00:51–00:52 UTC returned HTTP200
on the first attempt for all nine panels and the positive control from both
the Mac Azure CLI user and Node5 Arc identity. All panels were populated:
state/latest/chart rows were 1/12/5; job activity/events, revisions, warnings
and console panels also returned data. This validates query execution, not
an assurance that the system has no operational warnings.

The ordered default hosts are the workspace region, westus, eastus,
northcentralus, westcentralus, then global `api.loganalytics.io`, deduplicated.
`--hosts` or `LOG_ANALYTICS_QUERY_HOSTS` overrides the complete order as CSV.
The existing singular host option/env remains a compatible primary-host override;
the default alternatives follow it. All hosts use valid TLS and the same `.io`
audience; this is normal API routing, not IP pinning.

HTTP403 **InsufficientAccessError**, HTTP429, HTTP5xx and connect/DNS failures
advance immediately to the next host with a new connection. At most one extra
pass is allowed, preceded by 3–6s jitter, within an absolute 90s query budget
(token acquisition and metadata lookup are separate). Other 403s, 401, 400,
response-body read failures and partial-success responses fail closed.
Attempt/status/host/`via`/request-ID diagnostics go to stderr, never tokens.
A persistent authorization error still fails; retries do not confer permission.
The BOINC execution-log reader, saved-query CLI, cluster report and quality-row
reader share this implementation.

Validation runs a constant positive control and all nine source workbook KQL
queries (default seven days, grain one hour), emitting per-panel status, actual
row counts, query hashes, attempts and aggregate-only samples. Raw log messages
are excluded. A failed/partial response is never represented as zero rows.
Empty successful panels do not establish actual status/filter coverage or fresh
work-volume telemetry. The normal BOINC post-deploy health step runs validation
as a warning-level check; it does not redeploy resources to query them.
The monitoring modules expose `logAnalyticsQueryHosts` arrays as Bicep outputs;
the BOINC stack passes its ordered array as CSV to this smoke step. The main
stack also exports `LOG_ANALYTICS_QUERY_HOSTS`. Singular outputs remain for
backward compatibility. Older stacks use workspace metadata discovery and the
same default alternatives; no provision is required merely to query.
Its receipt is retained in a separate private `.boinc-work-*` directory beneath
the checkout (path printed to stderr), outside the deployment scratch cleanup.
Remove that directory manually after inspecting the receipt.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../images/pipeline-boinc-dark.svg">
  <img alt="OCR and search pipeline with BOINC: LoC scans go through the on-prem scan fetcher and OCR nodes to ocr-out, ocr-sync, the curated lake, release, index and API. The scan fetcher pushes cached scans to a hand-off container; a hand-off job in the main environment reserves pages and later collects results; the BOINC bridge pulls each job from the container, volunteers run the pinned worker, and the result spool and gold trust list go back through the container, so collect publishes to the same ocr-out as the OCR nodes for trusted hosts and holds an untrusted host's result in quarantine." src="../images/pipeline-boinc.svg" width="960">
</picture>

## Boundaries

- Five registered BOINC platforms (`contrib-compute/boinc/project.xml`): `x86_64-pc-linux-gnu`, `aarch64-unknown-linux-gnu`, `arm64-apple-darwin`, `x86_64-apple-darwin`, and `windows_x86_64` (#533).
- Dual native + container distribution:
  - **Container apps** (`usnm_ocr`, `usnm_assess`, `usnm_gold`, `usnm_surya`, plus container versions of `usnm_quality` and `usnm_qwen`) use BOINC's `docker_wrapper` under plan classes `docker`, `docker_mt`, and `docker_nvidia_cuda`. The stock wrapper supports both Docker and Podman on Linux, Podman via `Run_Podman` on macOS (with our `--format` pipe quoting fix in `scripts/boinc/patch-mac-wrapper.py`; see [2026-10-14-buda-mac-offline-tests.md](../notes/2026-10-14-buda-mac-offline-tests.md)), and WSL2 `boinc-buda-runner` Podman on Windows. The [volunteer guide](../boinc-volunteer.md) supplies an opt-in Linux rootless Podman profile and a mandatory startup preflight (`scripts/boinc/volunteer_preflight.py`, parameterized by `--job-toml`) for slot access, actual cgroup limits, and pausing and resuming.
  - **Native apps** (`usnm_quality` and `usnm_qwen`) run directly on the volunteer's host without a container runtime ([boinc-quality.md](../boinc-quality.md)). `usnm_quality` ships as a standalone CPU binary (`crates/usnm-quality-app`, built by `.github/workflows/boinc-quality-native.yml`) across all five platforms. `usnm_qwen` ships as a native driver binary paired with the pinned `llama.cpp` release archives in `ocr/boinc/qwen/llama.lock` and `<sticky/><no_delete/>` GGUF model files under plan classes `apple_gpu_8b` (Apple Silicon Metal, >= 7,000 MB available unified RAM), `nvidia_cuda_8b` (>= 7,000 MB VRAM), `nvidia_cuda_2b` (>= 3,000 MB VRAM), and `mt` (4–8 CPU threads).
  - **The `boinc_finish_called` contract**: because BOINC's client treats a plain executable exiting `0` without writing `boinc_finish_called` in the slot directory as a premature exit and restarts it up to 100 times before aborting with `ERR_TOO_MANY_EXITS` (`client/app_control.cpp:533-546`), native binaries use `<copy_file/>` on all slot inputs and outputs, update `fraction_done`, and write `boinc_finish_called` containing `0\n` as their final act before exiting `0`.
- Explicit cohorts of 1-100 pages from one batch, selected from the prefetch
  manifest, or pages the [batch controller](../boinc-batch-scheduler.md#queue-fed-work)
  claims off `ocr-bulk` as a second consumer beside the on-prem worker. Neither
  fetches from LoC: a page needs a scan the scan fetcher already cached.
- Central bridge: native work reservations, BOINC staging and submission,
  validation and publishing. Only it can access Azure through the existing Entra
  credential path. Volunteers get images and processing metadata, never
  credentials.
- The job and result file transport (version 1) is separate from the planned
  ADR-0016 long-lived engine protocol. Engine provenance remains `contract=0`
  with `route=boinc@r1`; models and routing are shared with the current worker.
- A task's container runs Tesseract directly. There is no nested Podman or
  Docker daemon. BOINC's Docker wrapper controls the outer container.

```
explicit page selection + cached scans,
  or a message claimed off the Azure work queue + its cached scan
  -> native requested-work reservation (or, for a queue message, its ledger row
     checked, not rewritten) + local dispatch journal
  -> stock BOINC -> pinned worker container
  -> stock batch_collect_assimilator spool
  -> usnm-boinc collect -> native ALTO/Parquet in ocr-out
     (a queue message is deleted here)
  -> existing ocr-sync -> existing release/index
```
