# 16. `tank` disk replacement and authenticated scan access

Follow-up to [design 15](15-onprem-cold-storage.md) (#895). **Design only.** Nothing here has been bought, changed on `pve-big`, deployed or changed in Azure. The maintainer chose (1) to plan replacing failing disks with two larger matching CMR drives and (2) to design authenticated on-prem scan access for the BOINC bridge. Mirror-only redundancy was already chosen. Facts below were read from `pve-big` on 2026-10-18 (`zpool status`, `zpool get`, `smartctl`, `lsblk`; read-only).

## 16.1 Pool membership and which disks are failing

`tank` is two mirrors, `ashift=12`, `autoexpand=off`, `autoreplace=off`, `feature@device_removal` enabled, last scrub 12 October with 0 errors. Members are partition 1 of each disk, by stable id.

| Mirror                     | Disk | By-id (`/dev/disk/by-id/`)             | Port / link   | SMART                                                                                                                                      |
| -------------------------- | ---- | -------------------------------------- | ------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| mirror-0 (1.81 TiB, 587 G) | sda  | `ata-Hitachi_HUA722020ALA331_BFHJZ1WF` | ata1 / 3 Gbps | Clean: 0 reallocated, 0 pending                                                                                                            |
| mirror-0                   | sdd  | `ata-Hitachi_HUA722020ALA331_B9JD60NF` | ata4 / 3 Gbps | **91 reallocated**, 0 pending, last extended test passed                                                                                   |
| mirror-1 (2.72 TiB, 727 G) | sdb  | `ata-ST3000DM001-1ER166_Z501ZJ2W`      | ata2 / 6 Gbps | **48 reallocated, 88 pending, 88 offline-uncorrectable**, 8 ATA errors (UNC); extended self-test **failed** (read failure, LBA 4107256464) |
| mirror-1                   | sdc  | `ata-ST3000DM008-2DM166_Z504MEVK`      | ata3 / 6 Gbps | Clean, CMR                                                                                                                                 |

All four are about 10,000 power-on hours. The two failing disks are in **different mirrors**: `sdb` is the worse one (mirror-1) and `sdd` is degrading (mirror-0). Each mirror therefore still has one clean member today, so the pool tolerates the loss of either failing disk, but a second fault in the same mirror (for example `sdc` dying while `sdb` is failing) loses data. `sdb`'s pending sectors mean ZFS may already hit read errors on it; with the clean `sdc` it self-heals them from the mirror.

All six SATA ports are used (two Samsung system SSDs plus these four). A USB 3.1 controller (ASM1142) and a 1 TB USB disk (`usbbak`) are present.

## 16.2 Do two larger disks expand the pool?

A mirror vdev grows only when **every** member is larger. So:

- Replacing one disk in each mirror (`sdb` and `sdd`, the two failing disks) gives **no capacity growth**, whatever the new sizes: each mirror stays at its smaller member.
- Replacing **both disks of mirror-1** (`sdb` and `sdc`) grows mirror-1 to the new size. This is the only two-disk path that expands. It removes `sdb` (worst) and also a healthy but 3 TB disk, and leaves `sdd` (91 reallocated) in service in mirror-0.
- Replacing both disks of mirror-0 (`sda`, `sdd`) also expands, retires `sdd`, but leaves `sdb` failing: not acceptable first.

Recommended: **mirror-1 first** (`sdb` then `sdc`), because it holds the disk that failed its self-test. `sdd` is then a watched risk: it is covered by `sda` and by monitoring (16.5), and is retired by the optional step below.

Optional follow-on (a decision, not required): once mirror-1 is large, `zpool remove tank mirror-0` evacuates mirror-0 (587 G) into mirror-1. It works because `device_removal` is enabled, the pool has only mirror vdevs and `ashift` matches. It retires both Hitachis and frees two SATA ports for later. The cost is a small permanent indirect-mapping table (memory) and no way to undo except adding a vdev. It is not needed to get the capacity.

### Capacity options (no purchase made)

TB are decimal; ZFS shows TiB. A 2x mirror gives one disk's capacity.

| Mirror-1 disks | Mirror-1 usable | Pool with mirror-0 kept | Pool after optional removal of mirror-0 | Compared with 4.53 TiB today |
| -------------- | --------------- | ----------------------- | --------------------------------------- | ---------------------------- |
| 2 x 8 TB       | ~7.3 TiB        | ~9.1 TiB                | ~7.3 TiB                                | 2.0x / 1.6x                  |
| 2 x 12 TB      | ~10.9 TiB       | ~12.7 TiB               | ~10.9 TiB                               | 2.8x / 2.4x                  |
| 2 x 16 TB      | ~14.6 TiB       | ~16.4 TiB               | ~14.6 TiB                               | 3.6x / 3.2x                  |

Figures lose a few percent to ZFS overhead and the 80 % planning limit. Against design 15's projections (Phase 1 about 2.4 to 2.9 TB with the buffer; Phase 2 about 4 to 4.5 TB), 2x8 TB is enough through Phase 2 with headroom; 2x12 TB is the comfortable choice if the index also grows or a second dataset copy is wanted; 2x16 TB buys the longest runway but raises the single-drive loss exposure and resilver time. **Recommendation: 2 x 12 TB CMR**, with 2 x 8 TB as the minimum. Price is not stated here; the maintainer should check current prices.

Selection rules: a **CMR** (not SMR) 3.5-inch drive, 7200 rpm class, 5-year warranty, rated for 24x7 NAS use; confirm CMR in the vendor datasheet for the exact model (for example WD Red Plus and Red Pro rather than the SMR WD Red of 6 TB and below; Seagate IronWolf or IronWolf Pro; Toshiba N300; WD or Seagate enterprise lines). Two drives of the **same model** from different purchase batches if possible. Check 512e/4Kn compatibility (`ashift=12` already matches 4K). Check the drive's SATA speed (6 Gbps ports on ata2 and ata3 are the ones mirror-1 uses today).

## 16.3 Replacement procedure (future; not performed)

Gates for every new disk, before it is attached:

1. **Receive and record**: serial, model, firmware; `smartctl -a` baseline (0 reallocated, 0 pending, 0 CRC errors).
2. **Burn-in on the bench port** (USB dock or spare SATA while the disk is empty): SMART long self-test; a full destructive write and read pass (`badblocks -wsv`, or `fio` verify) of the whole drive; a second SMART comparison. Pass: no change in reallocated, pending or CRC counters, no I/O errors, long test passes. A drive that fails is returned, not used.
3. Record the new by-id name; **always use by-id names** in ZFS commands, never `sdX`.

Pool gates before the first replacement:

4. `zpool status -x` is healthy and a **fresh scrub** has finished with 0 errors (`sdb` has read faults, so expect some repaired reads; any "permanent errors" stop the plan).
5. `sdc`'s SMART is clean and its extended test passes now (it will be the only good source if `sdb` drops).
6. A backup exists **if the maintainer wants one**. Mirror-only was chosen; an independent copy (the `usbbak` pool with `syncoid`) is **optional**. It is not a prerequisite for this plan.

Order, one disk at a time (the pool must never have two disks of one mirror out at once):

| Step | Action                                                                                                                                                                                                                                                                                                   | Gate to continue                                                      |
| ---- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------- |
| A    | Attach the new disk #1 over USB (or any bench port) and run `zpool replace tank <sdb by-id> <new1 by-id>` **with `sdb` still online** as a read source. `sdb` and `sdc` both feed the resilver.                                                                                                          | Resilver completes with 0 errors; `zpool status` shows `sdb` replaced |
| B    | Power down, move the new disk to ata2 (a by-id name makes the import safe), boot, confirm membership. If the disk must be on a SATA port before resilvering (no USB route), `zpool offline sdb`, swap, `zpool replace`. In that case `sdc` is the only copy during the resilver; do it only after gate 5 | `zpool status` healthy, same by-id, no new SMART errors               |
| C    | Scrub; then SMART short test on both new and `sdc`                                                                                                                                                                                                                                                       | Scrub 0 errors; no counter changes                                    |
| D    | Same for `sdc` -> new disk #2, with the new disk #1 as the clean partner                                                                                                                                                                                                                                 | As A to C                                                             |
| E    | `zpool set autoexpand=on tank` (or `zpool online -e` per disk), then check `zpool list` shows the new size                                                                                                                                                                                               | Capacity as in 16.2; `autoexpand` can be set back to `off`            |
| F    | Optional: `zpool remove tank mirror-0`, wait for evacuation, scrub, then power down and recycle the Hitachis                                                                                                                                                                                             | Only on a separate approval; scrub 0 errors                           |

Resilver size: only allocated data is copied (727 G for mirror-1, so a few hours at the old disks' sequential speed, longer if `sdb` is slow on its pending sectors). A resilver that stalls, reports checksum errors on the new disk, or shows `sdc` errors **stops the plan** (do not detach anything).

Rollback: until `zpool detach` of the old disk (done automatically by `zpool replace` at the end), the old disk is the source of truth. To abandon a replacement in progress: `zpool detach tank <new by-id>` (the old disk remains in the mirror). After completion the old `sdb` is kept unplugged on a shelf until the post-replacement scrub passes; it is not a backup (it is failing).

## 16.4 What mirror-only means (risk, not a prerequisite)

A mirror protects against **one disk failing**. It does not protect against deletion, a bad command, ransomware, a controller or power fault that damages both members, fire or theft. Snapshots (design 15 §15.4) protect against deletion and mistakes while the pool is alive. The maintainer chose this level for Phase 1. An independent, optional extra copy (`usbbak` with `syncoid`, or any second device or site) would cover the rest; it is **not required** by this plan and no step here depends on it. What the scans and archive can be re-fetched from (LoC) is stated in design 15 §15.2.3; data that cannot be re-derived is exactly what the maintainer should decide on (D1).

## 16.5 Monitoring while disks are suspect

`smartd` mail on any change of reallocated, pending or CRC counters (`sdb` and `sdd` especially); `zpool status -x` check every 5 minutes with alert; scrub every month and **before** each replacement step; a weekly `smartctl -a` snapshot kept as text to see trends. These are the same alerts as design 15 and are future work on the host.

## 16.6 Authenticated on-prem scan access for the BOINC bridge

### Today

The bridge (Container Apps job in `usnm-boinc-vnet-prod`) reads the Azure scan archive first with its managed identity (Storage Blob Data Reader, ADR-0018 decision 7), checks sha256 (and md5 for a JP2), and falls back to LoC on any failure (`crates/usnm-boinc/src/scans.rs`; `SCAN_ARCHIVE_URL` / `--scan-archive`). Auth in this repo is Entra only (ADR-0009: no shared keys, no SAS). Relevant existing pieces:

- `usnm-store::credential` obtains Entra tokens from the Container Apps managed identity, Arc, or the CLI. The same code gets a token for another audience (`from_env_for(resource)`).
- On-prem nodes already authenticate to Azure with Arc managed identities (ADR-0015); there is no VPN between Azure and on-prem (ADR-0015, ADR-0019) and the BOINC database is private.
- `crates/usnm-scan-reader` (#649) is an unwired axum reader for the archive with Ed25519 tickets, per-ticket and per-IP budgets, `If-Match` ranged GETs, `private, no-store` and no proxy-header trust. Its README lists open gates: durable budgets, signing-key placement, activation review.
- `usnm-ocr-worker serve-scans` is **LAN-only plain HTTP** with a host allowlist and an nftables rule, and it also accepts `DELETE` on `buffer/` and `PUT` for manifests. It must **not** be exposed or reused for this.
- No server-side Entra JWT validation exists in the repo yet. This design adds it, and it is new code that needs review.

### Options compared

| Option                                                                                                                                                  | Inbound to home                                                            | New Azure cost / resource                                           | Fit                                                                                                                                                                |
| ------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------- | ------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| A. **Keep the Azure copy; sync on-prem to Azure** (today's design, the "pull" direction)                                                                | None                                                                       | The Azure copy stays (~$16/month for scans)                         | Safest; no savings for scans. Default.                                                                                                                             |
| B. **Stage needed pages**: the on-prem side pushes the pages a run needs to the Azure exchange container (`boinc-handoff`), the bridge reads them there | None (outbound only; the scan fetcher already does this with Arc identity) | Small, short-lived blobs; no standing 1 TB copy                     | Matches how the scan fetcher (CT 211) already feeds the bridge. No inbound exposure. Needs a run manifest of pages in advance. Recommended default for the bridge. |
| C. **Read-only HTTPS reader on-prem, bridge calls it with an Entra token** (this section's design)                                                      | Yes, one HTTPS port or a tunnel                                            | None, or a tunnel/relay service                                     | Lets Azure scan storage be retired (~$16/month); adds inbound exposure and new auth code. Only with the controls below.                                            |
| D. Site-to-site VPN gateway                                                                                                                             | Private                                                                    | VPN gateway (VpnGw1 about $0.19/hour, ~$139/month) plus a public IP | Costs more than the saving. Rejected. ADR-0015 says no VPN.                                                                                                        |
| E. Expose `serve-scans` or a generic file/S3 server                                                                                                     | Yes                                                                        | None                                                                | **Rejected**: unauthenticated or write-capable. Never.                                                                                                             |

Recommendation: **B** for Phase 2 if the page set per run is known in advance (it is: the hand-off job exports a manifest), else **C**. C is specified below so the maintainer can approve it; neither is implemented.

### Design for C: `usnm-scan-gateway` (new, read-only)

A separate small process (not `serve-scans`), reusing `usnm-scan-reader`'s router, integrity checks and `ArchiveReader` seam with a **filesystem** reader instead of the Azure blob reader and an **Entra bearer** authenticator instead of tickets (tickets stay for future volunteer clients).

1. **Surface**: `GET` and `HEAD` of one path shape only, `/v1/scans/keep/<batch>/<lccn>/<date>/<edition>/<seq>.<jp2|jpg>`. Every segment is validated against a strict pattern (the same pattern `blob_name` generates). No directory listing, no index, no other verbs (`PUT`, `DELETE`, `POST` return 405), no query-string use, no `..`, no encoded separators.
2. **Containment**: the service runs as its own unprivileged user with a **read-only bind mount** of `/tank/ocr/scans/keep` only (not `buffer/`, not `manifests/`, not `/tank`); paths are resolved with `openat2`-style `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS` (or `O_NOFOLLOW` per component plus a final check that the opened file is a regular file inside the root). Systemd sandbox: `ProtectSystem=strict`, `ReadOnlyPaths`, `NoNewPrivileges`, no capabilities.
3. **Token validation** (every request): `Authorization: Bearer <Entra access token>`. Validate the RS256 signature against the tenant's JWKS (cached, refreshed on unknown `kid`, bounded), then require: `iss` equal to the tenant's v2.0 issuer (`https://login.microsoftonline.com/<tenant>/v2.0`) or the v1 issuer, whichever the app registration issues (pin one); `tid` equal to the tenant; `aud` equal to **a dedicated application ID URI** for this gateway (not the storage audience, which would let any storage token in); `exp` and `nbf` within a small skew; and `oid` (the principal) **equal to the bridge job's managed identity** object id from configuration. Optionally also require an app role (`Scans.Read`) assigned to that identity. Any other principal, including the other project identities, gets 403. Tokens are never logged.
4. **Identity on the Azure side**: a new app registration (resource) with the `Scans.Read` app role, assigned to `usnm-boinc-id-<env>` only; the bridge requests a token for that resource with its existing managed identity (`from_env_for`). No secret and no certificate is created. This is an Azure change and needs approval (decision P2).
5. **Transport**: TLS 1.2+ end to end. Preferred: an **outbound-only tunnel** from the home host to a public hostname (for example a Cloudflare Tunnel or similar; no inbound port, no home IP exposed, WAF and rate limits at the edge), with the gateway still validating the token itself (the tunnel is not the authorization). Alternative: port-forward 443 on the router to the gateway behind a TLS certificate (ACME). The Container Apps consumption outbound IPs can change (ADR-0008), so an IP allowlist is not the control; it may be added as defence in depth only. The choice of tunnel provider and ingress is decision P1.
6. **Abuse limits**: per-principal and global request-rate and bytes-per-second caps, a concurrency cap, a response-size ceiling matching the largest scan, small timeouts, request logging to the host journal (path, status, bytes, principal, no tokens). The 1 Gbps uplink is shared with everything else; the cap keeps the bridge to a share (suggest 200 Mbps) so the site is not saturated.
7. **Integrity**: the bridge already verifies the sidecar sha256 (and md5) against the blob metadata. For the gateway, the manifest of the keep set (path, size, sha256) is generated on `tank` and the gateway returns the sha256 in a response header (`x-usnm-sha256`) from that manifest. The bridge verifies the bytes against it and against the Azure copy's metadata during shadow mode. `ETag` is the sha256; ranged reads use `If-Match`, as in `usnm-scan-reader`.
8. **Responses**: `Cache-Control: private, no-store`, `X-Content-Type-Options: nosniff`, no `Server` detail, no redirect, no proxy-header trust (as in `usnm-scan-reader`).

### Failure behavior

The bridge treats the gateway as one more source in its existing chain: **gateway, then Azure archive copy (while it exists), then LoC**, and any of 401/403, 404, 429, 5xx, timeout, connection failure, or a checksum mismatch moves to the next source and is logged. The on-prem side being down therefore delays nothing as long as the Azure copy exists; after the Azure copy is retired the chain is gateway then LoC, which is already how the bridge behaves when a page is not archived. No failure is allowed to hold the BOINC hand-off or its coordination, which stay entirely in Azure (exchange container, Cosmos lease, ledger). A 403 (identity or audience misconfiguration) is alerted, not retried in a loop. LoC pacing rules (5 s gap, no retries) are unchanged.

### Staged rollout

1. **Gateway built and reviewed** (not part of this PR): code in the usnm-scan-reader style, unit tests for path containment and token validation (wrong `aud`, wrong `iss`, wrong `oid`, expired, `alg=none`, bad `kid`, `..`, symlink, encoded slash, write verbs).
2. **Shadow**: the bridge keeps reading Azure; for a sample it also fetches from the gateway and compares hashes and latency. No reads depend on it.
3. **Primary with fallback**: `SCAN_ARCHIVE_URL` (or a new `--scan-gateway` setting) points at the gateway first; Azure copy still present as fallback.
4. **Retire the Azure scan copy** only after: the on-prem copy is checksummed against the Azure blobs (sha256 per blob, 224,399 images), a scrub of `tank` passed after the disk replacement, the Cold-tier 90-day minimum and soft-delete retention are accounted for (design 15), D1 is decided, and the maintainer approves the specific deletion. Not part of this task.

Rollback at every stage: set the source setting back to the Azure archive URL (one environment change; the Azure copy is untouched until stage 4); stop the gateway or the tunnel. Nothing is lost by a rollback because the gateway only reads.

### Open risks

- A new internet-facing service on a home host. Mitigated by read-only bind mount, a separate user, token pinning and a tunnel, but it is still new exposure; a compromised gateway can read only the keep set (public-domain LoC images).
- JWT validation is security-critical new code; use a reviewed library (not hand-rolled) and the tests above.
- Home uplink, power and the single mirror now sit in the BOINC critical path for page reads (but not for hand-off), softened by the Azure and LoC fallbacks.
- JWKS fetch needs outbound access to Entra from the gateway; cached keys must survive a short outage.

## 16.7 Decisions needing the maintainer's approval

- **R1 — disks.** Approve the mirror-1-first plan (`sdb` then `sdc`), and the size: 2 x 8 TB (minimum), **2 x 12 TB (recommended)** or 2 x 16 TB. Approve or reject the optional later removal of mirror-0 (and retiring `sdd`).
- **R2 — burn-in.** Approve a destructive burn-in of the new drives before use (they are empty), and the USB-attach resilver route (old disk stays as a read source) versus the offline-and-swap route.
- **R3 — optional backup.** Whether to add an independent copy (`usbbak` plus `syncoid`) at all. Not required by this plan.
- **P1 — scan access route.** B (stage pages to the exchange container, no inbound), C (authenticated reader), or keep the Azure copy. If C: tunnel versus port forward, and the provider.
- **P2 — Entra.** A new app registration and app role for the gateway audience, assigned to the BOINC bridge identity only (Azure change), and the pinned issuer version.
- **P3 — code.** Build `usnm-scan-gateway` on `usnm-scan-reader`, and its rate caps and uplink share.
- **P4 — retirement.** The later, separate approval to delete the Azure scan copy, after the checksum, scrub and retention gates in 16.6.

## 16.8 Not done and not known

- No drives were bought, benchmarked or attached; no `zpool` command that changes state was run; no Azure resource, role or firewall changed.
- The exact drive model, price and datasheet CMR confirmation are open. SATA cabling and USB dock throughput for the USB-attach route were not measured.
- Whether the router or ISP allows an inbound port, and the tunnel's terms and limits, are not checked.
- The gateway code and its JWT library choice do not exist yet.
