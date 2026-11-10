# First current-worker OCR pages through BOINC

**Status:** record; superseded by the queue-fed controller ([batch controller](../boinc/server/batch-scheduler.md)) · **Date:** October 2026 · **Measured on:** the Azure-hosted prod BOINC server, its publish and hand-off executions, and the `ocr-local` import records, 17 to 19 October 2026 · **Code:** `boinc/scripts/batch_scheduler.py`, `crates/usnm-boinc` · **Issues:** #762, #866

These are the records of the first `usnm_ocr` 1.5 publication on the current worker (`247c`), the first fresh page a Mac client computed and the first `ocr-priority` page through the controller. They were moved here from the BOINC server pages on 10 November 2026.

## Current-worker OCR 1.5 publication

The native app inventory found that Linux OCR 1.3/app 20 and Mac OCR 1.4/apps
23 and 24 still pin worker `80bf`, whereas current export and scoped trust
name worker `247c131a599403645c4c96782c0556dc27f27a5d991c764b4b9565182a8c71aa`.
Publishing current-worker Gold does not update these OCR payloads. Do not
export a current-worker pilot and assume its signed OCR app matches, or
weaken the worker's identity check.

The normal prepare-only signing workflow produced immutable `usnm_ocr` 1.5
siblings with all three payload signatures verified for each platform.
Publisher repository: `crusnmprodffapzy.azurecr.io/usnm-boinc-appver`.

| Platform              | ACR build | Publisher digest                                                          | Succeeded execution suffix | Native app version |
| --------------------- | --------- | ------------------------------------------------------------------------- | -------------------------- | -----------------: |
| `x86_64-pc-linux-gnu` | `ch7a`    | `sha256:b9bf07591a265cb7b9a260221bcfd719b320cd96e204cc78ec8d7196eec70dd2` | `9rbj04a`                  |                 41 |
| `arm64-apple-darwin`  | `ch7b`    | `sha256:ccdf4a46376776ede3621be447b15e518e15e5594bdf99ad97bcd56feb07c8d2` | `sia7vgx`                  |                 42 |
| `x86_64-apple-darwin` | `ch7c`    | `sha256:74a718918a031125e5103125a1142a35533fc73efe91b525cb8899bca96ee626` | `n7hkbcb`                  |                 43 |

All pin the exact current `247c` worker; existing signed versions remain
unchanged. The desired `images.appver` pin names the final Intel sibling;
it is not a multi-platform publisher. After exact-head Copilot GREEN review,
zero threads, all CI and merge of #762, Linux, ARM and Intel were published
separately and serially. Each execution has prefix
`usnm-boinc-publish-caj-prod-` plus the suffix above and terminated Succeeded
before the next started. Normal production mutex, per-job idle preflight
(then all-six-idle; [#856](../boinc/server/azure.md#coordination-boundaries) later replaced it),
infinite controller lease and expected-previous-image guards protected each
image-only update. Exact comparisons preserved every other template,
configuration and identity property; no broad stack provision was run.

Independent checks verified all three unique active version-105 / `docker`
registrations, exact `247c` Dockerfiles and three payload/signature pairs per
platform. All nine native download URLs returned anonymous HTTP 200 and
byte-identical SHA-256 hashes. Scoped global trust stayed unchanged.
Publication and registration are **not actual OCR-client acceptance**. The
[current-worker native Gold evidence and English-only scoped promotion](../ocr/gold-set.md#current-worker-native-canonical-evidence-and-scoped-promotion)
are separate from fresh-page OCR execution, native handoff, release/search and
Intel-client acceptance. No allocation, continuous-feed activation or
backlog-policy exception is authorized by this publisher pin.

## Fresh-page Mac computation, collection and import

The normal one-page controller admitted
`sn82014782/1789-04-04/ed-2/seq-1` from base batch `kyu_frenchie`
(LoC version `kyu_frenchie_ver02`) under plan
`boinc-mac-current247c-20261017`. Four earlier ticks correctly held at
617 pending workunits against the normal target of 600. The existing managed
client had 59 successful completed results waiting to report; an
identity-guarded ordinary project update reported them, reducing pending
work to 558. The unchanged allocation/configuration and persisted controller
state then admitted exactly one page. No cap exception, task cancellation,
reprioritization, account change or client-preference change was used.

The existing Apple Silicon client fetched the page through ordinary scheduling; this was not
a targeted Gold challenge or a relabeled Linux result. Native evidence:

| Field                        | Accepted value                                                     |
| ---------------------------- | ------------------------------------------------------------------ |
| Job                          | `cae8426495087616cdf46bcadbf648406c96c1e24561b81aa99f6d7fcee6b232` |
| Workunit / canonical result  | `16738` / `32119`                                                  |
| Canonical host / app version | `1` / `42` (Apple Silicon OCR 1.5)                                 |
| Outcome / validation / exit  | `1` / `1` / `0`                                                    |
| Assimilation / error mask    | `2` / `0`                                                          |
| Canonical report SHA-256     | `d5ac93f0be4e05258affaddecb642b01e38f4caf31f4fb40dde40e0168f51b6e` |
| Input scan SHA-256           | `64c0a1122e17ced977e1540536acdd88a8fbdeff268192fb634f832de729d958` |
| Output text SHA-256          | `92d54913897d94a50dda409f1610f9c291e88dfe0a557e482ba7e2c88e4d52de` |

The canonical report names current worker `247c`, Tesseract 5.5.0 and
profile `eng` / PSM 3 / DPI 300. Global scoped trust stayed at SHA-256
`3bfc19016cf38b47455faa4ddbe8fbbe27577c410c1d8e74a2ccdd8a0a6a651b`.
Normal controller collection then completed through `spool-out` and
trust-checked handoff execution `usnm-boinc-handoff-caj-prod-71j1xrr`
(Succeeded). The durable dispatch record is `published`; the one-page
controller reports selected 1 / published 1 / collection backlog 0.
This marker means published to `ocr-out`, not to public search.

Normal pickup execution `usnm-ocrs-caj-prod-ye8ldw4` also Succeeded.
Its durable import record
`ocr-local/import/20261017T091059970936Z-2a10320b-0000.json`
records the exact BOINC page part and ALTO as `imported`. The merged part is
`ocr-local/pages/sn82014782/20261017T091059970936Z-2a10320b-0000.parquet`,
SHA-256 `ed7f2968107ced85fec4031c7d327bcf26413c3be9680bb94d4ae702a6e0e86e`.
Its one row names the expected document, node `boinc-cae842649508`,
worker `247c`, `route=boinc@r1` and 8,669 normalized characters. The
normalized text SHA-256 is
`0747212670a1df26014df567c1226180eaee373da196775990aeb55ba99c8d4b`;
the collector applies `normalize_ocr` after validating the raw canonical
text hash, so this is not the raw worker text hash above.

Stored metric-v2 scores detect English for both readings: our damage rate
is `0.20512820512820512`, versus LoC's `0.2841726618705036`, with head-word
counts 341 versus 199. These stored scores clear rule v1's 0.05 margin and
head-count test, but are an estimate: a real release scores the texts again,
checks title languages, revocations and competing candidates, and records
its actual selection. The already-running `pages-v20261017-1` rebuild
started before this import; do not assume its frozen local-OCR inputs
include the new part or restart its writer to force inclusion.

This proves fresh-page Mac computation, native canonical assimilation,
trust-checked handoff and native import. Actual release selection,
new-version search/source credits and Intel-client execution remain open.
Retain the same controller state and journals; never manufacture another
submission or reset the page to fresh.

## The first priority page through the controller (19 October 2026)

On 19 October 2026 page `sn99066040_1865-08-12_ed-1_seq-2` (plan `priority-feed-1`, job `a9ba35b339d479442d0efcfeefa9bbab2f31ab4989b88db569cd3c0c0f0761bb`) went from `ocr-enqueue --reason user` (04:36:52 UTC) to `imported` (06:28:01): claimed 05:10:14, returned by host 10 05:18:33, published 06:25:06. Collection first held it for missing exact-image gold; one targeted challenge (CER 0.0716 against 0.08734328574534386) put hosts 1, 8 and 10 in the v5 trust list with no override. Calibration and a throttling wait dominated; it isn't a latency guarantee. Bounded scanning promises neither ownership nor a fixed tick count, and admission doesn't make the returning host calibrated, so keep gold onboarding and fail-closed quarantine.
