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
