# Cherokee line finding and recognition on fresh pages (#2128)

Code for the preregistered study in issue #2128. Data stays out of the repository: scans, crops,
expert transcriptions, readings and the silver set live on the dev host under the study's data
directory, along with a manifest of the inputs and their sha256 values.

The scripts reuse earlier code unchanged and import it from the repository:

- the #1622 finder and clean-up (`docs/reports/2026-11-cherokee/scripts/chr_layout_find.py` and
  `chr_layout_post.py`);
- the line classifier (`chr_line_script.py`) and the page check (`chr_layout_qa.py`);
- the NT aligner of #1620 (`scripts/ocr-survey/cherokee_testset.py`, `align_block`, with its drop rules).

All command-line options are named.

| Script | What it does |
|---|---|
| `s0_find.py` | S0: Tesseract PSM 3 on the Sauvola-binarised scan, then the clean-up. It gives the same boxes as `chr_layout_find.py tess --prep sauvola` with `chr_layout_post.py clean`, and keeps Tesseract's block.paragraph.line numbers (`tid`) so the reading order is known. |
| `classify_lines.py` | Calls each box Cherokee or Latin, using PR #1830's classifier: shape features without `h`, `w` and `contrast`, two stages, trained on `feats-train-truth.jsonl`, threshold 0.5. |
| `s0_stats.py` | Per-page boxes, Cherokee lines, merged-lines share and ink coverage. A page is flagged when its merged-lines share is over 0.08. |
| `read_lines.py` | Reads the Cherokee lines with one model: 12/8 px padding, `--oem 1 --psm 13 --dpi 300`. It saves each crop once, so every model reads the same file. |
| `nt_verses.py` | Gives the book, chapter and verse of each letter of the aligner's NT letter stream. |
| `silver.py` | `align`: blocks are runs of consecutive Cherokee lines in one Tesseract block, aligned with `align_block`. `build`: a line is kept if either reading is accepted. It writes the lines, the sorted id list and its sha256, the verses covered, and optionally the crops. |
| `s1_columns.py` | S1: the #2107 banded column finder, frozen at `337fc0aa` and passed in as a file, splits the page into column regions. S0 runs on each region and the clean-up runs on the whole page. |
| `s2_split.py` | S2: boxes taller than 1.8 times the page median, and at least as wide as they are tall, are split at the deep minima of the row-ink profile. |
| `score_layout.py` | The Experiment 1 scorer. It matches at IoU ≥ 0.5 against LoC ALTO lines and reports Cherokee recall, all-line recall and precision, with a 2,000-draw page bootstrap and the issue's adoption rule. |

## Order of runs (phase 1)

Paths are relative to the data directory.

```
s0_find.py      --pages work/ghn-order.txt --scans inputs/ghn/images --layout ghn --tessdata inputs/tessdata --out work/s0/ghn-s0.jsonl --jobs 6
classify_lines.py --found work/s0/ghn-s0.jsonl --scans inputs/ghn/images --layout ghn --train inputs/ghn-699/feats-train-truth.jsonl --out work/s0/ghn-s0-classified.jsonl
s0_stats.py     --classified work/s0/ghn-s0-classified.jsonl --scans inputs/ghn/images --layout ghn --test-pages work/ghn-test-pages.txt --out-pages work/s0/ghn-qa.jsonl --out-summary work/s0/ghn-s0-summary.json
read_lines.py   --classified work/s0/ghn-s0-classified.jsonl --pages work/ghn-test-pages.txt --lang chr         ... --crops-dir work/crops --out work/reads/test-chr.jsonl
read_lines.py   ... --pages work/ghn-test-pages.txt  --lang chr_phoenix --out work/reads/test-chr_phoenix.jsonl
read_lines.py   ... --pages work/ghn-train-pages.txt --lang chr_phoenix --out work/reads/train-chr_phoenix.jsonl
silver.py align --classified ... --pages work/ghn-test-pages.txt --reads chr=... --reads chr_phoenix=... --nt-text inputs/testset/src/nt/cnts.raw.txt --out work/silver/aligned.jsonl
silver.py build --aligned work/silver/aligned.jsonl --reads chr=... --reads chr_phoenix=... --prefer chr --crops-dir work/crops --nt-text ... --out-dir out/silver --copy-crops
(the same align/build on the training pages with chr_phoenix only -> out/r4-candidates)
```

S1, S2 and the scorer are run on the 154 layout-test pages only after their commit is posted on the issue.

## Phase 2

### Experiment 1

At `89fb1017`, `s0_find.py`, `s1_columns.py` and `s2_split.py` run on the 154 layout-test pages, followed by
`score_layout.py` (S0 as the baseline, 2,000 draws, seed 2128). `exp1_report.py` then writes `results.json`,
with the verdicts and the per-page outliers.

### R3 and R4c (Amendment 1)

| Script | What it does |
|---|---|
| `exclusion.py` | The 274 excluded verses (every verse an aligned GHN test line touches). A text counts as excluded if it falls inside one of their letter spans or holds a run of 20 of their letters. |
| `exclude_manifest.py` | One manifest of the test pages no training step may use: test set v2's 29 pages and the 210 GHN test pages. |
| `r3_corpus.py` | The synthetic text: the NT verses minus the excluded ones, plus the Cherokee of ChrEn's 2020 parallel splits and its monolingual file. Any unit holding an excluded run is dropped, the units are shuffled, and the result is asserted. |
| `r3_synth_lines.py` | Round two's `chr_synth_lines.py` functions, with random left and right margins (0.05 to 1.5 body heights) and slivers only in a margin of at least 0.6 bodies. It asserts that no line holds excluded text. |
| `augment_line_ends.py` | The same margins and slivers for the real LoC lines (half of them get re-cut margins). |
| `ghn_words.py` | Re-reads phase 1's GHN training-page crops with `chr_phoenix`, keeping word boxes, and with stock `chr`. |
| `ghn_alto.py` | Stacks each GHN training page's Cherokee crops into one image and writes pixel ALTO for it from `chr_phoenix`'s words, so `usnm-glyphs build` can cut glyphs as it does from LoC's ALTO. Boxes over 1.8 times the page median height, or over 1.8 times the median of the page medians, are left out. |
| `r4c_lines.py` | R4c's GHN lines: consensus lines (stock `chr` equals `chr_phoenix`, at least 10 letters, every word in the model B word list) plus the 51 aligned lines. Any line aligned to an excluded verse, or holding excluded text, is dropped. |
| `train/start_model.sh` | Builds the start model: round two's base (`7af4310b…`, stock `chr` with the NT word list) with Ꮐ added to the unicharset. The new starter traineddata comes from `combine_lang_model` with the base's own word, punctuation and number lists. Training uses `--continue_from base.lstm --old_traineddata base.traineddata`, so lstmtraining keeps the base network and maps its output layer onto the new character set, adding one output for Ꮐ. |
| `train/make_lstmf.sh`, `train/lists.py` | Round two's lstmf and list steps. The evaluation list is the real LoC lines of about one page in twenty, chosen by hash; R4c's GHN lines are training-only, so both arms share the evaluation list. |
| `train/train.sh`, `train/select_checkpoint.py` | lstmtraining with at most 20,000 iterations and a 0.005 target. Every kept checkpoint is then scored with `lstmeval` on the evaluation list, and the lowest error wins. |
| `train/prep_r3_r4c.sh` | Runs all the data steps in order. |

The glyph bank is built with `usnm-glyphs` from this commit: `build`, then `clean --k 9 --min-agree 0.5
--exclude-manifest`. Its input is LoC's 190 pages plus the GHN training-page composites.
