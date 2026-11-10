# Gold bundle v1 on the 80bf worker: offline, Linux and Apple Silicon measurements

**Status:** measured; no trust threshold or host promotion came from it. The English calibration that followed is [the 17 October note](2026-10-17-gold-english-trust-calibration.md) · **Date:** October 2026 · **Measured on:** worker `80bf2d6586fb3a9f6ef644937ddd40ca2304b1b433d3b9e657e193ba6f573ae0`, the locked bundle v1 (tar `42520de4…`, manifest `150e20d7…`); offline AMD64 and ARM64 Docker runs, then canonical BOINC results on hosts 8 and 1 · **Code:** `usnm-ocr-survey bundle-build`, `usnm-boinc gold prepare`, `gold collect`; `boinc/project/gold-engine-platforms.json` · **Issues:** #527, #526, #741, #747

## The question

Every canonical Gold report in production on 16 October 2026 (18 of them, host 8's newest included) carried `boinc.engine_image` equal to the historical `9c8eac04…`. Registered Linux Gold 1.0, 1.1 and 1.2 all referenced a `Dockerfile_1` that still pinned that old worker, and updating campaign metadata or `images.worker` does not update an already signed app. Before calibrating trust on the current worker, we needed to know what it scores on the bundle, whether the score is the same on both architectures, and whether real BOINC hosts reproduce it.

## What we did

**Offline, 16 October.** The published `80bf` worker was run on an AMD64 Linux host and on ARM64 Linux Docker, from the locked v1 tar and manifest, with all 19 cached originals checked by SHA-256 and copied to isolated directories. One CPU, 1.5 GiB, no network, PSM 3, 300 DPI, 300 s per crop. Tesseract bases: AMD64 `d0b841b521a4a68aab2d585a019d9ad1d011e25a65f2f9286d6851fcc64ded57`, ARM `86418d991a798c885a7a505bc94ce27cbfe0c5361fa3dde319b7d6b400d9ebad`. A holdout split was fixed in the issue before reading baseline scores: group by `remote|member-or-path` (`bundled|path` for bundled images), and odd SHA-256 parity of `usnm-current-20261016|<group>` selects holdout.

**Gold 1.3 on Linux, 17 October.** Gold 1.3 was published in execution `usnm-boinc-publish-caj-prod-nbtft5q`, registering Linux `docker` version 103 as app_version 28, pinned to `80bf`. At the time, whether the NonCommercial terms allowed the full v1 bundle was still being checked, so a separate 52-crop CC BY 4.0 bundle was built from the converted NewsEye validation inputs (BnF 24, Finnish 18, Swedish 10), with no BLN600, Overproof or NZZ content and no remote inputs:

```sh
usnm-ocr-survey bundle-build \
  --source newseye-bnf=/project/gold/build-v1/out/newseye-bnf \
  --source newseye-nlf-fi=/project/gold/build-v1/out/newseye-nlf-fi \
  --source newseye-nlf-sv=/project/gold/build-v1/out/newseye-nlf-sv \
  --version 2 --seed 457 --per-source 30 \
  --out /project/gold/ccby-newseye-20261016-80bf/build \
  --lock /project/gold/ccby-newseye-20261016-80bf/bundle-v2.lock.json
```

This one-off `--version 2` bundle lives only on the share. It is not the bundle v2 whose lock was committed on 26 October 2026 as `fixtures/ocr-gold/bundle-v2.lock.json` (all six sources, corrected Overproof geometry; see the [gold check](../boinc/server/gold-check.md#build-the-gold-bundle)).

`bundle-verify` and `gold prepare` passed. With all six lifecycle jobs idle, one targeted Gold job went under the controller lease to host 8 / user 2: WU 16571 / result 31526 / app_version 28, job `ae4e2cd9e6e86201e368819be699fcfc7c7826ec6cfe4a3d4554c9b2729564e9`. The collector's reports and crop tables are under `/project/gold/ccby-newseye-20261016-80bf/collected`; the report is in the assimilator's `results/0/` spool as `usnm_gold_<job>__file_gold-result.json`.

**Gold 1.3 on Apple Silicon, 17 October.** ACR build `ch6r` prepared the signed `arm64-apple-darwin` / `docker` sibling, pinned to the same `80bf` worker (publisher image `crusnmprodffapzy.azurecr.io/usnm-boinc-appver@sha256:89a345d9a6f765cae467448438ee61ba70066562dcdff1280f816064aed9880c`). After the release in #741, execution `usnm-boinc-publish-caj-prod-twa4osl` registered it as app_version 38; Linux stayed app_version 28.

The scheduler first sent host 1 two existing generic Gold jobs with historical AMD64 bases (WU 15712 / result 32108, WU 15721 / result 32109). Both exited 1 before OCR because the strict engine-base check rejected them; they are not measurements. Routing Gold jobs only to compatible platforms became #747, which led to the routing check in the [gold set](../ocr/gold-set.md#gold-routing). A separate campaign at `/project/gold/mac-arm-20261017-80bf` was prepared from the unchanged v1 bundle and cached originals, and one targeted job naming the ARM base went to host 1 / user 2: WU 16733 / result 32110 / app_version 38, job `d0f4b1eada04766e01f2a8ac03972ff86b66ab9d6d6cd992213cfedffa5e9460`.

## What we measured

Offline, both architectures scored 142 crops with zero skips over 125,622 reference characters: CER 0.090239, WER 0.259798, with identical per-crop character and word counts (score parity, not byte-identical text). Summed per-crop wall time was 518.058 s on AMD64 and 236.944 s on ARM, not a controlled performance comparison.

| Source / language      | Crops | Reference characters |      CER |      WER | Exploratory holdout CER |
| ---------------------- | ----: | -------------------: | -------: | -------: | ----------------------: |
| BLN600 / English       |    30 |               87,576 | 0.096259 | 0.248056 |                0.067381 |
| NewsEye BnF / French   |    24 |                9,973 | 0.089141 | 0.232764 |                0.190691 |
| NewsEye NLF / Finnish  |    18 |               11,895 | 0.079361 | 0.431330 |                0.071369 |
| NewsEye NLF / Swedish  |    10 |                6,267 | 0.108186 | 0.464625 |                0.107829 |
| NZZ / German           |    30 |                1,565 | 0.123323 | 0.383929 |                0.160628 |
| Overproof CA / English |    30 |                8,346 | 0.024203 | 0.095533 |                0.019124 |

Bundled crops have no original-page identifiers, so the holdout split is exploratory, not proven page-independent; the French holdout differs a lot from its analysis cohort (CER 0.047928). Two AMD64 controls scored all 142 crops: empty output gave CER and WER 1.0, and deliberately unsuitable PSM 10 gave CER 0.986977 and WER 0.992874 overall, yet improved NZZ's line crops to CER 0.073482. A control need not degrade every source, and an overall threshold hides that, as it hides BLN600's 69.7% share of the characters. French, Finnish and Swedish are now measured, where the historical worker skipped 52 crops for missing models.

Canonical BOINC results, each with outcome 1, validation 1, exit 0, assimilation 2 and error mask 0, and the database host matching the report's host claim:

| Run                    | Host | Result | Crops, skips | Reference characters | Edits  | CER           | Report SHA-256                                                     |
| ---------------------- | ---: | -----: | ------------ | -------------------: | ------ | ------------- | ------------------------------------------------------------------ |
| CC BY 52-crop, Linux   |    8 |  31526 | 52, 0        |               28,135 | 2,511  | 0.089248      | `8fad861dbd3f5a112babbb33d23dc6473efbec01ca1b4f4c62dc5a44db2ca5b7` |
| Full v1, Apple Silicon |    1 |  32110 | 142, 0       |              125,622 | 11,336 | 0.09023897088 | `0a41d70086c4bed79d89489f293853a7bc3fc64fb0e94386145e3841618f32fb` |

The CC BY bundle's manifest is `3fcafeabc8e77ad1118a724a0e844d7c8def74022e0c4b31178599e6ddecfee6` and its tar (8,893,952 bytes) `9e13ed3a2294889c76efd1ba325f0edca725e321f659410631139c657c8fe547`. Both runs match the offline baselines exactly in every crop's source, profile and integer counts. A [later full-v1 check on #527](https://github.com/tgoodyear/usnewsmap.com/issues/527#issuecomment-6227645406) found four more `80bf` executions, on hosts 6, 9, 2 and 8, each with all 142 crops, zero skips and the same counts. The [Apple Silicon acceptance receipt](https://github.com/tgoodyear/usnewsmap.com/issues/527#issuecomment-6228584575) records the same limits as below.

## What is left

- These are reused benchmark crops, not an independent unseen holdout. Several hosts are independent executions, not independent truth sets.
- No production threshold was derived, no trust promoted, and no legacy `0.1` threshold revived. No client preference, task priority, abort or queue-admission override was used.
- None of this calibrates the newer `247c` worker; that is [the 17 October note](2026-10-17-gold-english-trust-calibration.md).
- The [project-purpose record on #526](https://github.com/tgoodyear/usnewsmap.com/issues/526#issuecomment-6227671263) settled the NonCommercial-purpose question. It does not waive NoDerivatives, attribution or other licence terms; the original archives and licence files stay unchanged.
