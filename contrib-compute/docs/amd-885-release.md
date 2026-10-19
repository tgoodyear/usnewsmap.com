# AMD #885 scoped production release

PR #889 merged at `5e12de6d7a12b86314959005c4c73c055c0f461c`;
native workflow `39188557570`, full CI `39188533783` and CodeQL
`39188533710` passed. The signed Linux x64 and Windows x64 payloads use
that exact native source and the unchanged b8799 archives from `llama.lock`.
The PCI helper, ggml registry/base/Vulkan libraries, SONAME aliases on Linux,
and Microsoft CRT on Windows are part of the signed bundles.

The production runtime at release preparation is `tls464-rb3`, immutable bridge
digest `4cd42232b18d9a7b6317da4be490d50ca569de82c6ca6b935784fe173c896201`.
Main's server/web pins do not describe that runtime, and a whole-stack provision
would import unrelated changes. `images/server/amd-885.Containerfile` therefore
uses that **exact live base**, retaining its scheduler, collector, TLS/config
rendering, credentials, validators and daemon configuration. It adds only:

- the reviewed AMD class and admission-window-preserving policy refresh;
- the reviewed pinned-BOINC `create_work --no_new_results` binary and a startup
  refresh of that one project tool;
- the reviewed server acceptance adapter and cohort helper, installed in
  `/usr/local/lib/usnm-boinc/amd-885/`.

The staging binary is copied from the normal newly built BOINC base, not from a
stale overlay. Build-time dependency/help checks run against the live runtime.
The previous entrypoint remains byte-for-byte intact and is invoked after the
tool refresh. No other binary or startup configuration is replaced.

Production publication uses the normal signed app-version image and
`APPVER_REUSE_PROJECT=1`: existing live credentials/config symlinks are retained;
registration is isolated to this app/platform/version. Publisher workspaces
are private UUID directories on the project filesystem, not system temporary
directories. The publish job's full existing template is preserved except its
image and the scoped-reuse variable. Only that job is gated idle under its start
lease; other running jobs and their **entire templates** are untouched.

All live writes require the #529 claim, authoritative
`boinc-handoff/scheduler/controller.lock` and normal per-job guard for the publish
template/start. The supported local mutex setting is explicitly
`USNM_BOINC_LOCK_DIR=/Users/tgoodyear/code/.usnm-boinc-prod.lock`, already used by
the #860 transaction; no system temporary path or lease takeover is used.

The maintainer authorized preserving MySQL `require_secure_transport=OFF`
**as found**; this release neither resumes #464 nor toggles enforcement.
An actual verified encrypted read-only DB connection must pass independently.
AMD remains **closed `2/1`**. Existing Intel/NVIDIA Vulkan `17099–17100` and
every other class bound must survive unchanged. No workunit/result changes,
volunteer attachment/preferences, dispatch or hardware acceptance are part of
this publication. Initial real-card qualification remains pending under #885;
steady-state compatible-host automatic scheduling is a separate admission/supply
follow-up, not a requirement for manual cohorts per volunteer.

Final image digests, app-version IDs and public hash verification are recorded
in the #885/#529 deployment receipts. Preparation is not publication.

## Immutable publication candidates

All images are in `crusnmprodffapzy.azurecr.io`. Version **1.7** is reserved for
both AMD sibling platforms (the fresh live inventory ended at 1.6).

| Image repository / purpose                  | SHA256 digest                                                      | ACR build |
| ------------------------------------------- | ------------------------------------------------------------------ | --------- |
| `usnm-boinc-base`, new pinned staging tool  | `7526fa79c6259c4d12d3c057da4d3bcff24138f1ce44246de800786e218265a0` | `cha1`    |
| `usnm-boinc-appver`, signed Linux x64 1.7   | `a9f27d7307d05b68fa368105d1656e8dab6ddb3bb8ae027173012e4a80621647` | `cha2`    |
| `usnm-boinc-appver`, signed Windows x64 1.7 | `8659f1444a416f333336d7885d26fc8a2dd6b6184fd98f7cb244a1c84e51e0f3` | `cha4`    |
| `usnm-boinc-bridge`, additive runtime       | `067d640d8dce040ca226159beff64599dca2d8b9ea6f44805f65a501656876d5` | `cha5`    |

The runtime overlay source is `bb6a06103e34d52d81efdc5dbf57d24398268983`.
Actual-image verification compared **5,660 files** under `/etc` and `/usr/local`
against the immutable live base (excluding only container-generated hostname,
hosts and resolver files). Only the four intended replacements differ:
`create_work`, the entrypoint adapter, plan template and policy refresh.
The two operator adapters, dependency-check receipt and byte-identical saved
entrypoint are the only added files. Docker runtime environment, entrypoint,
command, user, working directory, volumes and exposed ports are unchanged.
The actual image's policy merger preserves all 13 existing classes exactly
while adding AMD closed `2/1`. Its real startup refresh passes first/repeat
serve checks, retains non-serve delegation, and exposes `--no_new_results`;
all dynamic dependencies resolve in that exact runtime.

The server pin is reconciled to the **unchanged live init/secrets image**
`3b6d73b4e56dfe9edd2290432a10d8e251234972bce49e2a366992bbd6728251`,
not deployed to another image. The priority bridge and worker pins are unchanged.
Only the web runtime and publish-job images are intended to change. Busy
quality/gold templates retain their entire existing state; these differing
per-job live images are not permission for a broad stack provision.
