# Native usnm_ocr on macOS arm64: parity and challenge

Run on 12 November 2026 for [#1418](https://github.com/tgoodyear/usnewsmap.com/issues/1418). The macOS arm64 build's output is not byte-identical to Linux's, and its accuracy on the calibrated English challenge is within 0.0003 CER of Linux's. This run led to trust per build digest (`docs/boinc/apps/native-ocr.md`, "Trust per build").

## The build

- `boinc-ocr-native.yml` run 43155081356 on main `c622865e`, artifact `boinc-ocr-native-arm64-apple-darwin` (artifact sha256 `bd3ddd39…`).
- `engine.image` `sha256:78065a0a43d8f8bf67c00c014ae2b0540d51d0038e4050dbbd509e14b63fa836`, `engine.base` `sha256:4af660d0a8181c9734c1ac577f4d91089b5ceb17930b8a0a5a217beb99c82032`.
- `check-native-ocr-build.py` passed on it (`check-native-ocr-build-arm64.txt`, repository at `ac0843d6b`).
- Run on an Apple M1, one thread per task, with the same models and job options as Linux.

The Linux references are the Linux native build from the same CI run (`4d6e2a3b…`) and the container worker: `dac9…` for pages, `247c…` for the challenge.

`x86_64-apple-darwin` (`dd7a112f…`) was not run, because the machine had no Rosetta. Its identity is in `check-native-ocr-build-x86_64.txt`.

## Pages

The 56-page set of 1 November isn't on the dev container, so a smaller set was used.

| Pages                                      | Text identical to Linux native | ALTO identical |
| ------------------------------------------ | -----------------------------: | -------------: |
| English JP2, `sn83030272`, 7 to 11 Jul 1865 |                        0 of 10 |        0 of 10 |
| English JPEG, `s9-en-asgold` 001 to 004    |                         0 of 4 |         0 of 4 |

The 10 JP2 pages also differ from the container worker. Most differences are word text: 6 to 20 percent of words on 8 of the 10 JP2 pages. Some pages also change their word count, and 23 to 153 matched words per page have shifted bounding boxes. The ALTO metadata doesn't differ. Page `1865-07-08_1_3` diverges most, with a bag-of-words overlap of 0.53. JPEG page 002 has the same word count and coordinates as Linux, with 4 text and 227 confidence differences.

JPEG decoding is not the cause: the JP2 pages, which OpenJPEG decodes, differ as much or more, and in the challenge 24 of 27 TIFF crops differ as well as all 3 JPEG crops. This run can't separate the remaining candidates, such as Apple's `libm` and the different float operation order of arm64 NEON and x86 AVX2 in the LSTM. That needs the x86_64 macOS build on an Intel Mac.

## Calibrated English challenge

Challenge `d8cd7279…`, 30 crops, 82,794 reference characters, threshold 0.08734.

| Build                     | Character edits |    CER |    WER |
| ------------------------- | --------------: | -----: | -----: |
| macOS arm64 `78065a0a…`   |            5957 | 0.0719 | 0.2003 |
| Linux native `4d6e2a3b…`  |            5929 | 0.0716 |  0.199 |
| Linux native `c353f7bf…`  |            5929 | 0.0716 |  0.199 |
| Container `247c…`         |            5929 | 0.0716 |  0.199 |

The three Linux builds have identical edit counts on all 30 crops. macOS has the same edit counts as Linux on 3 crops, which doesn't show the same text: the crop outputs weren't compared. On the other 27 the difference is between -7 and +8 character edits per crop. In `challenge-gold.json`, `identical_to_linux` and `crops_identical_to_linux` mean equal character and word edit counts, nothing more.

## Files

| File                                  | What it is                                                                                     |
| ------------------------------------- | ---------------------------------------------------------------------------------------------- |
| `parity-pages.json`                   | Per page: text sha256 for both builds, the ALTO comparison, and run times.                     |
| `challenge-gold.json`                 | Per crop: edit counts for macOS and the three Linux builds.                                    |
| `gold-result-macos-arm64.json`        | The macOS gold report as the worker wrote it.                                                  |
| `original-json-sha256.txt`            | The sha256 of the three JSON files as written. They are stored here with their keys sorted.  |
| `sha256-outputs-arm64.txt`            | The sha256 of every macOS output file (`text.txt`, `layout.xml`, `result.json` per page).      |
| `sha256-linux-native-ref-pages.txt`   | The sha256 of the Linux reference outputs.                                                     |
| `sha256-artifact-*.txt`               | The sha256 of every file in both macOS CI artifacts.                                           |
| `sha256-models-arm64.txt`             | The sha256 of the models the macOS run used.                                                   |
| `check-native-ocr-build-*.txt`        | What `check-native-ocr-build.py` printed for each artifact.                                    |
| `repo-tools-commit.txt`               | The repository commit the tools came from, and the `models.lock` hash.                         |
| `commands.txt`, `run-mac.sh`, `analyze.py`, `rsync-filter.txt` | How the run was made and analysed. `run-mac.sh` now resolves its paths to absolute ones, which the relative paths in `commands.txt` need. |

The macOS output files themselves (14 MB) are not in the repository. Their hashes are in `sha256-outputs-arm64.txt`, and each page's text hash is also in `parity-pages.json`.
