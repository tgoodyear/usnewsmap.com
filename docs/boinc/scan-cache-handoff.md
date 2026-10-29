# The scan-cache hand-off

Part of the [BOINC batch pilot guide](README.md).

## One page from the scan cache

This is the route for one page (#469) when the scan cache, the services that reserve and
publish work (Cosmos, the lake, `ocr-out`) and the BOINC server share no filesystem. One
blob container, `boinc-handoff`, sits between them. Nothing reads `/tank/ocr` except the scan
fetcher, and nothing asks LoC for a page again: the scan fetcher (CT 211) stays the only LoC
fetcher, and volunteers download the page from our project server.

| Who                                                 | Runs                                                                                                                                        | Reads                                                                 | Writes                                                                   |
| --------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| Scan fetcher, CT 211 (Arc identity)                 | `scripts/ocr-node/push-boinc-scan.sh` (az only)                                                                                             | `/tank/ocr/scans`, `/tank/ocr/manifests`                              | `scans/`, `manifests/`                                                   |
| Hand-off job, main environment                      | `handoff-job.sh`: `export`, later `collect --trust`; `claim` and `release` for [queue-fed work](../boinc-batch-scheduler.md#queue-fed-work) | `scans/`, `manifests/`, `spool/`, `gold/`, `claims/`, the work queues | `dispatch/`, `claims/`, Cosmos lease, ledger, `ocr-out`, the work queues |
| BOINC bridge step, on-prem host (`usnm-boinc-step`) | `submit-remote`, later `spool-out`                                                                                                          | `dispatch/jobs/<id>/`                                                 | `spool/<id>/`, `gold/trust.json`                                         |

The hand-off job runs in the main Container Apps environment because that environment
already reaches Cosmos and the lake through their private endpoints; neither is opened to
the LAN. It uses the bridge image with the command
`/usr/local/lib/usnm-boinc/handoff-job.sh`. Its dispatch root (the journal `export` and
`collect` share) lives in the container under `dispatch/`: each run copies it down and
writes it back.

`submit-remote` copies `dispatch/jobs/<id>/` (the page, `request.json` and `record.json`)
into the server's own hand-off root. It checks that the record is that request's, that the
request's identity is the job id, and that the page's size and sha256 are the request's.
Then it runs the existing `submit`, which checks the page again and stages it as a BOINC
download. A job the server already has past `reserved` is left alone, so a repeated run
submits nothing. The dispatch root's copy stays `reserved`, which `collect` accepts.

`spool-out` writes the job's spool files (terminal error markers included), the canonical
result's host id (`result.hostid` of the workunit's canonical result, read from the BOINC
database; UNVERIFIED against a live database) and `/project/gold/trust.json` to the
container. The hand-off job's `collect` step reads them back and publishes to `ocr-out`
only for a host that passed the gold check.

### Infrastructure

- the container `boinc-handoff` on the exchange account (`stusnmo<suffix>`,
  `infra/modules/ocr-exchange.bicep`), with the `boinc-handoff/scans/` prefix expiring after
  30 days, like `ocr-out`;
- the BOINC environment's `aca` subnet allowed on the exchange account's firewall, through a
  `Microsoft.Storage.Global` service endpoint on that subnet
  (`contrib-compute/infra/boinc/modules/network.bicep`). The BOINC VNet is in westus2 and the
  exchange account in eastus2, and a plain `Microsoft.Storage` endpoint covers only its own
  region and its pair. A private endpoint is not an option: the main VNet's `privatelink.blob`
  zone would then fail to resolve the exchange account for `ocr-sync`. Storage refuses the
  firewall rule until the endpoint exists, so the rule comes with the hand-off job (below). On
  a new environment: `scripts/azure/provision.sh` with `USNM_BOINC_GROUP` (the group), then
  `scripts/boinc/provision-boinc.sh` (the network, the identity and the images), then
  `USNM_BOINC_HANDOFF_IMAGE` and `scripts/azure/provision.sh` again (the job, the rule and the
  grants);
- the hand-off job `usnm-boinc-handoff-caj-<env>` and its identity
  `usnm-boinc-handoff-id-<env>` in the main environment (`infra/modules/boinc-handoff-job.bicep`),
  with its own settings `AZURE_CLIENT_ID`, `HANDOFF_URL`, `COSMOS_ENDPOINT`, `CURATED_URL`,
  `REFERENCE_URL` and `OUT_URL`. Its image is the setting `USNM_BOINC_HANDOFF_IMAGE`
  (`infra/env/<env>.json`): `images.bridge` from `contrib-compute/infra/boinc/env/<env>.json`,
  so a new bridge image means updating both. `scripts/boinc/start-boinc-bridge.sh --target handoff`
  starts it with a run's settings. It reads the job name and resource group from the main
  stack's outputs saved by `scripts/azure/provision.sh`, so it doesn't need an
  `infra/names/<env>.json` entry.

The role assignments:

| Identity                                     | Scope                             | Role                                                       |
| -------------------------------------------- | --------------------------------- | ---------------------------------------------------------- |
| Hand-off job (`usnm-boinc-handoff-id-<env>`) | container `boinc-handoff`         | OCR node writer (list, read, create; no delete)            |
| Hand-off job                                 | Cosmos `usnm/ops`                 | Cosmos DB Built-in Data Contributor (the `ocr-sync` lease) |
| Hand-off job                                 | lake `curated`, `ocr-local/` only | `usnm-boinc-ledger-role-<env>` (list, read, create)        |
| Hand-off job                                 | lake `reference`                  | Storage Blob Data Reader                                   |
| Hand-off job                                 | exchange `ocr-out`                | OCR node writer                                            |
| Hand-off job                                 | the shared registry               | AcrPull                                                    |
| BOINC bridge (`usnm-boinc-id-<env>`)         | container `boinc-handoff`         | OCR node writer                                            |
| BOINC bridge                                 | scan archive `scans`              | Storage Blob Data Reader (staging reads it first)          |
| Scan fetcher (`usnm-ocr-node5-arcs-<env>`)   | container `boinc-handoff`         | OCR node writer                                            |

The main stack makes all of them, the bridge's included (it reads the BOINC identity as an
existing resource), so the BOINC stack only adds the service endpoint. The Cosmos, ledger,
reference and `ocr-out` grants are `infra/modules/boinc-handoff-runner.bicep`'s; the
container grants are `ocr-exchange.bicep`'s, and the `scans` grant (with the BOINC subnet on the
archive's firewall) `scan-archive.bicep`'s.

### Runbook

On CT 211, signed in with `az login --identity --allow-no-subscriptions`:

```sh
scripts/ocr-node/push-boinc-scan.sh --account stusnmoffapzy \
  --scan /tank/ocr/scans/keep/<batch>/<lccn>/<date>/<ed>/<seq>.jp2 \
  --manifest /tank/ocr/manifests/<version>/manifest.parquet
```

Then, from the operator's machine, the hand-off job's export. It prints the job id in its
log, and `dispatch/jobs/<id>/` appears in the container:

```sh
scripts/boinc/start-boinc-bridge.sh --env prod --target handoff --step export \
  --var HANDOFF_PAGES=<page key> --var HANDOFF_BATCH=<batch> --var HANDOFF_PLAN=<plan id> \
  --var HANDOFF_MANIFEST=<version>/manifest.parquet \
  --var ENGINE_IMAGE=sha256:<images.worker's digest> --wait-mins 30
```

Then the bridge, on the on-prem host (a `"server": "azure"` environment starts both bridge
steps with `scripts/boinc/start-boinc-bridge.sh` and the same arguments):

```sh
scripts/boinc/onprem-step.sh --env prod --target bridge --step submit-remote --var JOB_ID=JOB_ID \
  --var HANDOFF_URL=https://stusnmoffapzy.blob.core.windows.net/boinc-handoff --wait-mins 30
```

When the workunit's result is assimilated (the ops pages show it done):

```sh
scripts/boinc/onprem-step.sh --env prod --target bridge --step spool-out --var JOB_ID=JOB_ID \
  --var HANDOFF_URL=https://stusnmoffapzy.blob.core.windows.net/boinc-handoff \
  --var HANDOFF_SPOOL=/project/usnmocr/results/0 --wait-mins 15
```

Then the hand-off job's collect (`HOST_ID` only when the server recorded none):

```sh
scripts/boinc/start-boinc-bridge.sh --env prod --target handoff --step collect --var JOB_ID=JOB_ID --wait-mins 30
```

It publishes to `ocr-out` for a trusted host and writes the
dispatch root back with the job `published`. An untrusted host's result is held in the
dispatch root's `quarantine/<job>/`. Stop there: `ocr-sync` is the next step, as usual. A terminal BOINC failure
(an `_error` or `_no_output_files` spool) needs neither a host nor the trust list: collect
marks the job failed. Each run writes back only the journal entries it touched; run one
execution at a time all the same. If a write-back keeps failing, the job's log carries the
record (`USNM-HANDOFF-RECORD` lines), since a lost reservation's record can't be made again.

For pages taken off the Azure work queues, the controller runs two more steps, `claim`
(`CLAIM_NONCE`, `HANDOFF_MANIFEST`, optionally `CLAIM_MAX`, `CLAIM_QUEUES`,
`CLAIM_LEASE_HOURS`) and `release` (`HANDOFF_DOCS`), and pushes with
`push-boinc-scan.sh --keep-page <batch>/<lccn>/<date>/<edition>/<seq>` instead of `--scan`.
`export` takes `HANDOFF_CLAIMS=1` for those pages, and `collect` then deletes the queue
message once the page is published. All of it is in [Queue-fed
work](../boinc-batch-scheduler.md#queue-fed-work).

### The scan fetcher's identity (scan-fetch container)

The scan fetcher (deployed by infrastructure tooling) holds the prefetch
manifest and the scans under `/tank/ocr` and pushes them with `push-boinc-scan.sh`. It is an
unprivileged Debian 12 LXC; the Azure Connected Machine agent runs in it like on a VM. Its
Arc machine, `usnm-ocr-node5-arcs-prod` in the environment's group, gives it a
system-assigned identity. It is not an OCR worker and is not in the nodes' Entra group, so
it has none of the queue grants.

Its grants go to the object id in the private setting `USNM_BOINC_HANDOFF_RUNNER`:

| Scope                                                            | Role                                                       | For                                                       |
| ---------------------------------------------------------------- | ---------------------------------------------------------- | --------------------------------------------------------- |
| exchange `boinc-handoff`                                         | the OCR nodes' writer role (list, read, create; no delete) | the scans, the manifest and the quality wave's page JPEGs |
| `usnm-boinc-log-<env>` workspace (only with `"server": "azure"`) | Log Analytics Reader (`boinc-handoff-logs.bicep`)          | the bridge jobs' logs                                     |

Until the hand-off job existed, `export` and `collect` ran here too, with Cosmos, ledger,
reference and `ocr-out` grants (#514); the job took those over and they are gone.

Set up once, from the operator's machine and the Proxmox host, after the BOINC stack is
deployed (with `"server": "azure"`, the workspace grant needs `usnm-boinc-log-<env>` to exist):

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

# On the operator's machine: the grant.
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

### The quality wave's page JPEG cache

**Legacy Azure-wave optimization, not the current on-prem quality feed.**
The `v1-wave10` follower in the batch container derives page JPEGs directly
from the scan server with `quality_feed.py page_jpeg`; it does not read either
legacy converter's fixed list or the `quality-jpeg/` blob cache. Its fetched-time,
then doc-id order, cross-campaign database deduplication, durable completion
records and READY-chunk backpressure replace that list's replenishment role.
Do not install another list-refresh producer alongside this follower.

Retire the exhausted legacy services without deleting their scripts, lists,
completion ledgers or cached blobs. The local converter in the scan-fetch
container is already disabled. For the remote converter on the laptop **host**,
first verify it is inactive, retain its unit and enablement receipt, then:

```sh
if test "$(systemctl show usnm-quality-jpeg-cache-remote.service -p ActiveState --value)" = inactive; then
  systemctl disable usnm-quality-jpeg-cache-remote.service
else
  printf '%s\n' 'Refusing retirement: legacy converter is not confirmed inactive.' >&2
  false
fi
```

Verify the batch container's `usnm-quality-follow.service` remains running and
the host's quality ship/results timers remain enabled and active. Disabling an
already-inactive converter must not start, stop or restart any of those units.
Rollback restores only its prior boot enablement with
`systemctl enable usnm-quality-jpeg-cache-remote.service`; it does not start
the exhausted list or establish recipe provenance. In particular, never
relabel the retained `done.txt` as `done-l-q75-r2.txt`.

The following recipe and installation procedure remain available for an
explicitly selected legacy runner; they are not the current feed's prerequisites.

A quality wave's runner turns each archived page into the worker's `page.jpg`: grayscale,
JPEG quality 75, and a JPEG2000 master over 4000 px decoded at half resolution. Decoding
JPEG2000 is most of its CPU; the former Container Apps job had 4 vCPUs (#850).
The scan-fetch
container holds the masters, so `scripts/ocr-node/quality-jpeg-cache.py` does the same
conversion there and puts each page in the hand-off container as
`quality-jpeg/l-q75-r2/<blob name>.jpg`. Blob metadata carries the master's sha256, the
recipe, the Pillow version, the size and the JPEG's sha256. The runner takes the cached page
when it is there and converts the master otherwise. A grayscale JPEG master under 40 Mpx and
20 MB goes to workers as it is, so the cache skips it.

`scripts/ocr-node/quality-jpeg-list.py` builds the page list: JPEG2000 pages only, in the
runner's shard order. The runner gives shard i every eighth batch, the manifest's first
(largest first), then tier 1: damage-index batches, in name order, that are not in the
manifest and have archived scans. It walks each batch's pages in doc_id order. The list
interleaves the shards, so the cache keeps ahead of all of them. Aim it past each shard's
current batch with `--current s=<batch>_verNN`, from the shard's status file
(`quality/v1-wave9/status/shard-<s>.json`, `current_batch`): the runner skips pages that are
already bundled or have no LoC text, so a shard's bundle count doesn't tell where it is in
its list. The runner lists the archive and the damage index once, when it starts, and both
grow, so a later list can't reproduce its tier-1 rotation. Each shard's list therefore
continues with every tier-1 batch after the one it names, and the lists are deduplicated: the
cache converts some pages another shard has passed, but none a shard still needs. Rebuild the list before the converter reaches
its end, or the cache falls behind as shards move into tier 1.

The output must be byte for byte what the runner would make, so the script runs only on
Pillow 13.0.0 from the same pinned wheel as the bridge image
(`contrib-compute/images/bridge/Containerfile`). It checks each master against its fetcher
sidecar and never replaces a blob. An existing blob must match the size and every
provenance field (source and JPEG sha256, recipe, Pillow version, width and height);
missing or different metadata stops the run without marking that page done. Reconcile
the existing blob with the wave owner rather than overwriting it or retrying with
weaker checks. The script lists done
pages in `/var/lib/usnm-quality-jpeg-cache/done-l-q75-r2.txt`. The exchange account's lifecycle policy deletes cached
pages 30 days after they are written (`expire-boinc-handoff-quality-jpeg`, #915), so a wave
must bundle a page within 30 days of its conversion or the runner converts the master again. `usnm-quality-jpeg-cache.service`
runs it under a dynamic user in the `himds` group, for the Arc token. It runs at low CPU
and I/O priority, so the prefetcher and the scan server go first. CT 211 has 6 cores and
8 GiB for it (`WORKERS=6`), from 19 October 2026: with 2 cores it converted about 1.1
pages a second, slower than the runner used pages, and with 6 about 2.9. This keeps the
work on pve-big rather than adding vCPUs to the runner's job.

The Pillow 13 upgrade uses recipe `l-q75-r2` and a recipe-specific completion
ledger so it neither reads Pillow 11's `l-q75-r1` blobs nor skips pages listed
in the old `done.txt`. Do not copy the old ledger into the new one. Stop the
converters before upgrading their wheel and code, rebuild their page lists
without the old completion ledger, and deploy the matching bridge and runner
before resuming. Check for a `JPEG_CACHE_PREFIX` override: the new runner must
use `quality-jpeg/l-q75-r2/`. Existing submitted work and r1 blobs stay unchanged;
retain the old image and scripts for rollback. When replacing an extracted wheel,
extract into a fresh directory, not over the old Pillow installation.

```sh
# On the operator's machine: the page list from the wave's manifest. --start-after s=k
# starts shard s after its batch k (0-based; the batch its status file names now), so the
# cache stays ahead of the runner. Copy the list, the script and the unit into CT 211's
# /root (pct push).
# damage.txt: az storage blob list on the lake's curated container, --prefix audit/damage/v2/.
# keep-files.txt: in CT 211, cd /tank/ocr/scans && find keep -type f (all formats: JPEG-only
# batches still take their turn in the rotation; only JPEG2000 pages are listed).
scripts/ocr-node/quality-jpeg-list.py --manifest wave9_manifest.parquet --out pages.txt \
  --damage-index damage.txt --keep-files keep-files.txt \
  --current 0=dlc_friendship_ver03,1=dlc_eject_ver01,...
# For later lists, add --done done-l-q75-r2.txt once that r2 ledger exists.
# The first r2 list must not use an r1 ledger or a nonexistent r2 ledger.
# In CT 211, as root: the pinned Pillow wheel, as the bridge image installs it.
d=/usr/local/lib/usnm-quality-jpeg-cache
mkdir -p "$d/py" && cd "$d"
curl -fsSLo pillow.whl https://files.pythonhosted.org/packages/36/46/f509b74787f40ce0627501089d630b2c741b63b244c1f40b640b3b01e181/pillow-13.0.0-cp311-cp311-manylinux_2_27_x86_64.manylinux_2_28_x86_64.whl
echo "a625df9a2a5597fa5e5266e01aefc82604bb85d291e3547981e863560d75da6c  pillow.whl" | sha256sum -c
python3 -m zipfile -e pillow.whl py && rm pillow.whl
install -m 0755 /root/quality-jpeg-cache.py "$d/"
install -m 0644 /root/usnm-quality-jpeg-cache.service /etc/systemd/system/
printf 'ACCOUNT=<exchange account>\nLIST=%s/pages.txt\nWORKERS=6\n' "$d" > /etc/usnm/quality-jpeg-cache.env
install -m 0644 /root/pages.txt "$d/"
systemctl daemon-reload && systemctl start usnm-quality-jpeg-cache
journalctl -fu usnm-quality-jpeg-cache
```

Another LAN host with idle CPU can convert too, reading masters from the scan server
(`--scan-server`, `usnm-quality-jpeg-cache-remote.service`) rather than `/tank`. It needs
three things. The first is its Arc machine identity in the on-prem hosts' group,
`usnm-onprem-grp-<env>` (`USNM_ONPREM_GROUP`, `scripts/ocr-node/group.sh --kind onprem`),
which has the hand-off writer role (list, read, create; no delete). Not the OCR nodes'
group: its member count sets the nodes' LoC pacing. The second is the host on the scan server's `--allow`
(`USNM_SCANSERVE_ALLOW` in CT 211's `/etc/usnm/scanserve.env`, then restart
`usnm-scanserve`). The third is its own share of the list: split the pages that remain
alternately, so the two hosts never convert the same page and both stay just ahead of
the runner. Install the Pillow 13.0.0 wheel for the host's Python, as above (cp313:
`pillow-13.0.0-cp313-cp313-manylinux_2_27_x86_64.manylinux_2_28_x86_64.whl`, sha256
`5f56499c1a74d2b862beb0c33b8016f99d095858455df9d21583bbbc1c15b582`). Before starting it,
check that a few of its JPEGs match CT 211's byte for byte (`--what-if` prints the size;
compare the sha256 of pages both convert).

```sh
# Stop CT 211's converter first: it loads its whole list at start, so a split taken while
# it runs would leave pages queued on both hosts.
pct exec 211 -- systemctl stop usnm-quality-jpeg-cache
# On the operator's machine: what CT 211 hasn't done, split alternately.
grep -vxFf <(cut -f1 done-l-q75-r2.txt) pages.txt > left.txt
awk 'NR % 2 == 1' left.txt > pages-ct211.txt
awk 'NR % 2 == 0' left.txt > pages-remote.txt
az ad group member add --group <USNM_ONPREM_GROUP> --member-id <host's Arc principal id>
# Install each half as its host's LIST (/usr/local/lib/usnm-quality-jpeg-cache/pages.txt),
# then start both. A host added to the group needs a fresh Arc token: restart himdsd.
# On the other host, as root, after the Pillow wheel and the script are in place:
install -m 0644 usnm-quality-jpeg-cache-remote.service /etc/systemd/system/
printf 'ACCOUNT=<exchange account>\nLIST=%s/pages.txt\nWORKERS=6\nSCAN_SERVER=http://<scan-fetch host>:8380\n' \
  /usr/local/lib/usnm-quality-jpeg-cache > /etc/usnm/quality-jpeg-cache.env
systemctl daemon-reload && systemctl start usnm-quality-jpeg-cache-remote
# Later lists: append each host's half to its pages.txt and restart it; done-l-q75-r2.txt skips
# what it has done.
```
