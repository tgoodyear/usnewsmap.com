# OCR benchmark source/licence and actual-coverage audit — 18 October 2026

For [#738](https://github.com/tgoodyear/usnewsmap.com/issues/738). This audits the
published v1.0.0 manifest and newly staged v1.1.0 public ground truth, not the
size of a language's newspaper collection. No production data or queues changed.
The [annotation policy](../ocr-bench-annotation-policy.md) defines admission, damage
and masks. No new human transcription or damage review is claimed in this pass.

## Included sources and rights

| Track             | Source and human GT evidence                                                                                                      | Image / reference rights                                                                          | This audit's evidence                                                                                    |
| ----------------- | --------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| English crops     | Overproof dataset 3 via HIPE, hand-corrected articles                                                                             | LoC public domain / **CC BY-NC-SA 4.0** structured copy                                           | Published v1.0.0 manifest and source notices; not relicensed, not asserted permissive for commercial use |
| German line pairs | Mannheim Reichsanzeiger GT, Transkribus OCR-D Level 2                                                                             | Public Domain Mark 1.0 / CC BY 4.0 (follow stricter Zenodo statement, notwithstanding GitHub CC0) | Published v1.0.0 manifest; [source survey](../ocr-gold-set.md#source-policy)                             |
| French regions    | [NewsEye BnF 4293602](https://doi.org/10.5281/zenodo.4293602), carefully corrected PAGE text, Muehlberger/Hackl, BnF/NewsEye/READ | **CC BY 4.0 / CC BY 4.0**                                                                         | Current Zenodo record pinned in inputs; validation archive MD5 checked against publisher; SHA-256 below  |
| Finnish regions   | [NewsEye NLF 4599472](https://doi.org/10.5281/zenodo.4599472), carefully corrected PAGE text, Muehlberger/Hackl, NLF/NewsEye/READ | **CC BY 4.0 / CC BY 4.0**                                                                         | Same checks; publisher describes 8 validation pages, but eligible selected regions cover only 6          |
| Swedish regions   | [NewsEye NLF 4599624](https://doi.org/10.5281/zenodo.4599624), carefully corrected PAGE text, Muehlberger/Hackl, NLF/NewsEye/READ | **CC BY 4.0 / CC BY 4.0**                                                                         | Same checks; publisher describes 6 validation pages, but eligible selected regions cover only 4          |

Each released item resolves its `source` to separate image/reference licences,
attribution and `LICENSES/<source>/`. v1.1.0 includes the actual public metadata
records and `SOURCE.json`/`IMPORT.json` receipts, not just this table. The three
new archives contain no separate licence/readme files found by the existing
converter; their record-level CC BY 4.0 is retained, not invented archive terms.
Attribution names the authors, source institution and NewsEye/READ; references
retain publisher conventions. These are publisher-corrected GT, **not** newly
independently reviewed diplomatic transcriptions by this project.

### Input verification and selection

Fetched only validation archives through the existing documented public API,
without access blocks or scripted-site workarounds. The current record metadata
explicitly licenses the datasets CC BY 4.0 and describes carefully corrected text
produced in Transkribus. For each imported pair, the staging check compared its
reference against the source PAGE region extraction, checked the original page
hash, and recut the image to reproduce the exact selected JPEG bytes. Per-item
provenance pins source archive URL/SHA-256, metadata SHA-256, original PAGE member
name/hash, full-page image hash, crop box, region id and selection parameters.
This checks binding/extraction; it does not independently adjudicate the
publisher's language readings or reading order.

| Validation archive                            | SHA-256                                                            | Transcribed regions read | Selected crops / pages |
| --------------------------------------------- | ------------------------------------------------------------------ | -----------------------: | ---------------------: |
| `ATR_ValidationSet_BnF_Newseye_M2+.zip`       | `6862c17af69712508979413ce1012a1aa36c478f1b945b74ed42354033f791d0` |                    1,332 |                 24 / 8 |
| `ATR_ValidationSet_NLF_Newseye_GT_FI_M2+.zip` | `64d2d143abcd174811f17546622fe344f438347e2cf285b649384a9ba02051ce` |                      372 |                 18 / 6 |
| `ATR_ValidationSet_NLF_Newseye_GT_SV_M2+.zip` | `bc4b2a6e8f84ed1e6f1cae5d8c36795f7e99e52ea914a4d672988d3bda51f94b` |                       78 |                 10 / 4 |

Commands used for each `DATASET` (`newseye-bnf`, `newseye-nlf-fi`, `newseye-nlf-sv`):

```sh
python3 scripts/ocr-survey/gold.py public-fetch --dataset DATASET \
  --src SOURCES/DATASET --subset validation --confirm-download
python3 scripts/ocr-survey/gold.py public-convert --dataset DATASET \
  --src SOURCES/DATASET --out CONVERTED/DATASET --count 100 --seed 738 --per-page 3
```

The default 200–2,500 normalized-character window and page-round-robin selection
apply; `--count 100` is a **cap**, not a claim of 100 samples. JPEG encoder:
Pillow 13.0.0. Nonempty direct line `TextEquiv/Unicode` text is stripped at its
ends and joined with LF in XML file order, falling back to region text only
where there are no transcribed lines. This is not word-level OCR and not a
claim of reviewed full-page reading order. The deterministic release builder
imports these already-frozen pairs without re-encoding or re-extracting them;
CI checks every input against the committed inventory and rebuilds the archive.

The [public input release](https://github.com/tgoodyear/usnewsmap.com/releases/tag/ocr-bench-inputs-v1.1.0)
holds `usnm-ocr-bench-inputs-v1.1.0.tar`, SHA-256
`2c22442efee22f130be228a96fceb6eff35754a6d311966943ec0ca3afe97c6b`.
No BOINC hosting/deployment change was needed. Two local independent build
directories produced byte-identical 9,397,760-byte benchmark archives; CI must
independently match the committed release index before publication.

## Language × damage × unit matrix (unique real samples)

`unknown` damage means **unannotated**, never clean. Counts below are manifest
members, not sums of overlapping smoke/all/legacy subsets. All references in
these rows are publisher human-verified GT; all splits are `public-eval`.

| Release / principal language                                                             | Unit      | Unknown damage: samples / distinct pages | Reviewed none | Mild | Moderate | Severe | Full-page units |
| ---------------------------------------------------------------------------------------- | --------- | ---------------------------------------: | ------------: | ---: | -------: | -----: | --------------: |
| v1.0.0 English                                                                           | crop      |                                 300 / 18 |             0 |    0 |        0 |      0 |               0 |
| v1.0.0 German                                                                            | line-pair |                                 700 / 20 |             0 |    0 |        0 |      0 |               0 |
| v1.1.0 French                                                                            | crop      |                                   24 / 8 |             0 |    0 |        0 |      0 |               0 |
| v1.1.0 Finnish                                                                           | crop      |                                   18 / 6 |             0 |    0 |        0 |      0 |               0 |
| v1.1.0 Swedish                                                                           | crop      |                                   10 / 4 |             0 |    0 |        0 |      0 |               0 |
| Japanese, Spanish, Italian, Danish, Lithuanian, Czech, Russian, Serbian, Hebrew, Yiddish | any       |                                **0 / 0** |             0 |    0 |        0 |      0 |               0 |

Each unlisted language × reviewed damage × unit cell is zero **in these two
releases**, not a statement that no suitable source exists. No verified
mixed-language unit is included. v1.0.0 inherits `mix: monolingual`; v1.1.0 leaves
mix `unannotated`, since a publisher's principal-language label does not prove a
region contains only that language. The pooled `multilingual/all-52` combines
three tracks, not three languages on each page. Typeface remains unlabelled.

Machine-readable evidence:
[`v1.0.0-coverage.json`](../../ocr/bench/audits/v1.0.0-coverage.json),
[`v1.1.0-coverage.json`](../../ocr/bench/audits/v1.1.0-coverage.json).
Both pin manifest SHA-256. Regenerate after a verified fetch:

```sh
python3 scripts/archive/2026-10/coverage.py --manifest RELEASE/manifest.json --out NEW-coverage.json
```

No per-language `standard-1000` or `extended-10000` target is met. v1.0.0's
unbalanced `all-1000` is not a per-track standard tier. v1.1.0's French/Finnish
smoke-10 subsets nest in their tracks; Swedish has only its all-10 subset.
Neither release reaches the `pages-10`, `pages-100` or `pages-1000` full-page
track. Scaling requires more admitted real samples, not duplication or synthetic
corruption labelled as historical damage.

## Other sources: disposition and remaining work

The broader [source audit](../ocr-gold-set.md#source-policy) is evidence inherited
from its dated checks, not a new live licence audit of all URLs in this pass:

- **Austrian NewsEye (German)**: CC BY 4.0, documented GT; the recent German
  diagnostic verifies conversion. Not repackaged here. Broader Reichsanzeiger
  scans were unavailable (HTTP 503 in that diagnostic); no substituted scans.
- **Japanese #323**: reference files are outside the repo and reader assessment
  is still required; no authenticated reviewed pairs admitted. NDL PDM has
  redistribution rights but human-verification provenance was not established
  by the earlier audit; CODH magazines are a separate fit/provenance investigation,
  not newspaper coverage already acquired.
- **BLN600**: CC BY-NC-ND 4.0; no new cropped/re-encoded derivatives. **NZZ**:
  conflict between record and bundled licence; follow stricter CC BY-NC, only
  corrected line text. Neither becomes unrestricted data through this release.
- **Europeana Berlin**: permissive rights do not repair uncorrected/replacement-
  character text; layout GT is not transcription GT. **American Stories**:
  the earlier source audit did not establish rights/provenance of its 10 GT scans.
- **Serial Set #814 / PR #826**: the committed pilot has **3 human-verified
  full-page references**, with public-domain government source text and a
  documented pilot policy, plus **1 reference-free annotation gap**. These are
  outside the immutable release catalog. Do not count the fourth page as GT,
  map narrative scan observations to severity, or claim the 3 pages meet a
  pages-10 tier. Full-page policy/reading-order compatibility needs explicit
  handling when importing that pilot into a central release.
- **LoC refreshed/legacy baselines (#809/#810)** are engine text, not reference
  truth. They do not add human-verified languages, pages or damage cells here.
- No newly admitted, rights-audited image/GT pairs for Spanish, Italian, Danish,
  Lithuanian, Czech, Russian, Serbian, Hebrew or Yiddish were acquired in this
  bounded pass. Source discovery, competent-language review and redistribution
  evidence remain work, not completed coverage.

#738 must remain open: verified damage/masks on real data, mixed-language units,
full-page release tiers, the absent languages, per-track 1,000/10,000 targets and
held-out split design are still unmet. Synthetic unit-test strings are never
included in data counts or presented as real transcriptions.
