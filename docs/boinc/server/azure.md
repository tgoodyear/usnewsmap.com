# The Azure server (fallback)

Prod's BOINC server has run on an on-prem host since 21 October 2026 ([BOINC server on-prem](onprem.md), #942), and the Azure hand-off job was retired on 6 November 2026 (#308). The only environment, `boinc/infra/boinc/env/prod.json`, sets `"server": "onprem"`, and no `infra/env/*.json` sets `USNM_BOINC_HANDOFF_IMAGE`.

The Azure code is kept as a fallback: `"server": "azure"` (the schema's default) deploys the Container Apps server described in [the hosting design](../design.md), and setting `USNM_BOINC_HANDOFF_IMAGE` brings the hand-off job back. Everything on this page applies only to such an environment.

## Starting a step

`boinc/scripts/start-boinc-bridge.sh` starts one execution of the bridge job (`boinc/infra/boinc/modules/bridge-job.bicep`) or, with `--target handoff`, of the hand-off job (`infra/modules/boinc-handoff-job.bicep`):

```sh
boinc/scripts/start-boinc-bridge.sh --env <env> --step <step> [--var NAME=VALUE ...] [--wait-mins N] [--what-if]
```

The steps and their variables are the same as on-prem ([the bridge steps](bridge-steps.md)); `--wait-mins` is optional here. The script's header explains why it writes the run's settings into the job and starts it without an override instead of using `az containerapp job start --env-vars`: an execution template drops the job's own settings and has no mounts. The next `boinc/scripts/provision-boinc.sh` restores the template's settings.

When a waited-for execution or a `provision-boinc.sh` lifecycle job fails, the script prints the execution's last 50 console lines from the stack's Log Analytics workspace. By hand:

```sh
az monitor log-analytics query --workspace <workspace customerId> --analytics-query \
  "ContainerAppConsoleLogs_CL | where ContainerGroupName_s startswith '<execution>-' | project TimeGenerated, Log_s"
```

Provisioning registers the apps in `boinc/project/project.xml` itself (the `--ensure-project` lifecycle job, then `bin/xadd`); the `register-apps` step stays available for recovery.

## The start guard

On-prem, `onprem-step.sh` allows one step at a time with a host-local lock. On Azure, `start-boinc-bridge.sh` takes an infinite Azure blob lease around the execution check, the template update, the start and the settings read-back, through `scripts/lib/boinc-start-guard.sh`. A second launcher for the **same job**, even on another host, fails before it updates the template.

- The lease is in the main stack's exchange account (`OCR_EXCHANGE_ACCOUNT`), container `boinc-handoff`, at `scheduler/starts/<lowercase-resource-group>/<lowercase-job>.lock`. The marker blob is created without overwrite. Storage access uses the operator's Entra login; every Azure call names the subscription. Missing outputs, denied access and an unavailable lease fail closed. `--what-if` takes no lease.
- After starting, the helper keeps the lease until the settings match and the new execution is visible as nonterminal (or has already ended), waiting up to 120 seconds (`BOINC_START_CONFIRM_SECS`, `BOINC_START_RECONCILE_SECS`). It then releases the lease before any `--wait-mins` poll.
- It is not `boinc-handoff/scheduler/controller.lock`. The batch controller holds that lease for its whole tick and runs this launcher as a child, so taking it again would deadlock.
- Different jobs have separate leases. The guard cannot fence raw `az` writers or scripts that don't take it.

### Azure call limits

`az` has no timeout of its own: on 20 October 2026 a status poll waited more than 12 minutes for an execution that had already succeeded (#932). The launcher and the helper bound their job and lease calls with `scripts/lib/bounded-az.sh`; a call that runs out of time is killed with its child processes.

- A read is killed after `BOINC_AZ_READ_TIMEOUT` seconds (60) and retried up to `BOINC_AZ_READ_ATTEMPTS` times (3), `BOINC_AZ_READ_BACKOFF` seconds (5) apart, never past the confirm or `--wait-mins` window. The first attempt always gets at least 10 seconds.
- A call that changes something (job update and start, marker upload, lease acquire and release) is killed after `BOINC_AZ_WRITE_TIMEOUT` seconds (300) and not retried, because it may have taken effect. A timed-out lease acquire stops the launcher before it touches the template and prints the blob and the proposed lease ID: if the blob is held under that ID, release it with that ID.
- The checks at the start of a run (sign-in, stack outputs, `boinc_az_check` in `scripts/lib/boinc.sh`) are not bounded. They run before the lease is taken.

### Coordination boundaries

Per [#856](https://github.com/tgoodyear/usnewsmap.com/issues/856), deploys and job runs use per-resource conflict checks, not a global all-idle gate. A running long-lived job does not block app-version publication, a server image rollout or starts of a different job. A start lease is not permission to deploy or to interrupt someone else's work; deploys are still claimed in [#529](https://github.com/tgoodyear/usnewsmap.com/issues/529).

| Change                                                                                      | Gate                                                                                                                                   |
| ------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| Add-only database or project changes (app versions, campaigns, workunits, new plan classes) | #529 claim and the controller lease                                                                                                    |
| One job's template or image                                                                 | That job's start lease and no nonterminal execution of that job                                                                        |
| Server, CGI or daemon image, configuration or TLS                                           | Claim and controller lease. Don't alter the template of a job with a running execution: defer that job's update, not the whole rollout |
| Full reprovision, database migration or anything destructive                                | All jobs idle, as a documented exception with the reason recorded in #529                                                              |

### Failure recovery

A normal exit releases the lease. An uncertain template update or start, mismatched or unreadable settings, or a failed visibility check keeps the infinite lease on purpose; a crash or a failed release can also leave it held. There is no expiry or automatic takeover.

Before breaking that one blob lease: coordinate in #529, confirm the former launcher has stopped, read the job's full execution list and settings, and reconcile every execution it may have started and any template change in flight. Then break it with the normal Azure blob lease tooling. Never delete marker blobs, break a live lease or retry blindly.

## The hand-off job

The Azure hand-off job ran `claim`, `export`, `collect` and `dead-letter` until 6 November 2026; those steps now run [on the BOINC host](onprem.md#hand-off-steps-on-prem). The 6 November provision removed `usnm-boinc-handoff-caj-prod`, its identity `usnm-boinc-handoff-id-prod` and its grants, the Azure bridge identity's grants on `boinc-handoff` and `scans`, and the BOINC `aca` subnet's rules on the lake, the exchange account and the scan archive (#967).

To bring it back: set `USNM_BOINC_HANDOFF_IMAGE` in `infra/env/<env>.json` (its last pin was `sha256:b4f2f00b2128b7eb32206f210e0673be7f9edc7017133e1684e493ed2922b877`) and provision through #529. The job runs the bridge image with the command `/usr/local/lib/usnm-boinc/handoff-job.sh` in the main Container Apps environment. Its identity gets Storage Queue Data Message Processor and Message Sender on `ocr-priority` and `ocr-bulk` only (`handoffQueueIds` in `infra/modules/ocr-exchange.bicep`), plus the grants in `infra/modules/boinc-handoff-runner.bicep`. The controller needs `AZURE_SUBSCRIPTION_ID`, `AZURE_RESOURCE_GROUP` and `BOINC_HANDOFF_JOB` from the main stack's outputs. Change pins image-only, with the jobs idle.

## The gold bundle job

With `"goldBundle": true`, the stack has `usnm-boinc-gold-caj-<env>` (output `goldBundleJobName`), which runs the [gold-bundle step](gold-check.md#build-the-gold-bundle) inside the VNet with `GOLD_CAMPAIGN=v1-tess550` set in the job:

```sh
az containerapp job start --subscription <subscription> --resource-group usnm-boinc-rg-<env> \
  --name usnm-boinc-gold-caj-<env> --query name -o tsv
```

## Workbook query validation

The BOINC workbook and its Log Analytics workspace exist only with an Azure server.

`python3 boinc/scripts/validate-boinc-workbook.py --workspace <customer-GUID>` runs a positive control and the workbook's nine queries (seven days, one-hour grain) and reports each panel's status, row count and query hash, with no raw log text. It uses your Azure CLI sign-in, or `--identity arc` on an Arc host whose identity has workspace Reader; `--subscription <id>` picks the account. Pass `--location westus2` for the BOINC workspace.

The shared client, `scripts/azure/log_analytics.py`, tries query hosts in order, because in October 2026 some Azure frontends refused the BOINC workspace while serving others (#528): the workspace region, westus, eastus, northcentralus, westcentralus, then `api.loganalytics.io`. `--hosts` or `LOG_ANALYTICS_QUERY_HOSTS` replaces the order. A 403 `InsufficientAccessError`, 429, 5xx or connection failure moves to the next host, with at most one more pass within 90 seconds. The receipt goes to a private `.boinc-work-*` directory in the checkout; delete it after reading.

## Moving the project share

The project share left Azure with the server on 21 October 2026. Before that it moved from a pay-as-you-go Standard Files account (about $21 in operations in four days of October 2026, 83% of them open, close and stat) to a provisioned v2 SSD share that bills only for its size (#917). `projectShare` in `boinc/infra/boinc/env/<env>.json` moves it in stages, each a pull request and a `boinc/scripts/provision-boinc.sh` run claimed on #529:

1. **`copy`.** Creates the provisioned account, share and private endpoint, and the copy job `usnm-boinc-sharecopy-caj-<env>` with its own identity (Storage File Data Privileged Reader on the old account, Privileged Contributor on the new one). The job runs AzCopy (pinned by version and SHA-256 in `main.bicep`): `azcopy sync https://<old account>.file.core.windows.net/project https://<new account>.file.core.windows.net/project --delete-destination=true`. Over REST, BOINC's symlinks are the small `mfsymlinks` files that store them, so they copy byte for byte. Run it while BOINC runs; files being written at that moment fail the execution, which is expected.
2. **Cutover.** Make the tree still: stop the batch controller and anything else that stages work, and wait for running bridge executions. As `boincadm` in `/project/usnmocr`, run `touch stop_maintenance`, `bin/stop`, then `touch stop_sched stop_upload`, and wait a minute. `stop_maintenance` keeps `boinc-daemons.sh` from running `bin/start`; the scheduler then answers "temporarily shut down for maintenance" and uploads fail with an error clients retry, while Apache keeps answering the liveness probe.
   - Run the copy job once more and wait for it to succeed. Set **`provisioned`** and provision with `BOINC_MAINTENANCE=1`, which lets the health check accept the closed scheduler. `provision-boinc.sh` refuses while a copy is running or when the latest copy didn't succeed within two hours. `BOINC_COPY_ACCEPTED=<execution name>` accepts a pass whose only failures are files the server regenerates (`run_state_*.xml`, `html/cache`).
   - The environment storage is `project-v2` on the new account, so the change makes a new revision. Check that it is the only one running and that `/project` is the new account (`grep " /project " /proc/mounts`), then remove `stop_maintenance`, `stop_sched` and `stop_upload` and run `bin/start`.
   - Provision once more without `BOINC_MAINTENANCE`, and start everything else again. The October 2026 cutover was a 35-minute outage, 24 minutes of it the final copy.
3. **`provisioned-final`**, after a few days without trouble: the stack deletes the old account.

To roll back before stage 3, stop the server the same way, copy back what was written after the cutover, set `standard` and provision; the storage name changes back to `project`, so a new revision replaces the running one.
