# The scan-cache hand-off

How a page gets from the scan cache to the BOINC server and its result back to `ocr-out` (#469). The scan cache, the services that reserve and publish work (Cosmos, the lake, `ocr-out`) and the BOINC server share no filesystem. Nothing reads `/tank/ocr` except the scan fetcher (CT 211), which stays the only LoC fetcher; volunteers download the page from the project server.

## One page from the scan cache

The [batch controller](batch-scheduler.md) runs these steps for every page; this section is the route one page takes, and how to run it by hand.

| Who                                                                   | Runs                                                                                                                               | Reads                                                                     | Writes                                                                                            |
| --------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------- |
| Scan fetcher, CT 211 (Arc identity)                                   | `scripts/ocr-node/push-boinc-scan.sh`                                                                                              | `/tank/ocr/scans`, `/tank/ocr/manifests`                                  | the BOINC host's `handoff-scans` (`--to-onprem`), or `scans/` and `manifests/` in `boinc-handoff` |
| Hand-off steps on the BOINC host (`usnm-boinc-step --target handoff`) | `handoff-job.sh`: `export`, later `collect --trust`; `claim` and `release` for [queue-fed work](batch-scheduler.md#queue-fed-work) | staged scans, `manifests/`, `spool/`, `gold/`, `claims/`, the work queues | `dispatch/`, `claims/`, Cosmos lease, ledger, `ocr-out`, the work queues                          |
| Bridge steps on the BOINC host (`usnm-boinc-step --target bridge`)    | `submit-remote`, later `spool-out`                                                                                                 | `dispatch/jobs/<id>/`                                                     | `spool/<id>/`, `gold/trust.json`                                                                  |

`boinc-handoff` is a blob container on the exchange account (`stusnmo<suffix>`, `infra/modules/ocr-exchange.bicep`); its `scans/` prefix expires after 30 days. The dispatch root (the journal `export` and `collect` share) lives there under `dispatch/`: each step copies what it needs down and writes it back. Until 6 November 2026 the hand-off steps ran as an Azure job ([the Azure server](azure.md#the-hand-off-job)).

`submit-remote` copies `dispatch/jobs/<id>/` (the page, `request.json` and `record.json`) into the server's own hand-off root. It checks that the record is that request's, that the request's identity is the job id, and that the page's size and sha256 are the request's, then runs the existing `submit`, which checks the page again and stages it as a BOINC download. A job the server already has past `reserved` is left alone, so a repeated run submits nothing. The dispatch root's copy stays `reserved`, which `collect` accepts.

`spool-out` writes the job's spool files (terminal error markers included), the canonical result's host id (`result.hostid` of the workunit's canonical result, from the BOINC database) and `/project/gold/trust.json` to the container. `collect` reads them back and publishes to `ocr-out` only for a host that passed the [gold check](gold-check.md#host-trust-for-usnm_ocr).

### Runbook

On CT 211, signed in with `az login --identity --allow-no-subscriptions`, push the scan and the manifest snapshot to the container:

```sh
scripts/ocr-node/push-boinc-scan.sh --account <exchange account> \
  --scan /tank/ocr/scans/keep/<batch>/<lccn>/<date>/<ed>/<seq>.jp2 \
  --manifest /tank/ocr/manifests/<version>/manifest.parquet
```

Then, as root on the BOINC host, the hand-off export. It prints the job id, and `dispatch/jobs/<id>/` appears in the container:

```sh
usnm-boinc-step --env prod --target handoff --step export \
  --var HANDOFF_PAGES=<page key> --var HANDOFF_BATCH=<batch> --var HANDOFF_PLAN=<plan id> \
  --var HANDOFF_MANIFEST=<version>/manifest.parquet \
  --var ENGINE_IMAGE=sha256:<images.worker's digest> --wait-mins 30
```

Submit it:

```sh
usnm-boinc-step --env prod --target bridge --step submit-remote --var JOB_ID=JOB_ID \
  --var HANDOFF_URL=https://<exchange account>.blob.core.windows.net/boinc-handoff --wait-mins 30
```

When the workunit's result is assimilated:

```sh
usnm-boinc-step --env prod --target bridge --step spool-out --var JOB_ID=JOB_ID \
  --var HANDOFF_URL=https://<exchange account>.blob.core.windows.net/boinc-handoff \
  --var HANDOFF_SPOOL=/project/usnmocr/results/0 --wait-mins 15
usnm-boinc-step --env prod --target handoff --step collect --var JOB_ID=JOB_ID --wait-mins 30
```

`collect` (`HOST_ID` only when the server recorded none) publishes to `ocr-out` for a trusted host and writes the dispatch root back with the job `published`. An untrusted host's result is held in the dispatch root's `quarantine/<job>/`. `ocr-sync` runs next. A terminal BOINC failure (an `_error` or `_no_output_files` spool) needs neither a host nor the trust list: collect marks the job failed. Each run writes back only the journal entries it touched; run one at a time all the same. If a write-back keeps failing, the step's log carries the record (`USNM-HANDOFF-RECORD` lines), since a lost reservation's record can't be made again.

For pages taken off the work queues the controller adds `claim` (`CLAIM_NONCE`, `HANDOFF_MANIFEST`, optionally `CLAIM_MAX`, `CLAIM_QUEUES`, `CLAIM_LEASE_HOURS`) and `release` (`HANDOFF_DOCS`), pushes with `push-boinc-scan.sh --keep-page <batch>/<lccn>/<date>/<edition>/<seq> --to-onprem` straight to the BOINC host (#1116), and exports with `HANDOFF_CLAIMS=1`; `collect` then deletes the queue message once the page is published. All of it is in [queue-fed work](batch-scheduler.md#queue-fed-work).

### Identities and grants

The main stack (`infra/main.bicep`) makes all the grants:

| Identity                                                          | Grants                                                                                                                                                                                                                                                                                | Bicep                                                              |
| ----------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------ |
| BOINC host's Arc machine identity (`boincOnPremHost`)             | Cosmos Built-in Data Contributor on `usnm/ops`; the ledger writer role on `curated` (list, read, create under `ocr-local/` only); Reader on `reference`; the node writer role on `ocr-out`; Message Processor and Sender on `ocr-priority` and `ocr-bulk`; Sender on `ocr-deadletter` | `modules/boinc-handoff-runner.bicep`, `modules/ocr-exchange.bicep` |
| Scan fetcher and controller, CT 211 (`USNM_BOINC_HANDOFF_RUNNER`) | the node writer role on `boinc-handoff` (list, read, create; no delete); the controller role (`USNM_BOINC_CONTROLLER_ONPREM`); `blobs/delete` on `boinc-handoff` for the archive (`USNM_BOINC_HELD_ARCHIVE`)                                                                          | `modules/ocr-exchange.bicep`, `modules/boinc-controller.bicep`     |

### The scan fetcher's identity

The scan fetcher (deployed by infrastructure tooling) holds the prefetch
manifest and the scans under `/tank/ocr` and pushes them with `push-boinc-scan.sh`. It is an
unprivileged Debian 12 LXC; the Azure Connected Machine agent runs in it like on a VM. Its
Arc machine, `usnm-ocr-node5-arcs-prod` in the environment's group, gives it a
system-assigned identity. It is not an OCR worker and is not in the nodes' Entra group, so
it has none of the queue grants.

Its grants go to the object id in the private setting `USNM_BOINC_HANDOFF_RUNNER` (the table above; with `"server": "azure"` also Log Analytics Reader on `usnm-boinc-log-<env>`, `boinc-handoff-logs.bicep`). Set up once, from an operator's workstation and the Proxmox host:

```sh
# On pve-big: the agent, locked down before it connects (no extensions, no
# machine configuration, no incoming connections), and az.
pct exec 211 -- bash -c 'curl -fsSL -o /root/arc.sh https://aka.ms/azcmagent && bash /root/arc.sh'
pct exec 211 -- azcmagent config set extensions.enabled false
pct exec 211 -- azcmagent config set guestconfiguration.enabled false
pct exec 211 -- azcmagent config set incomingconnections.enabled false
# Connect with a short-lived ARM token in a root-only file (never on a command line),
# --resource-group <the environment's group> --resource-name usnm-ocr-node5-arcs-prod
# --tags environment=prod,project=usnewsmap,role=boinc-handoff --config <file>.

# From an operator's workstation: the grant.
az resource show -g "$(scripts/azure/settings.sh --env prod --key AZURE_RESOURCE_GROUP)" \
  -n usnm-ocr-node5-arcs-prod --resource-type Microsoft.HybridCompute/machines \
  --query identity.principalId -o tsv
scripts/azure/settings.sh --env prod --key USNM_BOINC_HANDOFF_RUNNER --value <object id>
scripts/azure/provision.sh --env prod --what-if
scripts/azure/provision.sh --env prod
```

In the container, `az login --identity --allow-no-subscriptions` signs in (the
identity has data-plane roles only, no subscription role).

The grant opens no network path: the exchange account takes the home IP through
`USNM_OCR_NODE_IPS`.
