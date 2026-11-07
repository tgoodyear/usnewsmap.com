# BOINC GPU release follow-through

On 26 October 2026 UTC, the maintainer authorized the releases below with the
signing dataset already unlocked. Coordination and release receipts are on
[#529](https://github.com/tgoodyear/usnewsmap.com/issues/529#issuecomment-6406898151).
Publication is not volunteer hardware or scientific acceptance.

| App          | Version    | Platform / plan                      | Registered app-version ID |
| ------------ | ---------- | ------------------------------------ | ------------------------- |
| `usnm_surya` | 1.6 / 106  | Linux x86-64 / `docker_intel_vulkan` | 60                        |
| `usnm_qwen`  | 1.9 / 109  | Linux x86-64 / `amd_vulkan_8b`       | 61                        |
| `usnm_qwen`  | 1.10 / 110 | Windows x64 / `amd_vulkan_8b`        | 62                        |

## Inputs and verification

The native binaries and AMD PCI helpers came from reviewed main `88593bab`,
[native workflow 40259318615](https://github.com/tgoodyear/usnewsmap.com/actions/runs/40259318615).
All five target builds/tests passed. Both downloaded artifact commit identities
and all artifact checksums matched. Vulkan archives matched the committed
`boinc/apps/qwen/llama.lock` b8799 hashes; Windows app-local CRT files came from
the same CI artifact with its verified Microsoft-signature manifest.
These versions include #903's bounded failure diagnostics and #908's Windows
embedded-lock/PCI-refusal fixes, absent from the earlier AMD release source.

Surya reused the already published and CPU-smoke-tested Vulkan image from
reviewed main `614756c`, not a new engine:
`ghcr.io/tgoodyear/usnm-boinc-worker@sha256:e908ebe32270ec0bd6ae479478e0bcf5be97ff26b80d251d1afc2945b9e1259e`.
Its successful build, CPU inference and publication are in
[run 39767585113](https://github.com/tgoodyear/usnewsmap.com/actions/runs/39767585113).

All three used `boinc-app-release.sh --onprem-host --onprem-ct`: isolated
network-disabled signing with the private key mounted read-only only on the
signing host, signature-verifying app-version builds and the standard scoped
on-prem publisher. No signing material was copied out. Registration and public
project health checks passed. An independent anonymous HTTPS read of all
**72 new payloads** matched each published file's SHA-256.

The resulting local app-version image IDs were:

| Release   | Image ID                                                                  |
| --------- | ------------------------------------------------------------------------- |
| Surya 1.6 | `sha256:1c08d23f97c05f479a8d7db42474cebccf2272274646a2523320604a8a5e3bbe` |
| Qwen 1.9  | `sha256:b9cf20e3af012b8ad35b0a81c158f415612a73a0f362c43104bd2f0b485ee9ef` |
| Qwen 1.10 | `sha256:4c4fd263517cc5d5b0068de72c3efb91caf19b4be041b07a884779ca10379f31` |

## Preserved state and remaining gates

Every previously inventoried Qwen/Surya app-version row, including deprecation
state, remained unchanged. Before/after hashes of `server.env`, live scheduler
configuration and plan classes matched; the server stayed active and the
publication lock was removed normally. No server restart, host pause, client
preference change, forced RPC, new workunit or validation-policy change occurred.
Old signed versions remain intact.

Surya's Intel class remains closed (`min_wu_id=2`, `max_wu_id=1`). Its Vulkan
engine identity differs from the current CUDA campaign, so publication must not
open unrestricted dispatch into that queue. #842 still needs a separately
coordinated, correctly pinned bounded Xe probe and actual offload, memory,
timing and output evidence.

AMD's existing automatic admission is unchanged. #885 still needs actual
RX 9070 XT startup/inference and independent validation evidence; publishing
diagnostics does not prove the prior failures are fixed. Collect the next
naturally returned failure diagnostics before another speculative code change.

The operator-only fixes #1169, #1172 and #1173 do not require signed volunteer
app releases; their controller/converter deployment is separate. The retired
direct OCR adapter fixed in #1168 is not a reason to re-release BOINC.
