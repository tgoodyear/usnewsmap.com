# Iris Xe crop pilot: completed evaluation and source-only staging

This closes the authorized experiment in [#703](https://github.com/tgoodyear/usnewsmap.com/issues/703):
the original inspected 100-crop pilot, the expanded 1,000-crop paired comparison,
and staging 20,000 distinct full-page source images. It does **not** accept
full-page inference, authorize dispatch, establish independent-host consensus,
or claim every crop succeeded.

## What actually ran

The original pilot completed 100/100 terminal-valid jobs on host 9, with no
errors or pending work. Its five worst transcriptions were inspected; boundary
insertions, punctuation/hyphen differences and recognition errors remained.
Its character-weighted, whitespace-normalized CER was 1.975175%.
The inspected acceptance has SHA-256
`c672b203e571ba9131fcc59841ec7b5097c03053fff31c175ca56f7aba0371e1`;
the report has SHA-256
`5c0327a6f34f8d2f6a7e5267f0c88ea789e94a1f943dfc7d6530189f202aac8b`.

The expanded experiment is exactly ten retained 100-job campaign manifests:
the original pilot, two additional English campaigns, and seven German
campaigns. Their union is 1,000 distinct job IDs: 300 English Overproof crops
and 700 German Reichsanzeiger PAGE-GT adjacent-line pairs.
A fresh read-only BOINC database audit and retained-output inspection on
27 October found **997 successful canonical results and three terminal
failures**, all on host 9. Batch 104's broader 1,014-workunit population is
not the denominator.

The worker is
`ghcr.io/tgoodyear/usnm-boinc-worker@sha256:d89127d048438bfab6e06a7e81bc6bc7044a1779552fb45c8c19746fda8e2c8c`.
The existing job/result contracts were revalidated, including immutable model,
input and worker bindings. No new jobs, replicas, model/token changes or CPU
fallback were introduced.

## Full-allocation paired results

These are newly recomputed scores over **all allocated crops**, not the stale
108-English/zero-German intermediate paired reports. Normalization collapses
whitespace but preserves case and punctuation. Edit distance is the existing
sequential character Levenshtein implementation in
`scripts/boinc/iris-campaign.py`; CER is total edits divided by total reference
characters, not the mean of per-crop CERs.

Every failed Iris job is scored as an empty prediction: all its reference
characters count as deletions. The exact same allocated image/reference set
is used for each baseline. English and German are not pooled.

| Set     | Engine                                        | Allocated crops |  Edits | Reference characters |        CER |
| ------- | --------------------------------------------- | --------------: | -----: | -------------------: | ---------: |
| English | Iris Qwen3-VL-4B INT4, OpenVINO Intel GPU     |             300 |  2,314 |               83,116 |  2.784061% |
| English | Tesseract 5.5 English PSM 6                   |             300 |  2,963 |               83,116 |  3.564897% |
| English | Tesseract 5.5 English PSM 4                   |             300 |  3,127 |               83,116 |  3.762212% |
| English | docTR 1.1.0 DB ResNet50 + PARSeq multilingual |             300 |  4,100 |               83,116 |  4.932865% |
| English | LoC source ALTO text                          |             300 |  7,522 |               83,116 |  9.050002% |
| German  | Iris Qwen3-VL-4B INT4, OpenVINO Intel GPU     |             700 | 10,002 |               70,170 | 14.253955% |
| German  | Tesseract 5.5 Frak2021 PSM 6                  |             700 | 12,243 |               70,170 | 17.447627% |
| German  | Tesseract 5.5 Frak2021 PSM 4                  |             700 | 12,687 |               70,170 | 18.080376% |

LoC ALTO is a source-text baseline, **not** a new engine execution on crop pixels.
English image checksums and references match the retained baseline manifest and
clean truth; every German baseline row binds the exact allocated image hash.
No empty baseline outputs were found.

The failed crops are `pub-overproof-ca-236` (result 31290, 260 reference
characters), `pub-reichsanzeiger-lines-565` (31935, 115 characters), and
`pub-reichsanzeiger-lines-645` (32015, 103 characters). Authoritative stderr
identifies all three as explicit `token_limit_reached` refusals at 512 generated
tokens, `stage=generate`, `finish_reason=length`. They are not transient
dispatch/network failures. They stay failed and remain in these denominators;
blind retries or raising the frozen limit are not part of this acceptance.

Retained English baseline archive SHA-256:
`61f29d0d2c8990ef0d4fce174aa9d5bac3c008166da0a735672c8681865a7456`.
Its CT230 server path is
`/project/iris-paired-200-e984fcca/iris-paired-baselines.tar.gz`.
German archive:
`1469a90029e2d4d715a7b6fe2a8406cd8f7ce89ed9167e42ad8e3b8ae2fbd363`.
Its path is `/project/iris-reich-700-a379f5b0/baselines.tar.gz`.
German baseline image:
`sha256:5522512034b1ac3d07d5b920fb74e8a8b7227dffca13e707b100841ed2a3aeee`.

## The 20,000-page source stage exists

The actual persistent stage is on the **pve-big hypervisor**, not inside CT211:
`/tank/ocr/scans/experiments/iris-20000-v1`.
It contains 20,000 distinct page IDs and 20,000 distinct image checksums,
83,665,016,881 source bytes. Its retained completed verification records
20,000 read-back SHA-256 matches and 20,000 standalone, full-resolution JPEGs.
A fresh manifest check matches both the campaign and completed verification:
`09d93462e7140628701981e4dedf610f02c3e4e00f285c08b2ee26ca73be4755`.
This is retained full-image read-back evidence, not a claim that all 83 GB
were rehashed on 27 October.
The exact retained receipt is
`/tank/ocr/scans/experiments/iris-20000-v1/verification.json`, SHA-256
`a7e1eeb3244e05b9ae254d039b5f599f6ce56cc547c61ca59975ea09bab200ae`.
An unchanged copy is published as `retrye-iris-retained-verification.json` in
[the verification/binding evidence release](https://github.com/tgoodyear/usnewsmap.com/releases/tag/data-iris-pilot-bindings-20261027).

A separate idle-I/O verification on 27 October checked that **all 20,000
files still exist with their recorded sizes and one link each**. All 32
distributed sample images (132,756,340 bytes) also passed fresh SHA-256
read-back checks. The manifest remained unchanged and dispatch disabled.

The reviewed source-only stager is commit
`f01d42199d37a536b778a47b32bd7ed53ce27d47`, operator SHA-256
`5f7ef0c5ea07e2bd385716b1bf4403b0694df502e86f6ca66b7e3da34da9175b`.
The stage binds the inspected original-pilot acceptance and worker above.
`stage_only=true`, `dispatch_enabled=false`, `executable_workunits=0` and
`DISPATCH_DISABLED` remain in force.

The [larger-campaign gate](2026-10-16-boinc-iris-pilot.md#larger-campaign-gate)
already distinguishes source staging from executable work. Any future
full-page campaign still requires validated layout-aware segmentation and
page reconstruction, tile bindings, measured memory/truncation/runtime, and
explicit dispatch authorization. Those gates have not been waived or passed
by crop-level results.

## Reproducing the audit

Download both evidence releases and check their `SHA256SUMS` assets before
using any receipt; compare the input-hash receipt itself against the independent
SHA-256 pinned below. If using the complete private scoring snapshot, require
its pinned whole-file hash before parsing. Otherwise, check every reference's
UTF-8 hash, job file's byte hash, retained result file's byte/canonical-JSON
hashes and terminal snapshot against `retrye-iris-1000-input-hashes.json`
**before scoring**. Refuse missing, duplicate or changed evidence rather than
silently recomputing a different comparison.

On CT230's BOINC server, retain the manifests and `units/<id>/iris.json` under
`/project/iris-pilot-100-f175f2c3`,
`/project/iris-paired-200-e984fcca/iris-paired-campaign{100,200}`, and
`/project/iris-reich-700-a379f5b0/campaign-{000,100,200,300,400,500,600}`.
First verify all ten `campaign.json` files against the independently published
`campaign_sha256` map in `retrye-iris-1000-scored.json`; references are in these
manifests, not in the job files. The follow-up binding audit rechecked all ten.
Then check each job file against its manifest `job_sha256`, reject duplicate
IDs, and verify every `units/<id>/page.jpg` against the job's `page_sha256` and
`page_bytes`. A fresh check of all 1,000 actual retained images
(63,667,186 bytes) passed. The verification/binding release contains
`retrye-iris-1000-page-hashes.json`, SHA-256
`09da0dc2d1b0d86b7bac2533c4884c56def72c6dcec068092bd34486c692d8ab`,
plus its separate page-check checksum asset. Query only the exact allocated
workunit names.

Require one terminal result per allocated job, workunit batch **104**, host
**9**, `result.app_version_id=27`, joined `app_version.version_num=100`,
app `usnm_iris` and plan class `docker_intel_opencl_4b`. The fresh binding
audit checked these values for all 1,000 results, including the three failures.
Before accepting a successful retained `/project/usnmocr/iris/seen/` output,
also require canonical-result identity, successful outcome/validation and
zero exit status.

The registered app version's signed Dockerfile must be exactly the `FROM`
line for the pinned worker digest above. The existing
`iris-campaign.py::verify_version_files` procedure checks its registered file
reference, exact file bytes and code signature with `crypt_prog -verify`
against `/project/usnmocr/keys/code_sign_public`. That check passed; Dockerfile
SHA-256 is `860d657a52b8cf4fd5c73d8a1575be29132c627a883472676440dcf1e529dd81`.
See `retrye-iris-registered-binding.json` in the verification/binding release.

Validate retained jobs/results with `ocr/boinc/iris/contract.py`. Collapse
reference/prediction whitespace; use an empty prediction for each of the
three terminal failures. Sum `edit_distance(reference, prediction)` and
reference lengths separately for the 300-English and 700-German allocations.
Read the exact baseline archives above, reject duplicate/missing crop rows,
and compare image checksums before scoring the same references. Do not
silently intersect with the successful subset.

Public aggregate receipts preserve the ten campaign hashes and scoring-input
hashes without republishing licensed reference transcriptions or credentials.
They are attached to
[the checksummed evidence release](https://github.com/tgoodyear/usnewsmap.com/releases/tag/data-iris-pilot-closeout-20261027).
The verification/binding release additionally contains
`retrye-iris-1000-input-hashes.json`, SHA-256
`3275ad3c1e6ae18b4e9ac1f87e3a4d516404ea6d2aa66cf5a5bae05a9788ed87`.
For every allocation it pins the reference/job hashes and terminal snapshot;
for all 997 retained outputs it pins the filename, original file-byte hash and
canonical-JSON hash. A fresh comparison refused changed or duplicate outputs
and confirmed every retained output still matches the scored input. The
receipt also pins the complete private scoring snapshot:
`2c2cc79d727911ee1d441f4c1528eba88adc48be644fc5f02acd17a1cdb71c9d`.
