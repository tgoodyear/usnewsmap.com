# Running a BOINC node in your own Azure subscription

For contributors who have Azure credits and would rather spend those than run a
Proxmox host. The templates are in `infra/contributor-azure/`. Nothing here has
been deployed by the maintainer; compile and lint are the only checks so far.

Work distribution is BOINC (`docs/interface.md`). Both options run a BOINC
client attached to the project server. Neither builds an image of its own.

| Option                          | File              | What runs                                                                      |
| ------------------------------- | ----------------- | ------------------------------------------------------------------------------ |
| Spot VM scale set (recommended) | `vmss-spot.bicep` | Debian 12 with Debian's `boinc-client` package, installed by cloud-init        |
| Azure Container Instances       | `aci.bicep`       | The official `boinc/client` image, plus an init container that fetches the key |

Both need the same first step, `setup.bicep`, and then one `set-key` command.

## Scope and trust

- You get a project address and a BOINC account key from the maintainer. The key
  goes into a Key Vault in your subscription. Workers read it with a managed
  identity that can only read secrets. The key is never in a template, a parameter
  file, a deployment output, a log or a command line.
- The maintainer gives you no Azure credential, SAS token or storage key, and
  sees no part of your subscription. The maintainer sees a BOINC host and its
  public IP address.
- Redundancy, validation and trust are BOINC's: new hosts have every job
  replicated, trusted hosts are sampled, and the server compares the results.
  Nothing is approved by hand on your side.
- You pay Azure. The project does not.

## Cost

Prices are from the Azure retail prices API, pulled on 2026-10-13, region East
US 2 (one of the three regions where ACI Spot is offered), Linux, pay-as-you-go
list prices. Your credits, offer and region will differ. Spot prices change.

| Option                      | Unit                    | Price per hour | Per vCPU-hour |
| --------------------------- | ----------------------- | -------------- | ------------- |
| VM, D4as v6, Spot           | 4 vCPU, 16 GiB          | $0.0336        | $0.0084       |
| VM, D4as v5, Spot           | 4 vCPU, 16 GiB          | $0.0318        | $0.0080       |
| ACI Spot (preview)          | 4 vCPU + 10 GB          | $0.0620        | $0.0155       |
| VM, D4as v6, on demand      | 4 vCPU, 16 GiB          | $0.182         | $0.0455       |
| ACI, regular                | 4 vCPU + 10 GB          | $0.2065        | $0.0516       |
| Container Apps, consumption | 4 vCPU + 10 GiB, active | $0.4536        | $0.1134       |

The ACI rows are 4 x vCPU rate + 10 x memory rate (Spot $0.01215 and $0.001335;
regular $0.0405 and $0.00445). The Container Apps row is 4 x $0.000024 x 3600
plus 10 x $0.000003 x 3600, before any free grant, and without the $0.10 per
hour environment meters that the API lists from 2026-09-01 (it is not known
whether they apply). Disks, public IPs and egress are not priced here.

### Per page (estimates)

The estimate assumes 5 to 8 pages per vCPU-hour. This is an assumption, lower
than the maintainer's nodes (160 to 330 s per page, so 11 to 22 pages per
thread-hour, `docs/verified-facts.md`). The maintainer has not measured the
Azure sizes. The ranges below are per 1,000 pages, compute only.

| Option            | $ per 1,000 pages (8 down to 5 per vCPU-hour) |
| ----------------- | --------------------------------------------- |
| VM Spot (D4as v6) | 1.05 to 1.68                                  |
| ACI Spot          | 1.94 to 3.10                                  |
| VM on demand      | 5.69 to 9.10                                  |
| ACI regular       | 6.45 to 10.33                                 |
| Container Apps    | 14.17 to 22.68                                |

One Spot D4as v6 running all month is about $24.55 for compute (0.033634 x 730
hours) and, at the same assumption, about 14,600 to 23,400 pages.

### Cost comparison

A container group (ACI) has no OS to patch, and one `az group delete` removes
everything. Per core it costs more than a Spot VM. A Spot VM costs about
half as much per core as ACI Spot, and about a sixth of ACI on demand. Spot VMs
and ACI Spot are the cheapest per core; Container Apps is the most expensive
and is not offered as an option here.

## Preemption: what happens on a Spot eviction

A task that is lost is reissued by the server after its deadline (5 days;
`docs/interface.md`), so an eviction costs the unfinished tasks' compute, not
correctness. The OCR app is designed to checkpoint once per finished page
(`docs/interface.md`), so an eviction loses at most the page in progress. Not
tested: the app, the suspend step and a real eviction have not been run
(UNVERIFIED). To keep the
loss small, the VM runs `scripts/azure/boinc-evict.sh`. It polls Azure Scheduled
Events, and on a `Preempt` (or `Terminate`, `Reboot`, `Redeploy`) event it runs
`boinccmd --set_run_mode never` (suspend),
waits 8 seconds, and stops `boinc-client.service`. It does not approve the event.

Read 2026-10-13: the BOINC source treats SIGTERM as a request to quit
(`client/main.cpp`, `client/app_control.cpp`), and the `boinccmd` options are in
the BOINC wiki page `Boinccmd-tool`.

Not verified:

- **Azure gives about 30 seconds**, best effort (Spot VM docs, below). Whether
  BOINC's checkpoint interval (set by the app) is short enough that a suspended
  task keeps its progress is unknown; the OCR app's checkpointing is the
  maintainer's to design.
- The maintainer did not run the watcher against a real eviction, or `boinccmd` from the
  `boinc` user's data directory (it reads `gui_rpc_auth.cfg` from the working
  directory).
- **ACI Spot** has no scheduled events and no documented signal on eviction. The
  docs say the group is restarted automatically. The client handles SIGTERM, but
  whether ACI sends it, and how long it waits, is UNVERIFIED.

## Sources for the Azure facts (read 2026-10-13)

- ACI Spot containers, Microsoft Learn,
  `azure/container-instances/container-instances-spot-containers-overview`
  (page dated 2025-11-17): in preview and "not recommended for production
  scenarios"; up to 70% off regular ACI; Linux and Windows; no choice of
  eviction policy, and on eviction the container group "is automatically
  restarted"; no public IP, no virtual network, no confidential SKU, no
  availability zone pinning.
- ACI FAQ and quota pages, same site (dated 2026-07-25): default Spot quota of
  10 vCPU and 10 container groups, raised through a support request for
  "StandardSpotCores"; Spot container groups are limited to 4 vCPU and 16 GB
  each; Spot is offered in East US 2, West Europe and West US only "at this
  time"; ARM64 images are not supported.
- Azure Spot VMs, `azure/virtual-machines/spot-vms` (dated 2026-02-06): no SLA;
  eviction with a 30-second notice through Scheduled Events, best effort;
  eviction policy Delete removes the VM and its disks; max price `-1` means no
  eviction for price reasons and never more than the standard price; B-series
  and promo sizes are not supported; Spot quota is a separate pool shared by
  VMs and scale-set instances.
- The same page lists the offer types that support Spot: Enterprise Agreement,
  pay-as-you-go (003P), Sponsored (0036P and 0136P), and CSP.

## Things not verified

- **Your credit offer may not allow Spot.** The offer list above does not
  mention free trial, student or Visual Studio benefit credits. Check before
  planning on it. Some offers also limit regions or vCPU quota.
- **Spot quota** starts at zero or low on many new subscriptions
  (`az vm list-usage --location REGION`). Raising it is a support request.
- **ACI Spot scale.** One group is at most 4 vCPU and 16 GB, and the default
  quota is 10 vCPU, so you get two groups before asking for more.
- **ACI init containers with Spot, and the managed identity.** Whether a Spot
  group runs init containers, and whether the init container can get a Key Vault
  token from its user-assigned identity (the script tries the container-group
  endpoint variables, then the instance metadata endpoint), is untested.
- **ACI Spot outbound access.** The docs say no public IP and no virtual network;
  they do not say whether outbound internet access works the same way.
- **The official BOINC image.** Read 2026-10-13 (`boinc-client-docker` README and
  Dockerfile; Docker Hub tags): `ubuntu:latest` plus a PPA build of
  `boinc-client`, runs as root, default RPC password `123` (the port is not
  published in ACI), last pushed 2025-05-04. The VM path uses Debian's package
  instead.
- **A pre-placed `account_*.xml`** attaching a fresh client without
  `boinccmd --project_attach`. The BOINC source reads account files at start; no
  run has confirmed it for an empty data directory.
- **Debian's `boinc-client` on a cloud image.** Data directory
  (`/var/lib/boinc-client` per the wiki, `/var/lib/boinc` in Debian's file list);
  the scripts use the home directory of the `boinc` user. `--no-recommends`
  untested.
- **`D4as_v6` on Debian 12.** v6 sizes use NVMe disk controllers; the default
  list falls back to `D4as_v5`.
- **The flexible-size scale set** (`skuProfile`, `LowestPrice`) compiled in
  Bicep but has not been deployed.
- **Per-task memory and CPU time** are the maintainer's figures (up to 2.5 GiB per
  task, 11 to 22 minutes per 4-page workunit, `docs/interface.md`).

## Deploying

You need the Azure CLI, a subscription you control, and from the maintainer: the
BOINC project address and an account key.

### Phase A: setup

Write a parameters file (kept outside the repository):

```
using '<path to>/infra/contributor-azure/setup.bicep'
param keyWriterPrincipalId = '<az ad signed-in-user show --query id --output tsv>'
```

```sh
scripts/deploy-azure.sh --phase setup --subscription <id> --resource-group usnm-contrib \
  --location eastus2 --parameters-file setup.bicepparam --what-if   # needs the group to exist
scripts/deploy-azure.sh --phase setup --subscription <id> --resource-group usnm-contrib \
  --location eastus2 --parameters-file setup.bicepparam
```

`--what-if` only runs `az deployment group what-if`, and needs the resource
group to exist already (`az group create`).

Then store the key (put it in a file with mode 600 first). The command does not
print it:

```sh
scripts/deploy-azure.sh --phase set-key --subscription <id> --vault-name <vaultName output> \
  --account-key-file ~/usnm-account-key
```

Role assignments can take time to apply; not timed here (UNVERIFIED). Delete the key file afterwards.

### Phase B: workers

For the VM option:

```
using '<path to>/infra/contributor-azure/vmss-spot.bicep'
param vaultName = '<vaultName output of phase A>'
param projectUrl = 'https://<project host>/<project>/'
param imageVersion = '<pinned Debian 12 image version>'
param adminSshPublicKey = '<any ssh public key>'
param instanceCount = 1
param tasksPerVm = 4
```

Find image versions with
`az vm image list --all --publisher debian --offer debian-12 --sku 12-gen2`.
Deploy with `--phase vmss-spot`. The VMs have no inbound access: there is an
explicit deny-all inbound rule (template, not deployed), so ssh is not reachable. Outbound goes through a
per-VM public address (cheap) or a NAT gateway (`outboundMode`; costs more).

Spot settings: priority Spot, eviction policy Delete, max price -1, flexible
orchestration, sizes tried in the order of lowest price. Automatic OS upgrades
are off. To update, pick a newer `imageVersion` and re-deploy. The key is in the
vault, so replaced VMs re-attach to the same account. A BOINC host that is
deleted and recreated is expected to show up as a new host (UNVERIFIED); the
server's handling of old hosts is not documented here.

For the container option, use `aci.bicep` with `--phase aci`. Parameters:
`vaultName`, `projectUrl`, `boincImage` (pin a digest the maintainer has checked),
`groupCount`, `vcpu`, `memoryGb`, `priority` (Spot by default), `restartPolicy`
(Always by default).

If the key is missing from the vault, the VM setup fails and the ACI init
container fails with the HTTP status of the vault read.

### Cost cap

Both templates take `budgetAmount` and `budgetContactEmails` and create a
monthly budget on the resource group, with email alerts at 50% and 90% of
actual cost and at 100% forecast. A budget only warns; it stops nothing. Set
`instanceCount` or `groupCount` to 0 when you are done for the day.

### Teardown

```sh
az group delete --name usnm-contrib
```

This deletes the workers, the identity and the vault. The vault has purge
protection, so its name stays reserved and the key stays recoverable
for the retention period (length UNVERIFIED). Tell the maintainer so the account can be disabled.

## Status

- Nothing has been deployed; only `az bicep build`, shellcheck and dry runs.
- There is no custom worker, enrollment step or image to host: the BOINC client, with the
  project's account key, scheduler and validator, does that work.
- BOINC is a poor fit for ACI if the project ever needs Docker-wrapper apps: ACI
  gives no container runtime. The project's app is native, so it is fine today.
