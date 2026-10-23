# OCR gold set

## German Fraktur cutover diagnostic (18 October 2026)

[#835](https://github.com/tgoodyear/usnewsmap.com/issues/835) blocks the German
bulk-feed handoff in [#833](https://github.com/tgoodyear/usnewsmap.com/issues/833).
The existing shared trust policy is **English-only**; the German experiments
below neither replace it nor authorize German publication.

The pinned `247c` worker described below contains Tesseract 5.5.0 and
`frak2021.traineddata` with SHA-256
`1da2384254fa8462c776faf3b43307fe19ce51be0931c623b2fdd560f96e299a`,
matching the model lock. Native ARM64 Docker execution verified this identity;
it is not canonical BOINC host evidence.

The NewsEye/READ Austrian newspaper training and validation archives from
[Zenodo 3387369](https://doi.org/10.5281/zenodo.3387369) converted successfully
using the existing public-source tooling. Credit ONB, NewsEye and READ under
CC BY 4.0. Among regions meeting the default 200-2,500-character window, the
training archive covers eight newspaper/date issues, all also represented in
validation. The combined eligible universe has only **nine issues**. The
publisher's train/validation split is therefore not an issue-disjoint split
for this experiment. A bounded repository/history search found no previous
ONB outcome record; this does not establish absence of previous evaluation
or model-training overlap.

Before running OCR, SHA-ranked issue selection with seed
`835:onb-diagnostic-v1` froze four calibration issues, three heldout issues
and two reserve issues. Each issue contributes at most three pages, with one
region per page. Selection receipt SHA-256:
`e24325e6ad77df7f5345859b9dfb524b590cb6db728876aefabecdeea35c81cd`.
The partitions have no shared newspaper/date issue. Reference text, images,
source archives and executable experiment scripts remain local; issue comments
retain counts, provenance and artifact hashes.

| Partition   | Issues | Crops | Character edits / reference characters |          CER | Word edits / reference words |
| ----------- | -----: | ----: | -------------------------------------: | -----------: | ---------------------------: |
| Calibration |      4 |    12 |                            420 / 7,786 |     0.053943 |                  289 / 1,118 |
| Heldout     |      3 |     9 |                            582 / 5,860 |     0.099317 |                    253 / 844 |
| Reserve     |      2 |     4 |                           Not executed | Not measured |                 Not measured |

These runs used the exact worker, `frak2021`, PSM 3 and 300 DPI, with no
complete-loss crops. Calibration controls replaced one, two or four
deterministically hash-ranked real-engine predictions with empty output and
recomputed scores. These are **synthetic output-loss controls**, not separate
faulty-engine or volunteer runs. Their calibration CERs were 0.081171,
0.122656 and 0.235936.

Before heldout execution, the diagnostic candidate fixed a maximum CER of
`0.06755715386591318`, halfway between the calibration positive and nearest
loss control, plus complete-scoring and no-complete-loss guards. Candidate
SHA-256:
`3bde526c319ea225c852f60a5b6b0fbce4995ba838bc0ec3b552e8d1dc610a1f`.
**The candidate failed:** it refused the genuine heldout output. All three
heldout output-loss controls were refused too, but that does not repair the
false rejection. The threshold was not retuned against the spent heldout,
and the difficult region was not removed.

### Segmentation investigation on spent data

PSM 6 was evaluated only as development diagnosis on the already-used
calibration and heldout crops:

| Segmentation | Calibration character edits / 7,786 | Spent heldout character edits / 5,860 |
| ------------ | ----------------------------------: | ------------------------------------: |
| PSM 3        |                                 420 |                                   582 |
| PSM 6        |                                 445 |                                   404 |

PSM 6 worsens calibration while improving the spent heldout. This is not a
fresh acceptance result, a reason to reinterpret the frozen candidate as
passing, or approval to change the production PSM.

The largest heldout error is a 673-by-157-pixel advertisement region from
`ONB_aze_18950706_8`: crop PSM 3 produced 354 character edits / 538 reference
characters. A separate full-page PSM 3 TSV run selected words whose centers
lie inside the original region, retaining engine reading order without
text-dependent matching. It produced 316 edits / 538 characters. More page
context did not eliminate the error; this observation does not establish
whether recognition, segmentation, reference alignment or their interaction
is responsible.

The broader Reichsanzeiger-GT reference archive was retrieved, but its
maintainer-documented Mannheim image endpoint returned HTTP 503 on the
initial image request and a separate service check. The Zenodo archive
contains PAGE XML, not the corresponding scans; the checked GitHub releases
have no image assets. No missing image was replaced with an unmatched scan.
Broader image-backed evidence remains unavailable through that path until
the source service recovers or a verified alternate source is established.

Do not promote a German host or replace the shared English policy from these
diagnostics. Scoped policy coexistence (`policies` in `trust.json`) now allows
the shared `/project/gold/trust.json` artifact to retain existing calibrated v4/v5
`eng/eng` trust while merging a separately calibrated `(language, profile)`
scope (`usnm-boinc gold trust ... --out /project/gold/trust.json`), and both
collection and spot-check planning enforce the workunit's exact
`(language, profile, psm, dpi)` against its matching scoped entry without
authorizing cross-scope collection. Remaining requirements before promoting a
German scope include broader reference coverage, revised criteria frozen before
fresh evaluation, partial-omission/wrong-model controls, and canonical
exact-image BOINC Gold evidence. The two reserve issues remain unexecuted.

## Issue-disjoint current-worker calibration (17 October 2026)

For #527/#751, the original BLN600 archive's publication/date metadata excluded
all 28 issues represented by the old 142-crop benchmark. An outcome-blind hash
ordering selected one excerpt per unused issue: 30 for calibration and the next
30 for sealed testing. These are newspaper excerpts, not asserted independent
original pages or absent from model training. The experiment is English BLN
only; it does not establish multilingual or American-newspaper performance.
Selection SHA-256:
`204b2d1b468eea53cdab3b9fadd289b90e6cdc55454da88f046efcf44b8a0054`.
Normal reference-only bundle manifests are calibration
`d8cd7279090a6427c0cc6f0e82573e3771c27f90df6500bf5a44b268f6e0f05f`
and heldout
`2bc0e24ebdf20adfb5577eab8634279391ba135f3ffd066d0a6cc4a5307056ea`.
Original archives/licences stayed unchanged; only counts are reported here.

Actual ARM runs used production worker
`sha256:247c131a599403645c4c96782c0556dc27f27a5d991c764b4b9565182a8c71aa`,
base `sha256:7bc7b78658562430ec95a1b1048c2f689c26f8d2443d6ed0c5e981e311cbab93`,
Tesseract 5.5.0, PSM 3 and 300 DPI. Fault controls ran the real engine but
returned empty output for 1, 2 or 4 outcome-blind hash-ranked crops; report
scores were not edited. Each run scored all 30 crops with zero skips.
The external unmodified control matched native integer character/word counts
on every crop in both partitions.

| Partition   | Native / unmodified edits | One lost crop | Two lost crops | Four lost crops | Reference characters |
| ----------- | ------------------------: | ------------: | -------------: | --------------: | -------------------: |
| Calibration |                     5,929 |         8,534 |         10,874 |          14,491 |               82,794 |
| Heldout     |                     4,719 |         7,084 |          7,993 |          14,361 |               85,685 |

Before opening heldout, candidate
`9cfe342402d4fe5c3facd09e83fc048555d093f6f3f6a6333e7331abe73fc0f2`
fixed the aggregate CER limit at the calibration positive/nearest-fault
midpoint, `14463/165588 = 0.08734328574534386`. It required both positives
to pass and every fault to fail, without retuning on heldout.
**This candidate failed:** heldout lost-one-crop CER
`7084/85685 = 0.08267491392892572` incorrectly passed. This is not a usable
production threshold or statistical error guarantee. The numerical threshold
has not been retuned against this spent test partition.

Trust format v4 therefore binds one explicit calibrated manifest and also
requires complete scoring with no skips or crop whose character edits reach
its reference length. The normal validated collector derives the counts-only
`complete_loss_crops` field; old tables cannot silently satisfy this policy.
This structural guard catches empty-output controls but does not prove
resilience to partial omissions, wrong-language models or other fault families.
The revised candidate kept the same numerical limit and froze this structural
guard before OCR on the next 30 outcome-blind issues, disjoint from baseline,
calibration and the spent heldout. Selection SHA-256:
`11183a39b1b64616745e6e2bcb8adb86a757649704fd5d85a69cc91465ab9c3f`;
manifest `70600f006cdfce3cfba273028fc7a3f52bb30df3e7934b9ccde6e8f5265f468c`;
frozen candidate
`ff12ee4cc80d94f7812876cda2c4953f380f76cb529dca20b7488e64e7279023`.
Both real-engine positives matched every integer count and passed, with
6,255 edits / 87,111 reference characters. Lost-one/two/four-crop controls had
9,099 / 11,339 / 17,172 edits and respectively 1 / 2 / 4 complete-loss crops;
all failed. Every run scored 30 crops with zero skips. Thus this independently
tested revised policy passed its preregistered criteria, not the original
aggregate-only policy.

The shared handoff trust file cannot be isolated by passing a pilot-only path.
Trust v4 therefore additionally records source language `eng`, model profile
`eng`, PSM 3 and 300 DPI, and publication/spot checks require the actual job to
match that scope. The collector records actual profiles/configuration and trust
building checks those against the declared policy; language scope must also
be established by the operator's pinned-manifest evidence. This English-only
experiment does not authorize other languages or profiles on the same image.
These offline experiments alone did not promote a host; current-image canonical
evidence and explicit deployment were required before the scoped promotion
recorded below. None of these experiments is fresh page OCR, handoff publication,
release or search acceptance.

The reviewed routing registry also records the current `247c` image's separately
verified native base on each architecture: ARM above and AMD64
`sha256:e088ef6f8004aa66091ee4bdaa06a42e5061a42dfd505db31c5d7257b747d065`.
This permits future correctly signed current-image Gold apps; it is not app
publication or calibration by itself. The existing `80bf` mapping stays intact.
After #749/#750's image-only rollout, production revision 54 was healthy and
the native targeted path dispatched compatible Mac WU 16734/result 32114 using
app 38. It reached canonical 32114, assimilation 2, outcome/validation 1 and
exit 0; normal `gold check` and collection accepted all 142 crops with zero
skips (report SHA-256
`d2c0781be7863cf3c2170bed03c3abab50770192d279ce4a524a2823a7d582a5`).
That validates a live compatible dispatch on `80bf`, not a `247c` measurement
or an observed live incompatible refusal.

### Current-worker native canonical evidence and scoped promotion

The reviewed #755/#761 image-only rollout reached healthy production revision
55, including the separate handoff job without changing its command or other
settings. Signed Gold 1.4 publication then succeeded for Linux
(`usnm-boinc-publish-caj-prod-uucz4sm`) and Apple Silicon
(`usnm-boinc-publish-caj-prod-p57pptb`). Independent native registration checks
found active version 104 / `docker` apps 39 and 40 respectively, and both
published Dockerfiles pin the exact `247c` worker above. All six registered
public download URLs returned anonymous HTTP 200 with SHA-256 hashes identical
to the signed published payloads. Gold publication does not update `usnm_ocr`.

One normal targeted challenge per existing user-2 host used the unchanged
30-crop calibration manifest above:

| Platform      | Host | Workunit | Canonical result | App version | Canonical report SHA-256                                           |
| ------------- | ---: | -------: | ---------------: | ----------: | ------------------------------------------------------------------ |
| Apple Silicon |    1 |    16736 |            32117 |          40 | `082649110586b90dab6ab717ee9aea60914f11205cfbfdd7f8819aaa47bab2f4` |
| Linux x86_64  |    8 |    16737 |            32118 |          39 | `d9b67382e7a4127fd1ffa046d4792d688f1b32b1705903b47ca7ea56556c7a4e` |

Both native results have outcome 1, validation 1 and exit 0; their workunits
have assimilation 2 and error mask 0. Normal `gold check` and `gold collect`
accepted each report with no pending or failed result. All 30 unique crops
have zero skips and complete losses, and every source, profile and integer
character/word edit and reference count exactly matches the offline ARM
baseline: **5,929 edits / 82,794 reference characters**. A separate local AMD64
emulation also matched those counts (report SHA-256
`c17ff88f1051623b5f71879981b07b8d8a900e08771f751b8536522a1c1baf41`);
it is not a substitute for the actual Linux BOINC result.

At `2026-10-17T07:19:56Z`, the normal local production mutex, per-job idle
preflight (historically all-six-idle before [#856](boinc-start-guard.md#scope-boundaries-and-per-resource-deploy-gates))
and infinite controller lease guarded the global trust promotion.
Native receipts and the unchanged old policy were rechecked before publishing.
`/project/gold/current247c-scope-eng-20261017/` retains the before-policy
snapshot, generated candidate and promotion receipt. The actual shared
`/project/gold/trust.json` has SHA-256
`3bfc19016cf38b47455faa4ddbe8fbbe27577c410c1d8e74a2ccdd8a0a6a651b`:
v4, only worker `247c`, hosts **1 and 8**, calibration manifest `d8cd7279...`,
source language/profile **eng/eng**, PSM 3, 300 DPI and frozen limit
`0.08734328574534386`. It does not authorize another worker, language,
profile or configuration.

This is an English BLN fixed-worker quality pilot, not proof of American-corpus,
multilingual, statistical, availability or malicious-host performance.
Terminal/no-valid-report attribution remains [#756](https://github.com/tgoodyear/usnewsmap.com/issues/756).
The [native promotion receipt](https://github.com/tgoodyear/usnewsmap.com/issues/527#issuecomment-6231138896)
does not close the separate fresh-page Mac OCR, handoff, release or search gates.
No queue-admission exception, client preference, task abort or priority change
was used.

Part of [#296](https://github.com/tgoodyear/usnewsmap.com/issues/296). The [engine survey](reports/2026-10-ocr-engine-survey/report.md) has no ground truth, so it can't report a character error rate (CER) or word error rate (WER); it relies on the damage metric. The gold set is a fixed set of crops with a person's transcription, so any engine, profile or route change can be scored by CER and WER on the same text. The plan in [13 §13.9](design/13-ocr-engines.md#139-how-the-survey-and-production-share-engine-images) already names `score --truth <crops dir>`; the gold set is what goes in that directory.

The transcriptions of the 60 crops are not written yet. Every `transcription` field is empty, and none may be filled from an engine's output. Until they are, the 60 are scored as engine agreement only (`gold.py agree`), and the accuracy numbers come from public datasets that already have human transcriptions, described in [Public ground truth](#public-ground-truth).

## Selection

- **Source.** The survey's 80 scored pages (`data/scores.jsonl`; the sample's images are not in the repository). One crop per page.
- **Target.** 60 crops. The target is shared equally between the survey's groups; a group with fewer pages gives all of them and the others share the rest. The first selection is 10 each for German, worst-English, Serbian, Czech and control, and 5 each for Polish and Russian (all their pages).
- **Strata.** Group (which stands for language and print type: the survey groups were chosen as the worst-read cases per language, and the control is well-read English dailies), then era. Inside a group the picks cycle through decades in a seeded order, so a group spans as many decades as it has pages in. The era runs from the 1850s to the 1960s.
- **Seeded.** `gold.py select` with seed 296 reproduces `fixtures/ocr-gold/selection.jsonl`.
- **Not stratified by typeface.** The sample records no typeface (Fraktur or Antiqua, for German). Record it in `notes` when transcribing; if a stratum turns out to hold one typeface only, add pages.
- **Crop box.** One text region per page, chosen as the survey's `crops.py` does (a line of LoC's ALTO with 4 or more words in a block at least 600 pixels wide, and the region below it, up to 1,600 pixels wide and 6% of the page high), seeded per page. The box is fixed in the manifest.

The survey's `scores.jsonl` is in the `report-data` set, so fetch it first: `scripts/data-assets.sh --fetch --set report-data` ([reports/README.md](reports/README.md), "Data").

```sh
scripts/ocr-survey/gold.py select --scores docs/reports/2026-10-ocr-engine-survey/data/scores.jsonl \
  --out fixtures/ocr-gold/selection.jsonl --target 60 --seed 296
# where the survey's sample and work directories are:
scripts/ocr-survey/gold.py cut --selection fixtures/ocr-gold/selection.jsonl --sample SAMPLE --work WORK --out OUT
```

`select` with `--manifest SAMPLE/manifest-final.jsonl` gives the same choice and carries the image and ALTO URLs and checksums.

The 40 Czech, Serbian, Polish and Russian crops (ten each) were cut on 22 October 2026 with `gold.py cut`'s box rule at seed 296. Polish and Russian have five pages each, so each of those pages gives a second, non-overlapping box. Their manifest and the empty transcription template are in `fixtures/ocr-gold/cze-srp-pol-rus/` (`manifest.jsonl`, `crops.jsonl`; the crop images are not committed), and the crops are out for transcription. Surya reads the same boxes as BOINC crop units, so its character error rate can be scored once the transcriptions come back: see [Surya on the gold crops](boinc-surya.md#surya-on-the-gold-crops-968).

## Japanese crops

The 24 Japanese crops of the [#128](https://github.com/tgoodyear/usnewsmap.com/issues/128) comparison are not in the 60. Reference texts exist for them outside the repository: 23 files, and one more set aside. What their files and logs show about provenance:

- No script, log or note beside them or in the repository records how the refs were made. `ja-ocr/crops.py` says "transcribe each crop into refs/<page_id>.txt" and nothing more; the commit that added it has no message beyond its title.
- None is identical to any engine's text for the crop (best similarity per file 0.36 to 0.93 against NDLOCR-Lite, the prototype NDLOCR run, or Azure), so they are not copies of one engine's output.
- The files were written between 10:35:20 and 10:50:46 on 4 October, 20 to 60 seconds apart, some of 1,400 to 1,700 characters, and two in the same second. That pace is not typing by a person. It fits transcription by a program or an AI model reading the images, but nothing on disk says which.
- Fifteen lines contain the geta mark 〓 for an unreadable character. No engine's output contains it.

So they are not verified as hand-made, and most likely machine-made. They are not ground truth. The survey report is right that no crops were hand-transcribed. Until a person transcribes them from the images, they are a hint, like an engine's text. The crop images are not in the repository, so they are not in the manifest yet.

Needs transcription (crop from `sample.csv`, all unverified):

| page                                | group   | reference text                          |
| ----------------------------------- | ------- | --------------------------------------- |
| `sn83025518_1945-06-16_ed-1_seq-2`  | typeset | ref                                     |
| `sn83025518_1945-08-14_ed-1_seq-3`  | typeset | ref                                     |
| `sn83025518_1945-10-27_ed-1_seq-3`  | typeset | ref                                     |
| `sn83025517_1944-11-08_ed-1_seq-2`  | typeset | ref                                     |
| `sn83025517_1944-08-23_ed-1_seq-2`  | typeset | ref                                     |
| `sn83025518_1945-10-27_ed-1_seq-2`  | typeset | ref                                     |
| `sn83025518_1945-06-07_ed-1_seq-3`  | typeset | ref                                     |
| `sn83025517_1945-10-29_ed-1_seq-2`  | typeset | ref                                     |
| `sn83025517_1944-11-08_ed-1_seq-3`  | typeset | ref                                     |
| `sn83025517_1945-10-29_ed-1_seq-3`  | typeset | ref                                     |
| `sn83025517_1944-09-25_ed-1_seq-3`  | typeset | ref                                     |
| `sn83025517_1945-06-20_ed-1_seq-3`  | typeset | ref                                     |
| `sn85040043_1943-09-30_ed-1_seq-3`  | camp    | ref                                     |
| `sn94050512_1945-03-03_ed-1_seq-2`  | camp    | ref                                     |
| `sn84024049_1943-08-21_ed-1_seq-9`  | camp    | ref                                     |
| `sn83025352_1945-09-08_ed-1_seq-4`  | camp    | ref                                     |
| `sn83025521_1942-10-21_ed-1_seq-8`  | camp    | excluded (4 lines; why is not recorded) |
| `sn84025952_1944-04-20_ed-1_seq-8`  | camp    | ref                                     |
| `sn83025353_1943-11-20_ed-1_seq-8`  | camp    | ref                                     |
| `sn85040043_1943-09-21_ed-1_seq-5`  | camp    | ref                                     |
| `sn83025353_1942-10-14_ed-1_seq-8`  | camp    | ref                                     |
| `sn83025522_1943-09-01_ed-1_seq-10` | camp    | ref                                     |
| `sn83025333_1943-04-24_ed-1_seq-9`  | camp    | ref                                     |
| `sn84025952_1944-04-20_ed-1_seq-7`  | camp    | ref                                     |

The scorer for these is `scripts/ja-ocr/score.py`, which folds old character forms and also reports bigram F1. Japanese text has no spaces, so `gold.py`'s WER doesn't apply to it; once transcribed, the crops can be added to `crops.jsonl` with `lang` `jpn` and scored by CER.

## Crop format

- `manifest.jsonl`, one row per crop: `crop_id` (`<group>-<nn>`), `page_id`, `group`, `lang`, `era`, `stratum`, `lccn`, `date`, `edition`, `seq`, `title`, `box` (x0, y0, x1, y1 in the page image's pixels), `image`, `crop_sha256`, `encoder`, `page_url` (the loc.gov page), `page_image_url` (the IIIF full-size JPEG the survey used), `page_image_sha256`, `alto_url`.
- `images/<crop_id>.jpg`: the box cut from the page, grayscale, JPEG quality 85.
- `crops.jsonl`, the transcription template, one row per crop: `crop_id`, `group`, `lang`, `date`, `title`, `image`, `hints`, `transcription` (null until a person fills it in), `transcriber`, `reviewer`, `hint_copy_reviewed` (false), `notes`.
- `hints.texts` holds what LoC's ALTO and each engine read in the box, in visual line order. `hints.is_truth` is false and `hints.note` says so. A hint is there to find the passage on the page, not to be corrected. The CER of a hint against the transcription is a result, so a transcription that copies a hint spoils it.

## Where the images live

The page checksum pins the pixels. `page_image_sha256` is the checksum of the page JPEG the survey fetched, and `gold.py fetch` downloads each page (paced as the survey's `locfetch.py` does), checks it, and cuts the box. The crop JPEGs themselves are committed under `fixtures/ocr-gold/` only if the total is under about 10 MB; `cut` prints the total. Otherwise the repository holds the manifest, and `fetch` rebuilds the images. JPEG bytes can differ between Pillow versions, so a differing `crop_sha256` with a matching page checksum is a warning, not an error. 60 pages are about 60 full-resolution downloads from loc.gov, so run `fetch` on a machine with the bandwidth and disk for them.

## Transcription rules

1. **Verbatim.** Type what is printed, including errors, old spellings, long s (ſ), ligatures and printed punctuation. Don't correct, modernize or normalize. Serbian stays in the script printed (Cyrillic or Latin).
2. **Lines.** One line of the transcription per printed line in the crop, in reading order for the crop's column. If the crop cuts through a line at its edge, transcribe only the characters wholly inside the crop and note it in `notes`.
3. **Hyphenation.** Keep the hyphen at the end of a line as printed and don't join the word. The scorer joins it (see below).
4. **Uncertain characters.** `[?]` stands for one character that can't be read. `[?x]` stands for characters read as `x` with doubt. More than 2% of a crop's characters marked this way fails validation; drop the crop and add another page instead.
5. **Out of scope.** Leave out ornaments, rules and the text of images. Don't skip printed text inside the crop. The scorer can't align around a gap, so validation rejects a `[...]` marker; drop such a crop and add another page instead.
6. **Who.** The maintainer transcribes, or a person they name. Set `transcriber` to that person. An engine's text, or a model's reading of the image, is never the transcription.
7. **Second pass.** A different person reads every transcription against the image, fixes it, and sets `reviewer`. The validator rejects a reviewer equal to the transcriber. If only one person is available, run validate with `--allow-unreviewed`, and say so wherever scores are quoted.
8. **Hint copies.** A transcription equal to a hint after normalization fails validation. If the hint really is right, check the image and set `hint_copy_reviewed` to true.

```sh
scripts/ocr-survey/gold.py validate --crops OUT/crops.jsonl --manifest OUT/manifest.jsonl --images OUT/images
```

## CER and WER

`usnm-ocr-survey score` (the `crates/usnm-ocr-survey` crate, [#457](https://github.com/tgoodyear/usnewsmap.com/issues/457)) compares each hypothesis text with the transcription. It replaces `gold.py score`, which stays in the repository as the reference: both take the same flags and write the same files, byte for byte. This is the CER the plan describes in 13 §13.9, minus the part that finds the crop's span in a whole page's text; that part needs the engine runs' word boxes and is not built. The arithmetic is edit distance over the normalized transcription's length, summed over crops (a crop's weight is its length), reported for all crops and per group. WER is the same over whitespace-separated words.

Characters are Unicode scalar values, as Python's `str` counts them, and whitespace is what Python's `str.isspace` accepts (Rust's `White_Space` plus U+001C to U+001F). The edit distance is Myers' bit-parallel algorithm in Hyyrö's multi-word form, about n·m/64 word operations instead of gold.py's n·m Python steps. On the 600 BLN600 excerpts against Gale's OCR (1,696,781 reference characters), `gold.py score` took 880 s and `usnm-ocr-survey score` 0.66 s, with identical output files.

**Golden test.** `scripts/ocr-survey/golden_score.py` writes seeded synthetic crops and hypotheses into `crates/usnm-ocr-survey/tests/golden/` with gold.py's output for three runs (`--hyp`, `--hyp` plus `--hints`, `--hints --allow-unreviewed`). The inputs cover the edge cases: every Python whitespace character, line-end hyphens with CR and tabs, nested guess markers, combining characters, characters outside the Basic Multilingual Plane, a rounding tie, quotes and commas in CSV fields, an empty group, and each validation failure. The Rust test `tests/golden.rs` must reproduce the files byte for byte (the rust CI job), and the infra job runs `golden_score.py --check` whenever gold.py changes, so a change to either scorer that the other doesn't share fails CI. After a deliberate change to gold.py, regenerate with `scripts/ocr-survey/golden_score.py --out crates/usnm-ocr-survey/tests/golden`.

Normalization, on both sides:

1. Unicode NFC. Case, punctuation, diacritics, long s and ligatures are not folded.
2. A hyphen at the end of a line, followed by the line break, is removed and the line joined. The survey's text does the same.
3. Line breaks and runs of whitespace become one space.

In the transcription only, `[?x]` counts as `x` and `[?]` is dropped.

Hypotheses are the hint texts in `crops.jsonl` (`--hints`), or per-engine files of `{"crop_id", "text"}` lines (`--hyp tesseract-5.5=FILE.jsonl`). Crops that fail validation are left out and counted.

```sh
cargo run --release -p usnm-ocr-survey -- score --crops OUT/crops.jsonl --hints --out OUT/scores
```

## Public ground truth

The 60-page selection stays untranscribed: the maintainer will not hand-transcribe pages, though a named person may (see [Selection](#selection) for the 40 now out for transcription). For those crops `gold.py agree` compares the engines' texts for the same box with each other. That number is agreement, not accuracy: two engines can agree and both be wrong. It says which engine is the odd one out on our pages, and nothing about how close any of them is to the printed text.

Accuracy needs a human transcription, so this section looks for public datasets that have one. Checked on 13 October 2026 from public documentation, dataset cards and repository metadata only; nobody was contacted. Anything I could not confirm is marked UNVERIFIED. Licences are quoted from the source named in the table.

### Source policy

Other people use this repository, so every source used for scoring must be one anyone can check and that we can publish. A source is used for scoring only if all three hold:

1. **Public download.** Anyone can download it without a login, request form or agreement.
2. **High confidence.** The transcriptions were made or verified by people, and the source documents how.
3. **Use only under verified source terms.** No dataset content is committed, and fixtures are synthetic. `public-fetch` keeps source licence and attribution materials beside downloaded data (`SOURCE.json` plus the dataset's own licence files), and `public-convert` copies them into `LICENSES/`. NonCommercial datasets require the operator to verify that the specific use qualifies before serving them; this document does not decide that question. For BLN600, the Figshare v2 record identifies CC BY-NC-ND 4.0. Its linked deed and legal code condition sharing on NonCommercial use and attribution, and do not permit sharing adaptations. The workflow therefore mirrors only the unchanged original archive, preserves its `LICENSE` and `readme.md` with `SOURCE.json` and `ATTRIBUTION.txt`, and does not share derived BLN600 crops or re-encoded images. Volunteers cut crops locally ([Volunteer bundle](#volunteer-bundle)).

Checked on 13 October 2026 against each source's own licence and download pages:

| Source                                      | URL                                                                                                                                        | Licence                                                                                                                                    | Provenance                                                                                                                                                                                                                                                                 | Confidence   | Policy                                                                      |
| ------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------ | --------------------------------------------------------------------------- |
| Reichsanzeiger-GT                           | [GitHub](https://github.com/UB-Mannheim/reichsanzeiger-gt), [Zenodo](https://doi.org/10.5281/zenodo.10144094)                              | CC0-1.0 on GitHub, CC-BY-4.0 on Zenodo; images Public Domain Mark 1.0. We follow CC-BY-4.0 and credit UB Mannheim                          | Transcribed in Transkribus to the OCR-D Level 2 guidelines, documented in the README and paper                                                                                                                                                                             | High         | Passes. Used for scoring                                                    |
| Austrian newspapers                         | [Zenodo](https://doi.org/10.5281/zenodo.3387369)                                                                                           | CC-BY-4.0; credit ONB, NewsEye and READ                                                                                                    | Page images from the Austrian National Library; text corrected by people in Transkribus for NewsEye and READ, per the record                                                                                                                                               | High         | Passes. Used for scoring                                                    |
| BLN600                                      | [figshare](https://doi.org/10.15131/shef.data.25439023.v2)                                                                                 | CC BY-NC-ND 4.0 (Figshare record and `LICENSE`). Credit Callum Booth, Alan Thomas and Robert Gaizauskas                                    | "Human transcriptions of 600 images" from British Library Newspapers parts 1 and 2; "multiple error detection, image comparison, and error correction passes"; the readme claims "a high, but not 100% accuracy rate". English, 19th-century London, mostly crime articles | High         | Unchanged archive mirrored; source terms must be checked for deployment     |
| Overproof dataset 3 (Chronicling America)   | [HIPE-OCRepair-2026](https://github.com/hipe-eval/HIPE-OCRepair-2026-data), [Overproof](https://overproof.projectcomputing.com/evaluation) | HIPE copy: "CC BY-NC-SA 4.0", applying to the structured format; the newspaper text and LoC's images are public domain                     | 49 articles from 5 US titles, 1871 to 1922, "hand-corrected each article with reference to the PDF image of the corresponding page"; HIPE re-curated and checked them by hand                                                                                              | High         | Passes. Used for scoring; page images and ALTO fetched from LoC at run time |
| Overproof dataset 2 (Sydney Morning Herald) | [HIPE-OCRepair-2026](https://github.com/hipe-eval/HIPE-OCRepair-2026-data)                                                                 | HIPE copy: "CC BY-NC-SA 4.0". Trove image licence UNVERIFIED                                                                               | 159 Trove articles corrected to ground truth by Overproof, then checked again for HIPE                                                                                                                                                                                     | High (text)  | Text only, no images: not used for scoring                                  |
| NewsEye French (BnF)                        | [Zenodo](https://doi.org/10.5281/zenodo.4293602)                                                                                           | `cc-by-4.0` (record)                                                                                                                       | PAGE XML at line level with images, corrected by people in Transkribus for NewsEye and READ, per the record                                                                                                                                                                | High         | Passes. Used for scoring                                                    |
| NewsEye Finnish and Swedish (NLF)           | [Finnish](https://doi.org/10.5281/zenodo.4599472), [Swedish](https://doi.org/10.5281/zenodo.4599624)                                       | `cc-by-4.0` (records)                                                                                                                      | As BnF; the PAGE files carry Transkribus `status="GT"`                                                                                                                                                                                                                     | High         | Passes. Used for scoring                                                    |
| NZZ black-letter                            | [Zenodo 3333627](https://doi.org/10.5281/zenodo.3333627)                                                                                   | Zenodo record: `cc-by-4.0`; LICENSE.txt in the archive: "Attribution-NonCommercial 4.0 International". We follow the stricter CC BY-NC 4.0 | 167 front pages corrected in Transkribus. The README warns that pages corrected only per line "still contain the uncorrected text on the word level", so only line text is read                                                                                            | High (lines) | Passes at line level. Used for scoring                                      |
| Europeana Newspapers, Berlin State Library  | [Zenodo](https://doi.org/10.5281/zenodo.2583866)                                                                                           | "Creative Commons Zero v1.0 Universal"                                                                                                     | 50 pages of PAGE XML, text per region, not per line. Checked here: the text drops the ch and ck ligatures (`dur` for durch), 42 of 50 files hold U+FFFD, and OCR errors remain                                                                                             | Low          | Fails (b). Reader kept for comparison only                                  |
| American Stories gold set                   | [Hugging Face](https://huggingface.co/datasets/dell-research-harvard/AmericanStoriesTraining)                                              | `apache-2.0` in repository metadata only; the card is empty                                                                                | The paper says two annotators hand-entered 10 pages, but nothing says where the pages come from or what years, so the rights in the images are unknown                                                                                                                     | Medium       | Fails (b) and, for the images, (c). Not used for scoring                    |
| NDL PDM OCR set                             | [GitHub](https://github.com/ndl-lab/pdmocrdataset-part1)                                                                                   | Public Domain Mark 1.0                                                                                                                     | The README says characters were "entered as visually recognized" but not who entered or checked them. Books, not newspapers                                                                                                                                                | Unknown      | Fails (b). Not used for scoring                                             |

Our own 60-page selection has empty transcriptions, so it gives engine agreement only, never accuracy. Its page images come from Chronicling America and are public; see [Licensing](#licensing).

### What American Stories evaluated on

The [GitHub README](https://github.com/dell-research-harvard/AmericanStories) says: "Its vast size and silver quality make it ideal for innovation and research in this domain." It mentions no test data. The paper ([arXiv:2308.12477](https://arxiv.org/abs/2308.12477), read as the [ar5iv HTML](https://ar5iv.labs.arxiv.org/html/2308.12477) through a summarising fetch tool, so check the wording against the paper before quoting it elsewhere) and the Hugging Face repositories say this:

- **Dataset card** ([dell-research-harvard/AmericanStories](https://huggingface.co/datasets/dell-research-harvard/AmericanStories)): licence CC-BY 4.0, data split by year, annotation section "Not Applicable", no test set, no CER.
- **Layout detector.** Trained on "2,202 labeled newspaper images, consisting of 48,874 layout objects". The labels were "created by undergraduate research assistants". "Scans from Chronicling America comprise 13% of the training sample", so most of the layout training data is not Chronicling America. The paper describes no held-out layout test split; it reports mAP 91.31 for articles and 88.64 for headlines.
- **Line OCR.** Compared on "64 textlines drawn randomly from random scans in the Chronicling America collection" (the Carlson et al. sample, CER 0.015 for the EffOCR-Word model the dataset used) and on a hand-transcribed "randomly selected sample of 50 lines for each decade between 1850-1920" (CER 0.043 over the whole sample; 8.9% in the 1850s to 1.8% in the 1910s). These line samples are not released in the repositories I found.
- **End to end.** "Two student annotators labeled layout regions and hand-entered the texts for 10 full page scans, resolving all discrepancies by hand. The set consists of 597 content regions and 196,655 characters." CER 0.051 (0.044 after spell-checking), which includes layout errors.
- **Where the human-labelled set is hosted.** [dell-research-harvard/AmericanStoriesTraining](https://huggingface.co/datasets/dell-research-harvard/AmericanStoriesTraining), folder `gold_data/`: `gold_hand_transcription.json` (COCO format, 471,216 bytes) and 10 page images (1.7 to 8.1 MB each, 44.4 MB in `gold_imgs.zip`). Repository metadata: licence `apache-2.0`; the card text is empty. I checked the JSON: 10 images, 597 annotations, 196,655 characters of text, matching the paper's numbers, so this is that set. Annotation classes include article (307 regions with text), headline (199) and others.
- **Not confirmed.** The paper does not say where the 10 pages come from or what years they cover. Given that Chronicling America is 13% of the layout training sample, they are not necessarily Chronicling America pages: UNVERIFIED. The README is "silver" about the full dataset; this 10-page set is the only hand-made evaluation text in the repositories.

So American Stories evaluated its OCR on hand-transcribed text lines and 10 hand-transcribed full pages. The 10 pages are small but real ground truth, and they are public.

### Datasets surveyed

| Dataset                                                    | URL                                                                                                                               | Licence (as stated)                                                                                                                                       | Size                                                                                                                                                                                                                                                                                 | Format                                                                           | Language, period                                                                                                                                             | Images with transcription?                                                                                                                                              | Fits the pilot?                                                                                                  |
| ---------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| **Reichsanzeiger-GT** (UB Mannheim)                        | [10.5281/zenodo.10144094](https://doi.org/10.5281/zenodo.10144094); [GitHub](https://github.com/UB-Mannheim/reichsanzeiger-gt)    | GitHub LICENSE file: "CC0 1.0 Universal". Zenodo record: `cc-by-4.0`. Paper: images under "Public Domain Mark 1.0". The two repository licences disagree. | Zenodo zip 26.1 MB (PAGE XML only; images come from Mannheim's server by `download_images.sh`). 101 PAGE files and 101 image URLs in the repository; the README says 197 pages, METADATA.yml 117, the paper 101, so the count in the files is the one to trust. About 119,400 lines. | PAGE XML, line level, with baselines                                             | German (also English, French, Portuguese, Italian, Latin); Fraktur and Latin; 1820 to 1939                                                                   | Yes: line transcriptions and page images. Transkribus, OCR-D Level 2 guidelines (long s kept)                                                                           | German Fraktur: yes. A government gazette, not a daily paper                                                     |
| **American Stories gold set** (Dell Research)              | [HF AmericanStoriesTraining](https://huggingface.co/datasets/dell-research-harvard/AmericanStoriesTraining)                       | Hugging Face metadata: `apache-2.0` (card empty)                                                                                                          | 10 pages, 44.4 MB of images, JSON 0.47 MB                                                                                                                                                                                                                                            | COCO JSON, text per region (article, headline)                                   | English, 19th and 20th century newspapers; exact years and source UNVERIFIED                                                                                 | Yes: region boxes with hand-entered text, and page images                                                                                                               | English: yes, years uncertain                                                                                    |
| **NDL public-domain OCR set** (National Diet Library)      | [GitHub](https://github.com/ndl-lab/pdmocrdataset-part1)                                                                          | README: "PDM (パブリック・ドメイン・マーク)", Public Domain Mark 1.0, with a request to keep the free-use notice                                          | JSON form 5.2 GB (Content-Length from a HEAD request); PascalVOC form 1.8 GB (README)                                                                                                                                                                                                | JSON boxes with `text`, `orientation` vertical or horizontal; images beside them | Japanese, printed books from 1870 to the 1940s (directories by decade); books, not newspapers                                                                | Yes: line boxes with transcription                                                                                                                                      | Japanese: yes, as typeset print of the period; no newspaper layout                                               |
| NewsEye / READ Austrian Newspapers (ONB)                   | [10.5281/zenodo.3387369](https://doi.org/10.5281/zenodo.3387369)                                                                  | Zenodo record: `cc-by-4.0`                                                                                                                                | Training zip 166 MB, validation zip 16.5 MB; 148 + 13 pages                                                                                                                                                                                                                          | PAGE XML (Transkribus), images included                                          | German, 19th and early 20th century (the Mannheim revision says 1864 to 1911), Fraktur                                                                       | Yes: "carefully corrected text" at line level                                                                                                                           | German Fraktur: yes, and real newspapers. Wired too, with the same page reader, but the zip layout is UNVERIFIED |
| Europeana Newspapers GT (Berlin State Library, PRImA PAGE) | [10.5281/zenodo.2583866](https://zenodo.org/records/2583866)                                                                      | "Creative Commons Zero v1.0 Universal"                                                                                                                    | 50 pages; `img_full.zip` 31.0 MB, `gt_page.zip` 0.8 MB                                                                                                                                                                                                                               | PAGE XML (2010 schema), region level                                             | German, Fraktur ("Font class: gothic" in the XML)                                                                                                            | Images yes. The text in the XML I opened (50 files) looks like uncorrected OCR (garbled words, 1,415 lines with U+FFFD), so it is layout ground truth. Not used for CER | Layout only                                                                                                      |
| NZZ black-letter GT (impresso)                             | [10.5281/zenodo.7053264](https://zenodo.org/records/7053264); [GitHub](https://github.com/impresso/NZZ-black-letter-ground-truth) | LICENSE.txt: "Attribution-NonCommercial 4.0 International" (Zenodo says `other-open`)                                                                     | 472.6 MB zip; 167 front pages                                                                                                                                                                                                                                                        | PAGE XML, line level (word level for 134 years)                                  | German, Fraktur, 1780 to 1947, Swiss daily                                                                                                                   | Yes. But the guidelines normalise: long s becomes s, "sz" becomes ß, I/J spelled by modern rules                                                                        | Fits Fraktur newspapers well; not wired: non-commercial, and it normalises what our scorer counts                |
| BLN600 (British Library Newspapers, Sheffield)             | [10.15131/shef.data.25439023](https://doi.org/10.15131/shef.data.25439023)                                                        | CC BY-NC-ND 4.0 (Figshare v2 record)                                                                                                                      | `BLN600.zip` 68.1 MB (password protected; the password is in the record's description)                                                                                                                                                                                               | Cropped JPG/TIFF images, text files of the human transcription and of Gale's OCR | English, 1800 to 1900, 600 excerpts                                                                                                                          | Yes: image excerpts with transcription                                                                                                                                  | Original archive only; deployment is conditional on operator verification of source terms                        |
| GT4HistOCR                                                 | [10.5281/zenodo.1344132](https://zenodo.org/records/1344132)                                                                      | `cc-by-4.0`                                                                                                                                               | 4.0 GB tar; 313,173 lines                                                                                                                                                                                                                                                            | Line image (PNG) and `.gt.txt` pairs                                             | German Fraktur and Early Modern Latin, books, 15th to 19th century                                                                                           | Yes, line level                                                                                                                                                         | Fraktur typeface, but books, not newspapers. Too large for the benefit; not wired                                |
| ICDAR 2019 post-OCR competition                            | [10.5281/zenodo.3515403](https://zenodo.org/records/3515403)                                                                      | `cc-by-4.0`                                                                                                                                               | 54.3 MB zip                                                                                                                                                                                                                                                                          | Text: OCR and gold standard side by side                                         | Several languages; the record says the documents come from collections including BnF and the British Library, and that Finnish material is not redistributed | No images. Text only: cannot score our engines                                                                                                                          | No                                                                                                               |
| ICDAR 2017 post-OCR competition                            | not found on Zenodo                                                                                                               | UNVERIFIED                                                                                                                                                | UNVERIFIED                                                                                                                                                                                                                                                                           | UNVERIFIED                                                                       | UNVERIFIED                                                                                                                                                   | UNVERIFIED                                                                                                                                                              | Not checked: post-OCR competition data, so probably no images                                                    |
| Beyond Words (LoC) and Newspaper Navigator                 | [GitHub](https://github.com/LibraryOfCongress/newspaper-navigator), `beyond_words_data/`                                          | Repository licence: The Unlicense. A search summary says the annotations are CC0 / public domain (UNVERIFIED on the primary page)                         | 3,437 images, 6,732 verified annotations (README, as of 1 December 2019)                                                                                                                                                                                                             | COCO boxes for photographs, illustrations, cartoons, maps; captions              | English, World War I era Chronicling America                                                                                                                 | Boxes plus corrected captions only, not running text; the paper counts about 10,000 corrected annotations (UNVERIFIED)                                                  | English, but captions of pictures, not article text. Not used                                                    |
| KB historical newspapers OCR ground truth                  | [lab.kb.nl](https://lab.kb.nl/dataset/historical-newspapers-ocr-ground-truth)                                                     | "In copyright"                                                                                                                                            | 2,000 pages                                                                                                                                                                                                                                                                          | JP2 and ALTO                                                                     | Dutch, 1700 to 1995                                                                                                                                          | Yes                                                                                                                                                                     | Not available without writing to the KB, so not used                                                             |
| CODH modern magazines                                      | [codh.rois.ac.jp](https://codh.rois.ac.jp/modern-magazine/dataset/)                                                               | "CC BY 4.0"                                                                                                                                               | 2.2 GB; 1,985 pages, 59,465 lines                                                                                                                                                                                                                                                    | NDLOCR XML, line level                                                           | Japanese, Meiji era, two magazines                                                                                                                           | Yes                                                                                                                                                                     | Japanese print, magazines not newspapers; an alternative to the NDL set, not wired                               |

No public Japanese newspaper dataset with line transcriptions turned up (UNVERIFIED: this is absence in my searches, not proof). The NDL's own evaluation set is described as not public. Chronicling America itself has no human-corrected full-page text that I could find; see the last section.

### Recommendation

1. **BLN600, for English.** The recommended English source: 600 human-transcribed excerpts of 19th-century London newspapers, each image already cut to the excerpt. ND licence, so nothing derived is shared.
2. **Overproof dataset 3, for US newspapers.** Chronicling America pages from 1871 to 1922; the crops are cut from today's LoC images at the lines of today's ALTO. On 13 October 2026, 3,097 of 3,209 lines (96.5%) matched the 2013 OCR text, so LoC's reprocessing has not broken the match for these pages; unmatched lines give no crop.
3. **Reichsanzeiger-GT, for German Fraktur.** Line-level human transcription, page images fetched on demand, and a licence that allows redistribution under either statement. Its pages are a gazette, so layouts are denser and plainer than a daily paper's.
4. **Austrian newspapers and NZZ, for German Fraktur newspaper layouts.** Real newspapers, corrected by people; NZZ at line level only.
5. **NewsEye BnF, NLF Finnish and NLF Swedish, for French, Finnish and Swedish.**

There is no Japanese scoring source yet. American Stories and NDL fail the source policy and are kept only as comparison readers, as is the Europeana set (its text is not clean enough for CER). Overproof dataset 2 converts text only, since its Trove images have an unverified licence. GT4HistOCR is held back for fit.

### How it complements the 60 crops

|                 | Own 60 crops                                | Public sets                                                                                   |
| --------------- | ------------------------------------------- | --------------------------------------------------------------------------------------------- |
| Pages           | Ours: the worst-read cases per language     | Someone else's, not chosen for difficulty                                                     |
| Truth           | None yet                                    | Human transcription                                                                           |
| Number it gives | Agreement between engines, labelled as such | CER and WER: accuracy                                                                         |
| Use             | Which engine is the outlier on our material | Whether an engine is any good at the language and script, and whether a profile change helped |

Neither substitutes for the other. A public-set CER says nothing certain about our worst pages, and agreement on our pages says nothing about accuracy. Report them in separate tables with those labels. `score` prints the group per row (`pub-reichsanzeiger`, `pub-austrian`), so the public rows can't be read as the own-crop rows.

### Fetch and convert

For the steps to score an engine end to end, see the [scoring guide](ocr-scoring-guide.md).

`public-fetch` downloads a dataset to `--src`. It will not run without `--confirm-download`, because some downloads are large: the Reichsanzeiger images are 101 page JPEGs from Mannheim's server (paced one second apart, `--pages N` of them, 20 by default, in a fixed seeded order), the Austrian zips 183 MB, and the NDL zip 5.2 GB. Run it on any machine with network access and room for the files. `public-convert` reads what was downloaded and writes the layout `score` reads: `images/`, `manifest.jsonl`, `crops.jsonl`, with the dataset's transcription in `transcription`.

```sh
scripts/ocr-survey/gold.py public-fetch --dataset reichsanzeiger --src DATA/reichsanzeiger --confirm-download --pages 20
scripts/ocr-survey/gold.py public-convert --dataset reichsanzeiger --src DATA/reichsanzeiger --out OUT/reichsanzeiger --count 60
scripts/ocr-survey/gold.py validate --crops OUT/reichsanzeiger/crops.jsonl --manifest OUT/reichsanzeiger/manifest.jsonl --images OUT/reichsanzeiger/images
# engines' texts for the crops go in as --hyp ENGINE=FILE.jsonl, as for the own crops
cargo run --release -p usnm-ocr-survey -- score --crops OUT/reichsanzeiger/crops.jsonl --hyp tesseract-5.5=tess.jsonl --out OUT/reichsanzeiger/scores
```

`--dataset` is `bln600`, `overproof-ca`, `reichsanzeiger`, `austrian`, `newseye-bnf`, `newseye-nlf-fi`, `newseye-nlf-sv` or `nzz` for scoring. The NewsEye sets take `--subset validation|training|all` (default `validation`, 16 to 122 MB; the Finnish training zip is 5.4 GB). `bln600` checks the zip's sha256 before opening it; the zip is AES-encrypted with the password `BLN600` printed on the dataset page, which Python's zipfile can't read, so it needs `bsdtar` or 7-Zip. `overproof-ca` fetches loc.gov's issue JSON, then each page's ALTO and full image, paced as `locfetch` does, and `public-convert` writes the line match rate to `match.json`. Its crop geometry comes from matched ALTO strings. String text is retained even when its coordinates are invalid. Implausible dimensions are repaired from page medians when the TextLine bounds do not confirm them. Plausible TextLine bounds constrain String origins and extents and provide a fallback. Conversion fails if a matched nonempty line has no usable geometry. Matched lines are kept in one crop only while consecutive in ALTO and spatially adjacent; distant lines remain in separate crops rather than producing a page-spanning union. `overproof-smh`, `enp-berlin`, `americanstories` and `ndl-pdm` fail the [source policy](#source-policy); `public-convert` refuses them unless `--not-for-scoring` is given, and their rows then carry `for_scoring: false`. Don't quote their numbers as accuracy. A crop is one transcribed region (a block of lines for the first three, one line for NDL), chosen by seed: regions inside a length window (200 to 2,500 normalized characters; 12 to 300 for NDL lines, overridable with `--min-chars` and `--max-chars`), at most `--per-page` (3) per page, taking one from each page before a second from any. Each crop row carries `source_dataset`, `source_url` and `source_license`, and `transcriber` reads "`<dataset>`: published ground truth" with `reviewer` "not applicable: published ground truth", which is what lets `validate` accept it. Nobody here reviewed those transcriptions; the dataset's authors did, by the process they describe.

Three things were tested initially. Unit tests run on tiny synthetic inputs for every reader, the pick, and the end-to-end conversion (the last one needs Pillow, so it is skipped where Pillow is missing). A real run of `public-convert` on the American Stories JSON and one of its page images (0015.jpg, 1.7 MB) cut readable crops that line up with their text. The third is the set of real runs in the table below. The [18 October German diagnostic](#german-fraktur-cutover-diagnostic-18-october-2026) subsequently verified real Austrian archive parsing and conversion; the earlier inventory table's unverified-layout note is historical. Reichsanzeiger image retrieval returned HTTP 503, so its conversion still lacks real image-backed coverage. The NDL reader follows the README's example and expects `json/<dir>/<name>.json` beside `img/<dir>/<name>.jpg`, UNVERIFIED until it runs on the real files.

Real runs on 13 October 2026 (`public-fetch`, then `public-convert`, then `validate` with 0 errors):

| Dataset        | Downloaded                | Regions                                            | Crops written                     |
| -------------- | ------------------------- | -------------------------------------------------- | --------------------------------- |
| bln600         | 68 MB zip, sha256 checked | 600                                                | 600 (`--count 600 --per-page 1`)  |
| overproof-ca   | 18 LoC pages              | 414 runs of matched lines (96.5% of lines matched) | 354 (`--count 600 --per-page 50`) |
| overproof-smh  | text only                 | 159 articles                                       | 159 text-only rows                |
| newseye-bnf    | validation zip            | 1,332                                              | 24                                |
| newseye-nlf-fi | validation zip            | 372                                                | 18                                |
| newseye-nlf-sv | validation zip            | 78                                                 | 10                                |
| nzz            | 472 MB zip                | 43,160 lines                                       | 60                                |
| enp-berlin     | 32 MB                     | 2,458                                              | 60 (comparison only)              |

A sample score: Gale's own OCR for the first 20 BLN600 excerpts against the transcriptions gave CER 0.049 and WER 0.164 over 53,636 characters. For all 600 excerpts it gave CER 0.0684 and WER 0.2151 over 1,696,781 characters.

Caveats when reading the CER of a public set:

- The scorer folds nothing but the line-end hyphen `-`. Reichsanzeiger and Austrian text uses the double oblique hyphen `⸗` for a word broken across lines, and keeps long s and other old forms. An engine that prints `-` or `s` is charged for it. Read the numbers as "against this transcription convention", and compare engines with each other on the same crops.
- NZZ marks a line-end hyphen with `¬` and transcribes long s as `s`; Overproof's `¬` is turned into `-` on conversion.
- The NDL text is "as seen", old character forms included, and has no spaces, so WER is meaningless for `jpn`; use CER.
- A crop is a region of a page, and a region of a Reichsanzeiger page can be mostly figures and tables. The window above filters by length only.

## Volunteer bundle

BOINC volunteers score their engine on a fixed subset of the public sets, the gold bundle ([#457](https://github.com/tgoodyear/usnewsmap.com/issues/457)). It is versioned and checksummed, so every reported score names the exact crops it was measured on.

**Contents.** `usnm-ocr-gold-v<N>/` holds `manifest.json`, `crops/` and `licenses/<source>/`, and ships as a tar with a `.sha256` beside it. The manifest carries `bundle_version`, the seed, `files` (path, size and sha256 of every other file in the bundle), `sources` (licence, URL, attribution, how it ships, and its licence files), `remotes` (the original files the client downloads: URL, sha256, and the published password of BLN600's zip) and `crops` (id, source, language, group, where the image comes from and where the transcription comes from). The client reports `bundle_version` and the manifest's sha256 with every score.

**What ships is decided by the licence** (`bundle::policy` in the crate; a source not listed there can't go into a bundle):

| Source                                           | Licence                                  | Ships                                                                                                                                                                                        |
| ------------------------------------------------ | ---------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `bln600`                                         | CC BY-NC-ND 4.0                          | References only: the zip's URL and sha256, the image and transcription members' names and checksums. The client receives the unchanged zip from the project mirror and reads both unchanged. |
| `overproof-ca`                                   | CC BY-NC-SA 4.0 (text); LoC images       | The transcription and the box. The client downloads the LoC page image (URL and sha256 in the manifest) and cuts the box.                                                                    |
| `newseye-*`, `nzz`, `austrian`, `reichsanzeiger` | CC BY 4.0, CC0, CC BY-NC (NZZ's archive) | The crops and transcriptions.                                                                                                                                                                |

Every source's licence files go in `licenses/<source>/`, unchanged, with `SOURCE.json` and an `ATTRIBUTION.txt` that names the source, licence, licence deed, attribution and the changes made (for shipped crops: cut, converted to greyscale, re-encoded as JPEG). The builder refuses a source with no attribution, so `gold.py` now carries attribution for Reichsanzeiger (UB Mannheim) and Austrian (NewsEye/READ, ONB) as well. ShareAlike text (Overproof) ships with its licence and readme files. No BLN600-derived crop, image or text is included in the bundle. Its unchanged source `LICENSE` and `readme.md`, plus `SOURCE.json` and `ATTRIBUTION.txt`, are preserved under `licenses/bln600/`; the unchanged source zip is delivered separately.

**Building.** The bundle is built from `gold.py public-convert` outputs, reproducibly: the same inputs, version and seed give the same tar, byte for byte (no timestamps in the manifest; tar entries sorted, mtime 0, fixed modes). The pick spreads over pages: it puts the pages, and the crops in each page, in a seeded order, then takes them round robin, up to `--per-source`. Overproof needs `page_image_url` in its manifest rows, which `public-convert` now writes from `loc/pages.json`.

```sh
cargo run --release -p usnm-ocr-survey -- bundle-build --version 1 --per-source 30 --out OUT/bundle \
  --source bln600=OUT/bln600 --original bln600=SRC/bln600/BLN600.zip \
  --source overproof-ca=OUT/overproof-ca --source newseye-bnf=OUT/newseye-bnf \
  --source newseye-nlf-fi=OUT/newseye-nlf-fi --source newseye-nlf-sv=OUT/newseye-nlf-sv --source nzz=OUT/nzz \
  --lock fixtures/ocr-gold/bundle-v1.lock.json
cargo run --release -p usnm-ocr-survey -- bundle-verify --bundle OUT/bundle/usnm-ocr-gold-v1.tar
```

The deployed BOINC server can run the same build inside its VNet and check it against the lock: [Build the gold bundle](boinc.md#build-the-gold-bundle). `--original` is the downloaded archive of a reference-only source; the builder checks it against `SOURCE.json` and reads the members to record their checksums, and copies nothing from it. No dataset content is committed. The repository holds only the lock, `fixtures/ocr-gold/bundle-v1.lock.json`: version, seed, crop ids, file checksums, remotes, the manifest's and the tar's sha256.

**Version 1** (built 2026-10-13, seed 457, 30 per source): 142 crops, 30 BLN600, 30 Overproof CA (from 18 LoC pages), 24 NewsEye BnF, 18 NewsEye NLF Finnish, 10 NewsEye NLF Swedish and 30 NZZ lines. The tar is 9.6 MB (9.1 MB of shipped crops, a 140 KB manifest, 128 KB of licence files). A client also downloads 19 remotes: the 68 MB BLN600 zip and 18 page images. All 18 LoC page images matched the checksums recorded at conversion when fetched again on the same day.

**Client.** `usnm-boinc gold-stage` (network) and `usnm-boinc gold-run` (offline), in [boinc.md](boinc.md#gold-check). For `usnm_gold` (#462), the server-side prepare step fetches each original once into its cache; submission verifies the MD5 and SHA-256 from `prepared.json` and stages ordinary sticky project downloads. Volunteers receive the unchanged archive and image files from the project server, then cut crops locally; the run step stays offline ([boinc.md](boinc.md#as-boinc-work-usnm_gold)). A run with Tesseract 5.5.1 and only `eng` installed scored the 60 English crops (CER 0.0932 on BLN600, 0.0276 on Overproof CA) and reported the other 82 as skipped for lack of a model.

### Current-worker observations, 16 October 2026

For [#527](https://github.com/tgoodyear/usnewsmap.com/issues/527), the published
worker `80bf2d6586fb3a9f6ef644937ddd40ca2304b1b433d3b9e657e193ba6f573ae0`
was measured offline on pve-big CT 100 (AMD64) and the local Mac's ARM Linux
Docker runtime. Both used the locked v1 tar (`42520de4...`) and manifest
(`150e20d7...`); all 19 original cached inputs were verified by SHA-256 before
copying to isolated directories. No source download or live-client cache write
was needed. Runs used one CPU, 1.5 GiB, no network, PSM 3, DPI 300 and a
300-second per-crop timeout. The architecture-specific Tesseract bases were
`d0b841b521a4a68aab2d585a019d9ad1d011e25a65f2f9286d6851fcc64ded57`
(AMD64) and `86418d991a798c885a7a505bc94ce27cbfe0c5361fa3dde319b7d6b400d9ebad`
(ARM).

Both scored **142 crops with zero skips**, 125,622 reference characters:
CER **0.090239**, WER **0.259798**. All 142 paired crops had identical
character/word edit and reference counts across architectures; this is score
parity, not a claim of byte-identical OCR text. Summed per-crop OCR wall time
was 518.058 seconds on AMD64 and 236.944 seconds on ARM, not CPU time or a
controlled machine-performance comparison.

| Source / language      | Crops | Reference characters |      CER |      WER | Exploratory holdout CER |
| ---------------------- | ----: | -------------------: | -------: | -------: | ----------------------: |
| BLN600 / English       |    30 |               87,576 | 0.096259 | 0.248056 |                0.067381 |
| NewsEye BnF / French   |    24 |                9,973 | 0.089141 | 0.232764 |                0.190691 |
| NewsEye NLF / Finnish  |    18 |               11,895 | 0.079361 | 0.431330 |                0.071369 |
| NewsEye NLF / Swedish  |    10 |                6,267 | 0.108186 | 0.464625 |                0.107829 |
| NZZ / German           |    30 |                1,565 | 0.123323 | 0.383929 |                0.160628 |
| Overproof CA / English |    30 |                8,346 | 0.024203 | 0.095533 |                0.019124 |

The split was fixed in the issue before reading baseline scores: group by
`remote|member-or-path` (`bundled|path` for bundled images), then odd SHA-256
parity of `usnm-current-20261016|<group>` selects holdout. Page-region crops
sharing one remote stay together. Bundled crops lack original-page identifiers,
so their split is **exploratory, not proven page-independent**; the existing
public bundle is not a new unseen corpus. In particular the French holdout
differs substantially from its analysis cohort (CER 0.047928).

Two actual AMD64 controls scored all 142 crops: empty output gave CER/WER 1.0;
the same worker with deliberately unsuitable PSM 10 gave CER 0.986977 and
WER 0.992874 overall. PSM 10 nevertheless **improved NZZ's line crops** to
CER 0.073482. A control need not degrade
every source; an overall threshold obscures this distinction and BLN600's
69.7% character weight. French/Finnish/Swedish coverage is now measured rather
than the historical worker's 52 missing-model skips.

These are offline diagnostics, **not canonical BOINC host calibration**.
No production trust threshold was derived, no legacy `0.1` trust promoted,
and no assignment/backpressure bypass or client-preference change made.
Current-image canonical campaign acceptance, independently held-out material
and the fresh Mac OCR lifecycle remain separate gates.

The production inventory on 16 October found 18 canonical Gold reports, all
with `boinc.engine_image` equal to the historical `9c8eac04...`, including the
newly observed host 8 result. Registered Linux Gold versions 1.0, 1.1 and 1.2
all referenced `Dockerfile_1`, whose published contents still pinned that old
worker. Updating campaign metadata or `images.worker` does not update an
already signed application. A current-image campaign therefore needs a new
immutable signed Gold app version as well as matching job metadata. Gold
1.3 is prepared for Linux x86_64 `docker`, pinned to the measured `80bf2d65...`;
preparation alone is not registration, canonical execution or trust promotion.
Its publication does not authorize additional workunits or change the source,
assignment, backlog or trust gates.

### Current-image canonical CC-BY-only measurement, 17 October 2026

Gold 1.3 publication completed successfully in execution
`usnm-boinc-publish-caj-prod-nbtft5q`; the native database registered Linux
`docker` version 103 as **app_version 28**. The registered signed Dockerfile
and its public download both pin the measured `80bf2d65...` worker. The
publisher-only update preserved all other template, configuration and identity
properties and did not replace the concurrent server/bridge runtime.

At the time of this measurement, verification of NonCommercial-use eligibility
for the full v1 campaign was pending. The normal builder therefore prepared a separate **52-crop,
CC-BY-4.0-only** bundle from the existing converted NewsEye validation inputs:
BnF 24, Finnish 18 and Swedish 10. No BLN600, Overproof or NZZ content is in it.
All source licence files and attribution travel unchanged with the crops.
There are **zero remote inputs** and no source downloads were needed.

```sh
usnm-ocr-survey bundle-build \
  --source newseye-bnf=/project/gold/build-v1/out/newseye-bnf \
  --source newseye-nlf-fi=/project/gold/build-v1/out/newseye-nlf-fi \
  --source newseye-nlf-sv=/project/gold/build-v1/out/newseye-nlf-sv \
  --version 2 --seed 457 --per-source 30 \
  --out /project/gold/ccby-newseye-20261016-80bf/build \
  --lock /project/gold/ccby-newseye-20261016-80bf/bundle-v2.lock.json
```

The normal `bundle-verify` and `gold prepare` passed. Immutable receipts:

| Artifact                     | SHA-256                                                            |
| ---------------------------- | ------------------------------------------------------------------ |
| Manifest                     | `3fcafeabc8e77ad1118a724a0e844d7c8def74022e0c4b31178599e6ddecfee6` |
| Bundle tar, 8,893,952 bytes  | `9e13ed3a2294889c76efd1ba325f0edca725e321f659410631139c657c8fe547` |
| Canonical counts-only report | `8fad861dbd3f5a112babbb33d23dc6473efbec01ca1b4f4c62dc5a44db2ca5b7` |

After all six lifecycle jobs were idle, one normal targeted Gold measurement
was submitted under the controller lease to existing **host 8 / user 2**.
No client preference, task priority, abort, trust or OCR queue-admission override
was used. Actual **WU 16571 / result 31526 / app_version 28** finished with
outcome 1, validation state 1 and exit status 0; result 31526 became canonical,
the workunit reached assimilation state 2 and its error mask is zero.
The actual canonical database host matches the report's host claim. Its job
is `ae4e2cd9e6e86201e368819be699fcfc7c7826ec6cfe4a3d4554c9b2729564e9`,
with exact current worker/base, Tesseract 5.5.0, PSM 3, DPI 300 and timeout
300 seconds. The deployed validator and normal `gold collect` both passed.

All **52 unique crops, zero skips**, have exactly the same per-crop source,
profile and integer character/word edit and reference counts as the offline
baseline above: 28,135 reference characters, 2,511 edits, **CER 0.089248**.
The normal collector's reports and crop tables are retained under
`/project/gold/ccby-newseye-20261016-80bf/collected`; the immutable job, prepared
metadata and text-free bundle lock remain in that campaign root. The genuine
report is in the stock assimilator's `results/0/` spool, named
`usnm_gold_<job>__file_gold-result.json`.

This establishes actual current-image canonical execution and measured
French/Finnish/Swedish coverage on host 8, **not English accuracy or Mac host
calibration**. These are reused validation crops, not an independent unseen
holdout. No production threshold was inferred and no trust was promoted.
Subsequent [full-v1 canonical verification in #527](https://github.com/tgoodyear/usnewsmap.com/issues/527#issuecomment-6227645406)
established four successful `80bf2d65...` executions on hosts 6, 9, 2 and 8:
all 142 unique crops, zero skips, and exact per-crop integer-count parity with
the offline baseline (125,622 reference characters, 11,336 edits, CER
0.09023897088). This includes the English benchmark coverage missing from the
separate CC-BY-only bundle, but is still reused benchmark material, not an
independent unseen holdout. Four hosts are independent executions, not four
independent truth sets.

The [recorded project-purpose fact in #526](https://github.com/tgoodyear/usnewsmap.com/issues/526#issuecomment-6227671263)
resolves the earlier factual NonCommercial-purpose unknown. It does not waive
NoDerivatives, attribution or other licence conditions; the original archive
and licence/attribution files remain unchanged. A defensible trust disposition,
Mac calibration, and the fresh Mac OCR publication/search lifecycle remain open.

### Prepared Mac Gold sibling, 17 October 2026

ACR build `ch6r` prepared the signed Gold 1.3 `arm64-apple-darwin` / `docker`
sibling, with all three payload signatures verified. Its publisher image is
`crusnmprodffapzy.azurecr.io/usnm-boinc-appver@sha256:89a345d9a6f765cae467448438ee61ba70066562dcdff1280f816064aed9880c`.
It deliberately pins the same measured `80bf2d65...` worker as Linux Gold 1.3;
the newer production worker pin is not rolled back or implicitly calibrated.
Preparation is not publication, native registration, Mac execution or host
trust. A Mac job must name the ARM base, not the Linux AMD64 base. Existing
Linux Gold 1.3 payloads must remain unchanged when publishing this platform
sibling. No additional workunit or queue-admission exception is authorized by
this artifact pin.

### Canonical Mac Gold measurement, 17 October 2026

After the reviewed release in #741, publisher execution
`usnm-boinc-publish-caj-prod-twa4osl` succeeded and registered Gold 1.3
`arm64-apple-darwin` / `docker` as **app_version 38**. The image-only update
preserved the publisher's other template, configuration and identity settings.
Existing Linux Gold 1.3 remained registered as app_version 28.

The ordinary scheduler initially sent Mac host 1 two existing generic Gold
jobs with incompatible historical/AMD64 bases. WU 15712 / result 32108 and
WU 15721 / result 32109 both exited 1 before OCR: the strict engine-base check
correctly rejected them. They are not calibration measurements. Generic
architecture-safe Gold routing remains tracked in [#747](https://github.com/tgoodyear/usnewsmap.com/issues/747);
publishing a platform sibling does not by itself constrain queued jobs to
that platform's base.

With zero pending Mac Gold and zero global unsent Gold, the normal preparer
verified a separate campaign at `/project/gold/mac-arm-20261017-80bf` from
the existing unchanged full-v1 bundle and cached originals. Its prepared
metadata is identical to the Linux campaign's 142-crop expectations; no
source downloads or modified original archives were needed. With all six
lifecycle jobs idle and the controller lease held, one ordinary targeted
Gold job was submitted to existing **host 1 / user 2**, naming the ARM base
`sha256:86418d991a798c885a7a505bc94ce27cbfe0c5361fa3dde319b7d6b400d9ebad`.
No client preference, priority, abort, trust or fresh-OCR admission override
was used.

Actual **WU 16733 / result 32110 / app_version 38** completed with outcome 1,
validation state 1 and exit status 0. Result 32110 became canonical; the
workunit reached assimilation state 2 with error mask zero. The native
canonical database host matches the report's host claim. The deployed
validator passed and normal `gold collect` reported one collected, zero
pending and zero failed. The canonical counts-only report SHA-256 is
`0a41d70086c4bed79d89489f293853a7bc3fc64fb0e94386145e3841618f32fb`;
its job ID is `d0f4b1eada04766e01f2a8ac03972ff86b66ab9d6d6cd992213cfedffa5e9460`.
The immutable job, prepared metadata and collector tables remain in that
campaign root.

After receipt-hash verification, all **142 unique crops, zero skips**, match
both offline ARM and AMD64 baselines exactly in source, profile, and integer
character/word edit and reference counts: **125,622 reference characters,
11,336 edits, CER 0.09023897088**. This establishes actual Mac BOINC
execution/validation/collection and cross-architecture benchmark parity
for the pinned `80bf2d65...` worker. It does not calibrate newer worker
`247c131a...`, supply independent held-out truth, establish a production
threshold, promote trust, or complete the fresh-page OCR/native
handoff/release/search lifecycle. The [native acceptance receipt](https://github.com/tgoodyear/usnewsmap.com/issues/527#issuecomment-6228584575)
records the same boundaries.

### Gold architecture and image routing

The scheduler's Gold-specific compatibility hook reads the authoritative
staged `<job-id>_gold_json`, including for work queued before the hook was
deployed. Before dispatch, it requires the selected app's registered
Dockerfile to pin that job's exact worker image, and requires the job's base
to match the reviewed image/platform entry in
`contrib-compute/boinc/gold-engine-platforms.json`. Missing or malformed
metadata, anonymous app versions, unknown bindings, and mismatches refuse
dispatch with an explicit `[Gold routing]` scheduler log. Filesystem reads
run only in the unlocked slow check, assignment, locality, and lost-work
resend paths, not
the fast check under the global scheduler semaphore. Job inputs use the
workunit fanout; registered application Dockerfiles use the flat download
directory. Incompatible locality candidates are skipped without stopping
the search; incompatible lost-work resends are skipped without expiring
the existing result. Other apps retain their existing routing.

Adding a new Gold worker requires independently verifying its per-architecture
base digests and reviewing the registry entries before deployment; this is
execution compatibility, not accuracy calibration or host trust. A refusal
may leave a stale workunit pending for operator disposition rather than
sending it to an incompatible host. Do not change signed payloads, weaken
the worker/validator identity checks, or abort/reprioritize existing jobs
to disguise this gate.

## Licensing

The library believes the newspapers in Chronicling America are in the public domain or have no known copyright restrictions, and that those published more than 95 years ago are in the public domain in their entirety; newer ones may hold copyrighted third-party material ([Rights and Access](https://www.loc.gov/collections/chronicling-america/about-this-collection/rights-and-access/)). Pages in the sample run to 1963, so in 2026 crops from pages published in 1931 or later carry that caveat. Transcriptions are original work and go in under the repository's licence.

## Human-corrected text from LoC

Checked in public documentation only, nobody contacted.

- **Confirmed.** LoC's OCR text and ALTO for Chronicling America pages are machine-made. The survey's own LoC text is that machine-made text.
- **Partly confirmed.** The Newspaper Navigator paper ([arXiv:2005.01583](https://arxiv.org/abs/2005.01583)) says its training annotations were collected in LoC's Beyond Words crowdsourcing initiative. A search summary of the paper describes volunteers drawing boxes around photographs, illustrations, comics, cartoons and maps in World War 1-era pages and correcting the OCR of their captions, about 10,000 corrected annotations. That covers captions on illustrations in one era, not running article text, and the abstract page I fetched did not carry those details. UNVERIFIED: the count, the exact era, and whether the corrected captions can be matched to the sample's pages.
- **UNVERIFIED.** Whether LoC publishes corrected full-page text for any Chronicling America page. I found none in the documentation I could reach; the About page returned HTTP 403 to the fetch tool.
- **Not used.** None of this is in the gold set. Even if it were, volunteer corrections would need the same second-pass review before counting as truth.

## Open

- A person transcribes the 60 crops and the Japanese crops, or the 60 stay scored as agreement only.
- Run `public-fetch` and `public-convert` for Reichsanzeiger and Austrian, and check the readers against the real files.
- Decide whether to commit the images once `cut` has run and the total is known.
- `usnm-ocr-survey score` takes over the span-finding for whole-page runs and reads this format as `--truth`.
