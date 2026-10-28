# ADR-0028: Cloudflare proxies the existing Azure origin

- **Status:** Accepted by maintainer, 27 October 2026 ([#1274](https://github.com/tgoodyear/usnewsmap.com/issues/1274)).
- **Amends:** ADR-0027 decision 3's requirement to wait for the on-prem cutover before proxying public hostnames.

## Decision

The current serving path is **Visitors → Cloudflare → Azure API/web → Azure
searchers/index**. The searcher, index, application deployment and storage stay
in Azure. On-prem hardware and measurement gates still apply to any later origin
migration, but do not block enabling Cloudflare in front of Azure.

The apex, `www` and `api` retain their existing Azure origin records and become
proxied only after replacement origin certificates, privacy text and proxy
handling are ready. Azure's DNS-dependent managed-certificate renewal is not
suitable behind Cloudflare: use renewable DNS-01 public certificates bound to
all three names, with their resource ID retained in the existing private
certificate settings. Validate origin certificates with Full (strict), not
Flexible or unverified Full.

Use the cache rules staged in #1270. Cache static/version-qualified resources
under their origin headers, retain complete query strings in cache keys, and
bypass HTML, search results, sitemap, live endpoints and all `api`/`www` calls.
The existing basemap and OCR dashboard rules remain unchanged. Preserve PMTiles
range semantics and never force-cache `no-store` errors.

Apply `scripts/onprem/serving/azure-proxy-config-rule.json` only to these three
hostnames. It disables Browser Integrity Check, optional analytics, Zaraz,
fonts and HTML rewriting, and requires strict TLS without modifying other
hostnames' settings. Keep Bot Fight Mode off and do not introduce
cookie-setting challenges. Update `/privacy` before activating the proxy;
Cloudflare sees IP addresses, complete URLs and headers regardless of DNT/GPC.
The Azure application's own anonymous search log and retention do not change.
Disable the zone-wide Speed Brain, Replace Insecure JavaScript and Always
Online features, recording their prior values. This avoids unsolicited
prefetches, content rewriting and stale HTML fallback; these three changes also
apply to other proxied subdomains. Existing per-service cache rules are retained.

ACA's ingress supplies the verified connection source as the rightmost
forwarded address. In the existing one-hop ACA mode, the application peels one
additional hop only when that source is a published Cloudflare edge address.
Direct Azure requests keep their existing handling; an arbitrary
`CF-Connecting-IP` header is not trusted. Refresh the checked Cloudflare ranges
in `ratelimit.rs` if Cloudflare changes its published networks.

## Activation and rollback

Retain before/after receipts for DNS, host-specific rules and certificate
bindings. Check the real origin certificate and readiness first; activate
proxying, then verify all three domains, the apex redirect, privacy, warm cache
HITs, PMTiles 206 headers and bypassed live API calls. Do not equate staged rules
with active caching.

To roll back the proxy, restore only these three records' `proxied: false`
values. Their Azure targets remain unchanged. DNS-01 public certificates are
trusted directly as well as through Cloudflare, so rollback needs no certificate
swap. Keep renewal running in either routing mode. No Azure resource is retired.
