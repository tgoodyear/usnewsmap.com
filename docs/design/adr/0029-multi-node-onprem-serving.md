# ADR-0029: On-prem serving from more than one node

- **Status:** Proposed. Final after the checks in [Acceptance](#acceptance). Nothing here changes production, and ADR-0027 stands until this is accepted.
- **Would amend:** [ADR-0027](0027-onprem-serving-behind-cloudflare.md) decisions 1 and 5, its "One host, one uplink" point, and settled decision 8.

## Context

ADR-0027 serves from one container on pve-big. It rejects a second tunnel connector for two reasons: no other home host can hold the index, and Cloudflare sends traffic to a connector without checking the origin behind it.

On 30 October 2026 pve-big lost power when its UPS was damaged. usnewsmap.com stayed up, because Azure is still the public origin (ADR-0028). After the cutover in ADR-0027 Phase 3, the same failure would send every request to the fallback Worker, and after Phase 4 it would take the site down. Moving the index to NVMe changes what a second node would need.

Measured on 30 October 2026, using `pages-v20261026-1` (23,766,177 pages):

- **Index:** 1.20 TB by apparent size, 1.09 TiB allocated.
- **pve-big's NVMe:** the `nvme-serving` pool is 1.86 TiB, not the 4 TB in ADR-0027 decision 5, with 727 GiB free. It holds one version, not two.
- **Other hosts:** pve is a dual-core i5-4250U with its RAM at the 16 GB maximum, and it runs Home Assistant and the BOINC server under a sustained load of about 12 on 4 threads. pve-laptop is a Surface Laptop 4 with a 4-core i7-1185G7, 16 GB of soldered RAM, 10 Gbps USB and no room for the index internally.
- **Search latency:** ten searches per pass, each without a response-cache entry. At that sample size, p95 is the slowest search. The 2 s target is from 09 §9.1.

| Origin                             | Concurrency 1, p50 / p95 | Concurrency 4, p50 / p95 |
| ---------------------------------- | ------------------------ | ------------------------ |
| Azure, through Cloudflare          | 1.82 / 4.16 s            | 6.20 / 11.25 s           |
| pve-big NVMe, through the tunnel   | 3.12 / 11.51 s           | 7.40 / 15.00 s           |
| pve-big NVMe, inside the container | 4.45 / 9.80 s            | 5.68 / 17.59 s           |
| pve-big HDD, inside the container  | 10.50 / 36.86 s          | 36.46 / 138.03 s         |

Every NVMe response matched Azure's apart from timing. Cached replays through the tunnel took 0.15 s at p50 and 0.22 s at p95 at concurrency 4. The Azure and NVMe runs were on the same day. The HDD row is from an earlier run the same day, with other query date offsets.

These runs don't show what the NVMe can do at its best. The on-prem searcher still has `max_num_concurrent_split_searches: 8`, set during the HDD tests, where Azure has 48. It also has 2 API computation slots and 4 runtime threads.

The searcher used 0.4 to 0.8 GiB of memory in these runs and the API 0.1 to 0.3 GiB on the HDD runs, without Chromium. So a node doesn't need the full 12 GiB in ADR-0027 decision 1.

## Decision

### 1. Two nodes, each with its own copy of the index

- pve-big stays the primary node. pve-laptop is the only candidate for a second one, on a single-disk ZFS pool on external NVMe. pve isn't a candidate.
- A second node needs at least 2 TB of fast storage, enough for one version plus growth. ADR-0027's two-version requirement doesn't apply, because decision 4 replaces it. The previous version stays on `tank`.
- Each node runs the container `scripts/onprem/serving/` defines, reading its index from a read-only bind mount, under its own Arc identity. Its local state is the response cache and whatever open question 1 settles.
- pve-laptop runs serving with a higher CPU weight than its BOINC and ingest containers, and those are capped or moved until the drill passes.
- pve-laptop's battery counts as protection only if its dock and the network path are on the UPS, since its NIC is behind the dock.

### 2. One tunnel, a connector on each node

Both nodes join `usnm-serving`. When a host dies, its connector drops and Cloudflare moves traffic to the other. A second connector adds $0 on Cloudflare's Free plan.

The second node is for failover, not capacity. Cloudflare sends traffic to the closest connector, so don't count on an even split. A node that takes over starts with cold caches.

### 3. Watchdog on the connector, gated on search health

A unit in each container stops `cloudflared-usnm-serving.service` after N consecutive failed checks, and starts it after M consecutive passes. N and M come from the drill. This answers ADR-0027's objection for unplanned failures.

The check has two parts, both on loopback:

- `/readyz` through Caddy, which shows that Caddy, the API and the searcher's health endpoint respond.
- A search sent straight to Quickwit, with a timeout. This catches storage that has stopped responding, such as a suspended pool after a USB drop, which a health endpoint can miss. It goes to Quickwit, not the API, so probes never enter the search log (ADR-0012).

`USNM_READY_REQUIRES_PUBLISHED_INDEXES` is turned off on these nodes. With it on, `/readyz` fails whenever `current.json` names an index the node doesn't have, even while the node serves the version it has. The watchdog would then pull a working node during a release. Decision 4's search probes take over the job of proving a version is searchable.

### 4. Drain for planned work and releases

The watchdog only reacts. Until N checks fail, the node keeps taking traffic, and uncached searches get the API's own 503, which the fallback Worker passes through. So planned work has its own path:

1. Set a hold file so the watchdog doesn't restart the connector.
2. Stop the connector and wait for requests in flight to finish.
3. Do the work.
4. Pass the checks, clear the hold and start the connector.

This replaces the planned outage in settled decision 8, once there are two nodes. With one node, or with the other node down, a restart is still an outage.

A searcher reads one release's `file://` metastore, so a copied version isn't searchable until the searcher moves to it. pve-big has room for only one version, so a full release goes in this order:

1. Drain pve-laptop, replace its version and restart its searcher. pve-big serves the old version.
2. Search-probe the new version on pve-laptop, without routing visitors to it.
3. Connect pve-laptop, drain pve-big and switch `current.json`, so visitors move to the new version.
4. Replace pve-big's version, probe it and connect it.

During a full release one node carries all traffic, and a failure in that window takes the site down until the fallback Worker or the previous version on `tank` takes over. So `current.json` switches only after the search probe passes, not when a copy finishes. Whether a small delta release can go in beside the current version without this sequence depends on the `release.sh` delta mode and settled decision 6.

### 5. Fallback Worker when every connector drops

Both nodes share the house's power, ISP, gateway and switches. When every connector drops, the tunnel answers 530 and the Worker goes to Azure, or, after Phase 4, serves its error layer. ADR-0027's Phase 4 gate stands, and its UPS must also cover the second node.

## Acceptance

The maintainer accepts this ADR, and the second node is bought, only after both of these:

1. **Latency.** Rerun the matched cold workload on pve-big's NVMe with split searches at 48, and with the other limits varied one at a time. Then rerun on the candidate node's storage. The maintainer decides whether the bar is the 2 s target, which Azure doesn't meet either, or parity with Azure.
2. **Failover drill.** Run two connectors on `usnm-serving-dev`, with no public record touched. Send uncached searches throughout, varying the words, and record every non-200 answer with its time. Cases: kill the searcher without warning, cut a node's network, pull its storage, and drain and restart it as planned. Planned restarts must lose no searches. Unplanned ones must recover within a time set before the drill.

## Open questions

1. **Sitemap state.** `/srv/usnm-serving/sitemap-state` is durable and written on each node. Decide whether one node owns it or it's synced.
2. **Version during the switch.** In step 3 of the release order, the nodes serve different versions. Test what a client pinned to the old `v` gets if it fails over at that moment.
3. **Quickwit probe.** Pick a query that reads the index through storage each time, rather than answering from the searcher's caches.

## Alternatives considered

| Alternative                                     | Why not                                                                                                                                                                                                       |
| ----------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Serve from pve                                  | No spare CPU, and it already runs the BOINC server and Home Assistant                                                                                                                                         |
| Shared SSD NAS over NFS                         | Becomes the new single point of failure, and pve-big's 1 GbE NIC caps cache-miss reads at about 110 MB/s                                                                                                      |
| Ceph across the hosts                           | Built for changing data, not an immutable index. Safe replication needs three storage hosts and 3.6 TB per version. Reads often cross 1 GbE, and a USB drive dropping out starts recovery over that same link |
| Replicated S3 store such as Garage              | Adds a distributed system to run and still shares the house's power and uplink                                                                                                                                |
| One metastore holding both versions at a switch | Would avoid a restart, but pve-big can't hold two full versions                                                                                                                                               |
| 4 TB on every node                              | Pays for double capacity that the drain sequence doesn't need                                                                                                                                                 |
| Cloudflare Load Balancing                       | From $5 a month, and its health checks don't remove the need for a drain or a second copy of the index                                                                                                        |

## Consequences

- One host failure no longer takes the site to the fallback or error layer, and planned restarts stop being outages while both nodes are up, at the cost of a watchdog, a drain path and a longer release sequence.
- Serving has two copies of the index on separate hosts, plus the previous version on `tank`.
- A full release leaves a window with one node serving, and it needs a copy of about 1.2 TB over 1 GbE, taking about 3 hours at 110 MB/s.
- This doesn't fix pve-big's other roles. In the 30 October outage, BOINC file downloads, pve-laptop's release and locstats mounts, Proxmox backups, the dev site and the OCR dashboard all stopped. Each depends on pve-big regardless of how serving runs.
- Code that follows acceptance: the watchdog and drain units in `scripts/onprem/serving/` with tests in `test_serving.py`, the setting change in `gen-env.sh`, the release sequence in the on-prem release flow, and amendments to ADR-0027.
