# Tesseract 5.5.4 beside 5.5.0

Issue [#1835](https://github.com/tgoodyear/usnewsmap.com/issues/1835), steps 1 to 3: build the native Linux `usnm_tesseract` from upstream 5.5.4 without changing any default, compare it with the 5.5.0 build on the same pages, and time both. Measured on 12 November 2026 on the build container. Nothing here changes a default build, a lock file production uses, an app version or a model.

## Result in brief

- **The output changes.** Text differs between 5.5.0 and 5.5.4 on all 63 reads (33 pages, 30 challenge crops). The 5.5.0 build made from the upstream tarball through the same recipe, and a plain upstream build of 5.5.4, rule out the build as the cause: see [Build](#build). The issue's premise that nothing changes the recogniser does not hold for our pages.
- **Accuracy barely moves on the challenge and is mixed on pages.** On the 30 calibrated English crops the character error rate goes from 7.16% to 6.81% (17 crops better, 12 worse, 1 equal); one crop (193) accounts for 280 of the 288 edits saved. On 6 English pages with a human reference it goes from 10.8% to 12.1% (1 page better, 5 worse; one page, asgold-005, goes from 33.0% to 53.5%). On German pages the two differ by under 0.1 point.
- **ALTO changes in every class.** The version moved to its own element on all 63 files, 5.5.0's confidence formatting is gone, and paragraph and block counts differ on 34 of 63 files.
- **5.5.4 is faster on dense English pages**, by about a third on the densest: see [Timing](#timing).
- **Our ALTO readers take 5.5.4's ALTO.** Their tests pass unchanged, and all 63 real 5.5.4 files parse, rewrite and parse again.
- **Re-calibration is needed.** Gold results and host trust bind to builds, and the output differs, so the upgrade needs a new challenge run and new parity runs, as the issue's later steps say.

## Build

New files, all outside the default build:

| File | What it is |
| --- | --- |
| `boinc/apps/native/tesseract-upstream.lock` | Version, tarball URL, commit and sha256 for 5.5.0 and 5.5.4, the same discipline as `macos-sources.lock`. |
| `boinc/apps/native/Containerfile.upstream` | Builds `usnm_tesseract` from a locked tarball. It starts from the default Containerfile's `apt-native` stage, so Leptonica, the image libraries, the compiler and every other package are the same. |
| `boinc/scripts/build-native-tesseract-upstream.sh` | `--version 5.5.4 --out FILE`: builds the `apt-native` stage, then `Containerfile.upstream`. `--network host` is for build hosts where podman's default network doesn't start. |

The recipe deletes Debian's `libtesseract` and its headers, then builds `libtesseract.a` from the tarball with Debian's `debian/rules` options: `--disable-tessdata-prefix`, hardening `+all`, `-g -fPIC`, the `TESSDATA_PREFIX` define, and `-lleptonica -Wl,-z,defs` in `LDFLAGS`. `main()` is compiled and the binary is linked exactly as in the default Containerfile, with the same `stubs.c`. The tarball is checked against its sha256 and against the commit in its tar header.

| | 5.5.4 |
| --- | --- |
| Tag | `5.5.4` (28 October 2026) |
| Commit | `7e548e6eff172f38f97664afe7f7ce40db36c8d6` |
| Tarball sha256 | `a8dbf8b8c8ee427ff6ad1d99116502feaf59440af1adb893caf60a6576826cf4` |
| `usnm_tesseract` sha256 | `647c9d7354b1d0faf1be6d73ab08542f562a2194200c18b3841453e2fa92960b` |

Two flags differ from `debian/rules`, and neither changes code generation. `debian/rules` passes `dpkg-buildflags --get CFLAGS` as `CXXFLAGS`. This recipe passes `--get CXXFLAGS`, which lacks `-Werror=implicit-function-declaration`: 5.5.4's configure probes `-std=c++17` with `-Werror`, and g++ warns that this flag is not valid for C++, so the probe fails. 5.5.4's configure then chooses `-std=c++20` (read from the generated Makefile), so the library is compiled as C++20; 5.5.0's `configure.ac` has the same probes, but the Debian 5.5.0 library's standard wasn't checked. The control below checks that this doesn't matter.

**The default build is unchanged.** `git diff origin/main` is empty for `boinc/apps/native/Containerfile`, `stubs.c` and `base.lock`. The default build was run twice, once from this branch's files and once from `git archive origin/main` with `--no-cache`. Both produced `usnm_tesseract` sha256 `21877bc34785b43a6e405c8f41ee62261fb59ee377a1cc104d3577c792ba9904`, the digest of the build used for the 1 November parity and challenge runs.

**The build path is not the cause of the output differences.**

- The 5.5.0 tarball is byte for byte Debian's `tesseract_5.5.0.orig.tar.gz` (sha256 `f2fb34ca…`). Built through this recipe it gives a different binary (`c0ea4ed8…`, it has its own `libtesseract`), with identical text and ALTO to the default build on all 30 challenge crops and on 9 pages (`carson-02` and `cg-001` to `cg-004` under `frak2021` and `deu_latf+deu`).
- A plain upstream 5.5.4 (`./configure && make`, default flags, dynamically linked) gave the same text and ALTO as the 5.5.4 static build on 5 of the challenge crops (022, 056, 136, 193, 278).

### Debian's patches to 5.5.0

The source package the Containerfile downloads has `debian/patches/series` with two entries.

| Patch | What it does | In 5.5.4 upstream? | Carried? |
| --- | --- | --- | --- |
| `0001_helptext` | Adds a documentation URL to the `OCR Engine modes (OEM)` heading of the `--help-oem` text in `src/tesseract.cpp`. | No: 5.5.4 still has the plain heading. | No. It changes help text a task never prints. |
| `0002_disable_NEON.diff` | Adds an `arm` case to `configure.ac` for armel. | No. | No. It is commented out of `series` (`# 0002_disable_NEON.diff`), so it isn't applied to 5.5.0 either, and it only concerns armel. |

`debian/rules` changes no source. The 5.5.0 control above is also what shows the patches don't matter to output.

## Method

Every read ran `tesseract input.<ext> page -l PROFILE --psm 3 --dpi 300 txt alto` with `OMP_THREAD_LIMIT=1` and the 23 locked models of the 1 November native build, as the worker runs it (`crates/usnm-ocr-worker/src/engine/tesseract.rs`, `crates/usnm-boinc/src/native.rs`). Both versions ran the same copy of the same image from the same directory. `boinc/scripts/compare-tesseract-builds.sh` drives the runs and `boinc/apps/native/tesseract-compare` compares them; the character error rate is over whitespace-collapsed text with line-end hyphens joined (the gold scorer's normalization without its Unicode normalization).

The 56 parity pages of 1 November are not on the build container, so they were not re-run. The sets are:

- **Calibrated English challenge**: the 30 BLN600 crops of `usnm-ocr-gold-v3`, profile `eng`, read directly. The gold unit can't run a 5.5.4 build: the test fixture writes engine version 5.5.0 into the job, and the job's identity covers it. As a check on the harness, the 5.5.0 character edits it counts equal the gold result of the 5.5.0 native build for all 30 crops.
- **English pages**: 6 Chronicling America JP2 pages of the Carson City canary set (`eng`, no reference), and 6 American Stories gold pages (`eng`, human transcription).
- **German pages**: the first pages by rank of the Chronicling Germany set `de-cg` of `s9-heldout`: 13 under `frak2021` and 8 under `deu_latf+deu`.

Profiles not run: `srp+srp_latn` and the other single-model profiles. Page CER is against the whole-page reference and includes reading order, so it is high for multi-column German pages and above 100% for `cg-008`, which has over 300 paragraphs. Compare the two versions' numbers with each other, not with the challenge.

## Text and accuracy

Text identical: 0 of 63.

| Set | Reads | CER 5.5.0 | CER 5.5.4 | 5.5.4 better / worse / equal |
| --- | ---: | ---: | ---: | ---: |
| `eng` challenge crops | 30 | 7.16% | 6.81% | 17 / 12 / 1 |
| `eng` American Stories pages | 6 | 10.78% | 12.13% | 1 / 5 / 0 |
| `frak2021` pages | 13 | 58.15% | 58.14% | 5 / 5 / 3 |
| `deu_latf+deu` pages | 8 | 85.84% | 85.91% | 7 / 1 / 0 |
| `eng` Carson City pages | 6 | no reference | no reference | |

Without crop 193 the challenge is 3,767 edits for 5.5.0 and 3,759 for 5.5.4 on 78,978 reference characters. The challenge is calibrated on 5.5.0's output, and the 5.5.4 crop results move in both directions, so its threshold would need a new calibration run.

## ALTO differences

Classes, over all 63 files (the per-file counts are in the tables below):

| Class | What was counted | Count |
| --- | --- | --- |
| (a) Version element | Files whose `<Description>` differs | 63 of 63 |
| (b) Block and paragraph boxes | `ComposedBlock` boxes that differ, by position, in the 41 files with equal block counts | 472 of 1,105 blocks, on 11 files |
| | `TextBlock` (paragraph) boxes that differ, in the 29 files with equal paragraph counts | 12 of 253, on 6 files |
| | Files whose paragraph count differs | 34 of 63 (5.5.4 has more on some, fewer on others) |
| | Files whose block count differs | 22 of 63 |
| (c) Word boxes | Words matched by text whose box differs | 2,673 of 113,940, on 48 files |
| (d) Word confidences | Matched words whose `WC` text differs | 59,458 of 113,940 |
| | Of those, differing by more than 0.01 | 33,536 |
| (e) Other | Words with no match in the other version (5.5.0 / 5.5.4) | 26,388 / 25,151 of 140,328 / 139,091 |
| | `TextLine` boxes that differ, by position | 11,223 |
| | `SP` elements that differ, by position | 102,630 |
| | Element ids that differ, by position | 96,981 |

What is behind each class:

- **(a)** 5.5.0 writes `<softwareName>tesseract 5.5.0</softwareName>`. 5.5.4 writes `<softwareName>Tesseract</softwareName>` and `<softwareVersion>5.5.4</softwareVersion>`. Nothing else in the description changes.
- **(b)** Where the paragraph counts match, paragraph boxes are mostly identical (12 of 253), which fits upstream's change that makes a paragraph's box independent of the iterator. Where the counts differ, the segmentation itself differs, because the recogniser's output differs, and by-position counts for blocks, lines, spaces and ids then count the shift, not a box change. Those by-position numbers are an upper bound.
- **(c)** Word boxes differ on 2.3% of matched words.
- **(d)** Two causes. 5.5.0 writes `WC="0.` followed by the integer percent, so 1 to 9 percent prints as 0.1 to 0.9 and 100 as `0.100`; 5.5.4 writes the percent divided by 100 with two decimals (`altorenderer.cpp`). That produces the largest differences (up to 0.96) and explains `0.0` against `0.00`. The rest are real changes of a few hundredths in the recogniser's confidence, such as 0.51 to 0.47.
- **(e)** Words with no match come from the text differences. Line, space and id counts follow from them.

### Our ALTO readers

Run on the build container with `cargo test --locked -p usnm-alto -p usnm-corpus-signals -p usnm-ocr-worker --no-fail-fast`:

- Unchanged tree: 458 tests, 0 failures.
- With the version element of the three Tesseract fixtures (`tesseract-5.5.xml`, `tesseract-synthetic-columns.xml`, `tesseract-agave-1894-06-23.xml`) rewritten to 5.5.4's form: 458 tests, 0 failures.
- A scratch test (not committed) that reads every real ALTO file with `usnm_alto::parse`, runs `split_columns`, writes the page with `usnm_alto::write` and parses that again: all 63 files from 5.5.0 and all 63 from 5.5.4 pass, with every word confidence in 0 to 1.

Nothing fails. Two readers see the change in the version element. `usnm-ocr-worker`'s `loc_software.rs` maps a `softwareName` containing `tesseract` to the Tesseract family, which `Tesseract` still does, and reads `softwareVersion` separately. `usnm-corpus-signals`' `confidence.rs` keeps the first 64 characters of the first `softwareName`, so for 5.5.4 it holds `Tesseract` and no version. The worker reads the engine's own ALTO with `usnm_alto::parse` (`contract.rs`), which doesn't look at the software element.


## Timing

User CPU seconds from `getrusage` of each run (`time`'s user column), not wall time: the build container's load average was between 20 and 35 on 4 cores throughout. Each page was read three times by each version, the runs of one page alternating (5.5.0 first, then 5.5.4 first, then 5.5.0 first), three pages at a time with `OMP_THREAD_LIMIT=1`. The paragraph count is the number of `TextBlock` elements in that version's ALTO. The speed fix in 5.5.4 is said to depend on paragraph count, but the paragraph counts here are near each other for both versions, and the gain does not track them: `cg-005-frak` has 154 and 156 paragraphs and runs in 0.74 of the time, `asgold-001` has 152 and 180 and takes 0.83.

| Profile | Page | Paragraphs 5.5.0 / 5.5.4 | Runs | 5.5.0 median (min-max) | 5.5.4 median (min-max) | 5.5.4 / 5.5.0 |
|---|---|---|---|---|---|---|
| eng | asgold-006 | 333 / 303 | 3 | 401.8 (385.6-421.9) | 255.4 (237.5-265.3) | 0.64 |
| eng | carson-05 | 297 / 252 | 3 | 376.4 (374.0-381.5) | 254.7 (229.3-264.8) | 0.68 |
| eng | asgold-001 | 152 / 180 | 3 | 100.4 (98.7-106.9) | 83.1 (78.9-88.8) | 0.83 |
| eng | asgold-002 | 83 / 82 | 3 | 44.5 (43.2-45.2) | 42.2 (41.4-43.0) | 0.95 |
| frak2021 | cg-008-frak | 348 / 318 | 3 | 546.0 (516.7-572.5) | 339.3 (314.9-372.4) | 0.62 |
| frak2021 | cg-005-frak | 154 / 156 | 3 | 38.0 (35.1-39.2) | 28.0 (27.8-28.6) | 0.74 |
| frak2021 | cg-011-frak | 101 / 92 | 3 | 77.5 (66.3-100.3) | 49.9 (42.4-62.4) | 0.64 |
| frak2021 | cg-001-frak | 99 / 89 | 3 | 18.9 (18.7-19.1) | 16.8 (16.3-17.0) | 0.89 |
| frak2021 | cg-002-frak | 75 / 68 | 3 | 14.5 (13.4-15.2) | 13.5 (12.7-13.5) | 0.93 |
| deu_latf+deu | cg-005-latf | 152 / 154 | 3 | 109.0 (106.7-160.0) | 114.6 (85.8-120.3) | 1.05 |
| deu_latf+deu | cg-001-latf | 107 / 98 | 3 | 88.6 (75.1-92.3) | 79.7 (72.5-81.4) | 0.90 |

Medians per profile, over the pages above: `eng` 923 s for 5.5.0 and 635 s for 5.5.4 (ratio 0.69), `frak2021` 695 s and 448 s (0.64), `deu_latf+deu` 198 s and 194 s (0.98).

How far to trust it:

- **Noise.** Load on the host changes user CPU by more than the spread of the best pages suggests: `cg-005-latf` ranges from 107 to 160 s for 5.5.0, and `cg-011-frak` from 66 to 100 s. Where 5.5.4 and 5.5.0 differ by about 10% or less (`asgold-002`, `cg-002-frak`, `cg-001-frak`, the two `deu_latf+deu` pages), that is inside this noise.
- **The multi-language profile.** `deu_latf+deu` shows no clear gain on the two pages run three times. The densest `deu_latf+deu` page, `cg-008-latf` (319 and 323 paragraphs), was read once by each: 990.6 s for 5.5.0 and 684.1 s for 5.5.4 (0.69). One read each is not a measurement of spread. Its three-run timing was stopped because it would have taken hours at this load.
- **Sample.** 11 pages with three runs each, plus one with one. The densest pages gain the most (0.62 to 0.69 for the pages with 250 or more paragraphs), the sparse ones nothing measurable. A rate on a volunteer's CPU would also depend on the code path its CPU picks.

## What this means for the issue

- Step 1 (build) and step 2 (compare) are done for the profiles and sets above; step 3 (time) is done with the limits above. Nothing in the issue body is ticked.
- The recogniser's output changes in 5.5.4. The issue says nothing changes the recogniser; for our pages that is not the case, so the upgrade is not a drop-in. The re-calibration, parity, gold and host-trust steps all apply, and `chr_phoenix` and `yidj2` should be re-scored.
- On the 6 English pages with a reference, 5.5.4 is worse on 5. This is a small sample with reading-order effects in the CER, but `asgold-005` (33.0% to 53.5%, with 44 and 60 paragraphs) should be looked at before 5.5.4 is chosen.
- Not covered: the 56 parity pages themselves, `srp+srp_latn` and the single-model profiles other than `eng` and `frak2021`, JPEG-versus-JP2 within a profile, a second CPU model, the crash signatures of the issue's fourth bullet, and a second 5.5.4 build run to check that the build is reproducible. 5.5.0's own build is reproducible (two builds gave the same sha256).

## Reproducing

On a host with podman:

```sh
boinc/scripts/build-native-tesseract-upstream.sh --version 5.5.4 --out usnm_tesseract_5.5.4
boinc/scripts/build-native-tesseract-upstream.sh --version 5.5.0 --out usnm_tesseract_5.5.0_upstream   # the control
cargo build --release --manifest-path boinc/apps/native/tesseract-compare/Cargo.toml
boinc/scripts/compare-tesseract-builds.sh --old usnm_tesseract_5.5.0 --new usnm_tesseract_5.5.4 \
  --tessdata tessdata --pages pages.tsv --out compare --compare-tool tesseract-compare
```

`tessdata` holds the locked models by their open names (`eng.traineddata`) and `configs/alto` and `configs/txt`, as `crates/usnm-boinc/src/native.rs` lays them out. `pages.tsv` is one tab-separated line per page: id, image path, profile, reference text path or `-`, repetitions. `compare.jsonl` and `runs.tsv` in the output directory hold the per-file comparison and every run's CPU time. The per-file results behind this report are in [2026-11-12-tesseract-554-compare.json](../../notes/2026-11-12-tesseract-554-compare.json).

## Per-file results

Each box count is "differs of compared". Blocks and paragraphs are compared by position, so on a page where the counts differ the numbers include the shift. Words are matched by text. Word confidence is counted when it differs by more than 0.01.

### Pages

| Page | Profile | Text identical | CER 5.5.0 / 5.5.4 | Paragraphs 5.5.0 / 5.5.4 | Blocks: box differs | Paragraphs: box differs | Words: box differs | Words: confidence off by over 0.01 | Words with no match 5.5.0 / 5.5.4 |
|---|---|---|---|---|---|---|---|---|---|
| asgold-001 | eng | no | 5.4% / 5.2% | 152 / 180 | 60 of 70 | 69 of 152 | 3 of 3758 | 404 | 44 / 47 |
| asgold-002 | eng | no | 6.0% / 6.0% | 83 / 82 | 2 of 77 | 33 of 83 | 0 of 1443 | 46 | 8 / 7 |
| asgold-003 | eng | no | 8.3% / 8.5% | 254 / 252 | 112 of 163 | 246 of 254 | 29 of 5546 | 388 | 174 / 164 |
| asgold-004 | eng | no | 2.7% / 3.2% | 169 / 173 | 73 of 82 | 159 of 169 | 5 of 5899 | 467 | 68 / 68 |
| asgold-005 | eng | no | 33.0% / 53.5% | 44 / 60 | 20 of 32 | 31 of 44 | 23 of 860 | 243 | 294 / 301 |
| asgold-006 | eng | no | 22.1% / 23.4% | 333 / 303 | 176 of 179 | 291 of 333 | 105 of 5923 | 734 | 1081 / 1116 |
| carson-01 | eng | no | no reference | 260 / 237 | 152 of 157 | 234 of 260 | 131 of 6196 | 2704 | 1041 / 1053 |
| carson-02 | eng | no | no reference | 211 / 218 | 116 of 128 | 177 of 211 | 88 of 5524 | 2068 | 563 / 568 |
| carson-03 | eng | no | no reference | 232 / 251 | 162 of 172 | 224 of 232 | 104 of 5798 | 2634 | 1056 / 1048 |
| carson-04 | eng | no | no reference | 2 / 50 | 2 of 2 | 2 of 2 | 540 of 544 | 516 | 7842 / 6392 |
| carson-05 | eng | no | no reference | 297 / 252 | 163 of 163 | 252 of 297 | 124 of 3805 | 1670 | 3195 / 3275 |
| carson-06 | eng | no | no reference | 168 / 172 | 84 of 110 | 121 of 168 | 94 of 5721 | 2205 | 999 / 1007 |
| cg-001-frak | frak2021 | no | 83.4% / 83.2% | 99 / 89 | 66 of 77 | 83 of 99 | 60 of 390 | 143 | 725 / 730 |
| cg-002-frak | frak2021 | no | 11.4% / 11.4% | 75 / 68 | 28 of 30 | 66 of 75 | 28 of 1213 | 318 | 117 / 121 |
| cg-003-frak | frak2021 | no | 53.6% / 53.6% | 78 / 79 | 19 of 36 | 30 of 78 | 2 of 1500 | 95 | 10 / 8 |
| cg-004-frak | frak2021 | no | 68.7% / 67.8% | 41 / 39 | 5 of 15 | 18 of 41 | 18 of 514 | 161 | 91 / 92 |
| cg-005-frak | frak2021 | no | 74.7% / 74.8% | 154 / 156 | 61 of 99 | 133 of 154 | 55 of 2665 | 1385 | 589 / 604 |
| cg-006-frak | frak2021 | no | 5.4% / 5.4% | 1 / 1 | 0 of 1 | 0 of 1 | 0 of 236 | 39 | 2 / 2 |
| cg-007-frak | frak2021 | no | 4.7% / 4.8% | 69 / 72 | 32 of 40 | 59 of 69 | 99 of 3030 | 500 | 88 / 85 |
| cg-008-frak | frak2021 | no | 247.4% / 247.2% | 348 / 318 | 159 of 247 | 287 of 348 | 203 of 5871 | 3415 | 2496 / 2514 |
| cg-009-frak | frak2021 | no | 5.3% / 5.3% | 26 / 26 | 0 of 16 | 0 of 26 | 0 of 1003 | 311 | 34 / 32 |
| cg-010-frak | frak2021 | no | 13.8% / 13.8% | 46 / 42 | 20 of 28 | 23 of 46 | 3 of 457 | 176 | 49 / 46 |
| cg-011-frak | frak2021 | no | 56.8% / 56.9% | 101 / 92 | 44 of 47 | 92 of 101 | 49 of 3717 | 2070 | 881 / 902 |
| cg-012-frak | frak2021 | no | 57.7% / 57.7% | 105 / 113 | 31 of 74 | 64 of 105 | 88 of 4896 | 1068 | 309 / 295 |
| cg-013-frak | frak2021 | no | 6.6% / 6.6% | 90 / 90 | 4 of 39 | 6 of 90 | 17 of 5567 | 769 | 136 / 140 |
| cg-001-latf | deu_latf+deu | no | 72.4% / 72.2% | 107 / 98 | 64 of 78 | 86 of 107 | 130 of 1037 | 635 | 1097 / 1117 |
| cg-002-latf | deu_latf+deu | no | 14.0% / 14.0% | 75 / 68 | 28 of 30 | 66 of 75 | 28 of 1257 | 345 | 145 / 144 |
| cg-003-latf | deu_latf+deu | no | 53.8% / 53.8% | 79 / 80 | 19 of 36 | 31 of 79 | 1 of 1495 | 135 | 27 / 26 |
| cg-004-latf | deu_latf+deu | no | 69.9% / 69.0% | 41 / 39 | 4 of 15 | 20 of 41 | 16 of 532 | 101 | 85 / 92 |
| cg-005-latf | deu_latf+deu | no | 75.6% / 75.6% | 152 / 154 | 54 of 99 | 131 of 152 | 40 of 2974 | 864 | 347 / 337 |
| cg-006-latf | deu_latf+deu | no | 7.4% / 7.3% | 1 / 1 | 0 of 1 | 0 of 1 | 0 of 232 | 67 | 10 / 10 |
| cg-007-latf | deu_latf+deu | no | 6.5% / 6.4% | 70 / 72 | 33 of 41 | 60 of 70 | 102 of 2997 | 632 | 167 / 161 |
| cg-008-latf | deu_latf+deu | no | 280.3% / 281.4% | 319 / 323 | 4 of 251 | 286 of 319 | 419 of 7247 | 3239 | 2152 / 2190 |

### Challenge crops

| Crop | Character edits 5.5.0 / 5.5.4 | Paragraphs 5.5.0 / 5.5.4 | Blocks: box differs | Paragraphs: box differs | Words: box differs | Words: confidence off by over 0.01 | Words with no match 5.5.0 / 5.5.4 |
|---|---|---|---|---|---|---|---|
| 022 | 91 / 95 | 10 / 10 | 0 of 3 | 1 of 10 | 1 of 467 | 98 | 7 / 7 |
| 046 | 31 / 32 | 6 / 6 | 0 of 1 | 0 of 6 | 0 of 466 | 44 | 6 / 6 |
| 049 | 110 / 113 | 6 / 6 | 0 of 3 | 1 of 6 | 2 of 428 | 76 | 9 / 9 |
| 052 | 153 / 164 | 3 / 3 | 2 of 3 | 2 of 3 | 5 of 582 | 210 | 47 / 45 |
| 056 | 63 / 58 | 5 / 5 | 0 of 1 | 0 of 5 | 1 of 254 | 69 | 7 / 7 |
| 101 | 189 / 188 | 13 / 16 | 0 of 2 | 7 of 13 | 1 of 361 | 89 | 16 / 14 |
| 113 | 127 / 126 | 9 / 9 | 0 of 3 | 0 of 9 | 2 of 570 | 85 | 11 / 11 |
| 136 | 116 / 117 | 20 / 20 | 0 of 5 | 0 of 20 | 0 of 839 | 129 | 15 / 17 |
| 168 | 141 / 143 | 7 / 7 | 0 of 3 | 0 of 7 | 5 of 563 | 160 | 19 / 19 |
| 169 | 95 / 95 | 2 / 2 | 0 of 1 | 0 of 2 | 12 of 738 | 131 | 27 / 28 |
| 193 | 2162 / 1882 | 40 / 32 | 29 of 31 | 30 of 40 | 14 of 611 | 227 | 92 / 79 |
| 196 | 87 / 91 | 7 / 7 | 0 of 2 | 1 of 7 | 0 of 475 | 92 | 9 / 11 |
| 216 | 184 / 181 | 7 / 7 | 0 of 1 | 0 of 7 | 0 of 328 | 107 | 21 / 22 |
| 225 | 84 / 90 | 10 / 9 | 0 of 1 | 6 of 10 | 4 of 245 | 63 | 9 / 11 |
| 237 | 113 / 118 | 3 / 3 | 0 of 3 | 0 of 3 | 0 of 789 | 104 | 10 / 10 |
| 278 | 818 / 807 | 15 / 14 | 11 of 14 | 11 of 15 | 7 of 353 | 118 | 41 / 39 |
| 301 | 38 / 36 | 2 / 2 | 0 of 1 | 0 of 2 | 0 of 232 | 36 | 2 / 3 |
| 342 | 56 / 58 | 1 / 1 | 0 of 1 | 1 of 1 | 0 of 185 | 45 | 3 / 3 |
| 357 | 150 / 145 | 4 / 4 | 0 of 3 | 0 of 4 | 2 of 481 | 83 | 14 / 11 |
| 396 | 124 / 121 | 8 / 8 | 0 of 6 | 0 of 8 | 1 of 412 | 88 | 8 / 7 |
| 401 | 117 / 122 | 4 / 4 | 0 of 2 | 0 of 4 | 1 of 358 | 85 | 8 / 8 |
| 426 | 41 / 45 | 1 / 1 | 0 of 1 | 0 of 1 | 1 of 344 | 61 | 5 / 5 |
| 443 | 29 / 28 | 6 / 6 | 0 of 1 | 0 of 6 | 0 of 260 | 39 | 3 / 4 |
| 446 | 71 / 66 | 3 / 3 | 0 of 2 | 0 of 3 | 0 of 364 | 56 | 7 / 7 |
| 462 | 80 / 77 | 3 / 3 | 0 of 3 | 0 of 3 | 0 of 399 | 89 | 11 / 11 |
| 491 | 183 / 182 | 1 / 1 | 0 of 1 | 0 of 1 | 3 of 547 | 155 | 18 / 19 |
| 524 | 187 / 178 | 11 / 11 | 0 of 3 | 0 of 11 | 1 of 752 | 153 | 19 / 19 |
| 552 | 149 / 147 | 4 / 4 | 0 of 1 | 0 of 4 | 3 of 741 | 149 | 12 / 15 |
| 584 | 40 / 39 | 2 / 2 | 0 of 1 | 0 of 2 | 0 of 395 | 42 | 2 / 2 |
| 599 | 100 / 97 | 7 / 10 | 0 of 2 | 2 of 7 | 3 of 554 | 106 | 8 / 8 |

