# On-prem serving: Phase 2 measurements and gates

Part of the [operations guide](README.md). This is the checklist for [#1101](https://github.com/tgoodyear/usnewsmap.com/issues/1101): every measurement in [ADR-0027](../design/adr/0027-onprem-serving-behind-cloudflare.md#measurements), and every gate [moved from #955](https://github.com/tgoodyear/usnewsmap.com/issues/1101#issuecomment-6555006071), with how to take it, what to record, what it is compared against, and where it stands.

Public serving moved on-prem at 02:01:07 UTC on 31 October 2026 ([cutover receipt](https://github.com/tgoodyear/usnewsmap.com/issues/955#issuecomment-6537903917)). [ADR-0029](../design/adr/0029-onprem-primary-azure-failover.md) keeps Azure as a cold spare behind a health-driven Worker, so ADR-0027's Phase 4 deletions are gone, but these measurements and gates still apply to serving on-prem.

An item is done when it has evidence or a disposition the maintainer has approved. The statuses below are as of **4 November 2026**. Earlier results that came from other hardware or other conditions (the HDD benchmark, the dev container's reboot) are listed as context, not as the Phase 2 result.

## Categories

- **Read-only**: public HTTPS and DNS checks, Cloudflare or Azure reads with a read-only credential, and inventory commands on the host. No window needed.
- **Drill**: anything that stops a service, injects a failure, loads the production origin or the uplink, or wakes the Azure spare. It needs an approved maintenance window, the drill Worker and hostname ([cold drill](search-serving.md#cold-drill)), and `DNT: 1` on every request.
- **Owner**: physical work or a confirmation only the owner can give (hardware, firmware settings, the ISP contract).

Don't commit private addresses, private host names, account ids or tokens with the evidence. Name the drill hostname, the serving container and the host generically, as this page does.

## Summary

| ID  | Item                                                 | Category              | Status, 4 Nov 2026                                                      |
| --- | ---------------------------------------------------- | --------------------- | ----------------------------------------------------------------------- |
| M1  | Cold-search latency on the NVMe vs Azure             | Drill                 | Open. HDD result only; no NVMe result published                         |
| M2  | TTFB, assets and LCP from several regions            | Read-only             | Partial: a few samples from the operator's network                      |
| M3  | 30 days of traffic vs Workers Free and the uplink    | Read-only             | Open. Window ends 30 Nov 2026                                           |
| M4  | Fallback cold-cache load, no-connector response, XFF | Drill                 | Partial: ADR-0029 failover drills; the three named checks not recorded  |
| M5  | Does `cloudflared` add an `X-Forwarded-For` entry    | Drill                 | Open                                                                    |
| M6  | Forced upstream failure leaves no query or IP in log | Drill                 | Open. Redaction is in code; the drill isn't recorded                    |
| M7  | Sustained upstream throughput; static home IP        | Drill + owner         | Open. Link speed only                                                   |
| M8  | Current zone settings                                | Read-only             | Partial: public inference; Speed Brain appears on                       |
| M9  | M.2 slot vs SATA ports                               | Read-only             | Answered: all six SATA ports enumerate with the NVMe in                 |
| M10 | Azure certificate renewal lead time                  | Read-only             | Superseded by DNS-01 origin TLS; disposition needed                     |
| M11 | Security Analytics / Events and the query string     | Read-only             | Open                                                                    |
| M12 | Share of the lake's Hot reads that is the index      | Read-only             | Open; likely superseded; disposition needed                             |
| M13 | Cloudflare Load Balancing prices                     | Read-only             | Answered: from $5 a month; not chosen (ADR-0029)                        |
| M14 | On-prem power draw                                   | Owner                 | Open                                                                    |
| G1  | UPS for the host and the network gear, USB shutdown  | Owner                 | Not met as far as the host shows: no UPS tooling and no UPS on USB      |
| G2  | BIOS powers on after a power loss                    | Owner                 | Unknown                                                                 |
| G3  | Plug-in Intel NIC                                    | Read-only, then owner | Not met: only the onboard I219-V                                        |
| G4  | 30 days at 99.0%, fallback answers counted as down   | Read-only             | In progress; fallback counts aren't exported, so early days are unknown |
| G5  | Rehearsed reboot                                     | Drill                 | Not met: no host boot since the cutover                                 |
| G6  | NVMe holds the serving index (ADR-0027 decision 5)   | Read-only + decision  | Installed (2 TB, one version); decision 5's two versions don't fit      |

## Read-only snapshot, 4 November 2026

Taken between 22:40:55 and 22:42:42 UTC by the agent working #1101.

- **Public checks** ran from the operator's Mac on the home network, which is the origin's own network, through Cloudflare's Atlanta colocation (`cf-ray` suffix `ATL`). A visitor elsewhere takes a different path, so these numbers show the edge and tunnel path from here, not what visitors see. Each request sent `DNT: 1` and a `usnm-phase2-readonly` user agent. No search was sent.
- **Host inventory** ran over SSH on the serving host with read-only commands only: `lsblk`, `zpool list`, `zpool status -x`, `lspci`, `ip -br link`, `ethtool`, `dmidecode`, `smartctl -A`, `lsusb`, `command -v`, `systemctl is-active`, `journalctl --list-boots`, `journalctl -k` (a count) and `pct list`/`pct config`.
- Nothing was started, stopped, changed or deployed, and the Azure spare was not contacted.

### Certificates (edge)

`openssl s_client -servername <name> -connect <name>:443 | openssl x509 -noout -issuer -enddate -ext subjectAltName`:

| Names               | Issuer                      | SANs                                 | Expires                  |
| ------------------- | --------------------------- | ------------------------------------ | ------------------------ |
| apex, `healthcheck` | Google Trust Services (WE1) | apex, `healthcheck`, `*.healthcheck` | 29 Jan 2027 02:16:46 UTC |
| `www`, `api`        | Let's Encrypt (YE2)         | apex, `*.usnewsmap.com`              | 19 Jan 2027 00:51:06 UTC |

These are the certificates Cloudflare presents, which Cloudflare renews. The origin certificate behind them is the DNS-01 one in [origin TLS](origin-tls.md) (M10). TLS 1.2 and 1.3 negotiate; this client can't attempt TLS 1.0 or 1.1, so the minimum version is unverified.

### DNS and zone facts

`dig @1.1.1.1` (and the apex again at `@8.8.8.8`):

- NS: two `ns.cloudflare.com` servers. The zone is signed: a DS record is published and validating resolvers set `ad`.
- Apex, `www`, `api`, `healthcheck` and `boinc` answer with the same two Cloudflare anycast IPv4 and two IPv6 addresses, TTL 300: all proxied. No answer exposes the origin.
- CAA: `issue` and `issuewild` for `comodoca.com`, `digicert.com`, `letsencrypt.org`, `pki.goog` and `ssl.com`.

### Response headers and edge cache

`curl -D -` on each path:

| Path                 | Status | `cf-cache-status`  | `X-USNM-Origin` | Notes                                                       |
| -------------------- | ------ | ------------------ | --------------- | ----------------------------------------------------------- |
| `/`                  | 200    | DYNAMIC            | `onprem`        | `cache-control: no-cache`, ETag                             |
| `/v1/meta`           | 200    | DYNAMIC            | `onprem`        | `cache-control: public, max-age=300`                        |
| `/robots.txt`        | 200    | HIT                | `onprem`        | The origin's file, no Cloudflare-managed block              |
| `/assets/index-*.js` | 200    | HIT (age 34,227 s) | `onprem`        | `max-age=31536000, immutable`                               |
| `www` `/`            | 301    | DYNAMIC            | `onprem`        | To the apex, from the origin, not a Single Redirect         |
| `http://` apex       | 301    | n/a                |                 | To `https://`, from Cloudflare (Always Use HTTPS)           |
| `healthcheck` `/`    | 200    | n/a                | `onprem`        | `{"available":true,"origin":"onprem","primary_ready":true}` |

Every proxied response also carries `strict-transport-security: max-age=300`, `alt-svc: h3=":443"` and `speculation-rules: "/cdn-cgi/speculation"`. None set a cookie. The home page's HTML holds no Cloudflare-injected script (no Web Analytics beacon, Rocket Loader or email obfuscation).

### Time to first byte

`curl -w` with a new connection each time, three samples per URL, a second apart. Milliseconds; first byte includes DNS, connect and TLS.

| URL                        | First byte, 3 samples | Median | Phase 0 (Azure, 24 Oct, US residential, median of 5) |
| -------------------------- | --------------------- | ------ | ---------------------------------------------------- |
| `/`                        | 84, 150, 82           | 84     | 67                                                   |
| `/datasets`                | 74, 82, 104           | 82     | n/a                                                  |
| `/v1/meta`                 | 79, 91, 71            | 79     | 65                                                   |
| JavaScript bundle (147 KB) | 77, 48, 62            | 62     | 62 (plus 48 to download)                             |

The bundle took 70 to 93 ms in total. Three samples from one network give a median, not a p95, and the path differs from Phase 0's ([baselines](https://github.com/tgoodyear/usnewsmap.com/issues/955#issuecomment-6355390978)). They show that the tunnel path answers in under about 150 ms here; they aren't M2's result.

### Host inventory (the serving host)

- **Board:** MSI Z170A SLI PLUS (MS-7998), BIOS 1.E0 (15 Jun 2018).
- **NVMe:** one 2 TB M.2 PCIe drive (DRAM-less MAXIO MAP1202 controller), firmware GTd74529, in its own ZFS pool, `nvme-serving`, 1.86 TiB, ONLINE and healthy. SMART: no critical warning, 0% used, no media errors, 38 °C, 2 unsafe shutdowns. It reports 8 power-on hours, which doesn't fit an install on 30 October, so treat that counter as unreliable.
- **SATA:** the chipset's AHCI controller enumerates a disk on each of `ata1` to `ata6`: the two 14 TB `tank` drives, two 3 TB drives and two SSDs. Two 8 TB SAS drives sit on a separate SAS3008 HBA, and two drives are on USB.
- **NIC:** one, the onboard Intel I219-V (`e1000e`, firmware 0.8-4), 1000 Mb/s full duplex with the link up. No add-in NIC. The kernel log since 31 Oct 00:00 (host time) has no line matching `reset adapter`, `hardware unit hang` or `link is down`.
- **UPS:** none of `upsc`, `upsd`, `upsmon`, `apcaccess`, `pwrstat` or `nut-scanner` is installed; `nut-server`, `nut-monitor`, `nut-client`, `apcupsd` and `pwrstatd` are inactive; `lsusb` lists no UPS.
- **Boots:** the current boot started at 23:00 UTC on 30 October, three hours before the cutover. The two before it were on the same day (the NVMe install). The serving container is set to start at boot.

## Checklist

Each item gives the method, the evidence to record, what it's compared against and its status. Run the drills only in an approved window, against the drill hostname unless the item needs the public one, and stop each drill and restore the service even if the measurement fails.

### M1. Cold-search latency on the NVMe

- **Category:** drill. It loads the production origin, needs a rebuild and a publish to run at the same time for two of its three conditions, and its same-day Azure run wakes the cold spare.
- **Method:** `scripts/ops/load-cold-searches.py --base-url <origin> --levels 1,4 --offset <N> --verbose` once per condition (idle, during a rebuild, during a publish) and once against the Azure spare through the drill Worker, the same day. A run makes one pass for the "first" report plus one per level, and each pass shifts every window back one more day, so a run with `--levels 1,4` uses shifts offset to offset + 2. Space the offsets by more than the passes (at least three apart here) and never reuse one (for example 600, 610, 620, 630); otherwise a later run meets responses an earlier one cached. Keep `--verbose` for per-search times, from which the percentiles come. Record the API image digest, the Quickwit build and the index version on both sides. `scripts/ops/bench-cold-searches.py` repeats three fixed searches with no offset, so it is a separate check of those searches, not this result.
- **Evidence:** p50, p95 and failures per level, the UTC window, the load conditions (rebuild or publish running, their progress), the image and index versions and the offsets.
- **Compared against:** uncached `/v1/aggregate` p95 ≤ 2 s ([09 §9.1](../design/09-operations-security-cost.md#91-service-level-objectives)).
- **Status:** open. The [30 October benchmark](https://github.com/tgoodyear/usnewsmap.com/issues/955#issuecomment-6513316394) ran on the HDD mirror and failed (local p95 36.86 s against Azure's 3.97 s at concurrency 1, ten samples). The [NVMe copy and follow-on run](https://github.com/tgoodyear/usnewsmap.com/issues/955#issuecomment-6525229749) started the same day and left its receipts on the host; no NVMe result has been posted.

### M2. Time to first byte, assets and LCP from several regions

- **Category:** read-only. It needs no window, but runs from outside the home network.
- **Method:** `python3 scripts/bench-site-speed.py --base-url https://usnewsmap.com --globalping --json <file>`; `python3 scripts/bench-site-speed.py --base-url https://usnewsmap.com --runs 5` from a network outside the home; `node scripts/bench-site-lcp.mjs --base-url https://usnewsmap.com --profile desktop` and `--profile mobile` ([#1006](https://github.com/tgoodyear/usnewsmap.com/pull/1006)). All send `DNT: 1`.
- **Evidence:** per-URL and per-country medians and worst cases, LCP per page and profile, the UTC time, and the `X-USNM-Origin` that answered.
- **Compared against:** the [Phase 0 baselines](https://github.com/tgoodyear/usnewsmap.com/issues/955#issuecomment-6355390978) (median first byte 383 ms over 12 probes for `/`), and SPA LCP p75 ≤ 2.5 s (09 §9.1).
- **Status:** partial. The snapshot above has three samples per URL from the home network. The Globalping and browser runs are still to do.

### M3. 30 days of traffic

- **Category:** read-only.
- **Method:** Cloudflare analytics for the zone and the production Worker (requests a day, busiest second and minute, bytes), read with a read-only token or the dashboard. `scripts/azure/logs.sh --env prod --query-name serving-load --span 30d` covers only requests that reach Application Insights, so check first that the on-prem API's requests are there.
- **Evidence:** the daily series, the busiest day, second and minute, bytes a day, the window and the source.
- **Compared against:** Workers Free's 100,000 requests a day and 10 ms CPU a request; the uplink (M7).
- **Status:** open. A full window of on-prem traffic ends at 02:01 UTC on 30 November 2026. ADR-0029's drill-day sample (1,900 production Worker requests to 03:36 UTC, fetch CPU up to 8 ms) is a sample, not this measurement.

### M4. Fallback cold-cache load, the no-connector response and `X-Forwarded-For` at Azure

- **Category:** drill. It stops the drill connector and wakes the spare.
- **Method:** the [cold drill](search-serving.md#cold-drill) on the drill hostname. With the connector stopped, run `node scripts/bench-site-lcp.mjs --base-url https://<drill hostname>` (a fresh browser context is a cold browser cache) and confirm every asset loads with `X-USNM-Origin: azure`. Record the Worker's outcome record for the failed primary probe. The API logs no client addresses, and DNT turns off the page-view path that uses one, so the header order needs its own probe: a temporary app in the same Container Apps environment, behind the same ingress, reachable only through the drill Worker, that answers `/readyz` with 200 and the exact body `ready` (the Worker's readiness check), returns the request headers in its response body on every other path, and logs nothing. Confirm the echoed response came from this app, not an `azure-unready` 503. Send one DNT request with a known `X-Forwarded-For`, save the response, then delete the app and confirm it is gone.
- **Evidence:** page load and LCP, failed requests, origin per request, the Worker's outcome reason and status, and the observed header order.
- **Compared against:** ADR-0029 decision 1 (a ready origin is chosen before the request) and decision 4 (an explicit 503 with `Retry-After` if neither is ready).
- **Status:** partial. The [cutover drills](https://github.com/tgoodyear/usnewsmap.com/issues/955#issuecomment-6537903917) had 0 of 865 synthetic requests fail, including a connector outage with both Azure apps at zero (first Azure answer after 68.3 s). They used readiness and aggregate requests, not a browser, and recorded neither the no-connector response nor the header order. Under ADR-0029 the Worker chooses by `/readyz`, not by a 530, so the maintainer may dispose of the no-connector question.

### M5. Does `cloudflared` add an `X-Forwarded-For` entry

- **Category:** drill, on the private test container.
- **Method:** in the private test container, run a temporary header-echo listener on loopback in place of Caddy, reachable only through the drill hostname's tunnel, logging nothing. It answers `/readyz` with 200 and the exact body `ready`, with no `X-USNM-Origin: azure` header, so the drill Worker keeps choosing it; check that the echoed response has no `X-USNM-Origin: azure`. Send one DNT request with a known `X-Forwarded-For`, save the echoed headers, then stop the listener and restore Caddy.
- **Evidence:** the header as sent, as Cloudflare forwards it and as Caddy receives it.
- **Compared against:** `USNM_TRUST_CF_CONNECTING_IP` and `USNM_TRUSTED_PROXY_HOPS` ([configuration](../configuration.md)), which assume the API takes the address from `CF-Connecting-IP` when the peer is loopback.
- **Status:** open.

### M6. A forced upstream failure leaves no search or client IP in a retained log

- **Category:** drill.
- **Method:** with the health Worker in front, a failed `/readyz` sends the marked search to Azure, so it never reaches the failing proxy and a zero count proves nothing. Use a test hostname routed straight to the test container's tunnel with no Worker route. Fail one layer at a time: stop the API behind Caddy (Caddy answers 502), then point the connector's ingress at a closed loopback port (`cloudflared` answers 502). For each, send a DNT search with a random marker word and note the client's public address. Accept the log counts only with proof of delivery: the 502 from the expected layer, no `X-USNM-Origin: azure`, and that layer's error-log line for the request at that time (counted, not copied). Then search the container's and the host's journals since the start for the marker and the address (`journalctl --since <start> | grep -c <marker>`).
- **Evidence:** per layer, the response status and the delivery proof, then counts per log source (zero expected), the window, and the marker's hash rather than the word. Restore the API and the ingress and remove the test route afterwards.
- **Compared against:** [09 §9.4.2](../design/09-operations-security-cost.md#942-privacy) and ADR-0027 decision 9. ADR-0029 already records that Worker invocation logs keep client-IP headers and geolocation with the query redacted.
- **Status:** open. `run-cloudflared.sh` cuts the query string from error lines and Caddy's error log drops the request field ([serving logs](../../scripts/onprem/serving/README.md#logs)); no drill has shown it.

### M7. Sustained upstream throughput, and whether the home IP is static

- **Category:** throughput is a drill (it fills the uplink production uses); the address is an owner confirmation.
- **Method:** a timed upload from the host to an external endpoint, capped and scheduled off-peak, while watching that the public probes stay healthy. For the address, the ISP contract, or the history of the home-IP-change runbook ([09 §9.3.1](../design/09-operations-security-cost.md#931-on-prem-serving)).
- **Evidence:** sustained Mbit/s over at least a minute, the time and the endpoint's region; the owner's answer on the address.
- **Compared against:** 1 Gbit/s symmetric as recorded in [ADR-0022](../design/adr/0022-onprem-cold-storage.md), and M3's peak rates.
- **Status:** open. The host's only NIC links at 1000 Mb/s full duplex, an upper bound.

### M8. The current zone settings

- **Category:** read-only, with a Cloudflare read token or the dashboard.
- **Method:** `GET /zones/<zone>/settings` and the Bot Fight Mode, Browser Integrity Check, Web Analytics and Speed Brain settings, saved with the time read. ADR-0027 decision 12 says the recorded values are the reference until a script applies them.
- **Evidence:** each setting's value and the time read.
- **Compared against:** ADR-0027 decision 3's list.
- **Status:** partial: inferred from public responses only.
  - Speed Brain looks **on**: every response carries `speculation-rules: "/cdn-cgi/speculation"`. ADR-0027 asks for it off unless the maintainer chooses it, so this needs the maintainer.
  - Always Use HTTPS is on (Cloudflare answers `http://` with a 301).
  - Rocket Loader, Email Obfuscation and Web Analytics' automatic setup show no trace in the home page's HTML, and robots.txt is the origin's. No response set `__cf_bm`, which suggests Bot Fight Mode is off but doesn't prove it.
  - HSTS is `max-age=300`, HTTP/3 is advertised and DNSSEC is on.

### M9. Does the M.2 slot share lanes with SATA ports

- **Category:** read-only.
- **Method:** `lspci` for the NVMe controller, `readlink -f /sys/block/sd*/device` for each disk's port, and `dmidecode -t baseboard`.
- **Evidence:** the board, the NVMe's bus and each SATA disk's `ata` port.
- **Compared against:** ADR-0027's note that all six SATA ports are in use.
- **Status:** answered, 4 Nov 2026. With the PCIe NVMe in the M.2 slot, the chipset's AHCI controller still enumerates a disk on all six ports. That is the OS's view, not a physical port map; an M.2 SATA drive could behave differently.

### M10. Azure's renewal lead time for the three certificates

- **Category:** read-only.
- **Method:** the [origin TLS](origin-tls.md) runbook's checks, and the `origin-tls` workflow's run history.
- **Evidence:** the origin certificate's expiry and the last scheduled renewal run.
- **Compared against:** Phase 3's hard end, which was set by the managed certificates' 28 March 2027 expiry.
- **Status:** superseded. [ADR-0028](../design/adr/0028-cloudflare-with-azure-origin.md) replaced the managed certificates with one DNS-01 Let's Encrypt certificate that a weekly workflow renews with under 30 days left; it expires 25 January 2027 ([origin TLS](origin-tls.md#live-acceptance-27-october-2026)). The last scheduled `origin-tls` run, on 2 November 2026, succeeded. Checking the origin certificate directly would wake the spare, so it wasn't done here. Proposed disposition: close as superseded.

### M11. Do Security Analytics or Security Events keep the query string

- **Category:** read-only (one synthetic DNT request with a marker, then a dashboard or GraphQL read).
- **Method:** send one request with a random marker in `q`, then look for it in Security Analytics' sampled logs and in Security Events.
- **Evidence:** whether the path, the query and the client IP appear, the retention shown, and the time.
- **Compared against:** the text of [09 §9.4.2](../design/09-operations-security-cost.md#942-privacy) and `/privacy` (settled decision 1).
- **Status:** open.

### M12. The share of the lake's Hot reads that is the index

- **Category:** read-only (Azure Storage metrics or `StorageBlobLogs`).
- **Method:** read transactions and egress by container over 30 days.
- **Evidence:** the index's share of reads and bytes.
- **Compared against:** the cost model ([09 §9.5](../design/09-operations-security-cost.md#95-cost-model-monthly-usd-list-prices-confirm-with-the-azure-pricing-calculator)).
- **Status:** open and probably superseded. The legacy lake indexes were retired ([#245](https://github.com/tgoodyear/usnewsmap.com/issues/245#issuecomment-6536934940)), on-prem serves from its NVMe, and the spare reads its own index account. Proposed disposition: close as superseded.

### M13. Cloudflare Load Balancing's prices

- **Category:** read-only.
- **Status:** answered, 4 Nov 2026: "Starting at $5/mo" on [cloudflare.com/plans](https://www.cloudflare.com/plans/); per-origin and health-check charges aren't shown there. ADR-0029 chose the Worker over Load Balancing.

### M14. On-prem power draw

- **Category:** owner.
- **Method:** a wall meter on the host, or the UPS's load reading once G1 is in place, over a day that includes a rebuild.
- **Evidence:** idle and busy watts, the meter and the window.
- **Compared against:** the cost model ([09 §9.5](../design/09-operations-security-cost.md#on-prem-serving-955)).
- **Status:** open. The host has no tool that reads wall power.

### G1. UPS

- **Category:** owner.
- **Method:** install a UPS that covers the host and the network gear on the path to the internet, with a USB link to the host; install NUT (or the vendor's daemon) and test a clean shutdown in a window.
- **Evidence:** the model, what it powers, the runtime at the host's load, `upsc` output and the shutdown test.
- **Compared against:** the [22 October decision](https://github.com/tgoodyear/usnewsmap.com/issues/955#issuecomment-6354758689), which made a UPS a condition of the move.
- **Status:** not met, as far as the host shows: no UPS software and no UPS on USB (snapshot above). The owner should confirm whether a UPS without a data link is in place.

### G2. BIOS powers on after a power loss

- **Category:** owner.
- **Method:** in the BIOS, the setting for restoring after AC power loss; then a test in a window (cut power at the wall, restore it, and watch the host boot unattended).
- **Evidence:** the setting and the test's time.
- **Status:** unknown. The OS can't read this setting.

### G3. Plug-in Intel NIC

- **Category:** read-only check, owner install.
- **Method:** `lspci -nn | grep -i ethernet` and `ethtool -i <iface>`; after the install, move the bridge to the new NIC in a window.
- **Evidence:** the NIC's model and driver, and the reset count in the kernel log over 30 days.
- **Compared against:** ADR-0027 decision 10 (the onboard NIC reset every 30 seconds for 6 minutes on 21 October 2026).
- **Status:** not met. The only NIC is the onboard I219-V. No reset has been logged since the cutover.

### G4. 30 days at 99.0%, with fallback answers counted as downtime

- **Category:** read-only.
- **Method:** the availability test's results for the window, and the Worker's fixed outcome records counted by origin (ADR-0029, "Code-backed edge configuration"). Every answer from `azure`, and every 5xx, counts as down. Native analytics have no origin dimension, and the outcome records expire after three days on Free (seven on Paid), so a query at the end of the window can't recover 30 days. Visitor counts must leave out the heartbeat's records (`trigger: "heartbeat"`) and the healthcheck hostname's invocations (by the invocation's hostname), because both log the same origin, reason and status fields. Export privacy-safe aggregate visitor counts (origin, reason, status, per hour) at least daily, before the records expire, and check the export's coverage from the cutover. Intervals with no export are unknown, not up.
- **Evidence:** good and bad minutes or requests, the number of fallback answers, and the window.
- **Compared against:** 99.0% (09 §9.1), about 7.3 hours a month.
- **Status:** in progress, with a gap. The window opened at the cutover and ends at 02:01 UTC on 30 November 2026. No recurring export of the outcome counts is recorded, so fallback counts older than the retention period are probably gone already and those days are unknown; the gate can't pass on this window unless the maintainer accepts that, or a new window starts once the export runs. On 4 November `healthcheck.usnewsmap.com` reported on-prem primary and ready.

### G5. Rehearsed reboot

- **Category:** drill.
- **Method:** the host reboot runbook in [09 §9.3.1](../design/09-operations-security-cost.md#931-on-prem-serving), in a window, with a DNT synthetic probe on the public hostname across the reboot, as in the ADR-0029 drills. Record the NVMe's unsafe-shutdown count before and after.
- **Evidence:** the time of the reboot, when the Worker moved to Azure and back, failed requests, `systemctl --failed` in the container, and the readiness checks.
- **Status:** not met. The host last booted three hours before the cutover and not since. The dev container's [reboot on 30 October](https://github.com/tgoodyear/usnewsmap.com/issues/955#issuecomment-6513316394) was a container, before the cutover.

### G6. NVMe for the serving index

- **Category:** read-only, plus a decision.
- **Evidence:** the snapshot above.
- **Compared against:** [ADR-0027 decision 5](../design/adr/0027-onprem-serving-behind-cloudflare.md#5-index-on-nvme-previous-version-on-tank) (a 4 TB drive holding two versions).
- **Status:** a 2 TB drive is installed and serving. The 1.2 TB index leaves room for one version, so the previous version stays on HDD, as the 30 October benchmark recommended. ADR-0027 decision 5 and [#1100](https://github.com/tgoodyear/usnewsmap.com/issues/1100) need the maintainer to accept that or buy a larger drive.

## Decisions needed

- Speed Brain is on (M8): keep it, or turn it off as ADR-0027 asks.
- Dispositions for M10 and M12 (superseded), and M4's no-connector question under ADR-0029.
- G6: one version on a 2 TB NVMe with the previous one on HDD, or a 4 TB drive.
- G4: start a daily export of the Worker's aggregate visitor outcome counts (heartbeat and healthcheck records left out), and whether the 30-day window restarts from it.
- A maintenance window for the drills: M1, M4, M5, M6, M7's throughput test and G5.
