# Boundaries and monitoring

Part of the [BOINC batch pilot guide](README.md).

## Live feed health (#1441)

`scripts/boinc/feed_status.py --config … --state-dir …` is the live evaluator.
The old explicit-value CLI remains an **offline diagnostic only**. Neither it
nor the new `feed_collect.py` owns admission, prepares pages, submits work,
renews `ENABLED_UNTIL`, changes budgets, or schedules a feed. Temporal remains
the prepare/ship owner. The new timers schedule **observations only**.

The source collectors read:

- **server:** the existing BOINC database connection, in a consistent read-only
  transaction (2-second SQL statement limits; 8,000-row/8-MiB limits), the live
  `plan_class_spec.xml`, and the actual cohort service's `ENABLED_UNTIL` env file.
  The local `--container` option runs bounded PHP config validation and database
  queries in the already-running BOINC container, where container-only
  `config.xml` symlink targets resolve. Only inventory is exported, never config
  XML or credentials. The host still reads the policy and window env files.
  The distinct-host flag uses the native scheduler's raw-token semantics:
  self-closing or whitespace-padded literal `1` in the sole effective `<config>`
  block, not empty pairs, CDATA, entity expansion or flags elsewhere.
  This does not restart or replace the container.
- **client:** authenticated **loopback-only** GUI RPC, reusing `client_account.RPC`,
  working CPU preferences, suspend/no-work flags, detected GPU counts, queued
  tasks, installed app versions and disk allowance. Only configured project-owned
  clients run this collector. Their host IDs come from their existing BOINC
  attachment, not another hard-coded fleet list. Never install it on volunteers.
- **controller:** the existing oneshot OCR priority controller's bounded journal
  and `systemctl show` timestamps/result. Both completed and currently running
  ticks are checked. Do not point this adapter at a permanently running Temporal
  worker: service uptime is not tick duration. Running durations use systemd's
  `CLOCK_MONOTONIC`, not suspend-inclusive boot time. Early failure records
  without timings still retain their failure timestamp; systemd supplies the
  current failed/running state, and later successful ticks restore duration evidence.
- **backlog:** the dashboard's existing read-only pgmq metrics (`ocr-priority`
  and `ocr-bulk`), immutable Qwen source requests and their inspector `.progress`
  receipts, the Qwen campaign's remaining `c*/READY` markers, and optionally the
  existing selector's `selected.jsonl`. A fresh selector output is needed to
  establish **empty** automatic-replenishment backlog; an empty request directory
  alone cannot do so. Missing/stale progress or selection is unknown unless
  another current source proves open backlog. Counts are evidence items, not
  deduplicated scientific pages. Keep the existing source inspector observations
  fresh; this monitor does not invoke preparation merely to refresh evidence.

Sources are polled every minute, with a 180-second evidence budget, bounded HTTP
reads without redirects, and no credentials, error transcripts, network names or
page text in health output. Source errors replace their previous success. The
evaluator validates all replies against one shared completion timestamp and
rechecks freshness after allocation; slow reads or matching cannot renew old
evidence. Truncated/malformed HTTP responses mark only their source unknown,
with the non-sensitive reason `http_protocol_error`.
The dashboard accepts observations only from each configured source's allowlisted
address, using its existing private push listener. These source records are
never served by a GET or the public listener.

CPU supply is the established `usnm_ocr` CPU lane; GPU supply is the established
`usnm_qwen` GPU lane. **These are modeled candidates, never guaranteed scheduler
dispatch.** The compatibility field `eligible_results` retains that name in
JSON/metrics but means supply within this bounded model. `idle_slots` means
**resource-eligible own slots**, not raw idle device count. Profile CPU/GPU-memory
limits apply even with empty stock; uniform workunit RAM/disk limits apply to
both the allocation and its comparison capacity. Known reservations by the
other lane reduce remaining capacity; unproven shared headroom is
`shared_capacity_unproven`, not a stock shortage.
Modeled checks include platform/installed dispatchable
version, authoritative account/venue app selection and resource flags, live
workunit/batch admission bounds, memory/disk/device constraints,
and prior host participation. Shared stock is matched against all own slots:
one unsent replica cannot fill two slots, nor can two replicas of a unit fill two
slots on one host. Assigned and homogeneous-redundancy work is excluded from
the ordinary feed. Detected unsupported plan/account gates, unknown profiles, mixed CPU
resource profiles or differing `gpu_count` profiles for the same GPU type on one host,
live `ngpus`/explicit CPU-demand mismatches with installed GPU profiles,
non-preferred-app fallback requiring other apps' inventory,
or oversized inventories produce unknown, not healthy.
Policy/profile validation is a separate phase **before any stock enumeration**:
empty stock or individually rejected workunits cannot bypass unsupported gates.
Every unblocked GPU type with positive idle capacity needs a compatible installed
and live profile. In the modeled non-regex policies, an absent/empty live
`gpu_type` cannot inherit a GPU type from the installed cache: it leaves the scheduler on its
[CPU-only resource branch](https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/sched/plan_class_spec.cpp#L1148)
even if `ngpus` is present. That mismatch produces
`live_resource_type_mismatch` unknown before stock enumeration.
CPU demand comes from the live fixed `avg_ncpus`, or from
`max_threads` capped by the observed host's effective CPU count; cached demand
must agree. Missing host capacity, performance-derived GPU CPU demand, and
unmodeled memory-scaling policies are unknown rather than inferred from cache.
An OpenCL GPU plan requires explicit GUI-RPC `have_opencl` support even without
a minimum-version gate, matching the
[pinned scheduler's OpenCL branch](https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/sched/plan_class_spec.cpp#L998).
Its minimum memory requirement uses the nested OpenCL `global_mem_size`, not
a larger native-device memory figure. Missing support/memory evidence, or
positive memory reservations with a larger generic than OpenCL memory budget,
produce explicit `opencl_*_unavailable` unknown reasons; the latter needs
per-profile memory accounting this model does not provide. Other unmodeled
OpenCL policy fields remain unsupported, rather than silently ignored.
Selected work is checked against cumulative per-host CPU, RAM and disk budgets,
shared between CPU and GPU allocations. GPU work consumes its live CPU demand,
not just a GPU slot; a CPU-app exclusion does not disable CPU support for permitted
GPU work. Existing queued work reserves its CPU and workunit RAM
bounds first; missing occupied-work RAM evidence is unknown. Queued task CPU/GPU
reservations use the workunit's **complete** resource record when present,
otherwise the app version's record, matching
[BOINC result initialization](https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/client/result.h#L129).
This includes other attached projects and CPU-only, GPU-only and fractional
overrides; fields are never merged with stale version defaults. Invalid or
ambiguous reservations produce `task_resource_usage_unavailable`.

**Active-task evidence limit:** BOINC can retain old reservations for started
tasks after app-version/configuration updates
([client_state.cpp](https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/client/client_state.cpp#L2462)).
GUI RPC exposes only rounded, cached task resource text, not an exact running
resource record. An unfinished task with a started or unrecognized active-task
state therefore makes its client source `active_task_resource_usage_unavailable`.
The monitor does not infer a precise reservation from that display string.
Consequently both stock checks can be unknown while any configured client has
such a task; refill, backlog and controller checks remain independently evaluated.
Unstarted tasks and completed uploads retain their normal accounting. This
explicit evidence limitation does not change the unobserved-policy boundary.

Uniform resource costs cap graph capacity directly. Per-task GPU memory is
`gpu_ram_used_mb`, falling
back to `min_gpu_ram_mb` when absent or zero, matching the
[pinned scheduler](https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/sched/plan_class_spec.cpp#L1079).
Positive reservations are enforced for
fractional slots on a single observed, otherwise unoccupied GPU of that type.
Multiple physical devices or already occupied GPU memory need per-device placement
evidence this collector does not have, and produce `gpu_memory_packing` unknown.
If a mixed-weight maximum match cannot fit, or independently maximal CPU/GPU
matches overlap shared resources, affected supply checks become unknown with
an explicit `reason`, not healthy or a greedy lower bound reported as a shortage.
The stock alerts remain present and still fire after 30 minutes of a proven
modeled shortage.

The account/venue reader reads selected apps and fallback from the effective
venue's stock `project_specific` block, retaining legacy flat preferences.
Ambiguous duplicate/mixed app-preference blocks are unknown. CPU/GPU resource
exclusions remain venue-level. Non-preferred-app fallback needing wider inventory
is rejected. The reader does **not**
validate every account XML field. Additional account restrictions, custom
scheduler hooks, per-user restrictions, host/application daily quotas and
in-progress limits are unobserved, as are scheduler request budgets, work-fetch
backoff/resource-share decisions, deadline/performance feasibility and reliability
rules. Only installed profiles are modeled. Ownership is verified through the
private source inventory, not inferred from attachment alone. These unobserved
gates do not automatically become unknown: human acceptance of this monitoring
boundary remains a merge/rollout prerequisite, requiring private, read-only
effective-policy/quota evidence and existing scheduler rejection logs.
This is not a replacement implementation of the BOINC scheduler or proof that
a driver/container can successfully execute an assigned task.

Candidate edges and slots are budgeted **before insertion**, at 100,000 each per resource;
at most 1,000,000 profile/workunit comparisons are allowed, including rejected
candidates. Exceeding either budget produces unknown. Other guards include
32 clients, 256 versions/plans, eight GPU types and 1,024 slots per host/device.
Database row/output limits and 2-second SQL hints do not bound rows examined or
prove that a particular database enforces the hints. Verify query plans and
collection cost privately. The supplied systemd units set 75-second source and
90-second evaluator startup timeouts and 256-MiB memory limits; those are not
limits on an existing container/database, nor universal direct-CLI deadlines.
HTTP/RPC timeout checks and bounded reads do not replace the service watchdog.
The bounded command reader, also used for the monitor's container-runtime info
probes, starts a private local process group and signals it
on timeout, output overflow, failure **and success**, including when its leader
has already exited. It uses `waitid(WNOWAIT)` to keep the leader unreaped until
group cleanup, then reaps the direct child; this preserves the group's identity
while signaling it. Real fork/socket fixtures verify descendant termination.
This is not a universal process-tree or wall-clock guarantee: descendants that
change groups/UIDs and uninterruptible processes are outside that assurance.
Darwin can report `EPERM` for a zombie-only group; that case is tolerated only
after observing the leader's exit. Killing a local Podman CLI/group does **not**
prove that an exec process in an existing container or its database query was
cancelled. Keep those remote-operation and service-watchdog boundaries explicit.

The evaluator fsyncs and atomically replaces `feed-health.json` and
`feed-health.prom`, then flushes its Syslog JSON line before committing
`duration.json` under one stable lock. A failed publication or acknowledgement
may repeat a controller failure on retry, but cannot silently consume it.
Malformed source objects become unknown without suppressing unaffected checks.
Recovery or unknown evidence resets
the affected condition. Process restarts preserve durations. Host reboot,
configuration changes, backwards time, wall/monotonic disagreement or a gap
over 180 seconds reset them and emit a collection warning. An open backlog
with no newly observed workunit for an uninterrupted hour fires; a new workunit
or closed backlog resets that timer. Shortage fires at exactly 30 minutes.
Refill expiry/remaining ≤6h, controller failure, or runtime >15 minutes fire
immediately. A failed tick or a successfully completed tick over 15 minutes is
retained by its journal timestamp, even if a short tick follows or a new tick
starts before collection. Both use the durable `last_failure_at` alert watermark,
acknowledged only after publication. The same watermark retains structured
systemd unit-failure and failed-start-job records, scoped to this system unit,
including failures before application JSON logging. A subsequent start or
`reset-failed` can clear systemd's `Result`, but does not erase the bounded
journal evidence
([unit failures](https://github.com/systemd/systemd/blob/v257/src/core/unit.c#L6027),
[start jobs](https://github.com/systemd/systemd/blob/v257/src/core/job.c#L729)).
Unrelated-unit records and condition skips are not counted as failures;
malformed recognized failure records make the controller source unknown.
This still requires readable journal evidence within the two-hour/2,000-record
collection window. The alert rule retains **all firing checks** for ten
minutes so a five-minute alert evaluation does not miss a shortage or other
failure that recovers between evaluations. Duration thresholds are already
enforced by the evaluator; this retention does not shorten them.

`feed-health.prom` exports the same seven checks as one-hot
`boinc_feed_check{check,state}` gauges (`healthy`, `pending`, `firing`, `unknown`),
their continuous durations, and `boinc_feed_observed_at_seconds`. There is no
new Prometheus service. The dashboard reads `feed-health.json` using
`USNM_OCRDASH_FEED_HEALTH`, displays all checks under **BOINC feed / stale data**,
and includes them in `/api/view.json`. It independently makes evidence older
than 180 seconds unknown; restarting or refreshing the dashboard does not renew
its age.

### Wiring after review (not deployed by this change)

Use the existing private deployment inventory for addresses and paths. Review
the client-source list against the project's own hosts (currently 1, 2, 9, 10,
12); do not add volunteer hosts 5 or 13 or give the monitor access to them.
No new third-party service, identity-provider grant, database writer, or feed
owner is needed. The source observations require existing local read access;
the evaluator needs read access to the dashboard's `feed-sources/` directory.
Keep configs and that directory private (0750/0640, shared monitoring group or
read ACL), and allow the dashboard to read the evaluator's aggregate output.

On each appropriate source host, from a reviewed checkout:

```sh
sudo install -d -m 0755 /opt/usnm-feed-monitor
sudo install -m 0644 scripts/boinc/{feed_collect,feed_status,client_account,acceptance_client}.py /opt/usnm-feed-monitor/
sudo install -m 0644 scripts/boinc/onprem/usnm-boinc-feed-source@.{service,timer} /etc/systemd/system/
sudo install -d -m 0750 /etc/usnm-boinc
```

Create a **private** `/etc/usnm-boinc/feed-source-<name>.env` for each source:

```sh
# Source-host examples; replace bracketed values from the private inventory.
SOURCE_ARGS="server --project [host-project-path] --window-env /etc/usnm-boinc/qwen-cohort-auto.env --container usnm-boinc-server --container-project /project/usnmocr"
PUSH_URL="http://[existing-dashboard]/api/feed-source/server"
# controller: SOURCE_ARGS="controller --unit usnm-boinc-priority-controller.service"
# backlog: SOURCE_ARGS="backlog --dashboard http://[existing-dashboard]/api/view.json --requests [qwen-state]/requests --outbox [outbox]/[qwen-campaign] --selection [read-only-selected.jsonl]"
# each own client: SOURCE_ARGS="client --data-dir [existing-boinc-data-dir]"
# Its PUSH_URL ends in /controller, /backlog, or /client-<inventory-name>.
```

For the **server instance only**, Podman needs writable metadata and lock paths
even for a read-only `exec`. Keep `ProtectSystem=strict` and the other template
restrictions. Inspect the existing rootful configuration:

```sh
sudo podman info --format 'graphroot={{.Store.GraphRoot}} runroot={{.Store.RunRoot}}'
```

The supplied server-only drop-in permits `/var/lib/containers/storage`,
`/run/containers/storage` and `/run/libpod`. A transient-systemd rehearsal with
these three paths succeeded; storage paths alone failed on `alive.lck`.
Verify the existing host's paths before installation; on differently configured
hosts adjust only this instance's allowlist to its actual Podman storage and
runtime paths, not the template or a broad parent directory. Do not install
this exception on client, controller or backlog instances.

```sh
sudo install -d -m 0755 /etc/systemd/system/usnm-boinc-feed-source@server.service.d
sudo install -m 0644 scripts/boinc/onprem/usnm-boinc-feed-source@server.service.d/podman.conf \
  /etc/systemd/system/usnm-boinc-feed-source@server.service.d/podman.conf
sudo systemctl daemon-reload
sudo systemctl show usnm-boinc-feed-source@server.service \
  -p DropInPaths -p ProtectSystem -p ReadWritePaths
```

Before enabling this instance, verify that `DropInPaths` includes that exact
server-only file, `ProtectSystem=strict` remains set, and the three tested paths
appear in `ReadWritePaths`. Installing the template alone is insufficient.

Configure `USNM_OCRDASH_FEED_PUSH` on the existing dashboard as a comma-separated
`server=[address],controller=[address],backlog=[address],client-[name]=[address]`
list; use one distinct source per own client. These addresses must also satisfy
the dashboard's existing `--allow`. Rebuild/restart only that reviewed dashboard.
It writes source envelopes below its existing state directory, `feed-sources/`.
On macOS clients run the identical read-only client command through the existing
host observation runner every minute (the provided units are Linux/systemd).

On the dashboard/evaluator host, install `feed_collect.py` and `feed_status.py`
as above, create the unprivileged `usnm-feed-monitor` system account if absent,
and install the health units. Create `/etc/usnm-boinc/feed-monitor.json`:

```json
{
  "server": "/var/lib/usnm-ocrdash/feed-sources/server.json",
  "controller": "/var/lib/usnm-ocrdash/feed-sources/controller.json",
  "backlog": "/var/lib/usnm-ocrdash/feed-sources/backlog.json",
  "clients": [
    "/var/lib/usnm-ocrdash/feed-sources/client-OWN-INVENTORY-NAME.json"
  ]
}
```

Repeat the `clients` entry for every own source. The file paths, or existing
private HTTP mirror URLs, are deployment configuration—not CLI-supplied metric
values. Set `USNM_OCRDASH_FEED_HEALTH=/var/lib/usnm-boinc-feed-health/feed-health.json`.

```sh
sudo install -m 0644 scripts/boinc/onprem/usnm-boinc-feed-health.{service,timer} /etc/systemd/system/
sudo systemctl daemon-reload
# On each source host, substitute the configured source name:
# The Podman source must use NAME=server to select the server-only drop-in.
sudo systemctl start usnm-boinc-feed-source@NAME.service
sudo systemctl enable --now usnm-boinc-feed-source@NAME.timer
# On the evaluator only:
sudo systemctl start usnm-boinc-feed-health.service
sudo systemctl enable --now usnm-boinc-feed-health.timer
sudo journalctl -u usnm-boinc-feed-health.service -n 2 --no-pager
```

Verify every `sources.*.state` is `fresh`, the reported own idle capacities
match the actual clients, the second tick has no reset warning, and the dashboard
shows the same state. Do not waive unknown eligibility or stale request coverage.

### Existing alert route and deployment gate

The on-prem BOINC stack's former `modules/monitor.bicep` declared a workspace
and log readers, **not an action group**; it was removed with the Azure server.
The only notification route is the main environment's action group
(`infra/modules/monitoring.bicep`, `actionGroupId`, emailing the private
`USNM_ALERT_EMAILS`), which the OCR node and pickup alerts already use. Do not
add an email, webhook or second action group for this feature, and do not
change the OCR workspace's daily cap.

The route, from the evaluator to an email:

1. `usnm-boinc-feed-health.service` logs one JSON line a minute to
   `local0.info` as `usnm-boinc-feed-health` (about 1.8 KB with five own
   clients).
2. `usnm-boinc-feed-dcr-<env>`, a data collection rule associated only with the
   evaluator host's Arc machine, sends `local0` at Info and above to the OCR
   nodes' workspace, `usnm-ocr-log-<env>`, and its transformation drops every
   line but the evaluator's: about 2.6 MB a day. It is not the
   nodes' rule, which would also send the host's counters and VM insights (20
   to 45 MB a day) against the workspace's 0.1 GB cap.
3. Two stateful scheduled-query rules (`infra/modules/boinc-feed-alerts.bicep`)
   run every five minutes on that machine's rows only (`_ResourceId`, which a
   node can't set) and email through the existing action group:
   - `usnm-boinc-feed-condition-alert-<env>`: a check that is firing or unknown
     in the latest line of the last three minutes, or firing in any line of the
     last ten. A `boinc_feed_delivery_test` line of the last ten minutes fires
     it with `Check=delivery_test` (below); that line never counts as health.
   - `usnm-boinc-feed-missing-alert-<env>`: no health line observed in the last
     five minutes (`Check=collector_heartbeat`): the evaluator, its timer, the
     host, the agent or the association stopped, or the workspace reached its
     cap.

Both rules split by `Check`, so each check is its own alert. Azure deduplicates
and recovers them (`autoMitigate`): one email when a check starts firing, none
while it stays firing, and a resolved email once it has stayed clear, typically
about 30 minutes or more later. The evaluator sends no transitions of its own.
The rules are billed by frequency and by the number of `Check` series.

`main.bicep` deploys the rule and the alerts with `USNM_BOINC_FEED_ALERTS=true`,
`USNM_OCR_NODES`, alert emails and the private `USNM_BOINC_FEED_MACHINE`; the
stack (ADR-0011) owns them, so changes go through `provision.sh`, not
`az deployment`. Both settings are off until the maintainer sets them. Turn them
on only after the panel shows the sources the alerts should judge: every check
that is firing or unknown at that moment emails at once (today
`controller_tick`, #1483, and `collection` while own-client and backlog sources
are missing).

**Prerequisites on the evaluator host.** These are maintainer steps; nothing in
this repository does them.

- The host is an Azure Arc machine in the environment's resource group, named
  as ADR-0017 says (`usnm-<role>-arcs-<env>`), connected like the other on-prem
  containers. Don't use `scripts/ocr-node/onboard.sh`: it adds the machine to
  the OCR nodes' group with their queue roles and associates the nodes' rule.
  The evaluator needs no role assignment.
- Its extension allow list holds only `Microsoft.Azure.Monitor/AzureMonitorLinuxAgent`,
  and that agent is installed with automatic upgrades, as `onboard.sh` does for
  a node (`az rest --method put` on the machine's
  `extensions/AzureMonitorLinuxAgent`).
- rsyslog runs and receives the journal's syslog; the agent reads syslog from
  it.
- Nothing but the evaluator logs to `local0`. The rule drops other lines
  before ingestion, but Azure can still bill for processing data a
  transformation drops. This must list only `usnm-boinc-feed-health`:

  ```sh
  journalctl SYSLOG_FACILITY=16 --since -1h -o json | jq -r .SYSLOG_IDENTIFIER | sort | uniq -c
  ```

The machine has Heartbeat and Syslog rows in the nodes' workspace, so
`ocr-node-health` lists it beside the nodes, without counters. Its volume is far
below the node volume alert's threshold.

**Deploy.**

```sh
scripts/azure/settings.sh --env prod --key USNM_BOINC_FEED_MACHINE --value '<Arc machine name>'
# Set "USNM_BOINC_FEED_ALERTS": true in infra/env/prod.json through a PR, then:
scripts/azure/provision.sh --env prod --what-if   # one DCR and two scheduled-query rules
scripts/azure/provision.sh --env prod
DCR_ID=$(scripts/azure/settings.sh --env prod --key BOINC_FEED_DCR_ID)
MACHINE=/subscriptions/<subscription>/resourceGroups/<resource group>/providers/Microsoft.HybridCompute/machines/<Arc machine name>
az rest --method put \
  --url "https://management.azure.com$MACHINE/providers/Microsoft.Insights/dataCollectionRuleAssociations/${DCR_ID##*/}?api-version=2022-06-01" \
  --body "{\"properties\": {\"dataCollectionRuleId\": \"$DCR_ID\", \"description\": \"usnewsmap BOINC feed evaluator (#1441)\"}}" \
  -o none
```

The association takes the rule's name (ADR-0017). Until the first lines
arrive, a few minutes after it, the missing rule fires; expect one
`collector_heartbeat` email and its resolution.

**Check ingestion.** After about ten minutes:

```sh
scripts/azure/logs.sh --env prod --query-name ocr-node-boinc-feed --span 1h
```

It should show the machine with `boinc_feed_health` lines about once a minute
and a recent `LastObserved`. No rows means the association, the agent or rsyslog
is missing; the agent's own log is under `/var/opt/microsoft/azuremonitoragent/log/`.

**End-to-end notification check.** On the evaluator host, log one synthetic
line. `feed_status.py --delivery-test` prints it and touches no state; the
dashboard never reads it.

```sh
logger --priority local0.info --tag usnm-boinc-feed-health \
  "$(python3 /opt/usnm-feed-monitor/feed_status.py --delivery-test)"
```

Then confirm, in order:

1. `ocr-node-boinc-feed` shows a `boinc_feed_delivery_test` row for the machine.
2. Within about 5 to 15 minutes, the alert addresses get an email from
   `usnm-boinc-feed-condition-alert-prod` with `Check=delivery_test`.
3. A resolved email for it follows, usually 30 or more minutes after the line
   leaves the ten-minute window.

If the row arrives but no email does, check the rule's history in the portal
and the action group's emails; if no row arrives, check ingestion as above. The
check proves the whole route and leaves the health checks unchanged.

**Rollback.** To silence the alerts at once, disable the two rules (the stack
denies deletes, not changes; the next provision enables them again). To remove
them, delete the association first, because Azure won't delete a rule that is
still associated and provision would fail:

```sh
az rest --method delete \
  --url "https://management.azure.com$MACHINE/providers/Microsoft.Insights/dataCollectionRuleAssociations/${DCR_ID##*/}?api-version=2022-06-01"
```

Then set `USNM_BOINC_FEED_ALERTS` back to `false` through a PR and provision;
the stack deletes the rule and both alerts. Stop the observation timers only if
they are the problem, and never restart a retired feed timer as rollback.

Local validation:
`python3 -m unittest scripts/boinc/test_feed_status.py scripts/boinc/test_feed_collect.py`
and `CARGO_BUILD_JOBS=2 cargo test -p usnm-ocr-dash --lib`. Tests include actual
loopback RPC/HTTP and PHP subprocess fixtures, shared-stock eligibility,
duration thresholds/recovery/restarts/clocks, and dashboard route/privacy/
freshness contracts; they are not claims of live ingestion or alert delivery.
`test_feed_status.py` also checks the alert queries' contract: only
`boinc_feed_health` lines are health events, so a delivery test line can't
change a check or quiet the missing rule. For the Bicep, run `scripts/ci/lint-bicep.sh`,
`scripts/ci/build-bicep.sh` and `python3 scripts/ci/check-names.py`; the KQL
runs only in Azure.
For the optional Python→dashboard-process→HTTP→durable-evaluator rehearsal,
build `cargo build -p usnm-ocr-dash --bin usnm-ocrdash` with
`CARGO_BUILD_JOBS=2`, then run the Python tests with
`USNM_FEED_DASH_BINARY="$PWD/target/debug/usnm-ocrdash"`. The test uses only
loopback, creates its fixtures under the checkout and stops its child process.

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
  claims off `ocr-bulk`, the queue's only consumer since the direct worker was retired (#966). Neither
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
