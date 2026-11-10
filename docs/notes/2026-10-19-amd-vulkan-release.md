# First AMD Vulkan release of usnm_qwen (#885)

**Status:** published; superseded by the [26 October GPU release](2026-10-26-boinc-gpu-release.md) (1.9 and 1.10) · **Date:** 19 October 2026 · **Measured on:** PR #889 at `5e12de6d7a12b86314959005c4c73c055c0f461c`; native workflow `39188557570`, CI `39188533783`, CodeQL `39188533710`; the live runtime `tls464-rb3` · **Code:** `boinc/images/server/amd-885.Containerfile`, `boinc/images/server/amd-885-entrypoint.sh`, `boinc/scripts/native-qwen-cohort.php`, `boinc/scripts/acceptance_server.py`; [AMD GPUs](../boinc/apps/amd-gpu.md) · **Issues:** #885, #529, #889, #894

## The question

How to publish signed `usnm_qwen` versions for `amd_vulkan_8b` (Linux x64 1.7, Windows x64 1.8) and give AMD hosts general admission, without a whole-stack provision. The production server then ran on Azure, and main's server and web pins no longer described the running runtime, so a provision would have imported unrelated changes.

## What we did

The signed Linux and Windows payloads were built from PR #889's native source and the unchanged b8799 archives in `llama.lock`. The bundles include the PCI helper, the ggml registry, base and Vulkan libraries, the Linux SONAME aliases and, on Windows, the Microsoft CRT.

`boinc/images/server/amd-885.Containerfile` builds on the exact live runtime (`tls464-rb3`, bridge digest `4cd42232b18d9a7b6317da4be490d50ca569de82c6ca6b935784fe173c896201`), keeping its scheduler, collector, TLS and config rendering, credentials, validators and daemon configuration. It adds only:

- the AMD plan class and a policy refresh that keeps existing admission windows;
- the pinned-BOINC `create_work --no_new_results` binary, copied from a newly built BOINC base, and a startup refresh of that one tool;
- the server acceptance adapter and cohort helper, in `/usr/local/lib/usnm-boinc/amd-885/`.

The previous entrypoint is kept byte for byte and runs after the tool refresh.

Publication used the normal signed app-version image with `APPVER_REUSE_PROJECT=1`, which keeps the live credentials and config symlinks and registers only this app, platform and version (the setting in `boinc/infra/boinc/main.bicep` dates from this release). Executed PHP registration files lived only in private directories under the `$SECRETS` tmpfs (`/dev/shm/boinc`), never on the Azure Files share, whose fixed `0775` modes can't enforce `0700`. Only the publish job's image and that one variable changed, and only that job was held idle under its start lease. Live writes needed the #529 claim, `boinc-handoff/scheduler/controller.lock` and the per-job guard, with `USNM_BOINC_LOCK_DIR` as the lock directory.

MySQL `require_secure_transport=OFF` was kept as found, with the maintainer's approval; this release didn't resume #464.

The maintainer then approved general AMD admission before hardware qualification. After publication, `native-qwen-cohort.php --plan-class amd_vulkan_8b --general --apply` removed the AMD workunit bounds and wrote the marker `usnm-amd-general-admission-v1`. Every other class's live bounds were kept. The preparation snapshot showed Intel and NVIDIA open at `17099` to `17100`, but #860 had closed that pilot at 03:08 UTC, and the newer closure was kept. No workunit, result, volunteer preference or attachment was changed.

## What we measured

All images are in `crusnmprodffapzy.azurecr.io`. The live inventory ended at 1.6, so 1.7 went to Linux and 1.8 to Windows; distinct numbers keep the diagnostic adapter's app and version routing unambiguous. Earlier Windows candidates with the same number were never published.

| Image repository and purpose                | SHA-256 digest                                                     | ACR build |
| ------------------------------------------- | ------------------------------------------------------------------ | --------- |
| `usnm-boinc-base`, new pinned staging tool  | `7526fa79c6259c4d12d3c057da4d3bcff24138f1ce44246de800786e218265a0` | `cha1`    |
| `usnm-boinc-appver`, signed Linux x64 1.7   | `57b0de0793df18a15701103ce2b8061214cacc49c5c496ff5ae2f6240a9f5303` | `cha7`    |
| `usnm-boinc-appver`, signed Windows x64 1.8 | `495f9c269291f13ec99439c83e6fd094f3c9cf343bcb42f4972130256693f02d` | `chaa`    |
| `usnm-boinc-bridge`, additive runtime       | `d417a2ad0469c2b0c54787a0e466d6e76cb78577e40a1e776870bef252b4da9e` | `cha6`    |

The runtime overlay source is `2cb09db4e8a8f77c3749ca7d41249eba4ecc2249`. Comparing 5,660 files under `/etc` and `/usr/local` with the live base (less the container-generated hostname, hosts and resolver files), only the four intended files differ: `create_work`, the entrypoint adapter, the plan template and the policy refresh. The two operator adapters, a dependency-check receipt and the saved entrypoint are the only added files. The runtime environment, entrypoint, command, user, working directory, volumes and ports are unchanged. The policy merger kept all 13 existing classes exactly and added AMD closed (`min_wu_id=2`, `max_wu_id=1`). The startup refresh passed its first and repeat serve checks and exposes `--no_new_results`, and every dynamic dependency resolves in that runtime.

The server pin was reconciled to the unchanged live init and secrets image `3b6d73b4e56dfe9edd2290432a10d8e251234972bce49e2a366992bbd6728251`. The priority bridge and worker pins were unchanged; only the web runtime and publish-job images changed. Final image digests, app-version IDs and public hash checks are in the #885 and #529 deployment receipts.

This measures publication only. No AMD card had run the release.

## What is left

Hardware acceptance on a real RX 9070 XT, tracked in #885. The [26 October release](2026-10-26-boinc-gpu-release.md) replaced these versions with 1.9 and 1.10, and the first validated AMD result came on 30 October on Windows 1.14 ([AMD GPUs](../boinc/apps/amd-gpu.md)).
