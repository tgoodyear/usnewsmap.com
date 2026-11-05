# Reading our OCR by column where Tesseract read across

**Status:** measured, not shipped; offline only, and the worker's text, the stored ALTO and BOINC are unchanged · **Date:** 5 November 2026 · **Measured on:** 21 synthetic pages rendered and read by Tesseract 5.5.0 (`deu`, PSM 3) on the build container; the committed fixtures named below · **Code:** `crates/usnm-alto/src/columns.rs`, `usnm-ocr-worker column-text`, 13 §13.4.7 · **Issues:** #925, #926

## The question

On some German pages our text reads a line of one column, then the same line of the next, where LoC's reads down each column (#925). Layout 3 keeps LoC's text beside ours so a phrase LoC has whole is still found (05 §5.5.7), and #1372 rebuilt three named pages offline from hand-drawn regions. Neither fixes our own text. Where does the wrong order come from: Tesseract's page segmentation, our assembly of text from its ALTO, or the order of blocks? And can it be fixed without regions drawn by hand?

## What we did

**Found the cause on synthetic pages.** Random German words (no real text) in five 520 px columns of DejaVu Serif at 30 px, with gutters of 8 to 45 px, with and without a rule in the gutter, read by Tesseract 5.5.0 with `deu` and PSM 3, the worker's settings. Then three more layouts, three seeds each: all gutters 12 px; 12 px gutters with one of 30 px and a ruled banner over three columns; six columns with gutters of 10 to 30 px, some ruled, and columns starting at different heights; and a control with 36 px ruled gutters.

- **It is Tesseract's page segmentation.** An unruled gutter narrower than about half a line height isn't found. Tesseract then makes one block and one line across the columns: with 14 px gutters, 40 lines each run across all five columns, where there are 200 column lines. No line crossed a gutter of 20 px or more, or a ruled one.
- **Not our assembly.** Tesseract's own text output for the same run has the same order, and `usnm_alto::text` reproduces the engine's lines faithfully.
- **Not the block order.** The lines themselves cross the gutters, so no reordering of blocks or lines can read by column. A word can even fuse across a gutter (`bedeutungs-er`, #925), and no reordering separates that.
- The issue's earlier comments found that PSM 4 doesn't help.

**Wrote a repair pass on the parsed page**, `usnm_alto::split_columns`. It works in units of H, the page's median line height, so it doesn't depend on dpi or units:

1. In each block, a candidate gutter is an x-interval at least 0.2 H wide that at most a third of the block's lines cover.
2. A candidate is confirmed where at least two lines cross it with a gap there of at most 2 H (two justified column lines read as one), and those lines plus the line ends aligned on both sides of it number at least three. A body-size line printed across it, with a word over the gap, outweighs four of those. Display type (words more than 1.5 times the usual height: a masthead, a headline over several columns) may cross a gutter and stays whole.
3. Gutters that overlap in x and y merge. The lines crossing a gutter must have, at the median, at least 4 H of words and two words on each side, so a table's narrow cells and a river of space in one column are left alone.
4. Lines in a confirmed gutter's band are cut there by word centre. A hyphen at a line end stays with the piece holding the line's last word. Then the pieces are read column by column, top to bottom, with a local XY cut.
5. A page where no line is cut is returned unchanged (`None`). The pass keeps every word, with its text, box and confidence, and keeps separators and illustrations where they were. It works only on left-to-right pages at word granularity.

**Added an offline command.** `usnm-ocr-worker column-text --alto <file or dir> --language deu --ocr-engine <engine> --out <dir>` writes the text the worker would store, after the repair, and prints one JSON line a page with the counts.

**Did not wire it into the worker or into BOINC's import.** #925's closeout comment of 30 October puts the integration after #926's multi-title acceptance, and after column-safe re-recognition of fused words. So the stored text, the stored ALTO and the BOINC worker image are as they were.

## What we measured

Cross is lines with words in more than one column. Switches is how often the segment (masthead, banner, a column) changes from one word to the next in reading order; ideal is one fewer than the segments. Figures from the Python prototype; the Rust port cuts the same number of lines on all 21 pages, and gives the same figures on the two committed fixtures (its tests).

| Synthetic page (Tesseract 5.5.0)                | Lines cut    | Cross before | Cross after | Switches before | Switches after | Ideal |
| ----------------------------------------------- | ------------ | ------------ | ----------- | --------------- | -------------- | ----- |
| 14 px gutters, unruled                          | 40           | 40           | 0           | 199             | 4              | 4     |
| 8 px gutters, unruled                           | 34           | 40           | 40          | 199             | 166            | 4     |
| narrow-all, seeds 1 to 3                        | 45 each      | 45 each      | 0 each      | 222, 223, 224   | 5 each         | 5     |
| narrow-banner, seeds 1 to 3                     | 87, 88, 87   | 87, 88, 87   | 0, 0, 1     | 212, 218, 214   | 6, 6, 8        | 6     |
| mixed-offset, seeds 1 to 3                      | 138, 92, 121 | 140, 95, 122 | 2, 3, 1     | 284, 199, 246   | 11, 15, 9      | 7     |
| 20, 30, 45 px; ruled 8 to 30 px; fine (3 seeds) | 0            | 0            | 0           | as before       | as before      |       |

The committed fixture `crates/usnm-alto/tests/data/tesseract-synthetic-columns.xml` is narrow-banner seed 1, compacted: 87 of 95 lines cross, and the repair reads masthead, banner and five columns once each (switches 212 to 6).

| Committed real fixture                                                                                   | Before                | After                 |
| -------------------------------------------------------------------------------------------------------- | --------------------- | --------------------- |
| `tesseract-agave-1894-06-23.xml` (three ruled columns, Tesseract 5.5.1, 45 lines)                        | 24 cross, 57 switches | 14 cross, 41 switches |
| `tesseract-5.5.xml`, `ja-ocr.xml`, four Serial Set pages (three ABBYY, one Tesseract 5.1; tables, prose) | read correctly        | unchanged (`None`)    |

On every page the repair keeps the multiset of words with their boxes. On the Agave page the 14 lines still crossing hold words fused with a column rule (`New|`); cut by centre, each stays whole on one side.

What the numbers don't show: these are synthetic pages and one real page, not the named 1910 and 1912 pages of #925 or #926's cohort. Our retained OCR of those is production data and wasn't used here. Order is measured, not CER or phrase recall.

## What is left

- **Fused words.** A word read across a gutter is assigned by its centre, unsplit. Column-safe re-recognition of the cut lines is #925's next step.
- **Very narrow gutters.** At 8 px (about 0.2 H) only some lines are cut. A single merged line on its own is left (two straddlers needed).
- **Only left to right and horizontal.** Right-to-left and vertical pages return `None`.
- **Integration**, after #926 accepts it on the multi-title cohort: derive the worker's and BOINC import's text from the repaired page, label it in provenance, and re-derive existing pages from their retained stored ALTO with `column-text`. Re-OCR isn't needed for the order, since the repair needs only the ALTO. Where the text is made is the integration's choice: the on-prem worker derives it after writing the stored ALTO, but BOINC's import stores the volunteer app's `text.txt`. Repairing it there means either deriving the text from the returned ALTO on the server (the import and the spot check, which compares hosts' texts, change; the volunteer app doesn't) or a new volunteer app version, which is a BOINC release. Either changes the text's identity for the same ALTO, so it needs its provenance label. Then check the named pages (sn84027101 1910-08-16 seq 10, 1910-09-20 seq 10, 1910-10-18 seq 2, and 1912-01-23 seq 8's phrase) in real search.
