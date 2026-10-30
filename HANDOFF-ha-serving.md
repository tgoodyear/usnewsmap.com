# Hand-off: highly available serving for usnewsmap.com

Written 2026-10-30 from a cloud session that could read the repository and the public site, but not the lab hosts. Everything about the hosts below is from the maintainer's description or is an unchecked guess, marked as such. Verify on the hosts before acting.

## Trigger

pve-big went down when its UPS was damaged (the cat). The question: what is the index size, and how can usnewsmap.com be served from any of our machines through the Cloudflare tunnel, so that one host going down doesn't take the site down?

## What was verified

- **The site stayed up.** At 22:14 UTC on 2026-10-30, `https://usnewsmap.com/healthz`, `/v1/meta` and `/v1/status` returned 200 through Cloudflare (`server: cloudflare`, `cf-cache-status: DYNAMIC`), and `/v1/aggregate?q="cross of gold"` returned 200 in 0.26 s. Live version `pages-v20261026-1`, 23,766,177 pages, published 2026-10-28.
- **Which origin answered was not checked.** The headers don't show it. Per the repository, public serving is still Azure (`ca-usnm-prod`) behind Cloudflare (ADR-0028), and the on-prem cutover (ADR-0027 Phase 3) hasn't happened. Confirm with the Cloudflare DNS, Worker routes and tunnel ingress that nothing public routes to pve-big.
- **Index size: about 1.26 TB** (design 15's figure for `usnmqwstprodffapzy`; ADR-0027 rounds it to 1.2 TB). Not measured for `pages-v20261026-1`. A release keeps two versions side by side, so a serving node needs about **2.5 TB** of fast storage, or 4 TB to have headroom. Measure the real size on `tank` with `zfs list -o name,used,refer` over the release's run directory (`<run dir>/qw`).

## Where the repository stands

| What                                                                          | Where                                                      | State                                                                                                   |
| ----------------------------------------------------------------------------- | ---------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| Decision to serve on-prem behind a Cloudflare Tunnel                          | `docs/design/adr/0027-onprem-serving-behind-cloudflare.md` | Accepted. One serving LXC on pve-big. Phase 4 gate includes **a UPS for pve-big and the network gear**. |
| Cloudflare in front of Azure, independent of the on-prem move                 | `docs/design/adr/0028-cloudflare-with-azure-origin.md`     | Accepted                                                                                                |
| Serving container: units, Caddy, blue/green, update timer                     | `scripts/onprem/serving/` (README)                         | Phase 0: written and tested, nothing runs them in production                                            |
| Fallback Worker: tunnel → Azure on 502/530/thrown fetch; later an error layer | `edge/fallback-worker/` (README)                           | Written; production routing gated, drills not run                                                       |
| Container, VLAN, firewall                                                     | home-infra `scripts/lab/serving-ct.sh`                     | Outside this repository                                                                                 |

Two points in ADR-0027 drive the design below:

1. Decision 1: "pve and pve-laptop can't hold the 1.2 TB index."
2. Decision 10: "A second tunnel connector wouldn't help without a second origin: replicas go to the nearest connector without checking the origin." Cloudflare doesn't health-check what sits behind a connector, so a host whose API or searcher is down still gets traffic and answers 502.

## Proposed design: N identical serving nodes, one tunnel

1. **Each node holds its own copy of the index on local fast storage**, bind-mounted read-only into the same serving container `scripts/onprem/serving/` defines (Caddy, blue/green API, Quickwit searcher, `cloudflared`), at about 4 cores and 12 GiB. Per-node state is only the response cache. The search log, Cosmos and reference stay in Azure, and `update-api.sh` already deploys on each node by itself.
2. **One tunnel, `usnm-serving`, with a connector on each node.** When a host dies, its connector drops and Cloudflare moves traffic to the others. This costs $0.
3. **A readiness-gated connector (new).** A systemd watchdog in each container polls a health check through Caddy. After N consecutive failures it stops `cloudflared-usnm-serving.service`; after M consecutive passes it starts it again. This answers decision 10's objection for unplanned failures. Two limits:
   - **It reacts, so it doesn't cover planned work.** Until N probes have failed, the node keeps taking traffic, and uncached searches get the API's own 503, which the fallback Worker passes through. Planned work (a searcher restart or upgrade) therefore needs a drain path: hold the watchdog (a hold file, like `/etc/usnm-serving/hold`), stop the connector, wait for requests in flight, do the work, and start the connector again only once the health check passes. Only that makes ADR-0027 settled decision 8's planned outage a rolling restart.
   - **Which check to gate on is open.** On-prem `api.env` sets `USNM_READY_REQUIRES_PUBLISHED_INDEXES=true` (`gen-env.sh`), so `/readyz` fails whenever `current.json` names an index the local searcher lacks (`crates/usnm-api/src/routes/meta.rs`), even though the node still serves the version it has. Gating on that `/readyz` during a release would pull every node that hasn't switched yet (see step 4). The watchdog needs a check that means "can answer searches now", either `/readyz` with that setting off on multi-node hosts, or a search probe.
4. **Releases: copy everywhere first, and keep at least one node ready throughout the switch.** The release host sends the new version to each node (`zfs send | recv`). A file copy alone doesn't make it searchable: each node's searcher reads one release's `file://` metastore, so it serves only one version, and moving to the new one takes a new `searcher.yaml` and a restart. The switch order is open and has to be settled before the code:
   - **Two groups:** drain group A, restart it onto the new version, switch `current.json`, then drain and move group B. This leaves one group serving throughout, but with two nodes it means one node carries all traffic for a while, and ADR-0027 settled decision 6's mount test decides how the restart is done.
   - **Or a metastore holding both versions**, so a searcher can serve the old and the new across the switch with no restart. This needs a test of whether Quickwit allows it with read-only `file://` metastores.

   Either way, `current.json` switches only after enough nodes have shown that the new indexes are searchable (a search probe on each), not after the copy finishes. This extends ADR-0027 decision 8's "once both origins hold it" to N nodes.

5. **The fallback Worker stays as the last resort.** Every node shares the house's power, ISP, gateway and switches. When all connectors drop, the tunnel answers 530 and the Worker goes to Azure, or, after Phase 4, serves its error layer.

Open design points:

- `/srv/usnm-serving/sitemap-state` is durable and written to on each node (`scripts/onprem/serving/README.md`, Setup step 2). Decide whether one node owns it, it gets synced, or it moves to shared storage.
- Settled decision 6 (how a new version reaches the running container) is still waiting for its Phase 2 test. The per-node copy depends on the answer.
- Rejected: an SSD NAS shared over NFS. It makes the NAS the new single point of failure, and pve-big's one 1 GbE NIC caps cache-miss reads at about 110 MB/s. Also rejected for now: a replicated S3 store such as Garage across the hosts, which adds a distributed system and still shares the house's power and uplink.

## The hosts

Specs below are guesses from the hardware's age unless marked as stated. Check them before buying.

| Host       | Hardware                      | Stated by maintainer                               | Fit as a serving node                                                                                                                                                                                                       | Check                                                                                                    |
| ---------- | ----------------------------- | -------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| pve-big    | Desktop                       | Has an M.2 NVMe SSD                                | Yes, the primary node                                                                                                                                                                                                       | Size and free space of the NVMe pool; whether the ADR's 4 TB serving pool exists                         |
| pve        | 2014 Intel NUC                | "Could probably get" an NVMe drive                 | Weak. 2014 (4th generation) NUCs mostly have **mSATA, not M.2 NVMe**; "H" models add a 2.5" SATA bay (a 4 TB SATA SSD fits). Likely at most 16 GB DDR3L and a dual-core Haswell CPU, so it would be the slowest node by far | `dmidecode -t baseboard -t memory`, `lsblk -o NAME,TRAN,SIZE,MODEL`, `lscpu`                             |
| pve-laptop | Surface Laptop, about 2021–22 | 16 GB RAM (believed); can't take an internal drive | Workable with external storage. The battery is a built-in UPS. Downsides: no Ethernet port, heat, and battery swelling at a constant 100% charge                                                                            | `cat /sys/class/dmi/id/product_name` (Laptop 4 has USB-C 10 Gbps; Laptop 5 has Thunderbolt 4), `free -g` |

### External storage for pve-laptop

| Option                                                                     | Approximate cost (late October 2026)  | Link                                            |
| -------------------------------------------------------------------------- | ------------------------------------- | ----------------------------------------------- |
| 4 TB M.2 NVMe in a 10 Gbps USB-C enclosure (ASM2362 or RTL9210B, aluminum) | $20–40 + about $480–530 for the drive | ~1 GB/s; works on either model. **Recommended** |
| 4 TB NVMe in a USB4/Thunderbolt enclosure (ASM2464PD)                      | $80–130 + drive                       | ~3 GB/s; Surface Laptop 5 only                  |
| Ready-made portable SSD (Samsung T7/T9, Crucial X10, SanDisk Extreme)      | Prices not checked                    | ~1–2 GB/s                                       |

How to set it up:

- **Ports:** power through Surface Connect, the SSD on USB-C, and a USB-A gigabit or 2.5GbE adapter for the network. Don't serve over Wi-Fi.
- **USB on Linux:** set `usbcore.autosuspend=-1` and check that the drive uses `uas`. If an RTL9210 enclosure keeps resetting, add a no-UAS quirk for it or use an ASM2362 enclosure.
- **Storage:** a single-disk ZFS pool, read-only bind mount into the container. If the USB link drops, the pool suspends, the health check fails, and the watchdog takes the node out of the tunnel.
- **Battery and heat:** cap the charge at about 80% if the firmware allows it, and give the laptop and enclosure airflow.

### Prices seen (late October 2026; they move fast)

- 4 TB NVMe: cheapest about $480–530 (TEAMGROUP G50, Silicon Power UD90), median about $720. About twice the price of November 2025.
- Mini PC or NAS boxes: Beelink ME Mini (Intel N150, 2.5GbE, 6 M.2 slots) about $209–359; TerraMaster F4 SSD about $400 on sale.
- If pve or pve-laptop doesn't work out, the fallback for a second node is a mini PC with at least 16 GB of RAM, 2.5GbE and a 4 TB NVMe drive, about $700–1,000 in total.
- ADR-0027 puts a minimal Azure standby at about $40–45 a month, so about $800 of hardware pays for itself in about 18 months compared with keeping that standby forever.

## Suggested order

1. **UPS first**, covering pve-big, any second node, the router and the switch, out of the cat's reach. It is an existing Phase 4 gate and would have prevented today's outage.
2. **Confirm the origin and pve-big's state.** Check that public traffic is on Azure, then find out what on pve-big came back after the outage: `tank` pools (`zpool status`), the release and rebuild containers, BOINC, `serve-scans`, and the OCR dashboard (CT 212).
3. **Run the hardware checks** in the hosts table.
4. **Prototype on pve-laptop** with a USB NVMe drive. Run two connectors on one tunnel, on the private test hostname only (`apply-tunnel-ingress.sh --hostname`). Add the readiness watchdog and drill it. Run uncached searches (vary the words so neither the API nor the edge has them cached) continuously for the whole drill, and record every non-200 answer (502, 530 and the API's own 503) with its time. Then measure how long it takes to recover in each case: kill the searcher without warning, cut a node's network, and do a planned drain-and-restart through the drain path. A pass means the planned restart loses no searches, and the unplanned cases recover within a stated time.
5. **Write it up as an ADR-0027 amendment:** decision 1 (one container on pve-big) becomes N nodes, decision 10's objection to a second connector is answered by the watchdog, settled decision 8 becomes rolling restarts through the drain path, and decision 8's release switch follows the order chosen in design step 4. Then the code: a watchdog unit and script with its drain and hold path in `scripts/onprem/serving/`, with tests in `test_serving.py`, and a per-node copy and gated switch in the release flow.

## Repository rules to remember

- Run the checks in `CONTRIBUTING.md` before a pull request. The serving scripts' tests are `scripts/onprem/serving/test_serving.py`; the Worker's are `node --test edge/fallback-worker/*.test.mjs` (Node 24).
- Host names and addresses stay in home-infra, not this repository (`scripts/onprem/serving/README.md`).
- Every script takes named flags and a `--dry-run`.
