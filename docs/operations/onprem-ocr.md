# On-prem OCR

Part of the [operations guide](README.md).

## On-prem OCR nodes

**Local OCR runs through BOINC only** (#917, #942, #966): see the [BOINC pages](../boinc/README.md). No node runs `usnm-ocr-worker` as a direct queue worker any more, and the worker's systemd unit, its installers (`install-worker.sh`, `install-worker-node.sh`) and its nightly update (`update-worker.sh`) are removed. This section keeps what is still in use: the exchange account, whose queues and `ocr-out` container BOINC's hand-off uses, and the nodes' Arc lifecycle and monitoring, which offboarding the remaining nodes needs.

Each node is an Azure Arc-enabled server running RHEL 10 (AlmaLinux 9 is also accepted), with a managed identity and no stored secret ([ADR-0015](../design/adr/0015-onprem-ocr-workers.md)). The exchange account is `stusnmo<suffix>`; a cloud job copies what lands in its `ocr-out` container into `curated`. This section sets up the account, and connects, monitors and removes a node.

**The exchange account.** Set the node IPs (the home connection's public IPv4 address; a comma-separated list or CIDR ranges also work) and turn it on. The IP is a private setting, in the settings store, never in the repository ([Settings](environments-and-deploys.md#settings)). The provision also adds a Microsoft.Storage service endpoint to `snet-cae`, which closes open connections from the subnet to storage public endpoints (the data lake is reached through its private endpoint and isn't affected), so run it while no ingest job is running.

```sh
scripts/azure/settings.sh --env prod --key USNM_OCR_NODE_IPS --value 203.0.113.7   # the home IP, private: the settings store
scripts/azure/settings.sh --env prod --key USNM_OCR_NODES --value true   # committed: pull request, merge, then provision from main
scripts/ocr-node/group.sh --env prod --dry-run
scripts/ocr-node/group.sh --env prod   # writes USNM_OCR_NODE_GROUP to the settings store (the local file until the store exists)
scripts/azure/provision.sh --env prod        # writes OCR_EXCHANGE_ACCOUNT, OCR_NODE_DCR_ID and OCR_NODE_APPINSIGHTS, grants the group
```

**Host infrastructure.** Provisioning, OS subscriptions and lab credentials are managed outside this application repository. The legacy `OCR_KEYVAULT` resource remains in its existing deployment stack until a separately reviewed ownership transfer; do not delete it during the source migration.

**The nodes' group.** The nodes get their roles through the Entra security group `usnm-ocr-nodes-grp-<env>` (`usnm-ocr-nodes-<env>` before the operator rename). The provision grants it Message Processor on `ocr-priority` and `ocr-bulk`, Message Sender on `ocr-deadletter`, the no-delete writer role on `ocr-out`, and Monitoring Metrics Publisher on the nodes' own component `usnm-ocr-appi-<env>`, not the main component. Each node's Arc identity is a member. `group.sh` creates a group once, saves its object ID in `USNM_OCR_NODE_GROUP` and makes you an owner. On rerun it uses that same ID, lists owners and updates `displayName`/`mailNickname` in place: no identity, member or grant changes. `--dry-run` only describes the rename. It never adopts a group by name or creates a duplicate of an unsaved legacy group: check its members and owners (`az ad group member list --group <id>`; `az ad group owner list --group <id>`) and save the verified ID by hand. Every member gets node roles, and owners can add members. Creating it needs the right to create security groups (default user setting or Groups Administrator); renaming requires rights to update the saved group. Deployment stacks cannot manage it (ADR-0015), and teardown leaves it. Delete by the saved object ID (`az ad group delete --group <id>`), never by display name.

**Prepared-host prerequisites.** Supply an AlmaLinux 9 or RHEL 10 host with key-based SSH and passwordless sudo. RHEL must already be registered, with BaseOS, AppStream and CodeReady Builder enabled. VM creation, OS registration, hardware and hypervisor configuration belong to the infrastructure operator, not application onboarding. Use a new node ID after rebuilding a host so provenance cannot mix the old and new machine.

**Connect a node.** The node needs AlmaLinux 9 or RHEL 10 and an admin account with key-based SSH and passwordless sudo. Its name (`--name`) must be `usnm-ocr-node<digits>-arcs-<env>`, match `--env` and be at most 32 characters (ADR-0017). A legacy node must first be drained and offboarded, renamed in the VM, its hostname and DNS, and onboarded again; changing only `--name` is not a migration. Run from your machine with rights to create app registrations (Application Developer or the default user setting), assign roles and write Arc machines, extensions and DCR associations (Owner, or User Access Administrator plus Contributor, or plus Azure Connected Machine Resource Administrator and Monitoring Contributor), and add group members (its owner, as `group.sh` makes you). Automatic agent upgrade requires `Microsoft.HybridCompute/machines/write`. The first onboarding also registers three providers, a subscription-level action (Contributor or Owner); with only resource-group rights, have them registered first:

```sh
scripts/ocr-node/onboard.sh --env prod --host claude@node1.example.org --name usnm-ocr-node1-arcs-prod --reboot-day Tue --dry-run
scripts/ocr-node/onboard.sh --env prod --host claude@node1.example.org --name usnm-ocr-node1-arcs-prod --reboot-day Tue
```

`onboard.sh` registers the `Microsoft.HybridCompute`, `Microsoft.GuestConfiguration` and `Microsoft.HybridConnectivity` providers. Unless the node is already connected, it then creates a throwaway app registration and service principal, `usnm-arc-onboard-<name>-<UTC time>`, with a client secret valid for 2 hours and only Azure Connected Machine Onboarding on the environment's resource group, and waits for the role assignment to show up. Then it runs `scripts/ocr-node/node-setup.sh` on the node with sudo. RHEL registration is checked before onboarding changes any resources; the application never reads OS subscription credentials. That script installs python3, podman, chrony, cron (for the agent's automatic upgrades), rsyslog and which, and initscripts where the distribution has it (the monitoring agent's prerequisites), the QEMU guest agent on a VM if the image lacks it, plus htop, iftop and screen from EPEL for watching the node. It installs no OCR engine: EL9 has only Tesseract 4.1.1 and EPEL 9 none, so the engines come as pinned container images. It creates the `usnm` user. It sets up the engine runtime of [ADR-0016](../design/adr/0016-pluggable-ocr-engines.md) ([13 §13.3](../design/13-ocr-engines.md#133-component-view)): a second system user, `usnmeng`, with its own subordinate ids, lingering and the cpu, memory and pids cgroup controllers delegated to its user manager, not in `himds`; its Podman API socket at `/run/usnm-ocr/podman.sock` (mode 0660, group `usnmocr`, which `usnm` shares), through which the worker runs engine containers with `podman --remote`; and the engines' scratch, a tmpfs at `/run/usnm-ocr/scratch` capped at `--scratch-size` (default 2G) and owned by `usnm`, so an engine's files count against its own memory limit. It installs the Connected Machine agent. It sets the agent's extension allow list to the Azure Monitor Linux Agent alone (with extensions off while a different list is replaced), reads it back, stops if any other extension is installed in `/var/lib/waagent` (the allow list doesn't remove one installed before it, and turning the extension manager on would run it again), and only then turns the extension manager on (an empty list allows every extension); it turns off guest configuration and incoming connections. It keeps the journal on disk (`/etc/systemd/journald.conf.d/50-usnm.conf`: `Storage=persistent`, `SystemMaxUse=1G`; the cloud image keeps it in memory only, so a reboot lost the logs). It sets sshd to offer post-quantum key exchange first where its OpenSSH supports it (a drop-in, `/etc/ssh/sshd_config.d/40-pq-kex.conf`; AlmaLinux 9.8's default crypto policy leaves it out). It connects with that service principal (the tenant, app id and secret go over ssh's stdin, never on a command line; the secret waits in a root-only file under `/run` while `azcmagent connect --config` reads it, and a refused connect is retried for a few minutes while the new principal propagates). Last, under the maintenance lock, it removes what an earlier install of the retired worker left: the nightly update's timer and scripts, then `usnm-ocr-worker.service` (stopped, disabled and its unit file deleted) and its line in the reboot window's resume list. It installs no worker unit or settings. It ends by checking that `usnm` can get a token (for Resource Manager, not storage: see below), and prints `azcmagent show` and `azcmagent config list`. Run again, it skips what's done and applies the lockdown as it is now. `onboard.sh` then waits for the machine's system-assigned identity and adds it to the nodes' group unless it's already a member, which gives the node its roles; no provision is needed. The machine identity's private key can live in the node's TPM (`--identity-key-store tpm`, the agent's TPM-backed identity, in preview; provision a vTPM 2.0 separately), so it can't be copied off the disk. Azure accepts that only once the subscription's `Microsoft.HybridCompute/HybridComputePreview` feature is Registered, which needs Microsoft's approval: `az feature register --namespace Microsoft.HybridCompute --name HybridComputePreview` leaves it Pending until then (requested 2026-10-10). `onboard.sh` checks the feature and asks `node-setup.sh` for the TPM only when it's Registered; otherwise the key stays in the agent's default store on disk. The key store is fixed at connect: moving an existing node to the TPM means `offboard.sh` and `onboard.sh` again, which makes a new machine and identity. A vTPM's state is a file on the Proxmox host, so this protects against the VM's disk or a backup of it, not against the host. `onboard.sh` also turns on Microsoft's automatic agent upgrade for the machine (in preview: the agent upgrades itself to within one version of the latest, in batches at off-peak hours, scheduled with cron), which `node-setup.sh` already asks for at connect with `--enable-automatic-upgrade`; the property is checked so a machine connected earlier gets it too. Last, it stops if the machine has any extension in Azure other than the Azure Monitor Linux Agent (delete it, then run again), associates the machine with the nodes' data collection rule (`OCR_NODE_DCR_ID`), waits until the machine reports the allow list to Azure (a few minutes after a change), and installs the Azure Monitor Linux Agent extension with automatic upgrades on, waiting until it's installed (an agent already installed with automatic upgrades off gets them turned on). As soon as `node-setup.sh` has finished, since the connect was its only use, or when `onboard.sh` stops before that, on failure or Ctrl-C too, it deletes the role assignment, the service principal and the app registration, purges them from Entra's deleted items and prints what it deleted; a warning names anything it couldn't delete, to remove by hand.

**A node connected before monitoring** (connected with extensions off): provision once, so the nodes' workspace, component and rule exist and the group has Monitoring Metrics Publisher, then run `onboard.sh` again. It needs no onboarding credential for a connected node, re-runs `node-setup.sh` (which applies the allow list) and installs the agent.

**Monitoring.** The nodes have a workspace of their own, `usnm-ocr-log-<env>`, with its own daily cap of 0.1 GB, and an Application Insights component on it, `usnm-ocr-appi-<env>` (`infra/modules/ocr-node-monitoring.bicep`). The nodes' group can write telemetry to that component only, so a node, or anyone with a node's token, can neither write rows the API alerts read nor fill the environment's 1 GB cap; at worst it fills the nodes' cap, which stops the nodes' monitoring until the reset (ADR-0015). The rule, `usnm-ocr-dcr-<env>`, collects into the nodes' workspace: syslog at Warning and above from the system's facilities (kernel OOM kills, systemd, sshd, chronyd, podman containers' stderr); the worker's own lines at Info and above, from `local0` (its unit's `SyslogFacility`), since journald records every line the worker writes at info, warnings and errors included; and every 60 s, CPU per core, memory used, free space and read and write bytes per file system, and network bytes per interface; and every 60 s, VM insights' log-based metrics (`InsightsMetrics`), which make the Arc machine's **Insights** pane show as configured and fill its **Performance** tab (no Map: no Dependency agent). The agent also sends a heartbeat a minute (free). The worker's traces and metrics go to the nodes' component as role `usnm-ocr-worker`, instance the node name (see the worker's README). The exchange account's queue logs (`StorageQueueLogs`, a row for every queue request) and the rule's errors (`DCRLogErrors`) go to the nodes' workspace too. The estimate is 20 to 45 MB a day per node before the worker's telemetry, which isn't measured yet (Perf measured 7 to 10 MB; VM insights estimated 10 to 20 MB, `InsightsMB` in `ocr-node-health`), so two working nodes come close to the cap; measure it after a day and raise the cap if needed (`ocrNodeDailyCapGb` in `infra/main.bicep`, and `cap_gb` in `ops/queries/ocr-node-health.kql`, then provision). With alert emails set:

- _OCR node log volume high_ (severity 2) fires when a node sends more than a tenth of the cap (10 MB) in an hour, through the agent (Syslog, Perf and InsightsMetrics) and its Application Insights telemetry together, so a crash loop or debug logging is caught before one node fills the cap.
- _OCR node: loc.gov asked to wait over an hour_ (severity 2, hourly) fires when a node's worker counts a `Retry-After` over its one-hour cap (`ocr.loc_slowdowns` with `capped`). The worker holds that server for the hour and takes no pages meanwhile; if loc.gov keeps asking, stop the workers and raise their gaps.
- _OCR nodes' daily cap reached_ (severity 2) fires on the nodes' workspace's `OverQuota` event, as the environment's cap alert does (#279). Until the reset, the node alerts and queries see nothing. In the nodes' workspace, `Usage | where IsBillable | summarize sum(Quantity) by DataType, bin(TimeGenerated, 1h)` shows what filled it.

`scripts/azure/logs.sh` runs the `ocr-node-*` queries against the nodes' workspace (`OCR_NODE_LOG_ANALYTICS_WORKSPACE_ID`):

```sh
scripts/azure/logs.sh --env prod --query-name ocr-node-health             # per node: heartbeat, counters, CPU, memory, free space on /, worker lines, PerfMB, SyslogMB, InsightsMB and WorkerMB; DayMB and CapPct for the workspace
scripts/azure/logs.sh --env prod --query-name ocr-node-errors --span 2d   # worker warnings and errors, panics, failed units, OOM kills, other errors
scripts/azure/logs.sh --env prod --query-name ocr-node-updates --span 7d  # reboots, and the retired nightly worker update's history (#471)
```

The same as charts: the "usnm OCR nodes (<env>)" workbook under Application Insights `usnm-ocr-appi-<env>` → Workbooks, with a time range and a node picker: CPU, memory, free disk space by mount, disk and network bytes per second, heartbeat gaps over 5 minutes (a table and a chart), the worker's warnings and errors (from Syslog and, once it sends telemetry, `AppTraces` and `AppExceptions`), pages done, failed and abandoned per hour (from its metrics; empty until it sends them), and the workspace's billed MB a day against its cap.

`no heartbeat` means the node is down or off the network, or has lost its agent or its association with the rule (run `onboard.sh` again): the agent sends heartbeats only to the workspaces of the rules it's associated with. `no counters` with heartbeats means the agent is associated but its counters aren't arriving: check the rule's counters and the agent's logs under `/var/opt/microsoft/azuremonitoragent/log`. A node with no heartbeat in the whole span isn't listed at all, so widen the span (`scripts/azure/logs.sh --env prod --query-name ocr-node-health --span 7d`) or check the Arc machine's status.

**Worker publication catch-up.** The `ocr-worker-*` release still carries the binary that the scan archive and scan-fetch hosts install, and the LAN dashboard. The release job publishes for the commit `main` points at, or for an older commit when none of the commits `main` has gained since change the worker or the dashboard build (`scripts/ci/head-of-main.sh --unless-changed ocr_worker,ocr_dash`, #1074), so a worker change followed by an unrelated merge is still released. A release can still be missing when a newer commit that changes either build has no release: its run failed or was cancelled, and the older commit's run skipped because the newer one would release. To publish current main without the default full CI dispatch's API rollout, use the explicit worker-only scope:

```sh
gh workflow run ci.yml --repo tgoodyear/usnewsmap.com --ref main \
  -f scope=ocr-worker -f correlation=ocr-worker-catch-up
```

This runs only the Rust checks, the worker's EL9 build, the RHEL and engine smoke checks and the existing attested release publication (plus the always-on private-address check). It does not select API images or rollouts, the BOINC server and bridge builds, infrastructure provisioning or host installation. `scope=full` (the default, preserving bootstrap behavior) is **not** publication-only. Worker-only scope refuses a non-main ref or unknown scope. The existing main-only/current-main guard, build checks, checksums and provenance attestation remain mandatory. If main advances to a commit that changes either build before publication, the guard still skips; verify the actual publication steps and rerun the same scoped dispatch against new main after checking its source. A successful run with skipped publication is not a release. Verify that the release tag's source includes the required fix, and verify the provenance of its binary and checksums, before any separately authorized installation. Publishing is not installed-source acceptance; separately managed host installation still applies.

**Updates and reboots** (#296). `node-setup.sh` turns on `dnf-automatic` (`/etc/dnf/automatic.conf`): every update, not only security ones, installed once a day at 06:00 UTC plus up to an hour, with `reboot = never` and the Connected Machine agent left out (it upgrades itself). A reboot waits for the node's weekly window: `usnm-ocr-reboot.timer` starts `usnm-ocr-reboot.service` at 04:00 US Eastern on the node's day (`--reboot-day`, passed through by `onboard.sh`; without it a node keeps the day it has, and a new node must have one no other node has). The service runs `/usr/local/sbin/usnm-ocr-reboot-if-needed` (`scripts/ocr-node/reboot-if-needed.sh`), which reboots only when `dnf needs-restarting -r` says an update needs it (the kernel, glibc, systemd, dbus and the like). It waits for a `dnf-automatic` run still going (up to 30 minutes, else no reboot that week), then stops the [Serial Set fetch](congress-pages.md#the-serial-set-on-prem) (`usnm-sset`) where it is running, and a worker unit an earlier install left (never started again afterwards), and waits for them to exit (at most 1,320 s), then reboots. The Serial Set fetch is enabled and starts at boot by itself until it has finished (`/var/lib/usnm-sset/done`). The fetch resumes from its raw store and takes over its own lock from before the reboot at once. A stopped unit that isn't enabled (an older install of the fetch's unit) is listed in `/var/lib/usnm-ocr-reboot/resume`, and `usnm-ocr-reboot-resume.service` starts it again at boot. Updates that only need a service restarted (`dnf needs-restarting -s`) wait for the next reboot.

| Node                               | Reboot window                |
| ---------------------------------- | ---------------------------- |
| Node 4, `usnm-ocr-node4-arcs-prod` | Sundays, 04:00 US Eastern    |
| Node 3, `usnm-ocr-node3-arcs-prod` | Wednesdays, 04:00 US Eastern |

Neither node runs OCR: node 4's worker has been disabled since 16 October 2026 and node 4 is shut down; node 3's since 18 October 2026 (#833), and node 3 is off. Both wait to be offboarded and deleted (#966). Direct bulk OCR is not coming back; the `ocr-bulk` and `ocr-priority` queues are read through BOINC's queue-fed work ([batch scheduler](../boinc-batch-scheduler.md#queue-fed-work)).

The window's lines are in the journal under `usnm-ocr-reboot`; the reboot itself and any problem are logged at Warning, so they reach the nodes' workspace too. On the node:

```sh
systemctl list-timers dnf-automatic.timer usnm-ocr-reboot.timer
journalctl -u dnf-automatic -u usnm-ocr-reboot --since -8d
dnf needs-restarting -r                                  # would the next window reboot?
sudo /usr/local/sbin/usnm-ocr-reboot-if-needed --dry-run # what it would do now
```

To patch by hand, one node at a time: `sudo systemctl stop usnm-sset`, `sudo dnf -y upgrade --exclude=azcmagent`, `sudo systemctl reboot` (the fetch starts again by itself at boot until it has finished); then check the kernel (`uname -r`) and `getenforce` (Enforcing) before doing the other node. `offboard.sh` turns the timers off first and stops a window already running before removing the window's units and script.

**When the home IP changes,** the nodes lose the exchange account until the firewall has the new address. Hidden messages come back on their own once their time runs out.

```sh
scripts/azure/settings.sh --env prod --key USNM_OCR_NODE_IPS --value <new IP>
scripts/azure/provision.sh --env prod
```

**Group changes take a while.** Membership is a claim in the identity's access token, and managed identity tokens are cached per resource for around 24 hours; Microsoft says a membership change can take several hours to apply, and a refresh can't be forced ([Microsoft](https://learn.microsoft.com/en-us/entra/identity/managed-identities-azure-resources/managed-identity-best-practice-recommendations#limitation-of-using-managed-identities-for-authorization)). So a node that fetched a storage token before it joined the group is refused by the exchange account until that token expires (that's why `node-setup.sh` checks with a Resource Manager token), and a removed node keeps its access for up to about a day on a token it already holds.

**Remove a node** with no provision:

```sh
scripts/ocr-node/offboard.sh --env prod --name usnm-ocr-node1 --host claude@node1.example.org --dry-run
scripts/ocr-node/offboard.sh --env prod --name usnm-ocr-node1 --host claude@node1.example.org
```

It removes the machine's identity from the nodes' group and deletes its data collection rule associations. With `--host`, it deletes the Azure Monitor Agent extension and waits while the node uninstalls it, then on the node stops and disables `usnm-ocr-worker.service`, removes any extension left (`azcmagent extension remove --all`) and runs `azcmagent disconnect --force-local-only`, which clears the agent's local state without an Azure sign-in, so no onboarding credential is needed. OS registration and Lightspeed reporting are left unchanged; their lifecycle belongs to the infrastructure operator. It turns off `dnf-automatic.timer` and the weekly reboot window before it stops the worker. It leaves the [Serial Set fetch](congress-pages.md#the-serial-set-on-prem) alone (`usnm-sset.service`, `usnm-sset-status.timer`, `/etc/usnm/sset.env` and `/var/lib/usnm-sset`): on a node that runs it, stop and disable it first (`sudo systemctl disable --now usnm-sset usnm-sset-status.timer`), since without the Arc identity it can only fail and retry every 15 minutes. Last, it deletes the Arc machine with your `az` sign-in, which deletes its identity so it can get no new token. Without `--host` (a node that's gone or unreachable) it leaves the extension to go with the machine and prints the node commands to run later. It needs ownership of the nodes' group and the right to delete resources in the environment's resource group. To cut a node off at once, rather than when its tokens expire, also take its address out of `USNM_OCR_NODE_IPS` and provision; with both nodes behind the same home IP, that cuts off every node. Offboard every node before turning `USNM_OCR_NODES` off. The provision then deletes the exchange account with its queues and `ocr-out`, and the nodes' monitoring: their workspace with its data, their Application Insights component with the group's Monitoring Metrics Publisher grant, the data collection rule and the three node alerts. Azure won't delete a data collection rule that still has associations: with a node still associated, the provision fails until its association is gone. `offboard.sh` deletes the machine's associations. The provision stops before it starts if the rule in `OCR_NODE_DCR_ID` still has any. The Arc machines stay in the resource group, and their identities in the nodes' group, until offboarded.

### Prefetched scans (#368)

Deploy the scan service and storage using infrastructure tooling. The application's `ocr-prefetch-manifest` command builds the manifest; `scripts/ocr-node/pull-manifest.sh` verifies and submits it to the configured scan-server URL. BOINC takes cached scans through the [scan-cache hand-off](../boinc/scan-cache-handoff.md); the direct workers that read the service with `--scan-cache-url` are retired. The scan-fetch service's `status-full.json`, `failures-full.jsonl` and journal explain buffer capacity, source cooldown and failures.

### The scan archive (#442)

The keep-set scans have been copied to a Cold-tier container, `scans` in the storage account `SCAN_ARCHIVE_ACCOUNT` (a deployment output) ([ADR-0018](../design/adr/0018-scan-archive.md)): an off-site copy, and the source the contributor-compute bridge reads before LoC. The uploader runs on pve-big, as its Arc identity, never through a laptop. Production has Cold default tier, shared keys and public blob access disabled, and firewall default Deny. The archive-only identity grant and host installation were applied through #641; the JPEG compatibility fix (#644) was deployed in attested release `ocr-worker-20261016-00fbdd9`. **The 16 October 2026 11:26:03 UTC recovery point is fully verified: 148,127 scans present; no missing scans, mismatches, invalid local files, conflicts or request errors; and no verification skips.** Acceptance used a temporary ZFS snapshot so upload and full property verification covered exactly the same immutable cohort while the source fetcher kept running. Its final top-up uploaded 5,203 scans (17,530,564,063 bytes); the temporary snapshot was destroyed afterward. **On 17 October, daily top-up and monthly verification timers were disabled under the local-first cost decision (#783); existing bytes and manual units remain intact.** That verification covers a dated off-site copy; it says nothing about whether a later fetch manifest is complete or whether scans fetched since then lag the archive. Read the current reports instead of assuming later coverage. Direct-client API and tickets remain deferred in #649. See [local BOINC download hosting](../boinc-file-host.md) for the revised cost and serving boundaries.

1. **Provision.** The account is part of the stack with `ocrNodes`, and takes its firewall from `USNM_OCR_NODE_IPS` like the exchange account. `scripts/azure/provision.sh --env prod --what-if`, then without `--what-if`: the policy step adds the exemption first, the stack adds the account, the writer role and the grant to the nodes' group (`USNM_OCR_NODE_GROUP`). `SCAN_ARCHIVE_BLOB_ENDPOINT` in the deployment outputs is the account's URL.
2. **The identity.** Keep pve-big out of the OCR nodes' group: that group grants queue permissions and is counted for node pacing. Save its Arc principal as the archive-only uploader, then provision to grant the existing archive role (create and read, no delete) on `scans`:

   ```sh
   az resource show -g <Arc resource group> -n <Arc machine name> --resource-type Microsoft.HybridCompute/machines --query identity.principalId -o tsv
   scripts/azure/settings.sh --env prod --key USNM_SCAN_ARCHIVE_UPLOADER_ID --value <that object id>
   scripts/azure/provision.sh --env prod --what-if
   scripts/azure/provision.sh --env prod
   ```

   If the full-stack preview includes unrelated active-job changes, do not apply it just to grant archive access. `bash scripts/ocr-node/grant-archive-uploader.sh --env prod --dry-run` previews only the same deterministic archive role assignment, and the command without `--dry-run` applies only that assignment. It validates the existing Arc principal and the archive role (read and write, no delete), adds no OCR group membership, and preserves the main template's resource identity for subsequent full provisions.

3. **The unit.** Install the worker binary of a release on pve-big (the scan-fetch container has it; the host needs its own copy at `/usr/local/bin/usnm-ocr-worker`), the user `usnm` in the `himds` group (as for the Serial Set unit), and the units:

   For repeatable host installation, first download `usnm-ocr-worker` and `SHA256SUMS` from the selected release and verify both with `gh attestation verify --repo tgoodyear/usnewsmap.com --signer-workflow tgoodyear/usnewsmap.com/.github/workflows/ci.yml --source-ref refs/heads/main`. Verify the binary digest against the attested checksum file. Stage that binary and the four archive units, `usnm-scan-archive.conf` and `check-scan-archive.sh` from reviewed main in a root-controlled directory on the host. Run `bash scripts/ocr-node/install-archive-host.sh --dir <staged-directory> --release <release> --binary-sha256 <verified-sha256> --archive-url <archive-container-url> --dry-run`, then without `--dry-run`. It checks the digest again, installs the service identity (Arc and scan-read groups), missing `jq`, binary, environment, tmpfiles and units, and validates systemd configuration. It refuses a conflicting environment or an active archive or OCR service, and never starts uploads or timers. The equivalent unit installation is:

   ```sh
   sudo install -m 0644 scripts/ocr-node/usnm-scan-archive.service scripts/ocr-node/usnm-scan-archive.timer \
     scripts/ocr-node/usnm-scan-archive-verify.service scripts/ocr-node/usnm-scan-archive-verify.timer /etc/systemd/system/
   sudo install -m 0644 scripts/ocr-node/usnm-scan-archive.conf /etc/tmpfiles.d/
   sudo systemd-tmpfiles --create /etc/tmpfiles.d/usnm-scan-archive.conf
   printf 'USNM_ARCHIVE_URL=%s\n' "<SCAN_ARCHIVE_BLOB_ENDPOINT>scans" | sudo tee /etc/usnm/scan-archive.env
   sudo systemctl daemon-reload
   ```

4. **Bounded acceptance.** On the host holding the scans, run the automated dry-run, sample upload and verify sequence:

   ```sh
   sudo bash scripts/ocr-node/check-scan-archive.sh --limit 100
   # Subsequent bounded checks without uploading:
   sudo bash scripts/ocr-node/check-scan-archive.sh --limit 100 --verify-only
   ```

   The helper requires the installed binary, service user, Arc group, tmpfiles configuration and environment file. It runs each phase as `usnm` with the unit's Arc environment and supplementary group, and holds the same lock as the upload and verify services throughout all phases. The lock lives in root-controlled `/run/lock/usnm-scan-archive`, not the service-writable state directory, so the service account cannot replace it with a symlink. A concurrent upload, verify or acceptance check makes it fail before starting a sample. It never enables timers or starts a full upload. The dry run writes nothing; the upload creates at most the requested number of missing scans; verify compares size, md5, tier and stored sha256 from properties without downloading Cold bytes. Limits are 1–1,000 scans (default 100), and acceptance requires a nonempty, consistent-size sample with clean reports and every sampled scan present at verification. The transient units remove `USNM_ARCHIVE_STATE` and set `RUST_LOG=off`, overriding the environment file: acceptance never touches the service state file, and progress logs cannot contaminate the single JSON report.

5. **The first upload (manual, cost-approved only).** `sudo systemctl start usnm-scan-archive` uploads the whole keep set, 1.16 TB when the fetcher is done (about 26 hours at 100 Mbps, UNVERIFIED for this uplink); `journalctl -u usnm-scan-archive` shows progress every 1000 scans and the report at the end. It can be stopped and started: it skips what is there. The unit keeps `/var/lib/usnm-scan-archive/archived.tsv`, one line per scan it has archived (name, size, modification time in nanoseconds, inode, change time); an older three-field line is ignored, so that scan is checked once more; on later runs a listed, unchanged scan is not read and gets no request, so a day with nothing new makes none. Delete that file to make the next run look every scan up again. **Do not automatically enable the timers under the current local-first policy.** Their explicit rollback commands are documented in [local BOINC download hosting](../boinc-file-host.md). The verify unit reads and hashes every local scan and makes one properties request per blob; run it by hand only after cost approval with `sudo systemctl start usnm-scan-archive-verify`. The upload, verify and bounded acceptance helper hold one `flock` on `/run/lock/usnm-scan-archive/lock`, so a verify that starts during an upload waits for it (and the other way round) instead of reporting scans not yet uploaded as missing. A blob with no stored MD5 is never counted as present, in any mode (`conflicts` in an upload or dry run, `mismatched` in verify), and is never written to the state file.
6. **What a report means.** `bad_local` is a scan whose sidecar is missing or doesn't match its bytes, full resolution or format (not uploaded: investigate before fetching it again). JPEG sidecars use `format="jpeg"` for `.jpg` files; old sidecars without a format default to JPEG. Releases through `ocr-worker-20261016-c66d480` incorrectly rejected valid JPEGs by comparing the sidecar name with the file extension; retry these with a corrected release rather than changing sidecars or re-fetching good bytes. `conflicts` is a blob of that name that differs from the local file (never overwritten: look before doing anything). `errors` are failed requests: run it again. `skipped` (upload runs with the state file) are scans recorded as archived and unchanged; they are not counted in `scanned`. In `--verify`, `mismatched` includes a blob with no stored sha256 metadata or one that differs. Progress logs include failure counters; the final JSON report includes up to 20 problem paths. The command exits non-zero unless all are zero.

The buffer set is not archived (ADR-0018, decision 11): `--set buffer` exists for a test only.

## OCR pickup: ocr-sync (#296)

`usnm-ingest ocr-sync` takes the nodes' output into the lake ([12 §12.5 Step 8](../design/12-onprem-ocr-pipeline.md)). It runs hourly at minute 17 (`USNM_OCR_SYNC_CRON`; `manual` turns the schedule off) as `usnm-ocrs-caj-<env>` (saved as `OCR_SYNC_JOB`; `usnm-ocrsync-caj-<env>` before the October 2026 rename, #309), on the ingest image as `id-usnm-ingest`, 0.5 vCPU and 1 GiB. The provision creates the job only when `USNM_OCR_NODES` and the ingest jobs are on, together with the exchange account's `ocr-quarantine` container, the identity's grant on it and two alerts. Runs take the Cosmos lock `ops/ocr-sync`, so two never overlap. Logs carry page ids, paths and reason codes, never text.

**On-prem (#917).** With `USNM_OCR_SYNC_ONPREM` (on in prod), the pickup runs on pve-laptop CT 215 (`usnm-batch`) as that container's own Arc machine identity (`USNM_BATCH_HOST`), and `USNM_OCR_SYNC_CRON=manual` stops the Azure job's schedule in the same provision. The container's `usnm-ocr-sync.timer` (home-infra `scripts/lab/batch-ct-ingest.sh`) runs `usnm-ingest ocr-sync --max-runtime-secs 3000` at minute 17, from a released ingest image's binary pinned by commit, with the job's settings in `/etc/usnm/ingest.env`. Its grants (`infra/modules/ocr-sync-runner.bicep`) are narrower than `id-usnm-ingest`'s:

- the lake role (read, write, create; no delete) on `curated`, writing only under `ocr-local/`; on `reference`, writing only `status/ocr-local.json`; and on `ocr-requests`;
- the exchange role (read, write, create, delete; no container actions) on `ocr-out` and `ocr-quarantine`;
- Message Processor on `ocr-deadletter`, Message Sender and Queue Data Reader on all three (`--from-quarantine` puts dead letters back on `ocr-deadletter`);
- Cosmos Data Contributor on `ops` only, Monitoring Metrics Publisher, and AcrPull to install the binary.

The lock holder then reads `usnm-batch-<pid>-<nanos>` (#1133). Its log is the container's journal (`journalctl -u usnm-ocr-sync` in CT 215); metrics go to the environment's Application Insights as before, and the alerts below read them.

Each run:

1. Deletes sources that an earlier run imported or quarantined but stopped before deleting. Its import record (`curated/ocr-local/import/<run>.json`) lists them with their ETags.
2. Drains `ocr-deadletter`. Valid dead letters are written to `curated/ocr-local/deadletter/<day>/<run>.jsonl`, re-encoded from their checked fields, with a `dead_lettered` outcome for each page. The rest go to quarantine. A run that stops between writing and deleting them drains them again after 10 minutes, so a dead letter can appear twice.
3. Checks each part in `ocr-out`, oldest first, at most `--max-parts` (2,000) per run in imports of `--chunk` (200). A part passes when its name and size are right, the strict reader takes it, the page is outstanding in the ledger with the same batch and reason, its text matches its hash and length, the engine, batch, title and image check out, and its ALTO passes the bounded reader. Its ALTO goes to `curated/ocr-local/alto/`, its row to one merged part per title, `curated/ocr-local/pages/<lccn>/<run>.parquet`, with both texts' scores, and an `imported` outcome goes to the ledger. A part that fails moves to `ocr-quarantine` with its ALTO. Then the sources are deleted with If-Match.
4. When configured, consumes at most one oldest incomplete closed UTC day from the API's `ocr-requests` container and checks at most 10,000 shuffled IDs, retaining at most one matched score per requested ID. Only qualifying `/v1/hits` responses contribute page IDs; requests with DNT or GPC, crawler requests and requests from outside the site are excluded. No query text or request metadata is stored. Eligible pages with LoC damage scores of at least 0.1 are queued to `ocr-priority`, with the message's `queued_at` rounded to midnight UTC of the request day. The ledger retains the actual enqueue time for ordering and stores only the rounded request day for the cap. Already imported, outstanding, Japanese or undamaged pages are skipped. A failed send or runtime deadline leaves the day incomplete for retry; already-sent and ambiguous sends remain charged, while confirmed-unsent attempts do not. Unattempted messages get `not_sent` outcomes when pausing. The daily distinct-page maximum is `USNM_OCR_USER_DAILY_MAX` (default 1,000); set it in the environment settings and provision to change it. After processing, `ocr-sync` writes a durable `completed/<day>.json` marker before emptying the day payload, retaining an empty tombstone to prevent concurrent closes from recreating it. An interrupted cleanup resumes without spending the next run's one-day slot. Do not delete the completion markers or tombstones: they prevent replay. Status rereads the ledger after request processing even without bulk seeding.
5. With a seed plan, tops `ocr-bulk` up (below).
6. Writes `status/ocr-local.json` to the reference container: the last run's and the running totals of imports, rejects by reason and dead letters by cause, the queue depths, pages outstanding, the parts left with the oldest one's age, and the run's seeding.

Request lookups scan each relevant damage table once in published batch order, retaining only matches for the day's bounded set of IDs, and send messages in the original shuffled order. `curated/ocr-local/requests/<day>/lookup.json` checkpoints the published version, requested IDs, completed batch cursor, and resolved scores after each batch. A deadline pause resumes that checkpoint rather than repeating completed lookups or losing resolved candidates; a changed published version resets it. The checkpoint contains no query or request time. Completion empties both the checkpoint and daily payload; interrupted cleanup retries even if the payload is already empty, without spending the next day's processing slot. Their historical versions expire after one day under the lake lifecycle policy (plus configured soft-delete retention). Retries skip every page still charged to that request day, including terminal dead letters; confirmed-unsent attempts remain eligible. Request-step failures are returned after publishing status for completed imports and successful request sends, including successes before a later send or completion write failed; bulk seeding still runs when its deadline allows.

**Queue pages for the pilot.** Only the pickup's identity can add messages, so `ocr-enqueue` runs in CT 215 with the pickup's settings (`usnm-ingest-run`, run as root on pve-laptop; home-infra `docs/lab.md`, "Ingest commands"). It finds each page in the published version (Step 6's damage tables, or the curated text where they lack the batch). It then writes a ledger row, `curated/ocr-local/queued/<day>/<run>.parquet`, and puts the message: the base batch, the page's detected language in ISO 639-3 (its title's first language for `und`), and LoC's damage rate as the score. It skips pages already outstanding and Japanese titles. `--dry-run` only counts. The ledger rows are written before the messages. If a send fails, the pages after it get a `not_sent` outcome, so running the same command again queues them; the page whose send failed may have gone out, and `--include-outstanding` queues it again. Outside a dry run, `ocr-enqueue` and `ocr-requeue` hold the `ops/ocr-sync` lock, so they fail while a pickup run is going; start them again once it ends.

```sh
pct exec 215 -- /usr/local/bin/usnm-ingest-run --command ocr-enqueue -- --pages sn83045462_1890-05-03_ed-1_seq-7,... \
  --reason bulk --plan pilot-german [--batch nbu_gage] [--dry-run]
```

**Queue dead-lettered pages again,** by cause (`dequeue_count`, `invalid`, `permanent`, `unroutable`) and, for `invalid`, kind. Pages queued since their dead letter, imported since, or whose latest dead letter has another cause, are skipped.

```sh
pct exec 215 -- /usr/local/bin/usnm-ingest-run --command ocr-requeue -- --cause dequeue_count [--since 2026-10-20] [--dry-run]
```

**Seeding** ([12 §12.5 Step 10](../design/12-onprem-ocr-pipeline.md)) is off while `USNM_OCR_SEED_PLAN` is empty. With a plan, each run ranks (batch, title) units by their summed damage over 0.1 from Step 6's tables for the published version, and queues the pages at or above the minimum damage (`USNM_OCR_SEED_MIN_DAMAGE`, default 0.25), unit by unit in issue order. Run `ocr-damage` on the version first: the seeder refuses to run without its tables. It keeps at most `USNM_OCR_BULK_WINDOW` bulk pages outstanding: pages in the ledger with no result yet, or the queue's message count if that is larger. It tops up only when `ocr-bulk` holds fewer than `USNM_OCR_BULK_REFILL_BELOW` messages (default half the window). A window of 0 (the default) is two days of pages at the rate `ocr-sync` imported them over the last week, at least 500. Bulk pages that `ocr-enqueue` and `ocr-requeue` queue count toward the window, but the window doesn't limit them. Other settings: `USNM_OCR_SEED_LANGUAGES` (ISO 639-3, default the Tesseract pilot's `ces,deu,eng,pol,rus,srp`), `USNM_OCR_SEED_BATCHES`, `USNM_OCR_SEED_YEARS` (`1890` or `1850-1919`), `USNM_OCR_SEED_MAX_PAGES` (the plan's budget), `USNM_OCR_SEED_PILOT` (one language or a batch list, and a budget of at most 10,000) and `USNM_OCR_SEED_SKIP_AMERICAN_STORIES` (off; also skips pages whose American Stories text scores under the minimum). Set them with `scripts/azure/settings.sh` and provision, then write CT 215's settings file again (`scripts/onprem/ingest-env.sh`, as below): the pickup there reads `/etc/usnm/ingest.env`, not the stack.

It skips Japanese titles and pages already in the ledger, and copies of a page that another batch's copy wins (the release's duplicate rule). A page of the plan outstanding for 14 days, or dead-lettered with cause `dequeue_count`, is queued again at most 3 times. One dead-lettered as `unroutable` is queued again after 6 hours, doubling per cycle up to 48. A page whose message a failed run never sent (`not_sent`) goes out in the next run. Each batch's ranked units are stored under `curated/ocr-local/seed/rank/`, and the units a plan has finished under `curated/ocr-local/seed/plans/<plan>/done/`. A run that reaches its time limit before every batch is ranked queues no new pages; the next run continues from there.

**Seeding from fetched scans** (Step 11, #368). With `USNM_OCR_SEED_SOURCE=fetched` the plan's new pages come from the prefetch manifest instead of the ranked units: the keep set of `audit/damage/v2/prefetch/<version>/manifest.parquet` in the curated store (`USNM_OCR_SEED_MANIFEST`, default the published version), in the order the fetcher takes it (German before the 1920s first, each issue together), in the languages, years and damage asked for, and only pages on the newest fetched list in `ocr-out/fetched/<version>/`. A node publishes that list from the scan server's index (`usnm-ocr-worker publish-fetched`), because the job can't reach the LAN. The list only narrows the manifest's pages, so a node can't add a page. The window, budget, ledger, retries and skips are the same as above. A MARC code in `USNM_OCR_SEED_LANGUAGES` (`ger`) is read as its ISO code (`deu`).

`ocr-seed` is the same step on its own. Use `--dry-run` to print what it would queue as JSON, page ids per unit included, without writing anything. Its flags override the settings. For the pilot (Step 11), preview it, then set the plan:

```sh
pct exec 215 -- /usr/local/bin/usnm-ingest-run --command ocr-seed -- --dry-run --seed-plan pilot-deu --seed-pilot \
  --seed-languages deu --seed-years 1850-1919 --seed-max-pages 3000 --bulk-window 1000
scripts/azure/settings.sh --env prod --key USNM_OCR_SEED_PLAN --value pilot-deu
scripts/azure/settings.sh --env prod --key USNM_OCR_SEED_PILOT --value true
scripts/azure/settings.sh --env prod --key USNM_OCR_SEED_LANGUAGES --value deu
scripts/azure/settings.sh --env prod --key USNM_OCR_SEED_YEARS --value 1850-1919
scripts/azure/settings.sh --env prod --key USNM_OCR_SEED_MAX_PAGES --value 3000
scripts/azure/settings.sh --env prod --key USNM_OCR_BULK_WINDOW --value 1000
scripts/azure/provision.sh --env prod
# CT 215 reads its own copy of the settings: write it again.
scripts/onprem/ingest-env.sh --env prod | ssh root@pve-laptop.tjg.fyi /root/batch-ct-ingest.sh --env-file -
```

**Quarantine.** Rejects are kept for 30 days as `rejects/<run>/<source path>` (a dead letter as `rejects/<run>/deadletter/<message id>.txt`), each with a `.reason.json` record: the reason code, the source, its ETag, size and creation time, never its content. A source over its size limit isn't read, so it has a record only. Read them with your own sign-in from home (the firewall allows the node IPs and the subnet). After a fix, put one reason's rejects back where they came from; the same run checks them again:

```sh
pct exec 215 -- /usr/local/bin/usnm-ingest-run --command ocr-sync -- --from-quarantine --reason not_requested
```

Reason codes: `name`, `part_too_large`, `part_unreadable`, `rows`, `issue_mismatch`, `alto_missing`, `alto_too_large` (checks 1 and 2); `not_requested`, `batch_mismatch`, `reason_mismatch` (3); `text_null`, `text_too_large`, `text_control`, `text_mismatch` (4); `engine`, `engine_tag` (an `ocr_engine` with `image=local`, or not printable), `reason`, `batch_unpublished`, `title_unknown`, `title_japanese`, `image_url` (5); `alto_*` from the ALTO reader (6); `deadletter_*` for dead letters.

**The gain estimate.** `ocr-sync --report` applies rule v1 (Step 9) to the scores stored in every merged part, without picking anything up. It writes `curated/ocr-local/report/<run>.json` with pages considered and replaced, the reasons LoC's text stays, and counts by language and by engine.

```sh
pct exec 215 -- /usr/local/bin/usnm-ingest-run --command ocr-sync -- --report
```

**Alerts** (with alert emails). For the Azure job, a failed execution is reported by the pipeline's job-failed rule, `usnm-ingest-job-failed-alert-<env>` (severity 2, its `Execution` names the job; #308), and `usnm-ocrs-backlog-alert-<env>` (severity 3) fires when the last run left a part in `ocr-out` older than 72 hours. `ocr-out` deletes blobs 30 days after their last change, so a backlog left that long is lost.

For the on-prem pickup, `USNM_OCR_SYNC_ALERTS` (on in prod, with `USNM_OCR_SYNC_ONPREM`; #1139) deploys three log alerts on the environment's workspace (`infra/modules/ocr-sync-alerts.bicep`). They read the metrics the run sends to Application Insights, so CT 215 needs no agent:

- `usnm-ocrs-failed-alert-<env>` (severity 2): a run recorded `ingest.command_failures` (command `ocr-sync`) in the last 30 minutes. Every `usnm-ingest` that ends in error records it before it exits. The error itself is in the journal.
- `usnm-ocrs-stale-alert-<env>` (severity 2): no `ocr.out.oldest_age_s` for more than 2 hours (2 h 20 min). Only a run that gets through the pickup records it, so this rule catches a stopped timer, a container that is down, a run killed before it could record a failure, and telemetry that no longer arrives.
- `usnm-ocrs-backlog-alert-<env>` (severity 3): the last run's `ocr.out.oldest_age_s` is over 72 hours, with `ocr.out.parts_left` in the email.

Each costs $0.50 a month (checked every 15 minutes, the backlog rule hourly). The failure count needs a `usnm-ingest` from #1139 or later in CT 215 (`batch-ct-ingest.sh --install`); the other two work with any release.

## Our OCR in releases (#296)

With `USNM_OCR_LOCAL` (the ingest job's `--ocr-local`), a release weighs the text `ocr-sync` took into `curated/ocr-local/` against LoC's on the pages of the batches it indexes, and uses ours where rule v1 prefers it ([12 §12.5 Step 9](../design/12-onprem-ocr-pipeline.md)). It is off by default, and with it off a release reads nothing from `ocr-local` and its documents and snapshot files don't change. `id-usnm-ingest` already has Blob Data Contributor on `curated`, so the setting needs no new grant.

```sh
scripts/azure/settings.sh --env prod --key USNM_OCR_LOCAL --value true
scripts/azure/provision.sh --env prod
```

- The weekly delta applies it to the batches it indexes. Published batches take it only at a [full rebuild](rebuild-and-index-storage.md#full-rebuild) with the setting on. Each release that builds a main index logs `our OCR in ocr-local` (parts, titles, revocations) and, when the version has published batches, `pages of ocr-local that would replace LoC's text in published batches`, an estimate from the scores `ocr-sync` stored. A release of only the Japanese overlay reads nothing from `ocr-local` and logs neither line. `ocr-sync --report` ([above](#ocr-pickup-ocr-sync-296)) gives the same kind of estimate without a release.
- Records: `reference/<version>/ocr_local.json` (the rule and metric versions, a parts summary, pages considered, replaced and kept by reason, language and engine, and the revocations applied), `ocr_local_pages.bin` (the pages that hold our text), the manifest's `built_from.ocr_local` and the build record's `features.ocr_local`. The API marks hits on those pages "Our OCR", with a viewer link that has no highlight. On a page the Japanese overlay also has, the Japanese overlay wins.
- To take a node's or engine's output out, write a create-only `curated/ocr-local/revoked/<stamp>.json` with `reason`, `reference` and at least one of `node`, `engine`, `run`, `from` and `to`; every field given must match. Matching rows are left out from the next release that indexes their batches, so text already published stays until a full rebuild. A file the release can't read stops the release.
- Clearing the setting stops new replacements. A delta carries the published version's `ocr_local_pages.bin` forward either way; the next full rebuild without the setting goes back to LoC's text everywhere.

## The OCR dashboard on the LAN (#296)

`usnm-ocrdash` summarizes source acquisition, local OCR, volunteer compute, evaluation and publication on an access-controlled internal endpoint ([12 §12.2.5](../design/12-onprem-ocr-pipeline.md#1225-the-lan-dashboard)). Pipelines opens directly beneath the summaries, independently of Operational details. On phones, diagrams become readable step cards with the same descriptions and live rates; wider screens retain the SVG maps. Operational details (workers, batches, pages, queues, host stats and provenance) remain collapsed by default. The same data is at `/api/view.json`, and per panel at `/api/hosts.json`, `/api/fetch.json`, `/api/nodes.json`, `/api/next.json` and `/api/ocred.json`. DNS, certificates, container provisioning and access rules are infrastructure concerns.

Summaries use fresh reports, not a shared pipeline percentage. Local completions are worker output; BOINC counts are assigned/unsent result replicas, not unique pages or actively computing devices. The evaluation card groups observed assignments by purpose, but campaign readiness, paired results and acceptance decisions are not connected. Catalog counts cover imported evidence only. Missing publication evidence stays unknown, never zero or inferred success. Snapshot generation time is shown separately from evidence age, so old historical observations do not label all live lanes stale.

**Pipelines.** The collapsed reference section draws the original LoC text, keep-set OCR, buffer-set OCR, Serial Set and volunteer/experiment flows, as inline SVG in `crates/usnm-ocr-dash/src/diagrams/`. Storage lanes and the tables below them identify sources, OCR requirements and publication gates. The volunteer lane is independent of local OCR; queued/assigned replicas do not establish active computation, experiment success or acceptance. Worker boxes describe configured workers, not fixed machine assignments. Imported OCR can be considered by an enabled release indexing that batch; previously published batches need a full rebuild.

Active boxes include a rate or "rate unknown". Fetch rates are last-hour completions shared across keep/buffer and the fetch/store boxes; worker rates are last-hour successful completions from fresh reporting workers across both sets. Do not add repeated rates or interpret them as per-dataset measurements. Serial Set volume throughput is the collector's run-to-date average, including empty/incomplete volumes, not an upload rate. An active-step table below each diagram gives the measurement window and scope. Queue stock and BOINC assignments have no connected throughput counter; missing rates are not zero. Separate upload, import, acceptance and publication rates are not inferred from worker completions. Stale/error sources do not animate as live work.

States come from the operational view; steps without telemetry show "not tracked here". The dashboard does not directly query Azure ingest, release or search, and user-driven priority queueing is still marked "not built". Counts are not hard-coded. When the pipeline changes, edit the SVG and the answers in `diagrams.rs`; tests check XML, marker substitution, unique ids, source freshness, rate units/scope and the absence of private addresses or internal host names.

**Deployment.** Use infrastructure tooling to install the released dashboard binary, configure its source URLs, mounts and TLS, and install host-stat collectors. The application repository provides the binary, API schemas and service definitions, not lab provisioning.

**Corpus data flow snapshot (#704).** The optional "Corpus data flow" panel reads a local metadata-only `corpus-catalog` JSON file at the usual file polling cadence (15 seconds by default). It appears at `/api/pipeline.json` and as `pipeline` in `/api/view.json`, under the same LAN access controls. It never queries a database or cloud service and needs no credentials. Cataloged stage, run and artifact counts are separate from local worker journal totals, not whole-corpus totals: zero can mean no catalog loaded. The producer must include coverage and issues for absent sources and must not include OCR text or storage URIs.

The panel spans the operational-details section below "At a glance". Its stage list scrolls independently; expand a stage for provenance, or "Coverage, provenance and issues" for source limits and diagnostics. Open details and list positions survive automatic refresh. Closed operational details retain their existing panels and refresh on reopening, preserving list positions while summaries continue to update.

Have the producer atomically publish a readable snapshot file and configure `--pipeline-snapshot` / `USNM_OCRDASH_PIPELINE_SNAPSHOT` through the deployment's infrastructure tooling. Preserve existing source, TLS and access settings. `USNM_OCRDASH_PIPELINE_MAX_AGE_SECS` / `--pipeline-max-age-secs` sets evidence freshness (default 900 seconds; allowed 1–604800).

Input is limited to 1 MiB and schema version 1, producer `corpus-catalog`; unknown fields, invalid UTC RFC3339 times, times more than 60 seconds ahead, negative/fractional or over-trillion counts, and `verified + missing > locations` are rejected. Bounds: 32 coverage strings (128 characters), 64 stages (id 256, label 128, detail 512), 128 issues (code 64, detail 512). No control characters; stage ids must be unique; unknown stage counts must be null. The wrapper reports `retrieved_at`, `last_error`, `age_secs`, `stale`, optional `snapshot`, and `stage_freshness`. Source `observed_at`, not `generated_at` or retrieval time, determines freshness; null means unknown/unavailable. Optional stage `observed_at` supplies independent evidence times (explicit null is unknown; omission uses the snapshot evidence time). Stale stage evidence remains visible even when another source was just observed. Missing, oversized or malformed files retain the last valid snapshot with an explicit error indication and suppress its live summary counts; no valid snapshot shows unavailable, never a silent fallback to worker totals. The API's aggregate `stale` flag remains unchanged; the diagnostic heading says "Some evidence old or unknown", with actual read errors shown beneath it.

**Resumable local scan-cache inventory (#704).** For indexes larger than the
single-batch adapter's 10,000-row/16 MiB limits, run the stdlib orchestrator
**on the POSIX machine holding the scan files**:

```bash
python3 scripts/corpus/scan_cache_inventory.py \
  --db catalog.sqlite --index scans/index-full.jsonl --scan-root scans \
  --work-dir inventory-20261017 --source local-cache-stat \
  --cache-set keep --cache-set buffer --max-batches 10
```

Repeat the identical command until `finished` is true. Select only cache sets
that this operator intends to observe; the index does not encode membership.
No scans are downloaded, hashed, modified or deleted. A regular file with the
expected size is `present`, **not checksum-verified**. Missing/wrong-size copies
are observations, not deletion requests; absent rows are not deletions either.
This does not change workers, cloud archives, acceptance, publication or indexing.

The first invocation fixes an input byte prefix, inode, and prefix SHA-256.
That hash protects the **index metadata**, not image bytes. Each resume checks
the captured prefix by streaming it with at most 1 MiB reads; replacement,
truncation, or edits fail closed. The index must remain append-only during an
invocation. Appends are deliberately excluded; use a new inventory directory
to observe a later prefix (overlapping observations are allowed). A trailing
record without a newline is excluded and remains explicitly uncovered.

Each slice is limited to 256 rows/1 MiB and the existing 64 KiB line limit.
It is statted separately for each selected set and imported through the existing
transactional catalog ingestion. `--max-batches` limits newly completed slices
per invocation, not the prefix integrity scan. Output reports covered/captured
bytes, uncovered bytes, rows, completed batches, selected sets, and later
appended bytes. Counts are index rows/observations, **not unique pages or corpus
completeness**. Even `finished: true` means only that the fixed prefix's complete
records were processed; LAN dashboard coverage remains partial.

Keep the private inventory directory on durable local storage. It contains
bounded raw index slices (including original discarded metadata), immutable
`batch-*/keep.jsonl` / `batch-*/buffer.jsonl` evidence and atomic `state.json`
checkpoints. Published evidence is reused without re-statting after interruption.
Checkpoints advance only after all selected sets commit. A crash after a commit
is recovered by idempotent replay, while an interrupted unpublished build is
retained as replaceable `.interrupted` scratch. A filesystem lock prevents
concurrent use of one inventory directory; do not share its catalog with a
different inventory job concurrently. State/evidence storage grows with the
observed prefix; working memory is bounded to one slice/adapter batch.

To rebuild a lost catalog, replay only the retained evidence, not `index.jsonl`
slices or scratch files, into the **same database pathname**:

```bash
for batch in inventory-20261017/batch-*/keep.jsonl inventory-20261017/batch-*/buffer.jsonl; do
  [ -f "$batch" ] || continue
  python3 scripts/corpus/data_catalog.py --db catalog.sqlite ingest --batch "$batch" || break
done
```

The normal resume command refuses a missing catalog checkpoint; after evidence
replay it can resume. Evidence is independent of the original scan files and can
also rebuild a different database for inspection. Do not hand-edit checkpoints
or evidence. Input transport, scheduling and retention policy are separate work.

**Binary updates.** Build with `cargo build --locked --release -p usnm-ocr-dash --bin usnm-ocrdash`. Deploy the reviewed artifact with infrastructure tooling, retaining rollback and preserving existing settings. Verify `/healthz`, `/api/pipeline.json`, `/api/nodes.json` and the UI; health alone does not prove fresh evidence.
