# English Gold trust calibration on issue-disjoint BLN600 excerpts

**Status:** promoted on 17 October 2026 as a v4 `eng/eng` trust list for worker `247c`; trust format v5 has since replaced v4 for publication (see the [gold check](../boinc/server/gold-check.md#host-trust-for-usnm_ocr)) · **Date:** 17 October 2026 · **Measured on:** production worker `sha256:247c131a…`, Tesseract 5.5.0, PSM 3, 300 DPI; offline ARM runs and canonical BOINC results on hosts 1 (Apple Silicon) and 8 (Linux x86_64) · **Code:** `usnm-boinc gold trust`, `gold check`, `gold collect`; `crates/usnm-boinc` trust format v4 · **Issues:** #527, #751, #755, #761, #756

## The question

Gold reports could only gate `usnm_ocr` publication once a CER limit was calibrated on material the worker had not been tuned on, and once that limit was shown to refuse faulty output. The old 142-crop bundle could not serve as fresh test material. The question was whether a limit set on one set of unused BLN600 excerpts would accept the real worker and refuse lost-crop controls on a sealed second set, and then whether canonical BOINC hosts reproduce the offline counts exactly.

## What we did

BLN600's publication/date metadata excluded all 28 issues represented in the old 142-crop benchmark. An outcome-blind hash ordering then took one excerpt per unused issue: 30 for calibration and the next 30 for a sealed test. These are newspaper excerpts, not independent original pages, and may have been in model training. The experiment is English BLN only; it says nothing about other languages or American newspapers. Selection SHA-256 `204b2d1b468eea53cdab3b9fadd289b90e6cdc55454da88f046efcf44b8a0054`; reference-only bundle manifests: calibration `d8cd7279090a6427c0cc6f0e82573e3771c27f90df6500bf5a44b268f6e0f05f`, heldout `2bc0e24ebdf20adfb5577eab8634279391ba135f3ffd066d0a6cc4a5307056ea`. The original archives and licences stayed unchanged.

Runs used worker `sha256:247c131a599403645c4c96782c0556dc27f27a5d991c764b4b9565182a8c71aa` on ARM base `sha256:7bc7b78658562430ec95a1b1048c2f689c26f8d2443d6ed0c5e981e311cbab93`. Fault controls ran the real engine but returned empty output for 1, 2 or 4 hash-ranked crops; report scores were not edited. Every run scored all 30 crops with zero skips, and an external unmodified control matched the native integer character and word counts on every crop in both partitions.

### First candidate: aggregate CER only

Before opening the heldout, candidate `9cfe342402d4fe5c3facd09e83fc048555d093f6f3f6a6333e7331abe73fc0f2` set the aggregate limit at the midpoint between the calibration positive and the nearest fault, `14463/165588 = 0.08734328574534386`. Both positives had to pass and every fault fail, with no retuning on the heldout.

### Revised candidate: plus a complete-loss guard

Trust format v4 binds one calibrated manifest and also requires complete scoring with no skips and no crop whose character edits reach its reference length. The collector derives the counts-only `complete_loss_crops` field, so older tables cannot satisfy the policy. The guard catches empty-output controls; it does not prove resilience to partial omissions, wrong-language models or other faults.

The revised candidate kept the numerical limit and froze this guard before OCR on the next 30 outcome-blind issues, disjoint from the baseline, calibration and spent heldout. Selection SHA-256 `11183a39b1b64616745e6e2bcb8adb86a757649704fd5d85a69cc91465ab9c3f`, manifest `70600f006cdfce3cfba273028fc7a3f52bb30df3e7934b9ccde6e8f5265f468c`, frozen candidate `ff12ee4cc80d94f7812876cda2c4953f380f76cb529dca20b7488e64e7279023`.

The shared handoff trust file can't be isolated by passing a pilot-only path, so v4 also records source language `eng`, model profile `eng`, PSM 3 and 300 DPI, and publication and spot checks require the job to match that scope. The collector records actual profiles and configuration and the trust build checks them; the language scope rests on the operator's pinned-manifest evidence.

### Canonical BOINC runs

The image-only rollout of #755/#761 reached production revision 55. Signed Gold 1.4 publication then succeeded for Linux (`usnm-boinc-publish-caj-prod-uucz4sm`, app 39) and Apple Silicon (`usnm-boinc-publish-caj-prod-p57pptb`, app 40), both version 104 / `docker`, with Dockerfiles pinning the `247c` worker. All six registered public download URLs returned anonymous HTTP 200 with SHA-256 equal to the signed payloads. Gold publication does not update `usnm_ocr`.

The registry also records the `247c` image's AMD64 base, `sha256:e088ef6f8004aa66091ee4bdaa06a42e5061a42dfd505db31c5d7257b747d065`, beside the ARM one; the earlier `80bf` mapping stays. Before this, on revision 54, the targeted path dispatched compatible Apple Silicon WU 16734 / result 32114 on app 38; normal `gold check` and collection accepted all 142 crops with zero skips (report SHA-256 `d2c0781be7863cf3c2170bed03c3abab50770192d279ce4a524a2823a7d582a5`). That validated a live dispatch on `80bf`, not a `247c` measurement.

One normal targeted challenge per existing user-2 host used the unchanged 30-crop calibration manifest. At `2026-10-17T07:19:56Z`, under the production mutex, the per-job idle preflight and the controller lease, the native receipts and the old policy were rechecked and the trust list promoted. `/project/gold/current247c-scope-eng-20261017/` keeps the before-policy snapshot, the candidate and the promotion receipt.

## What we measured

| Partition   | Native / unmodified edits | One lost crop | Two lost crops | Four lost crops | Reference characters |
| ----------- | ------------------------: | ------------: | -------------: | --------------: | -------------------: |
| Calibration |                     5,929 |         8,534 |         10,874 |          14,491 |               82,794 |
| Heldout     |                     4,719 |         7,084 |          7,993 |          14,361 |               85,685 |

**The first candidate failed:** the heldout lost-one-crop CER, `7084/85685 = 0.08267491392892572`, passed when it should have failed. The limit was not retuned on that spent partition.

**The revised candidate passed its preregistered criteria** on the fresh 30 issues: both real-engine positives matched every integer count and passed with 6,255 edits over 87,111 reference characters; the lost-one, two and four controls had 9,099, 11,339 and 17,172 edits and 1, 2 and 4 complete-loss crops, and all failed. Every run scored 30 crops with zero skips.

Canonical results on the calibration manifest:

| Platform      | Host | Workunit | Canonical result | App version | Canonical report SHA-256                                           |
| ------------- | ---: | -------: | ---------------: | ----------: | ------------------------------------------------------------------ |
| Apple Silicon |    1 |    16736 |            32117 |          40 | `082649110586b90dab6ab717ee9aea60914f11205cfbfdd7f8819aaa47bab2f4` |
| Linux x86_64  |    8 |    16737 |            32118 |          39 | `d9b67382e7a4127fd1ffa046d4792d688f1b32b1705903b47ca7ea56556c7a4e` |

Both have outcome 1, validation 1 and exit 0, with assimilation 2 and error mask 0. `gold check` and `gold collect` accepted both. All 30 crops have zero skips and no complete loss, and every source, profile and integer count matches the offline ARM baseline: 5,929 edits over 82,794 reference characters. A local AMD64 emulation matched too (report SHA-256 `c17ff88f1051623b5f71879981b07b8d8a900e08771f751b8536522a1c1baf41`), but does not replace the Linux BOINC result.

The promoted `/project/gold/trust.json` had SHA-256 `3bfc19016cf38b47455faa4ddbe8fbbe27577c410c1d8e74a2ccdd8a0a6a651b`: v4, worker `247c` only, hosts 1 and 8, calibration manifest `d8cd7279…`, language and profile eng/eng, PSM 3, 300 DPI, limit `0.08734328574534386`. It authorizes no other worker, language, profile or configuration. The [native promotion receipt](https://github.com/tgoodyear/usnewsmap.com/issues/527#issuecomment-6231138896) on #527 records the same.

## What is left

- This is an English BLN fixed-worker pilot. It does not show performance on the American corpus, other languages, error bounds, availability or malicious hosts, and gives no statistical guarantee.
- Fresh-page OCR, handoff, release and search acceptance were separate gates. No queue-admission exception, client preference, task abort or priority change was used.
- Attribution of terminal attempts without a valid report went to [#756](https://github.com/tgoodyear/usnewsmap.com/issues/756), since closed; the revocation rule is in the [gold check](../boinc/server/gold-check.md#host-trust-for-usnm_ocr).
- The German counterpart is in [the 18 October diagnostic](2026-10-18-german-fraktur-diagnostic.md).
