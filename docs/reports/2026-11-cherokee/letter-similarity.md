# Cherokee Phoenix: can image similarity carry a letter decision?

15 November 2026. Analysis only. Script: [`scripts/chr_letter_similarity.py`](scripts/chr_letter_similarity.py), test: `scripts/test_chr_letter_similarity.py`.

## Verdict

The fixed rule was: propagation is worth building only if letter-level 1-NN accuracy on look-alike pairs is at least 98% at some threshold that covers at least 30% of letters, and the word-level precision does not contradict that.

Applied literally, the rule's letter condition is met in one of four cells and not in the other three. It is a necessary condition for building, not a reason to build, and the one passing cell is not robust.

| Embedding | Evaluated on | Best coverage at 98% look-alike accuracy | Look-alike letters covered | Condition (30% coverage) |
| --- | --- | ---: | ---: | --- |
| DINOv2-small | tier A (the stated evaluation set) | 63.2% | 22 of 22 correct | met |
| pixel+HOG | tier A | 28.7% | 8 of 8 correct | not met |
| pixel+HOG | all letters | 29.5% | 71 of 72 correct | not met, by half a point |
| DINOv2-small | all letters | 14.8% | 26 of 26 correct | not met |

The passing cell is fragile. The threshold was chosen on the same 22 look-alike letters it is scored on, and 22 of 22 has a 95% lower bound of about 85%. On all letters (260 look-alike letters, of which the tier A ones are a subset) DINOv2-small reaches only 84% look-alike accuracy at 50% coverage. Tier A is also small and skewed: 38 look-alike letters, 31 of them Ꮧ/Ꮨ or Ꮐ/Ᏻ. Pixels plus HOG is the more accurate embedding overall (92.6% against 79.0% 1-NN), yet it misses the condition. The word-level test (31 words with a same-pair neighbour, 31 correct) does not contradict the letter result but is too small to support it.

The evidence supports a larger tier A and a repeat of the test, not a build.

## Question and data

The paper used one typeface, so the same letter should embed close to itself on every page. The test asks whether a nearest-neighbour match to a labelled crop is accurate enough to copy that crop's label to a new crop without a person looking.

Two task sets from the check site were used, both archived on 14 November 2026 with their clips: `chr/phoenix-nt-v1` (reference reading against LoC) and `chr-lines/disputes-v1` (LoC against `chr_phoenix`). Held tasks were dropped from everything, using today's public export (`held: true`; 286 tasks, all in `chr/phoenix-nt-v1`, reason `reference-mismatch-v1`). Outcomes and `settled` come from today's export. The sets have 289 and 680 unheld tasks. The `chr-rare` set was not used: its tasks are not pairs of two readings of the same word.

Clips are small. A word clip is about 170 by 38 pixels for a six-letter word and a line clip is about 680 by 49, so a letter is about 20 to 25 pixels wide. Each crop comes from one JPEG that the site serves, not from the scan.

## Method

**How letters were cut.** The data has no per-letter boxes. A task has a word clip and the word's box in the line clip. The report's `check-glyph-index.jsonl` has word boxes on the scan, and the letter pictures under `phoenix-glyphs-v2/examples` are three hand-picked examples per letter (228 in all), not a labelled set. So each word clip was cut into as many pieces as the reading has letters with `split_word`, a Python port of the approach in `crates/usnm-glyphs` (`split_word`): Otsu binarisation, connected components in the text band, merging of stacked components, then merging the narrowest gaps or cutting the widest box at its thinnest column until there are exactly n pieces. A split with very uneven widths is refused and the word is dropped. These are segmented crops, not ground-truth boxes. A merge or cut that lands in the wrong place gives a crop with the wrong label, and nothing here measures how often.

Only words whose two readings have the same length were used, so that position i means the same letter in both. A letter is labelled when the two readings carry the same letter at that position and the position is outside the task's differing positions. For `chr/phoenix-nt-v1` the pair is the reference reading against LoC, which is a different pair from the disputes set (LoC against `chr_phoenix`); results are reported pooled and per set.

Words dropped: nt set, 96 of 289 with unequal readings, 43 with a failed split, 150 used. Disputes set, 170 of 680 with unequal readings, 4 with no agreed letter, 96 with a failed split, 410 used. That gives 2,148 letter crops over 75 distinct letters.

**Tiers.** There is no Cherokee New Testament or ChrEn word list in the data, so attestation comes from the task sets themselves. The pool is the reference spellings of the unheld nt tasks. A word's candidate spellings are its settled winner (when the outcome is `a` or `b`), else either reading. Tier A: a candidate spelling appears as the reference reading of a different unheld nt task. Tier B: agreed only. Tier A says the spelling exists in the reference text. It does not say the Phoenix printed it that way. Tier A has 321 letters (124 nt, 197 disputes) and tier B 1,827 (396 nt, 1,431 disputes). No disputes-set task is settled, so tiers in that set come only from the repeated-spelling rule.

**Embeddings.** Each crop is padded to a square with its median grey. (1) Pixels plus HOG: 32 by 32 grayscale, mean-removed and unit length, concatenated with a HOG vector (9 orientations, 8-pixel cells, 2 by 2 blocks), unit length. (2) DINOv2-small (`facebook/dinov2-small`) on CPU, 224 by 224, the class-token vector.

**Retrieval.** Cosine k-NN (k = 1 and 5) with five folds grouped by page: no page is in both train and test, shuffled with a fixed seed. Every letter is predicted once, out of fold, from the letters of the other folds (tiers A and B together), and the tier A slice is the evaluation set. Look-alike pairs: Ꭱ/Ꮢ, Ꮧ/Ꮨ, Ꮎ/Ꮻ, Ꮘ/Ꮱ, Ꮐ/Ᏻ. Look-alike accuracy counts test letters whose true label is in any of the five pairs. Coverage is the share of evaluated letters at or above a 1-NN similarity threshold.

## Letter results

### Letter k-NN accuracy (page-grouped folds)

| Embedding | Slice | n | 1-NN | 5-NN | 1-NN 95% interval |
| --- | --- | ---: | ---: | ---: | --- |
| pixel+HOG | overall | 2148 | 92.6% | 92.2% | 91.4% to 93.6% |
| pixel+HOG | tier_A | 321 | 97.2% | 96.6% | 94.8% to 98.5% |
| pixel+HOG | tier_B | 1827 | 91.7% | 91.5% | 90.4% to 92.9% |
| pixel+HOG | nt_set | 520 | 90.6% | 90.4% | 87.8% to 92.8% |
| pixel+HOG | disputes_set | 1628 | 93.2% | 92.8% | 91.9% to 94.3% |
| pixel+HOG | lookalike | 260 | 92.7% | 92.7% | 88.9% to 95.3% |
| pixel+HOG | lookalike_tier_A | 38 | 92.1% | 97.4% | 79.2% to 97.3% |
| DINOv2-small | overall | 2148 | 79.0% | 81.1% | 77.2% to 80.7% |
| DINOv2-small | tier_A | 321 | 85.0% | 85.0% | 80.7% to 88.5% |
| DINOv2-small | tier_B | 1827 | 77.9% | 80.4% | 76.0% to 79.8% |
| DINOv2-small | nt_set | 520 | 77.1% | 78.8% | 73.3% to 80.5% |
| DINOv2-small | disputes_set | 1628 | 79.6% | 81.8% | 77.6% to 81.5% |
| DINOv2-small | lookalike | 260 | 79.6% | 83.5% | 74.3% to 84.1% |
| DINOv2-small | lookalike_tier_A | 38 | 86.8% | 89.5% | 72.7% to 94.2% |

### Look-alike pairs, 1-NN, all letters

| Embedding | Pair | n | Correct | Errors that are the other member |
| --- | --- | ---: | ---: | ---: |
| pixel+HOG | Ꭱ/Ꮢ | 33 | 31 | 1 |
| pixel+HOG | Ꮧ/Ꮨ | 138 | 131 | 2 |
| pixel+HOG | Ꮎ/Ꮻ | 53 | 46 | 0 |
| pixel+HOG | Ꮘ/Ꮱ | 3 | 3 | 0 |
| pixel+HOG | Ꮐ/Ᏻ | 33 | 30 | 0 |
| DINOv2-small | Ꭱ/Ꮢ | 33 | 12 | 5 |
| DINOv2-small | Ꮧ/Ꮨ | 138 | 120 | 1 |
| DINOv2-small | Ꮎ/Ꮻ | 53 | 45 | 0 |
| DINOv2-small | Ꮘ/Ꮱ | 3 | 0 | 0 |
| DINOv2-small | Ꮐ/Ᏻ | 33 | 30 | 0 |

### Look-alike pairs, 1-NN, tier A letters

| Embedding | Pair | n | Correct | Errors that are the other member |
| --- | --- | ---: | ---: | ---: |
| pixel+HOG | Ꭱ/Ꮢ | 3 | 3 | 0 |
| pixel+HOG | Ꮧ/Ꮨ | 20 | 18 | 1 |
| pixel+HOG | Ꮎ/Ꮻ | 4 | 3 | 0 |
| pixel+HOG | Ꮘ/Ꮱ | 0 | 0 | 0 |
| pixel+HOG | Ꮐ/Ᏻ | 11 | 11 | 0 |
| DINOv2-small | Ꭱ/Ꮢ | 3 | 2 | 0 |
| DINOv2-small | Ꮧ/Ꮨ | 20 | 16 | 0 |
| DINOv2-small | Ꮎ/Ꮻ | 4 | 4 | 0 |
| DINOv2-small | Ꮘ/Ꮱ | 0 | 0 | 0 |
| DINOv2-small | Ꮐ/Ᏻ | 11 | 11 | 0 |

### Top confusions, 1-NN, all letters

pixel+HOG: Ꭲ read as Ꭺ (5), Ꮝ read as Ꭸ (4), Ꮎ read as Ꮔ (4), Ꭲ read as Ꮄ (4), Ᏺ read as Ꮒ (3), Ꮝ read as Ꮧ (3), Ꮧ read as Ꮝ (3), Ꭸ read as Ꮝ (2), Ꮝ read as Ꭺ (2), Ꭺ read as Ꭲ (2)

DINOv2-small: Ꮥ read as Ꭶ (11), Ꭸ read as Ꮧ (9), Ꭴ read as Ꮕ (7), Ꮕ read as Ꭴ (6), Ꮝ read as Ꮅ (5), Ꮝ read as Ꭸ (5), Ꭱ read as Ꮒ (5), Ꭶ read as Ꮥ (5), Ꮒ read as Ꭺ (5), Ꭱ read as Ꭼ (4)


### Similarity threshold analysis, tier A

| Embedding | Threshold | Coverage | Covered | 1-NN accuracy | Look-alike letters covered | Look-alike accuracy |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| pixel+HOG | 0.937 | 10.0% | 32 | 100.0% | 1 | 100.0% |
| pixel+HOG | 0.932 | 19.9% | 64 | 100.0% | 5 | 100.0% |
| pixel+HOG | 0.928 | 29.9% | 96 | 99.0% | 9 | 88.9% |
| pixel+HOG | 0.920 | 49.8% | 160 | 98.8% | 19 | 94.7% |
| pixel+HOG | 0.907 | 70.1% | 225 | 98.7% | 25 | 96.0% |
| pixel+HOG | 0.680 | 100.0% | 321 | 97.2% | 38 | 92.1% |
| pixel+HOG (best at 98% look-alike) | 0.929 | 28.7% | 92 | 100.0% | 8 | 100.0% |
| DINOv2-small | 0.974 | 10.0% | 32 | 100.0% | 2 | 100.0% |
| DINOv2-small | 0.970 | 19.9% | 64 | 98.4% | 5 | 100.0% |
| DINOv2-small | 0.966 | 29.9% | 96 | 99.0% | 8 | 100.0% |
| DINOv2-small | 0.956 | 49.8% | 160 | 96.9% | 16 | 100.0% |
| DINOv2-small | 0.944 | 70.1% | 225 | 92.9% | 28 | 92.9% |
| DINOv2-small | 0.619 | 100.0% | 321 | 85.0% | 38 | 86.8% |
| DINOv2-small (best at 98% look-alike) | 0.949 | 63.2% | 203 | 95.6% | 22 | 100.0% |

### Similarity threshold analysis, all letters

| Embedding | Threshold | Coverage | Covered | 1-NN accuracy | Look-alike letters covered | Look-alike accuracy |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| pixel+HOG | 0.937 | 10.0% | 215 | 96.3% | 20 | 100.0% |
| pixel+HOG | 0.932 | 20.0% | 430 | 96.7% | 43 | 100.0% |
| pixel+HOG | 0.927 | 30.0% | 644 | 96.9% | 73 | 97.3% |
| pixel+HOG | 0.917 | 50.0% | 1074 | 96.2% | 132 | 96.2% |
| pixel+HOG | 0.905 | 70.0% | 1504 | 95.8% | 172 | 95.9% |
| pixel+HOG | 0.633 | 100.0% | 2148 | 92.6% | 260 | 92.7% |
| pixel+HOG (best at 98% look-alike) | 0.927 | 29.5% | 633 | 97.2% | 72 | 98.6% |
| DINOv2-small | 0.973 | 10.0% | 215 | 94.0% | 18 | 100.0% |
| DINOv2-small | 0.968 | 20.0% | 430 | 93.7% | 41 | 92.7% |
| DINOv2-small | 0.963 | 30.0% | 644 | 92.2% | 65 | 87.7% |
| DINOv2-small | 0.954 | 50.0% | 1074 | 90.0% | 114 | 84.2% |
| DINOv2-small | 0.941 | 70.0% | 1504 | 86.3% | 166 | 81.9% |
| DINOv2-small | 0.619 | 100.0% | 2148 | 79.0% | 260 | 79.6% |
| DINOv2-small (best at 98% look-alike) | 0.970 | 14.8% | 318 | 95.0% | 26 | 100.0% |

The look-alike counts are small (38 tier A letters across all pairs, 3 for Ꮘ/Ꮱ in the whole set), so the intervals are wide. The gold-example supplement: classifying the 228 hand-picked letter pictures against all cut letters gives 1-NN accuracy of 89.5% for pixels plus HOG and 48.7% for DINOv2-small. These pictures are cleaner than the crops and come from other pages, so this is a loose check only.

## Words (descriptive)

The settled, unheld nt words with a winner (outcome `a` or `b`) number 87: 63 where the reference reading won and 24 where LoC won (15 settled `neither`, 3 `unsure` and 1 `disputed` are left out). For 44 the region around the differing letters was cut (the union of the cut boxes at the differing positions); for 43 the whole word clip was used because the readings differ in length or the split failed. Each word's nearest settled neighbour on a different page with the same differing-letter pair was found. Only 31 of the 87 words have such a neighbour. The precision is the share of those whose neighbour won on the same side (reference or LoC).

| Embedding | Threshold | Words with a neighbour at or above it | Same winning side |
| --- | ---: | ---: | ---: |
| DINOv2-small | 0.5 | 31 (of 87) | 31 |
| DINOv2-small | 0.6 | 31 (of 87) | 31 |
| DINOv2-small | 0.7 | 26 (of 87) | 26 |
| DINOv2-small | 0.8 | 21 (of 87) | 21 |
| DINOv2-small | 0.9 | 9 (of 87) | 9 |
| pixel+HOG | 0.5 | 27 (of 87) | 27 |
| pixel+HOG | 0.6 | 26 (of 87) | 26 |
| pixel+HOG | 0.7 | 17 (of 87) | 17 |
| pixel+HOG | 0.8 | 17 (of 87) | 17 |
| pixel+HOG | 0.9 | 11 (of 87) | 11 |

Precision is 100% at every threshold, on at most 31 words, and the reference reading wins 72% of settled words anyway, so a neighbour of the same side is not strong evidence. With these sizes the test is descriptive. It does not contradict the letter result and does not carry it.

## Shared errors

The task sets contain only words where the two readings differ, so errors the two readings share in fully agreed words cannot be counted from this data. What can be said:

- In the nt set the two readings are the reference and LoC, and they differ on every task by construction. A shared error on a disputed word is therefore impossible, and at agreed positions inside disputed words the settled spelling is one of the two readings, so those letters agree with it (241 agreed letters in the 87 settled words with a winner; the settled answer says nothing about them beyond that). Settled outcomes in the nt set: 63 reference, 24 LoC, 15 neither, 3 unsure, 1 disputed. The 15 `neither` words are cases where both readings are wrong somewhere in the word; the data does not say where.
- In the disputes set no task is settled (76 of 680 have an answer), so nothing can be computed.
- Across the sets, 36 lines occur in both. In 11 settled nt words the word could be aligned by index and LoC's reading matched the disputes manifest. In the 5 where the reference won, `chr_phoenix` equalled the reference 4 times, equalled LoC (a shared error) 0 times and read something else once. In the 6 where LoC won, `chr_phoenix` equalled LoC every time. This is 11 words and no look-alike pair has more than a few, so no per-pair rate can be given.

## Limits

- Letters are segmented crops, labelled by position. Segmentation errors count as classifier errors and are not measured separately.
- Labels come from agreement of two machine readings, which can share an error. Tier A filters with spelling attestation in the reference text, which is a weaker check than a person looking at the print.
- Look-alike support is small: 260 letters overall, 38 in tier A, 3 for Ꮘ/Ꮱ. The 98% condition on tier A rests on single-digit look-alike counts at the 30% coverage point.
- One typeface, one paper, low-resolution crops from clips of about 40 pixels in height. Only 75 distinct letters occur, and some have a handful of crops.
- Both embeddings were run unmodified. No tuning was done and the threshold grid was fixed before the run.
- Words with unequal readings (266 of 969) and failed splits (139) are missing, and they are not random: they are probably the harder ones.

## Reproducing

```
python3 scripts/chr_letter_similarity.py --check-site-dir CHECK_SITE --export-dir EXPORTS --out-dir OUT
```

`EXPORTS` holds `export-chr.jsonl` and `export-chr-lines.jsonl`, the public exports from `https://check.usnewsmap.com/api/export?lang=chr` and `?lang=chr-lines` fetched on 15 November 2026. The run used the default seed (20261115) and five folds, and writes `results.json` and `tables.md`. It needs numpy, pillow, scikit-image, scikit-learn, torch (CPU) and transformers.
