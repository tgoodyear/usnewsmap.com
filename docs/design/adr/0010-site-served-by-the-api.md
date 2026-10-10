# ADR-0010: The API app serves the site

- **Status:** Accepted
- **Date:** 2026-09
- **Amends:** ADR-0006, decision 1 (Static Web Apps Free hosted the SPA)
- **Resolves:** the open exception in ADR-0009 (the Static Web App's deployment token)

## Context

ADR-0009 requires every authentication to be an Entra identity authorized by RBAC. Static Web Apps can't meet that rule. Research in September 2026 found:

- **Every supported deploy path uses the site's deployment token,** a shared secret. That covers the GitHub action, the SWA CLI (which fetches the token through ARM) and `az staticwebapp`, which has no deploy command at all.
- **The token can't be turned off,** only rotated.
- **Neither Entra-only path works today.**
  - The ARM `staticSites/zipdeploy` action is undocumented, and the only public report of using it got `NotImplemented` (Azure/azure-rest-api-specs#22267).
  - The deploy client's Azure-access-token mode is also undocumented, and it still requires a token-shaped string.
- **The documented "GitHub" deployment policy trusts a GitHub identity, not Entra.** It also needs the site linked to the repository with a personal access token.

Building the site into the API image was also meant to end the Static Web Apps config failures: until now, rules were only fully checked by Azure at deploy time.

## Decision

The API app (`ca-usnm-{env}`) serves the built SPA from its own image. `USNM_SITE_DIR` points at `web/dist`, and the image build writes brotli and gzip copies of each text file.

- **One origin.**
  - The site calls `/v1` on its own origin, so it needs no CORS and no preflights.
  - The app answers on `usnewsmap.com`, `www.usnewsmap.com` and `api.usnewsmap.com`, each with a free managed certificate.
  - The apex is an A record to the environment's static IP, validated over HTTP. `www` and `api` are CNAMEs.
- **Routing and headers are code.**
  - Paths with no file behind them (app routes) get `index.html`. The app's pages (`/` and `/status`) are 200. Other paths get the same shell with a 404, and the app shows a not-found page. Missing files are 404.
  - Requests for `www.usnewsmap.com` get a 301 to `https://usnewsmap.com` with the same path and query (`USNM_SITE_HOST`).
  - Search permalinks (`/?q=…`) and `/status` send `X-Robots-Tag: noindex`. `robots.txt`, `sitemap.xml` and the IndexNow key file are static files in `web/public`.
  - Unknown `/v1` paths stay RFC 9457 problems.
  - Hashed assets are cached for a year, and `index.html` is revalidated on each load.
  - The security headers (CSP and the others) are set on every response.
  - All of this is tested in Rust, by Playwright against the API serving the build, and by a smoke test of the built image in CI.
- **One release.** The site ships in the same image as the API and rolls out in the same revision. Deploys are the existing Entra-only path: an OIDC sign-in, a push to the private registry, then `az containerapp update`.
- **No Static Web App.** Bootstrap deletes the old one, so no deployment token exists anywhere.

## Alternatives

| Option                                                     | Why not                                                                                                               |
| ---------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| Keep Static Web Apps, rotate the token after every deploy  | Still a shared secret, and concurrent deploys race the rotation                                                       |
| Static Web Apps `zipdeploy` or the Azure-access-token mode | Undocumented, reported not to work, and unsupported                                                                   |
| A separate small static-file app in the same environment   | Separate releases and availability, but about $4–14/month with a warm replica, or a cold start when it scales to zero |
| Blob static website plus Front Door Standard               | Entra-only uploads, but about $35–40/month (half the budget), and SPA rewrites need rules-engine configuration        |
| App Service B1                                             | About $13/month, and another plan to run                                                                              |

## Consequences

- **The site shares the API's availability.** A broken revision or replica takes both down. The site is of little use without the API, and a revision rollback restores both at once.
- **No global edge.** Everything is served from East US 2. Hashed assets are cached by browsers indefinitely, so mostly first visits from far away are slower. Front Door can go in front later without redesign, which is also the growth-profile answer to press spikes.
- **Static traffic uses the API's replica** (0.25 vCPU; max 2 replicas). Precompressed files keep that cheap. Page and asset requests count toward the Container Apps free 2M requests a month.
- **A site-only change rebuilds and rolls the image.** That takes a few minutes and has no downtime.
- **The site and the API can never be on mismatched versions.**
- **Static Web Apps' 100 GB/month bandwidth cap** and its config quirks no longer apply. Egress is ordinary Azure bandwidth (the first 100 GB/month are free).
