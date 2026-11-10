# German Fraktur cutover diagnostic

**Status:** measured, not shipped; the candidate threshold failed. German results are now held from publication: see [Holding German BOINC results until Fraktur trust is calibrated](2026-11-05-german-trust-hold.md) · **Date:** October 2026 · **Measured on:** the pinned `247c` Tesseract worker (Tesseract 5.5.0, `frak2021`, PSM 3, 300 DPI) on native ARM64 Docker, against NewsEye/READ Austrian newspaper crops (Zenodo 3387369) · **Code:** `scripts/ocr-survey/gold.py` (`public-fetch`, `public-convert --dataset austrian`), `usnm-boinc gold trust`; [gold check](../boinc/server/gold-check.md#host-trust-for-usnm_ocr) · **Issues:** #835, #833

## The question

[#835](https://github.com/tgoodyear/usnewsmap.com/issues/835) blocked the German bulk-feed handoff in [#833](https://github.com/tgoodyear/usnewsmap.com/issues/833). The shared trust policy was English-only, and nothing calibrated a threshold for German Fraktur pages. The question was whether a CER limit set on one set of Austrian newspaper crops would accept the worker's real output on a second set, and refuse output with crops deliberately lost.

## What we did

The pinned `247c` worker contains Tesseract 5.5.0 and `frak2021.traineddata` with SHA-256 `1da2384254fa8462c776faf3b43307fe19ce51be0931c623b2fdd560f96e299a`, matching the model lock. Native ARM64 Docker execution verified this identity; it is not canonical BOINC host evidence.

The Austrian training and validation archives converted with the existing public-source tooling, so the converter is verified on the real archive layout. Credit ONB, NewsEye and READ under CC BY 4.0. Among regions inside the default 200 to 2,500 character window, the training archive covers eight newspaper/date issues, all of which also appear in validation. The combined eligible set has only nine issues, so the publisher's train/validation split is not issue-disjoint for this experiment. A search of the repository and its history found no earlier ONB result; that does not rule out earlier evaluation or overlap with model training.

Before running OCR, SHA-ranked issue selection with seed `835:onb-diagnostic-v1` froze four calibration issues, three heldout issues and two reserve issues, with at most three pages per issue and one region per page. Selection receipt SHA-256: `e24325e6ad77df7f5345859b9dfb524b590cb6db728876aefabecdeea35c81cd`. The partitions share no newspaper/date issue. Reference text, images, source archives and the experiment scripts stayed local; the issue comments keep counts, provenance and artifact hashes.

Calibration controls replaced one, two or four hash-ranked real predictions with empty output and rescored. These are synthetic output-loss controls, not separate faulty-engine or volunteer runs.

Before the heldout run, the candidate fixed a maximum CER of `0.06755715386591318`, halfway between the calibration positive and the nearest loss control, plus a complete-scoring guard and a no-complete-loss guard. Candidate SHA-256: `3bde526c319ea225c852f60a5b6b0fbce4995ba838bc0ec3b552e8d1dc610a1f`.

## What we measured

| Partition   | Issues | Crops | Character edits / reference characters |          CER | Word edits / reference words |
| ----------- | -----: | ----: | -------------------------------------: | -----------: | ---------------------------: |
| Calibration |      4 |    12 |                            420 / 7,786 |     0.053943 |                  289 / 1,118 |
| Heldout     |      3 |     9 |                            582 / 5,860 |     0.099317 |                    253 / 844 |
| Reserve     |      2 |     4 |                           Not executed | Not measured |                 Not measured |

No crop was a complete loss. The calibration loss controls gave CER 0.081171, 0.122656 and 0.235936.

**The candidate failed.** It refused the genuine heldout output. It also refused all three heldout loss controls, which does not make up for the false rejection. The threshold was not retuned on the spent heldout, and the difficult region was not removed.

### Segmentation on the spent data

PSM 6 was tried only as diagnosis, on the calibration and heldout crops already used:

| Segmentation | Calibration character edits / 7,786 | Spent heldout character edits / 5,860 |
| ------------ | ----------------------------------: | ------------------------------------: |
| PSM 3        |                                 420 |                                   582 |
| PSM 6        |                                 445 |                                   404 |

PSM 6 is worse on calibration and better on the spent heldout. This is diagnosis only: it does not pass the frozen candidate or change the production PSM.

The largest heldout error is a 673 by 157 pixel advertisement region from `ONB_aze_18950706_8`: the crop at PSM 3 gave 354 character edits over 538 reference characters. A separate full-page PSM 3 TSV run, keeping words whose centres fall inside the region in engine reading order, gave 316 edits over 538. More page context did not remove the error, and this does not show whether recognition, segmentation, reference alignment or their interaction causes it.

### Reichsanzeiger images

The broader Reichsanzeiger-GT reference archive was downloaded, but the Mannheim image endpoint its maintainers document returned HTTP 503 on the first image request and on a separate service check. The Zenodo archive holds PAGE XML, not the scans, and the GitHub releases checked have no image assets. No missing image was replaced with an unmatched scan, so no image-backed Reichsanzeiger evidence came from this run.

## What is left

- Nothing here promotes a German host or replaces the English policy. Scoped policies (`policies` in `trust.json`, #873) now let one shared `trust.json` keep the calibrated `eng/eng` scope beside a separately calibrated `(language, profile)` scope; see the [gold check](../boinc/server/gold-check.md#host-trust-for-usnm_ocr).
- Before a German scope can be promoted: broader reference coverage, revised criteria frozen before a fresh evaluation, partial-omission and wrong-model controls, and canonical exact-image BOINC Gold evidence. The two reserve issues are still unexecuted.
- A provisional `deu/frak2021` policy (`max_cer` 1.0, 12 hosts) went into the shared `trust.json` on 20 October 2026. On 1 November 2026 the maintainer decided German pages stay out of publishing until calibrated; the [5 November note](2026-11-05-german-trust-hold.md) records the hold that replaces it.
