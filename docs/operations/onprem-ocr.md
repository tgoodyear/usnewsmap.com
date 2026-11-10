# On-prem OCR

Part of the [operations guide](README.md).

## Offline reading-order reconstruction

`scripts/ocr-node/repair-reading-order.py` repairs retained multi-column ALTO whose **lines**, not only blocks, mix columns (#925). It needs the exact retained scan, its stored pixel ALTO and an explicitly ordered region annotation:

```sh
python3 scripts/ocr-node/repair-reading-order.py \
  --alto /private/original.alto.xml --image /private/original.jp2 \
  --layout scripts/ocr-node/fixtures/reading-order/19101018.json \
  --out /private/new-reconstruction
```

The three annotations freeze the 1910 regression inputs' hashes and scan-inspected candidate regions; they are **not** accepted article-order transcriptions. Review boundaries against the full scan (headlines, portraits and ads can span columns), and never reuse a layout on another page or derive a global seven-column default from it.

It rejects mismatched image/ALTO hashes, ambiguous source image identifiers, overlapping or out-of-page regions, uncovered word centres, non-pixel or multi-page ALTO, explicit reading-order references and existing cross-line substitutions. Nonzero rotation, composed-block style inheritance and line-level language/style overrides need separate review.

Regions are half-open pixel rectangles `[x0,y0,x1,y1]` in reading order; word centres decide membership. Wide or already fused recognized words are not split or corrected. Within a region, fragments go by visual row (half the median word height), then left to right; region geometry does not by itself establish editorial order. The annotation must supply named `order_checks`, each a `word_ids` sequence checked against the scan; each must come out consecutive, and the receipt records whether it was consecutive in the original. These are ordering checks, not spelling, CER or page acceptance.

Output goes to a **new private directory**: `page.alto.xml`, literal unnormalized `page.txt`, and a last-written `receipt.json` binding input/output hashes (including the exact layout file bytes) and preservation counts. Word attributes (content, coordinates, confidence, IDs), hyphen ownership, illustrations and recognition provenance are kept; line/block IDs and bounds are rebuilt, and stale line-level layout attributes and spacing elements dropped. A post-processing record binds the parent ALTO and canonical JSON layout. No new recognition or accuracy measurement is claimed.

The command never runs a worker, submits BOINC work, replaces curated output, publishes search text or changes layout3's retained-source separation. A receipt proves mechanical preservation, not scientific or publication acceptance. Fused cross-column tokens need a separately recorded segmented recognition pass. Before shipping any ordering change, verify scan-supported phrase adjacency and snippet/source attribution on the named originals and the multi-title German cohort (#926); agreement with another OCR system is not human-reference accuracy.

### Column repair without regions

`usnm-ocr-worker column-text` uses only ALTO geometry to find narrow gutters Tesseract read across, cuts those lines and writes the text column by column ([note](../notes/2026-11-05-column-reading-order.md)):

```sh
usnm-ocr-worker column-text --alto /private/stored-alto/ --language deu \
  --ocr-engine tesseract/5.5.0 --out /private/column-text/
```

`--alto` is a file or a directory of `*.xml` (any case); each page goes to `<stem>.txt` in `--out`, with one JSON line reporting `changed` and the gutters, lines and blocks cut. Stored ALTO is in pixels; engine ALTO in `inch1200` or `mm10` also needs the scans' `--dpi`. With the worker's language and engine, a page without such a gutter comes out as the worker wrote it. Words fused across a gutter stay whole on their centre's side. The command reads only local files and changes no ALTO, and its output is not accepted for publication until #926's cohort check.

## Rebuilding the dashboard data-flow observer and catalog

The observer/catalog scripts and their five units install separately from the dashboard binary. Pick a reviewed full commit SHA and stage an immutable Git archive:

```sh
COMMIT=<reviewed-40-character-commit>
git archive --format=tar --output=data-flow.tar "$COMMIT" \
  scripts/corpus/data_catalog.py scripts/corpus/data_flow_observer.py \
  scripts/ocr-node/usnm-data-catalog.service \
  scripts/ocr-node/usnm-data-flow-observer.service \
  scripts/ocr-node/usnm-data-flow-observer.timer \
  scripts/ocr-node/usnm-data-catalog-refresh.service \
  scripts/ocr-node/usnm-data-catalog-refresh.timer scripts/ocr-node/install-data-flow.py
git show "${COMMIT}:scripts/ocr-node/install-data-flow.py" > install-data-flow.py
shasum -a 256 data-flow.tar
```

Copy both to the dashboard host, record the digest independently, and validate (Python 3.11+, on a systemd host) before a read-only dashboard maintenance window:

```sh
python3 install-data-flow.py --archive data-flow.tar --commit FULL_SHA --sha256 ARCHIVE_SHA256
```

It checks the digest, the archive's commit header (not a signature), the exact file set, Python syntax and units. `--apply` needs root, the `ocrdash` account, and the catalog service, observer service and timer, and refresh service and timer all stopped: stop only those, wait for an in-flight observer, and note their active/enabled states, since the installer never starts or stops units. Pass `--ocr-url` with the credential-free JSON endpoint (for example `http://127.0.0.1:8090/api/view.json`), not its HTML base URL, on first install only; later installs keep the environment and reject an overwrite.

It replaces code and units atomically, records `COMMIT`, keeps rollback copies under `/var/lib/usnm-data-flow-install` (hard links, so a full disk can't block recovery) and restores them if verification or daemon reload fails, attempting every restoration, reporting all errors and keeping the needed recovery links. After success or failure, restore the original activation; on a new install enable the observer timer. Installing does not start the new refresh timer: once a dashboard accepting `historical` and `max_age_secs` runs, run `systemctl enable --now usnm-data-catalog-refresh.timer` once per host. The catalog listens on `127.0.0.1:8764` only. Nothing (SQLite database, snapshots, evidence, producer timers, queue backend, source budgets) is migrated, deleted or reset; on a rebuilt host restore the catalog through its own procedure first.

## On-prem OCR nodes

**Local OCR runs through BOINC only** (#917, #942, #966; [BOINC pages](../boinc/README.md)). The direct queue worker's unit, installers (`install-worker.sh`, `install-worker-node.sh`) and nightly update (`update-worker.sh`) are removed, and `ocr-bulk` and `ocr-priority` are read by BOINC's [queue-fed work](../boinc/server/batch-scheduler.md#queue-fed-work). Node 4 (`usnm-ocr-node4-arcs-prod`, worker off since 16 October 2026) is shut down and node 3 (`usnm-ocr-node3-arcs-prod`, off since 18 October, #833) is off; both wait to be offboarded and deleted (#966). Nodes were Arc-enabled RHEL 10 / AlmaLinux 9 servers with a managed identity and no stored secret ([ADR-0015](../design/adr/0015-onprem-ocr-workers.md)). Host provisioning, OS subscriptions and lab credentials live outside this repository. Leave the legacy `OCR_KEYVAULT` in its stack until a reviewed ownership transfer.

### Exchange account and nodes' group

The exchange account `stusnmo<suffix>` holds the OCR queues and `ocr-out`, which [`ocr-sync`](#ocr-pickup-ocr-sync-296) copies into `curated`. Its firewall allows `USNM_OCR_NODE_IPS` (the nodes' public IP; a list or CIDR ranges also work), kept private in the settings store and never in the repository ([Settings](environments-and-deploys.md#settings)). The provision adds a Microsoft.Storage service endpoint to `snet-cae`, which drops open connections from the subnet to storage public endpoints (not the lake's private endpoint), so run it while no ingest job runs.

```sh
scripts/azure/settings.sh --env prod --key USNM_OCR_NODE_IPS --value 203.0.113.7   # the nodes' public IP, private: the settings store
scripts/azure/settings.sh --env prod --key USNM_OCR_NODES --value true   # committed: pull request, merge, then provision from main
scripts/ocr-node/group.sh --env prod --dry-run
scripts/ocr-node/group.sh --env prod   # writes USNM_OCR_NODE_GROUP to the settings store (the local file until the store exists)
scripts/azure/provision.sh --env prod        # writes OCR_EXCHANGE_ACCOUNT, OCR_NODE_DCR_ID and OCR_NODE_APPINSIGHTS, grants the group
```

When the nodes' public IP changes, the nodes (and the scan archive uploader, which shares this firewall list) lose access until the firewall has the new address. Set it and provision; hidden messages reappear when their timeout ends:

```sh
scripts/azure/settings.sh --env prod --key USNM_OCR_NODE_IPS --value <new IP>
scripts/azure/provision.sh --env prod
```

**The nodes' group**, Entra security group `usnm-ocr-nodes-grp-<env>` (formerly `usnm-ocr-nodes-<env>`), gets Message Processor on `ocr-priority` and `ocr-bulk`, Message Sender on `ocr-deadletter`, the no-delete writer role on `ocr-out`, and Monitoring Metrics Publisher on `usnm-ocr-appi-<env>` only. Node Arc identities are members. Every member gets node roles, and owners can add members.

- `group.sh` creates it once (needs Groups Administrator or the default user right), saves its ID in `USNM_OCR_NODE_GROUP` and makes you owner. A rerun only renames in place (`displayName`/`mailNickname`), which needs rights to update the saved group; `--dry-run` only describes the rename.
- It never adopts a group by name or creates a duplicate of an unsaved legacy group: verify one with `az ad group member list --group <id>` and `az ad group owner list --group <id>` and save its ID by hand.
- Deployment stacks can't manage it (ADR-0015), so teardown leaves it. Delete it by ID (`az ad group delete --group <id>`), never by name.

Membership is a token claim, and managed identity tokens are cached about 24 hours with no forced refresh ([Microsoft](https://learn.microsoft.com/en-us/entra/identity/managed-identities-azure-resources/managed-identity-best-practice-recommendations#limitation-of-using-managed-identities-for-authorization)): a node that got a storage token before joining is refused until it expires, and a removed node keeps access for up to a day.

### Connecting a node

With the direct worker retired, `onboard.sh` is mainly for rerunning against a connected node:

```sh
scripts/ocr-node/onboard.sh --env prod --host admin@node1.example.org --name usnm-ocr-node1-arcs-prod --reboot-day Tue --dry-run
scripts/ocr-node/onboard.sh --env prod --host admin@node1.example.org --name usnm-ocr-node1-arcs-prod --reboot-day Tue
```

- **Requirements.** AlmaLinux 9 or RHEL 10 (registered, with BaseOS, AppStream and CodeReady Builder), key-based SSH, passwordless sudo. `--name` is `usnm-ocr-node<digits>-arcs-<env>`, at most 32 characters (ADR-0017); renaming a node means offboard, rename VM/hostname/DNS, onboard. You need rights to create app registrations, assign roles, write Arc machines, extensions and DCR associations, and own the group; the first run registers `Microsoft.HybridCompute`, `Microsoft.GuestConfiguration` and `Microsoft.HybridConnectivity` (subscription Contributor or Owner).
- **`onboard.sh`** creates a throwaway service principal `usnm-arc-onboard-<name>-<UTC time>` (2-hour secret, Azure Connected Machine Onboarding only; deleted and purged as soon as setup ends or fails), runs `scripts/ocr-node/node-setup.sh`, adds the machine identity to the group, turns on automatic agent upgrade, refuses any Azure extension other than the Azure Monitor Linux Agent, associates `OCR_NODE_DCR_ID` and installs that agent. A node connected before monitoring existed needs a provision, then a rerun (no credential needed for a connected node).
- **`node-setup.sh`** (idempotent) installs the Connected Machine agent, with an extension allow list holding only the monitoring agent, and turns guest configuration and incoming connections off. It also sets up a persistent journal (`/etc/systemd/journald.conf.d/50-usnm.conf`, `SystemMaxUse=1G`); post-quantum-first sshd key exchange (`/etc/ssh/sshd_config.d/40-pq-kex.conf`); users `usnm` and `usnmeng` with the [ADR-0016](../design/adr/0016-pluggable-ocr-engines.md) engine runtime ([13 §13.3](../design/13-ocr-engines.md#133-component-view): Podman socket `/run/usnm-ocr/podman.sock`, tmpfs scratch `/run/usnm-ocr/scratch` sized by `--scratch-size`, default 2G); and removes the retired worker's unit and nightly update. It checks that `usnm` can get a Resource Manager token (not storage, because of the token caching above).
- **TPM key store** (`--identity-key-store tpm`) needs the subscription feature `Microsoft.HybridCompute/HybridComputePreview` Registered; `az feature register --namespace Microsoft.HybridCompute --name HybridComputePreview` stays Pending until Microsoft approves (requested 2026-10-10); until then `onboard.sh` leaves the key in the agent's on-disk store. The store is fixed at connect.

### Node monitoring

The nodes have their own workspace `usnm-ocr-log-<env>` (0.1 GB daily cap) and component `usnm-ocr-appi-<env>` (`infra/modules/ocr-node-monitoring.bicep`), so a node can't write rows the API alerts read or fill the environment's 1 GB cap; at worst it fills the nodes' cap, which stops node monitoring until the reset. Rule `usnm-ocr-dcr-<env>` collects Warning+ syslog, worker `local0` lines at Info+, 60 s performance counters and `InsightsMetrics`, plus a heartbeat a minute; `DCRLogErrors` land in the same workspace. `StorageQueueLogs` on the exchange account (a row per queue request, about 3 MB an hour) are off so they don't eat the cap the BOINC feed alerts share (#1441); the diagnostic settings audit lists that queue service as non-compliant. Raise the cap with `ocrNodeDailyCapGb` in `infra/main.bicep` and `cap_gb` in `ops/queries/ocr-node-health.kql`, then provision.

Alerts, with alert emails set (severity 2): _OCR node log volume high_ (over 10 MB from one node in an hour), _OCR node: loc.gov asked to wait over an hour_ (hourly; `ocr.loc_slowdowns` with `capped`), and _OCR nodes' daily cap reached_ (`OverQuota`, as #279). Until the reset, node alerts and queries see nothing; `Usage | where IsBillable | summarize sum(Quantity) by DataType, bin(TimeGenerated, 1h)` in the nodes' workspace shows what filled it.

```sh
scripts/azure/logs.sh --env prod --query-name ocr-node-health             # per node: heartbeat, counters, CPU, memory, free space on /, worker lines, PerfMB, SyslogMB, InsightsMB and WorkerMB; DayMB and CapPct for the workspace
scripts/azure/logs.sh --env prod --query-name ocr-node-errors --span 2d   # worker warnings and errors, panics, failed units, OOM kills, other errors
scripts/azure/logs.sh --env prod --query-name ocr-node-updates --span 7d  # reboots, and the retired nightly worker update's history (#471)
```

These run against `OCR_NODE_LOG_ANALYTICS_WORKSPACE_ID`; the "usnm OCR nodes (<env>)" workbook on `usnm-ocr-appi-<env>` charts the same. `no heartbeat` means the node is down or lost its agent or rule association (rerun `onboard.sh`); `no counters` with heartbeats means check the rule and `/var/opt/microsoft/azuremonitoragent/log`. The BOINC feed evaluator's host (#1441, [boundaries and monitoring](../boinc/server/monitoring.md#existing-alert-route-and-deployment-gate)) shows `no counters` too, which is expected. A node silent for the whole span isn't listed: use `--span 7d` or check the Arc machine.

### Updates and reboots (#296)

A record for the two shut-down nodes. `dnf-automatic` (`/etc/dnf/automatic.conf`) installs every update daily at 06:00 UTC (plus up to an hour) with `reboot = never`, excluding the Connected Machine agent. `usnm-ocr-reboot.timer` runs `/usr/local/sbin/usnm-ocr-reboot-if-needed` (`scripts/ocr-node/reboot-if-needed.sh`) at 04:00 US Eastern on the node's `--reboot-day` (without the flag a node keeps its day).

- It reboots only if `dnf needs-restarting -r` says so. Updates that need only a service restart (`dnf needs-restarting -s`) wait for the next reboot.
- It waits up to 30 minutes for a running `dnf-automatic`, else skips that week's reboot.
- It stops the [Serial Set fetch](congress-pages.md#the-serial-set-on-prem) (`usnm-sset`) and waits up to 1,320 s. The fetch starts again at boot until it has finished (`/var/lib/usnm-sset/done`) and takes over its own lock.
- Non-enabled units it stopped are listed in `/var/lib/usnm-ocr-reboot/resume` and restarted by `usnm-ocr-reboot-resume.service`.
- It logs to the journal under `usnm-ocr-reboot`; the reboot and any problem are at Warning, so they reach the nodes' workspace.

| Node                               | Reboot window                |
| ---------------------------------- | ---------------------------- |
| Node 4, `usnm-ocr-node4-arcs-prod` | Sundays, 04:00 US Eastern    |
| Node 3, `usnm-ocr-node3-arcs-prod` | Wednesdays, 04:00 US Eastern |

```sh
systemctl list-timers dnf-automatic.timer usnm-ocr-reboot.timer
journalctl -u dnf-automatic -u usnm-ocr-reboot --since -8d
dnf needs-restarting -r                                  # would the next window reboot?
sudo /usr/local/sbin/usnm-ocr-reboot-if-needed --dry-run # what it would do now
```

By hand, one node at a time: `sudo systemctl stop usnm-sset`, `sudo dnf -y upgrade --exclude=azcmagent`, `sudo systemctl reboot` (the fetch restarts by itself), then check `uname -r` and `getenforce` (Enforcing) before doing the other node.

### Remove a node

```sh
scripts/ocr-node/offboard.sh --env prod --name usnm-ocr-node1 --host admin@node1.example.org --dry-run
scripts/ocr-node/offboard.sh --env prod --name usnm-ocr-node1 --host admin@node1.example.org
```

No provision is needed. It removes the identity from the group and deletes the machine's DCR associations. With `--host` it deletes the monitoring extension and waits while the node uninstalls it, then on the node turns off `dnf-automatic.timer` and the reboot window (stopping a running window before removing its units and script), stops and disables `usnm-ocr-worker.service`, and runs `azcmagent extension remove --all` and `azcmagent disconnect --force-local-only` (no Azure sign-in needed). Without `--host`, for a node that's gone or unreachable (as both shut-down nodes are), the extension goes with the machine and it prints the node commands for later. Last it deletes the Arc machine, and with it the identity. It needs group ownership and delete rights in the resource group. OS registration and Lightspeed reporting are left to the infrastructure operator. It leaves the [Serial Set fetch](congress-pages.md#the-serial-set-on-prem) alone: first run `sudo systemctl disable --now usnm-sset usnm-sset-status.timer`, or it retries every 15 minutes without an identity.

To cut a node off before its token expires, also remove its address from `USNM_OCR_NODE_IPS` and provision (with every node behind one IP, that cuts off all of them).

Offboard every node before turning `USNM_OCR_NODES` off; the provision stops before starting if `OCR_NODE_DCR_ID` still has associations. Turning it off deletes the exchange account with its queues and `ocr-out`, and the nodes' workspace with its data, component, rule and three alerts. Arc machines stay in the resource group, and their identities in the group, until offboarded. **Do not turn `USNM_OCR_NODES` off while BOINC runs:** `infra/main.bicep` gates the exchange account, the scan archive, the OCR Key Vault, the on-prem `ocr-sync` grants and BOINC's on-prem claim/collect grants on it, so BOINC's queue-fed work would lose `ocr-priority`, `ocr-bulk` and `ocr-out`.

### Worker publication catch-up

The `ocr-worker-*` release still carries the binary the scan archive and scan-fetch hosts install, and the LAN dashboard. The release job publishes for `main`, or an older commit if nothing since changes either build (`scripts/ci/head-of-main.sh --unless-changed ocr_worker,ocr_dash`, #1074). A release can still be missing: a newer build-changing commit's run failed or was cancelled, and the older commit's run skipped because the newer one would release. To publish current main without the API rollout:

```sh
gh workflow run ci.yml --repo tgoodyear/usnewsmap.com --ref main \
  -f scope=ocr-worker -f correlation=ocr-worker-catch-up
```

This runs only the Rust checks, the EL9 build, RHEL and engine smoke checks, the private-address check and the attested publication. It does not select API images or rollouts, the BOINC server and bridge builds, infrastructure provisioning or host installation; `scope=full` (the default) does, so it is not publication-only. Worker-only scope refuses a non-main ref or unknown scope, and keeps the main-only guard, build checks, checksums and attestation. If main gains a build-changing commit first the guard skips, and a successful run with skipped publication is not a release: check the publication steps and rerun against new main. Before any separately authorized installation, verify that the tag's source includes the required fix, and the provenance of its binary and checksums. Publishing is not installed-source acceptance; host installation is managed separately.

### Prefetched scans (#368)

`ocr-prefetch-manifest` builds the manifest and `scripts/ocr-node/pull-manifest.sh` verifies and submits it to the scan server. BOINC takes cached scans through the [scan-cache hand-off](../boinc/server/scan-cache-handoff.md) (the `--scan-cache-url` workers are retired). The scan-fetch service's `status-full.json`, `failures-full.jsonl` and journal explain capacity, cooldown and failures.

### The scan archive (#442)

Keep-set scans are copied to Cold container `scans` in `SCAN_ARCHIVE_ACCOUNT` ([ADR-0018](../design/adr/0018-scan-archive.md)): the off-site copy, and the source the contributor-compute bridge reads before LoC. The uploader runs on the host holding the scans as its Arc identity, not through a workstation. The account has shared keys and public access off and firewall default Deny. The buffer set is not archived (ADR-0018, decision 11; `--set buffer` is for tests).

**State.** Installed through #641; JPEG fix #644 in release `ocr-worker-20261016-00fbdd9`. The 16 October 2026 11:26:03 UTC recovery point is fully verified: 148,127 scans, none missing, mismatched, invalid, conflicting, errored or skipped. It was verified against a temporary ZFS snapshot (final top-up 5,203 scans, 17,530,564,063 bytes). Since 17 October the top-up and verify timers are disabled under the local-first cost decision (#783); existing bytes and manual units remain. Scans fetched later aren't covered; read current reports. Direct-client API and tickets: #649.

1. **Provision.** The account is in the stack with `ocrNodes` and uses `USNM_OCR_NODE_IPS` for its firewall. `scripts/azure/provision.sh --env prod --what-if`, then without it: the policy step adds the exemption first, then the stack adds the account, the writer role and its grant to the nodes' group (`USNM_OCR_NODE_GROUP`). `SCAN_ARCHIVE_BLOB_ENDPOINT` is the account URL.
2. **Identity.** Keep the scan host out of the nodes' group (queue permissions, node pacing). Grant its Arc principal the archive role (create and read, no delete) on `scans`:

   ```sh
   az resource show -g <Arc resource group> -n <Arc machine name> --resource-type Microsoft.HybridCompute/machines --query identity.principalId -o tsv
   scripts/azure/settings.sh --env prod --key USNM_SCAN_ARCHIVE_UPLOADER_ID --value <that object id>
   scripts/azure/provision.sh --env prod --what-if
   scripts/azure/provision.sh --env prod
   ```

   If the preview has unrelated active-job changes, use `bash scripts/ocr-node/grant-archive-uploader.sh --env prod --dry-run`, then without `--dry-run`: it applies only that assignment, consistent with later full provisions.

3. **Host install.** Download `usnm-ocr-worker` and `SHA256SUMS` from the release, verify both with `gh attestation verify --repo tgoodyear/usnewsmap.com --signer-workflow tgoodyear/usnewsmap.com/.github/workflows/ci.yml --source-ref refs/heads/main` and check the digest. Stage them with the four archive units, `usnm-scan-archive.conf` and `check-scan-archive.sh` from reviewed main in a root-owned directory, then run `bash scripts/ocr-node/install-archive-host.sh --dir <staged-directory> --release <release> --binary-sha256 <verified-sha256> --archive-url <archive-container-url> --dry-run`, then without `--dry-run`. It installs the binary at `/usr/local/bin/usnm-ocr-worker`, `usnm` in `himds` and the scan-read group, `jq`, environment, tmpfiles and units; refuses a conflicting environment or an active archive or OCR service; and starts nothing. By hand:

   ```sh
   sudo install -m 0644 scripts/ocr-node/usnm-scan-archive.service scripts/ocr-node/usnm-scan-archive.timer \
     scripts/ocr-node/usnm-scan-archive-verify.service scripts/ocr-node/usnm-scan-archive-verify.timer /etc/systemd/system/
   sudo install -m 0644 scripts/ocr-node/usnm-scan-archive.conf /etc/tmpfiles.d/
   sudo systemd-tmpfiles --create /etc/tmpfiles.d/usnm-scan-archive.conf
   printf 'USNM_ARCHIVE_URL=%s\n' "<SCAN_ARCHIVE_BLOB_ENDPOINT>scans" | sudo tee /etc/usnm/scan-archive.env
   sudo systemctl daemon-reload
   ```

4. **Bounded acceptance.** On the host holding the scans, run the dry run, sample upload and verify:

   ```sh
   sudo bash scripts/ocr-node/check-scan-archive.sh --limit 100
   # Subsequent bounded checks without uploading, sampled across the upload's manifest:
   sudo bash scripts/ocr-node/check-scan-archive.sh --limit 100 --verify-only \
     --sample-from /var/lib/usnm-scan-archive/archived.tsv --deadline 1800 --max-load 20
   ```

   It requires the installed binary, service user, Arc group, tmpfiles configuration and environment file, and never enables timers or starts a full upload. Each phase runs as `usnm` in a transient unit with the Arc environment, under the shared lock in root-owned `/run/lock/usnm-scan-archive` (so the service account can't swap it for a symlink). A concurrent upload, verify or check makes the helper fail before sampling. The dry run writes nothing, the upload creates at most `--limit` (1–1,000, default 100) missing scans, and verify compares size, md5, tier and stored sha256 from properties without downloading. Acceptance needs a nonempty sample, every scan reported and present, and clean reports. The units drop `USNM_ARCHIVE_STATE` and set `RUST_LOG=off`, so the state file is untouched and the JSON report clean.

   The helper fixes the sample itself (#895): the worker lists all of `--scans` before applying `--limit`, and on 28 October 2026 that walk of about 190,000 scans sat in filesystem wait for 8 minutes at load 25. With `--sample-from FILE` it spreads `--limit` names evenly over the file's first column; otherwise it takes the first names depth first. Names must be plain `keep/…/<name>.jp2` or `.jpg` with regular-file scan and sidecar. Each phase sees only those files, bind-mounted read-only on a tmpfs `--scans` root with `ProtectSystem=strict` and `ProtectHome=yes`. Output shows the sample size and a digest, never names.

   It runs at idle I/O, nice 19, `--concurrency 1`. `--deadline` (60–86,400 s, default 1,800) becomes `RuntimeMaxSec`, and a phase that hits it fails. Every `--progress` seconds (default 30) it prints elapsed time, bytes read and load; above `--max-load` it stops its own unit and fails. If the shell may close: `sudo systemd-run --unit=usnm-archive-check-manual --pipe --wait --collect -p RuntimeMaxSec=3600 -p IOSchedulingClass=idle -p Nice=19 bash <reviewed copy>/check-scan-archive.sh …`.

   **Restore drill (#895, maintainer approval each time).** `scripts/ocr-node/restore-drill.sh` restores a sample from Cold and from a ZFS snapshot and checks every copy. Run it only with approval for the scratch dataset and Cold read, while `tank` is quiet (no #1483 copy writing: `zfs get written` unchanged over a minute).

   ```sh
   sudo zfs create -o quota=2G tank/usnm-restore-drill-<date>   # never mounted into a CT
   sudo chmod 0700 /tank/usnm-restore-drill-<date>
   sudo systemd-run --unit=usnm-restore-drill --pipe --wait --collect -p RuntimeMaxSec=900 \
     -p IOSchedulingClass=idle -p Nice=19 bash <reviewed copy>/restore-drill.sh \
     --dir /tank/usnm-restore-drill-<date> --dataset tank/usnm-restore-drill-<date> \
     --snapshot <snapshot-name> --count 20 --budget-bytes 100000000
   sudo zfs destroy tank/usnm-restore-drill-<date>
   sudo umount /tank/ocr/scans/.zfs/snapshot/<snapshot-name>   # if the automount outlives zfs_expire_snapshot
   ```

   `<snapshot-name>` is the part after `@` of a recent daily snapshot from `zfs list -t snapshot -o name tank/ocr/scans` (for example `autosnap_2026-11-05_00:00:03_daily`).
   - It takes `--count` (1–99) distinct names spread over `archived.tsv`, half a step off the acceptance sample, and HEADs them, stopping before `--budget-bytes` (at most 100,000,000) would be exceeded.
   - It GETs each (capped at its HEAD length) and copies the same scans from the snapshot; scans newer than the snapshot are skipped there, and the drill fails if no sampled scan is in the snapshot.
   - Each Cold copy's length, sha256 and md5 must match the HEAD length, the sidecar (format `jp2`/`jpeg` matching the extension, `full` resolution), the blob's `sha256` metadata and Content-MD5, tier `Cold`, and the live and snapshot copies. Only JP2 sidecars carry an md5; one without it fails.
   - It writes only into `--dir` (the empty mountpoint of `--dataset`) and prints timings and a digest, never names or the token; missing files are reported by index.
   - It asks the Arc agent on 127.0.0.1, not `localhost`: curl tries `::1` first, where himds accepted on `[::1]:40342` and never answered. Token requests time out after 30 s.

   **5 Nov 2026 result ([#895](https://github.com/tgoodyear/usnewsmap.com/issues/895#issuecomment-6667800874)):** 20 scans (16 JP2, 4 JPEG), 88.3 MB from Cold in 9.3 s (9.5 MB/s) and 1.3 s from the snapshot, all byte-identical. The 870.6 GB keep set would take about 25 hours from Cold on one stream.

5. **Full upload (manual, cost-approved only).** `sudo systemctl start usnm-scan-archive` uploads the keep set (1.16 TB when the fetcher is done; about 26 hours at 100 Mbps, UNVERIFIED for this uplink); `journalctl -u usnm-scan-archive` shows progress every 1000 scans. It can be stopped and started, and skips what's there. `/var/lib/usnm-scan-archive/archived.tsv` lists archived scans (name, size, mtime ns, inode, ctime; older three-field lines are rechecked), and unchanged listed scans get no request; delete it to recheck all. Don't enable the timers under the local-first policy; resuming the old schedule after a cost review is `sudo systemctl enable --now usnm-scan-archive.timer usnm-scan-archive-verify.timer`. Verify (`sudo systemctl start usnm-scan-archive-verify`) hashes every local scan and makes a properties request per blob, so get cost approval first. Upload, verify and acceptance share the `flock` on `/run/lock/usnm-scan-archive/lock`: the upload and verify services wait for each other instead of reporting not-yet-uploaded scans as missing, while the acceptance helper fails on contention. A blob without stored MD5 never counts as present (`conflicts` on upload or dry run, `mismatched` on verify) and isn't recorded.
6. **Report fields.** Exit is non-zero unless all are zero; the final JSON lists up to 20 problem paths.
   - `bad_local`: sidecar missing or not matching bytes, resolution or format; not uploaded, so investigate before fetching it again. JPEG sidecars use `format="jpeg"` (old ones without a format default to JPEG). Releases through `ocr-worker-20261016-c66d480` wrongly rejected valid JPEGs: rerun with a fixed release, don't edit sidecars or refetch.
   - `conflicts`: the blob differs from the local file; never overwritten, investigate.
   - `errors`: failed requests; rerun.
   - `skipped`: recorded and unchanged, not in `scanned`.
   - `mismatched` (`--verify`): includes missing or different sha256 metadata.

## OCR pickup: ocr-sync (#296)

`usnm-ingest ocr-sync` imports OCR output from `ocr-out` into the lake ([12 §12.5 Step 8](../design/12-onprem-ocr-pipeline.md)). Runs hold the Cosmos lock `ops/ocr-sync` (holder `usnm-batch-<pid>-<nanos>` on-prem, #1133). Logs carry page ids, paths and reason codes, never text.

**Where it runs (#917).** With `USNM_OCR_SYNC_ONPREM` (prod), in the batch container, CT 215 on its Proxmox host, as the container's Arc identity (`USNM_BATCH_HOST`). Its `usnm-ocr-sync.timer` (home-infra `scripts/lab/batch-ct-ingest.sh`) runs `usnm-ingest ocr-sync --max-runtime-secs 3000` at minute 17 from a commit-pinned released binary, reading `/etc/usnm/ingest.env`, not the stack. After changing any setting below with `scripts/azure/settings.sh` and provisioning, rewrite that file with `scripts/onprem/ingest-env.sh` (shown under seeding). Journal: `journalctl -u usnm-ocr-sync`; metrics go to the environment's Application Insights. Grants (`infra/modules/ocr-sync-runner.bicep`):

- lake read/write/create (no delete) on `curated` (under `ocr-local/`), `reference` (`status/ocr-local.json` only) and `ocr-requests`;
- exchange read/write/create/delete on `ocr-out` and `ocr-quarantine`;
- Message Processor on `ocr-deadletter`, Message Sender and Queue Data Reader on all three queues (`--from-quarantine` puts dead letters back on `ocr-deadletter`);
- Cosmos Data Contributor on `ops`, Monitoring Metrics Publisher, AcrPull.

The Azure job `usnm-ocrs-caj-<env>` (`OCR_SYNC_JOB`; `usnm-ocrsync-caj-<env>` before #309) exists only when `USNM_OCR_NODES` and the ingest jobs are on, runs as `id-usnm-ingest` (0.5 vCPU, 1 GiB) on `USNM_OCR_SYNC_CRON`: hourly at minute 17 by default, while `manual` (as in prod) turns the schedule off. The same condition creates `ocr-quarantine` and the job's two alerts.

Each run:

1. Deletes sources a previous run imported or quarantined but didn't delete (listed with ETags in `curated/ocr-local/import/<run>.json`).
2. Drains `ocr-deadletter`: valid dead letters to `curated/ocr-local/deadletter/<day>/<run>.jsonl` with a `dead_lettered` outcome, the rest to quarantine. An interrupted drain repeats after 10 minutes, so a dead letter can appear twice.
3. Checks up to `--max-parts` (2,000) parts, oldest first, in imports of `--chunk` (200): name and size, strict reader, page outstanding with the same batch and reason, text hash and length, engine, batch, title and image, and the bounded ALTO reader. Passing ALTO goes to `curated/ocr-local/alto/` and the row (with both texts' scores) to `curated/ocr-local/pages/<lccn>/<run>.parquet`, with an `imported` outcome; failures go to `ocr-quarantine` with their ALTO. Sources are deleted with If-Match.
4. Processes user requests, when configured.
5. Tops up `ocr-bulk` with a seed plan.
6. Writes `status/ocr-local.json` to `reference`: last-run and running totals of imports, rejects by reason, dead letters by cause, queue depths, pages outstanding, parts left and oldest age, and seeding.

**User requests.** One oldest incomplete closed UTC day from the API's `ocr-requests` per run, at most 10,000 shuffled IDs, one score each. Only qualifying `/v1/hits` responses count; DNT, GPC, crawler and off-site requests are excluded, and no query text or request metadata is stored. Pages with LoC damage at least 0.1 go to `ocr-priority` with `queued_at` rounded to midnight UTC; the ledger keeps the real time for ordering and the day for the cap, `USNM_OCR_USER_DAILY_MAX` (default 1,000 distinct pages). Imported, outstanding, Japanese and undamaged pages are skipped.

- Lookups scan each damage table once in batch order; `curated/ocr-local/requests/<day>/lookup.json` checkpoints version, IDs, cursor and scores (no query or time). A deadline pause resumes it; a new published version resets it.
- A failed send or runtime deadline leaves the day incomplete for retry. Sent and ambiguous sends stay charged; confirmed-unsent ones and unattempted ones (`not_sent`) stay eligible. Retries skip pages charged to the day, terminal dead letters included.
- On completion it writes `completed/<day>.json` before emptying the payload and checkpoint, and leaves an empty tombstone so concurrent closes can't recreate the day. **Do not delete markers or tombstones: they prevent replay.** Interrupted cleanup doesn't spend the next day's slot. Old versions expire after a day (plus soft-delete retention).
- Request failures are returned after status is published; seeding still runs if time allows.

**Queue pages by hand.** Only the pickup identity can enqueue, so `ocr-enqueue` runs in the batch container through `usnm-ingest-run`, run as root on its Proxmox host (home-infra `docs/lab.md`, "Ingest commands"). It finds each page in the published version (Step 6's damage tables, or the curated text where they lack the batch) and writes a ledger row (`curated/ocr-local/queued/<day>/<run>.parquet`) before each message (base batch, ISO 639-3 language, title's first for `und`, LoC damage as score), skipping outstanding pages and Japanese titles; `--dry-run` only counts. After a failed send the rest get `not_sent`, so rerun; `--include-outstanding` resends the one that may have gone. `ocr-enqueue` and `ocr-requeue` hold `ops/ocr-sync` outside `--dry-run`, so they fail during a pickup run.

```sh
pct exec 215 -- /usr/local/bin/usnm-ingest-run --command ocr-enqueue -- --pages sn83045462_1890-05-03_ed-1_seq-7,... \
  --reason bulk --plan pilot-german [--batch nbu_gage] [--dry-run]
```

`ocr-requeue` requeues dead letters by cause (`dequeue_count`, `invalid` with kind, `permanent`, `unroutable`), skipping pages queued or imported since, or whose latest dead letter has another cause:

```sh
pct exec 215 -- /usr/local/bin/usnm-ingest-run --command ocr-requeue -- --cause dequeue_count [--since 2026-10-20] [--dry-run]
```

**Seeding** ([12 §12.5 Step 10](../design/12-onprem-ocr-pipeline.md)) is off while `USNM_OCR_SEED_PLAN` is empty. Each run ranks (batch, title) units by damage over 0.1 from Step 6's tables (run `ocr-damage` first; it refuses without them) and queues pages at or above `USNM_OCR_SEED_MIN_DAMAGE` (default 0.25) in issue order.

- **Window.** At most `USNM_OCR_BULK_WINDOW` bulk pages outstanding (ledger or queue count, whichever is larger), topped up when `ocr-bulk` falls below `USNM_OCR_BULK_REFILL_BELOW` (default half). Window 0 (default) is two days at last week's import rate, at least 500. Manual enqueues count toward it but aren't limited.
- **Filters.** `USNM_OCR_SEED_LANGUAGES` (ISO 639-3, default `ces,deu,eng,pol,rus,srp`; MARC `ger` reads as `deu`), `USNM_OCR_SEED_BATCHES`, `USNM_OCR_SEED_YEARS` (`1890` or `1850-1919`), `USNM_OCR_SEED_MAX_PAGES` (budget), `USNM_OCR_SEED_PILOT` (one language or batch list, budget at most 10,000), `USNM_OCR_SEED_SKIP_AMERICAN_STORIES` (off; skips pages whose American Stories text scores under the minimum).
- **Skips and retries.** Japanese titles, ledger pages and duplicate copies another batch wins are skipped. A page outstanding 14 days or dead-lettered `dequeue_count` is requeued at most 3 times; `unroutable` after 6 hours, doubling to 48; `not_sent` next run.
- **State.** `curated/ocr-local/seed/rank/` and `curated/ocr-local/seed/plans/<plan>/done/`. A run that times out before ranking every batch queues nothing new; the next run continues the ranking.

**From fetched scans** (Step 11, #368). `USNM_OCR_SEED_SOURCE=fetched` takes new pages from the keep set of `audit/damage/v2/prefetch/<version>/manifest.parquet` (`USNM_OCR_SEED_MANIFEST`, default the published version) in fetcher order (German before the 1920s first, each issue together), filtered by the plan's languages, years and damage, and limited to the newest list in `ocr-out/fetched/<version>/`. The scan fetcher's host publishes that list from the scan server's index with `usnm-ocr-worker publish-fetched` (#1056); the list only narrows the manifest, so it can't add a page. Window, budget, ledger, retries and skips are as above.

**Superseding by verified third-party OCR** (#811). Off while `USNM_OCR_SEED_SUPERSESSION` is empty. Store a reviewed list with `usnm-ingest ocr-alternate-supersession --list list.json` (checks entries, prints its SHA-256), pin the hash in a settings PR and rewrite the on-prem settings file. Seeding then rechecks each listed page and skips it as `superseded_alternate`; a missing list or hash mismatch fails the run, and revoked, baseline or unverifiable selections queue normally. Manual enqueues and retries are unaffected ([note](../notes/2026-11-05-loc-alternate-supersession.md)). `usnm-ingest ocr-alternate-accounting --supersession <sha256>` reports read-only which listed pages are avoidable, imported, in flight, settled or unverified ([accounting note](../notes/2026-11-05-loc-supersession-accounting.md)).

`ocr-seed` runs seeding alone; flags override settings and `--dry-run` prints the JSON plan. For a pilot, preview then set:

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
# The batch container reads its own copy of the settings: write it again.
scripts/onprem/ingest-env.sh --env prod | ssh root@<proxmox-host> /root/batch-ct-ingest.sh --env-file -
```

**Quarantine.** Rejects stay 30 days as `rejects/<run>/<source path>` (dead letters `rejects/<run>/deadletter/<message id>.txt`) with a `.reason.json` (reason, source, ETag, size, creation time; never content; oversized sources get only the record). Read them with an operator's own sign-in from an allowed IP (the firewall allows the node IPs and the subnet). After a fix, put one reason's rejects back where they came from; the same run checks them again:

```sh
pct exec 215 -- /usr/local/bin/usnm-ingest-run --command ocr-sync -- --from-quarantine --reason not_requested
```

Reason codes: `name`, `part_too_large`, `part_unreadable`, `rows`, `issue_mismatch`, `alto_missing`, `alto_too_large` (checks 1 and 2); `not_requested`, `batch_mismatch`, `reason_mismatch` (3); `text_null`, `text_too_large`, `text_control`, `text_mismatch` (4); `engine`, `engine_tag` (an `ocr_engine` with `image=local`, or not printable), `reason`, `batch_unpublished`, `title_unknown`, `title_japanese`, `image_url` (5); `alto_*` from the ALTO reader (6); `deadletter_*` for dead letters.

**Gain estimate.** `ocr-sync --report` applies rule v1 (Step 9) to stored scores without importing and writes `curated/ocr-local/report/<run>.json` (considered, replaced, reasons LoC's text stays, by language and engine):

```sh
pct exec 215 -- /usr/local/bin/usnm-ingest-run --command ocr-sync -- --report
```

**Alerts** (with alert emails). `USNM_OCR_SYNC_ALERTS` (on in prod with `USNM_OCR_SYNC_ONPREM`; #1139) deploys three log alerts on the environment's workspace from the run's Application Insights metrics (`infra/modules/ocr-sync-alerts.bicep`), so the batch container needs no agent. Each costs $0.50 a month, and each email's `Detail` gives the relevant time or day:

- `usnm-ocrs-failed-alert-<env>` (severity 2, every 15 min): `ingest.command_failures` for `ocr-sync` in 30 minutes; the error is in the journal. Needs `usnm-ingest` from #1139 or later (`batch-ct-ingest.sh --install`); the other two work with any release.
- `usnm-ocrs-stale-alert-<env>` (severity 2, every 15 min): no `ocr.out.oldest_age_s` for over 2 hours (2 h 20 min): a stopped timer, down container, killed run or lost telemetry.
- `usnm-ocrs-backlog-alert-<env>` (severity 3, hourly): oldest part over 72 hours (`ocr.out.parts_left` has the count). `ocr-out` deletes blobs 30 days after last change, so an old backlog is lost.

The Azure job instead uses `usnm-ingest-job-failed-alert-<env>` (severity 2; #308) and the backlog alert.

## Our OCR in releases (#296)

With `USNM_OCR_LOCAL` (`--ocr-local`), a release compares `curated/ocr-local/` text with LoC's on the batches it indexes and uses ours where rule v1 prefers it ([12 §12.5 Step 9](../design/12-onprem-ocr-pipeline.md)). Off by default, and then nothing in `ocr-local` is read. No new grant is needed.

```sh
scripts/azure/settings.sh --env prod --key USNM_OCR_LOCAL --value true
scripts/azure/provision.sh --env prod
```

- **Scope.** The weekly delta applies it to batches it indexes; published batches only at a [full rebuild](rebuild-and-index-storage.md#full-rebuild). Main-index releases log `our OCR in ocr-local` and, with published batches, `pages of ocr-local that would replace LoC's text in published batches`. Japanese-overlay-only releases read nothing. `ocr-sync --report` ([above](#ocr-pickup-ocr-sync-296)) estimates without a release.
- **Records.** `reference/<version>/ocr_local.json` (rule and metric versions, parts, pages considered/replaced/kept by reason, language and engine, revocations), `ocr_local_pages.bin`, the manifest's `built_from.ocr_local` and the build record's `features.ocr_local`. Hits on those pages show "Our OCR" with an unhighlighted viewer link; the Japanese overlay wins where both apply.
- **Inputs.** `ocr_local.json` keeps sorted `inputs` (each part's path, length and SHA-256 of decoded bytes), hashed by the manifest. They cover this run only; keep the immutable parts to reconstruct selection. Different bytes at the same path across batches stop the release. Finishing checks the per-batch input digest in the original JSON log; for an older interrupted run without it, finish with the original binary.
- **Revocation.** A create-only `curated/ocr-local/revoked/<stamp>.json` with `reason`, `reference` and at least one of `node`, `engine`, `run`, `from`, `to` (every field given must match) drops matching rows from the next release of their batches; published text stays until a full rebuild. An unreadable file stops the release.
- **Off.** Clearing the setting stops new replacements; deltas still carry `ocr_local_pages.bin` forward, and the next full rebuild without it restores LoC's text.

## The OCR dashboard on the LAN (#296)

`usnm-ocrdash` summarizes acquisition, local OCR, volunteer compute, evaluation and publication on an access-controlled internal endpoint ([12 §12.2.5](../design/12-onprem-ocr-pipeline.md#1225-the-lan-dashboard)). Pipelines sit under the summaries; Operational details (workers, batches, pages, queues, hosts, provenance) start collapsed. Phones get step cards instead of SVG maps and tables. Data: `/api/view.json`, and per panel `/api/hosts.json`, `/api/fetch.json`, `/api/nodes.json`, `/api/next.json`, `/api/ocred.json`, `/api/throughput.json`. DNS, TLS, containers, access rules, source URLs, mounts and host-stat collectors belong to infrastructure tooling.

**Counts.** Summaries use fresh reports, not one pipeline percentage. BOINC counts are assigned or unsent replicas, not pages or devices. The evaluation card groups assignments by purpose; campaign readiness and acceptance aren't connected to it. Catalog counts cover imported evidence only. Missing evidence is unknown, never zero, and snapshot time is shown apart from evidence age.

**Rates.** Every active box shows a rate or "rate unknown".

- Fetch rates are last-hour completions shared across keep and buffer.
- Worker rates are last-hour successes across both sets.
- Serial Set throughput is a run-to-date average, not an upload rate.
- The queue, `usnm_ocr`, assigned and validation boxes use the step throughput counters.

Don't add repeated rates together. Queue stock and BOINC assignments are stock, not rates. Steps without telemetry say "not tracked here", and user-driven priority queueing is marked "not built". The dashboard doesn't query Azure ingest, release or search.

**Diagrams** are inline SVG in `crates/usnm-ocr-dash/src/diagrams/`. When the pipeline changes, edit the SVG and the answers in `diagrams.rs`; tests check XML, markers, unique ids, freshness, rate units/scope and that no private addresses or internal host names appear.

### Step throughput (#704)

Per step: the last full hour, last 24 hours, seconds per page, known limit and a week of 4-hour bars; then valid, inconclusive, invalid, error and waiting results for `usnm_ocr`, `usnm_quality` and `usnm_qwen` (replicas; #1420) and controller phases. At `/api/throughput.json` and `throughput` in `/api/view.json`. The scan fetch comes from the fetch index; other sources run `scripts/ocr-node/throughput-collect.py` and PUT hourly counts to `/api/throughput/<source>`, accepted only from the configured address (`--throughput-push <source>=<host>` / `USNM_OCRDASH_THROUGHPUT_PUSH`).

| Source       | Runs on                        | Reads                                                                                                      | Gives                                                                                        |
| ------------ | ------------------------------ | ---------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| `controller` | the priority controller's host | its journal                                                                                                | ticks passed and failed, tick and phase seconds, pages claimed and collected                 |
| `ship`       | the quality shipper's host     | the shippers' and result loaders' journals                                                                 | pages shipped, staged, submitted and loaded                                                  |
| `boinc`      | the BOINC server               | two bounded aggregate queries on `result` and `workunit`, and podman start/died events for step containers | results by outcome, workunits with a canonical result, device seconds, bridge lock hold time |
| `batch`      | the host that runs `ocr-sync`  | `usnm-ocr-sync.service`'s journal                                                                          | pages in committed imports, rejected parts, dead letters, `ocr-bulk`'s depth at each run     |

Collectors recompute whole hours, so reruns correct rather than add. The `boinc` queries are read-only, aggregate-only and stop at five seconds; run it with `USNM_THROUGHPUT_HOURS=72` (quorums settle late) and journal sources with 3. Backfill once with `--hours 336` (`db_purge` keeps results 30 days). History is `throughput.json` in the state directory (two weeks, a few hundred KB). Uncovered hours are unknown. A source silent 45 minutes is stale, and its rates turn unknown as the hours pass. `usnm_quality` and `usnm_qwen` count workunits, not pages, while their load rows count pages. Lock hold time covers step containers only, so it is a minimum: the result loaders and native Qwen submission hold the lock without a container.

### Corpus data flow snapshot (#704)

The optional "Corpus data flow" panel polls a local metadata-only `corpus-catalog` JSON file (15 s default), served at `/api/pipeline.json` and as `pipeline` in `/api/view.json`. No database, cloud or credentials. Counts come hourly from the throughput history ([14: Current evidence](../design/14-corpus-data-flow.md#current-evidence)) and are not whole-corpus totals; zero can mean no catalog. The producer publishes atomically, covers absent sources with coverage and issues, and never includes OCR text or storage URIs. Set `--pipeline-snapshot` / `USNM_OCRDASH_PIPELINE_SNAPSHOT`, and `--pipeline-max-age-secs` / `USNM_OCRDASH_PIPELINE_MAX_AGE_SECS` (default 900; 1–604800). Open panels and scroll positions survive refresh.

**Input limits.** 1 MiB, schema version 1, producer `corpus-catalog`. Rejected: unknown fields, invalid UTC RFC3339 times, times over 60 s ahead, negative, fractional or over-trillion counts, `verified + missing > locations`. At most 32 coverage strings (128 chars), 64 stages (id 256, label 128, detail 512), 128 issues (code 64, detail 512); no control characters; unique stage ids; unknown counts null.

**Freshness.** The wrapper reports `retrieved_at`, `last_error`, `age_secs`, `stale`, optional `snapshot` and `stage_freshness`. Source `observed_at` sets freshness, not `generated_at` or retrieval time (null is unknown); per-stage `observed_at` overrides it (explicit null unknown, omitted inherits). Stale stage evidence stays visible even when another source was just observed. A bad or missing file keeps the last valid snapshot with an error and hides its live counts; with none, the panel says unavailable and never falls back to worker totals. The diagnostic heading reads "Some evidence old or unknown".

### Publication monitoring

`--publish-status /existing/publish-fetched.json` (`USNM_OCRDASH_PUBLISH_STATUS`) reads a bounded native record or fresh status-mirror envelope for `usnm-loc-publish-fetched.service` and shows its last completion time, result and exit status. Missing, expired or unreadable metadata is unknown, source observation time is not run time, and a successful run doesn't mean new pages. Metadata can vanish across a restart: wait for the next run rather than forcing one. Install a compatible release before adding the setting.

The home-infra v2 transport reads five properties by typed `busctl get-property` (not `GetAll` or `systemctl show`), never invoking the publisher or reading its environment or journal: `Unit.LoadState`, `Unit.ActiveState` (`s`), `Service.Result` (`s`), `Service.ExecMainStatus` (`i`), `Service.ExecMainExitTimestamp` (`t`). `busctl --json=short` returns scalar `data` (Debian 12: `{"type":"t","data":1793191480159818}`), UTC microseconds, zero for none. The timestamp is read before and after the others and active state rechecked; disagreement is unknown. The [collector](https://github.com/tgoodyear/home-infra/blob/d14176e9ba9798930244dfd2716e1fb7d2ff3d51/scripts/lab/ocrdash-source-status-v2.py) and [tests](https://github.com/tgoodyear/home-infra/blob/d14176e9ba9798930244dfd2716e1fb7d2ff3d51/scripts/lab/test_ocrdash_sources.py) need home-infra access; the dashboard checks only the record, so review the collector, run its tests and confirm the API on the source host before deploying.

### Public view

`--public-listen` / `USNM_OCRDASH_PUBLIC_LISTEN` (unset by default) adds a plain-HTTP listener for a Cloudflare Tunnel serving only GET and HEAD of `/`, `/healthz`, `/api/view.json`, `/api/pipeline.json` and `/api/fetch.json`, redacted (no workbook link, host, node or guest names, container numbers, paths, addresses or seed plan), with `Cache-Control: public, max-age=60`. No pushes, no `--allow` check: bind it to loopback or an address only cloudflared reaches.

### Physical cache stock (#368)

For unindexed files and real disk occupancy, run the read-only stock walker on the cache host, repeating with the same work directory until `finished` is true:

```bash
nice -n 19 ionice -c 3 python3 scripts/corpus/scan_stock_inventory.py \
  --scan-root scans --work-dir stock-20261029 \
  --cache-set keep --cache-set buffer --max-entries 1000 --max-seconds 20
```

- Outputs (SQLite checkpoints, `report.json`) are private and outside the cache; nothing in the cache or fetch index changes. The work directory must be yours with mode 0700. The root is identity-checked and pinned. Metadata calls can still block: use an external deadline and quiet I/O windows, and stop rather than compete with a rebuild. A killed slice rolls back.
- Each directory's names go once into a pending queue (at most 10,000; larger fails). Resumes don't replay; a changed pending directory or root fails. An unchanged resume keeps `observed_until`. Schema changes need a new work directory.
- The report splits keep/buffer bytes, distinct page paths, unplaced paths and other entries; inode counts dedupe hardlinks; sizes are logical, not ZFS blocks; symlinks aren't followed. Errors fail explicitly. Counts span an interval, not a snapshot: finished directories aren't rescanned for later arrivals or removals, so use a new work directory for a fresh observation. No checksum, coverage or acceptance is implied.

### Resumable local scan-cache inventory (#704)

For indexes over the single-batch adapter's 10,000-row/16 MiB limits, run on the machine holding the scans, repeating until `finished` is true:

```bash
python3 scripts/corpus/scan_cache_inventory.py \
  --db catalog.sqlite --index scans/index-full.jsonl --scan-root scans \
  --work-dir inventory-20261017 --source local-cache-stat \
  --cache-set keep --cache-set buffer --max-batches 10
```

- Choose cache sets deliberately; the index doesn't encode membership. Nothing is downloaded, hashed or changed. A file of the right size is `present`, **not checksum-verified**; missing or wrong-size copies are observations, not deletion requests. Counts are index rows, **not unique pages or corpus completeness**.
- The first run fixes the index prefix (bytes, inode, SHA-256, protecting **metadata**, not images); resumes re-stream it in reads of at most 1 MiB and fail on any change. The index must stay append-only during a run, and appends are excluded (use a new directory for a later prefix); a final line without newline stays uncovered.
- Slices are at most 256 rows/1 MiB (64 KiB lines), statted per set and ingested transactionally. `--max-batches` limits new slices per run, not the prefix integrity scan. Output reports covered, captured, uncovered and appended bytes, rows, batches and sets.
- Keep the directory on durable storage: raw slices, immutable `batch-*/keep.jsonl` / `batch-*/buffer.jsonl` evidence and atomic `state.json`. Checkpoints advance after every set commits; crashes replay idempotently, and unpublished builds remain as `.interrupted` scratch. A lock prevents concurrent use; don't share the catalog with another inventory job at once.

To rebuild a lost catalog, replay only the evidence into the **same database path**, then resume (resume refuses a missing catalog checkpoint):

```bash
for batch in inventory-20261017/batch-*/keep.jsonl inventory-20261017/batch-*/buffer.jsonl; do
  [ -f "$batch" ] || continue
  python3 scripts/corpus/data_catalog.py --db catalog.sqlite ingest --batch "$batch" || break
done
```

Don't hand-edit checkpoints or evidence.

### Binary updates

Build with `cargo build --locked --release -p usnm-ocr-dash --bin usnm-ocrdash`; deploy with infrastructure tooling, keeping rollback and settings. Check `/healthz`, `/api/pipeline.json`, `/api/nodes.json` and the UI (health alone doesn't prove fresh evidence). After a restart it reads the fetcher's files for a few minutes (about six and a half with 1,983 queued manifests): `/readyz` on the LAN listener answers 503 `warming`, the view's `warming` is set and queue totals are partial. Wait for 200 `ready`. The data-flow observer records `local-ocr-warming` for warm-ups under 30 minutes (scan stages unknown, run not failed) and `local-ocr-unavailable` beyond.
