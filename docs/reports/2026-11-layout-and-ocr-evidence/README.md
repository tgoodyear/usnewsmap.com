# Layout and OCR-evidence studies, November 2026

This report collects the layout and OCR-evidence studies run between 22 and 27 November 2026 on issues [#2103](https://github.com/tgoodyear/usnewsmap.com/issues/2103), [#2107](https://github.com/tgoodyear/usnewsmap.com/issues/2107), [#2122](https://github.com/tgoodyear/usnewsmap.com/issues/2122), [#2167](https://github.com/tgoodyear/usnewsmap.com/issues/2167), [#1922](https://github.com/tgoodyear/usnewsmap.com/issues/1922), [#968](https://github.com/tgoodyear/usnewsmap.com/issues/968) and PR [#1915](https://github.com/tgoodyear/usnewsmap.com/pull/1915). It adds no analysis. Every number comes from the public issue record or from the independent methods audit of 27 November (`methods-audit-20261127.md`, kept outside the repository). Bracketed numbers such as [7131094183] are GitHub comment ids and link to the comment. The latest comments cited are time-stamped 2026-11-28 00:31 UTC.

Where the sources disagree, the text says so. Code tags are `study/...` tags in this repository, given with the short commit. A tag points at a commit that contains the frozen code named in the plan, as checked with `git tag --contains`. It is not always the frozen commit itself, so the frozen commit is given where a comment names one.

## Summary

Each row gives the deciding number and the comment that records it. "Rule" is the threshold the plan froze before scoring.

### Adopted

| Decision | Deciding number | Source |
| --- | --- | --- |
| Full-resolution crops for Xiaomi v5 campaigns (#968) | Pooled CER 7.11% to 4.52%, difference −2.60 points (95% CI −3.13 to −2.11). Rule: gain of at least 0.5 points. | [7201889239] |
| Capped full resolution (4.2 Mpx) for Qwen 8B campaigns, with the fixed app (#968) | Pooled CER 6.87% to 4.27%, −2.60 (CI −4.14 to −1.52). `en-op` is the exception: 2.58% to 3.23%, CI crosses zero. | [7208268194], [7220974702] |
| Combined segmenter for La Liberté-type pages (#2107). The issue does not define the type; the test set was one French title, 1925–1928 | ΔF1 +0.039 (0.031 to 0.047); precision 0.903 to 0.974. Rule: ΔF1 at least 0.02. | [7147408816] |
| Combination (banded v1 plus American Stories gutters) for the same kind of page; superseded by the combined segmenter | ΔF1 +0.048 (0.043 to 0.053) on 500 fresh La Liberté pages | [7102798228] |
| Banded v1 stays the column finder for German and English pages | German fresh pages: banded alone F1 0.971, combination adds +0.0017 (rule 0.02). English: the rule could not pass. | [7103363654], [7105288191], [7147408816] |
| American Stories model stays the layout detector | The trained YOLOv8m scored pool F1 0.642; the rule needed 0.825 | [7101133928] |
| `cross_share` as a detector of merged-column ALTO, to flag pages. It is not a word-error feature. | Planted-merge AUC 0.9947 on 106 pages (bar 0.90) | [7090158532] |
| Proxy evidence rule for #1922, adopted by the maintainer on 27 November | Steps 1–5 can open a language's human blind-pick task. Replacing LoC's text needs steps 1–6, including a blind pick on at least 50 pages. | [7219412526] |

### Rejected

| Decision | Deciding number | Source |
| --- | --- | --- |
| Render-against-scan distance as a word-error signal (#2103) | Low-damage gain −0.044 (−0.073 to 0.039); rule needed above 0 | [7085802179] |
| ALTO-versus-columns layout signals added to the word-error model (#2107 step 2) | AUC gain −0.0027 (−0.0056 to +0.0016); rule needed at least 0.02 | [7090158532] |
| American Stories boxes as the corpus non-text mask | Mask recall 0.749 (0.736 to 0.762); rule at least 0.80 | [7090168950] |
| Trained YOLOv8m replaces American Stories | Pool F1 0.642 against 0.825 needed | [7101133928] |
| Combination for German pages | ΔF1 +0.0017; rule at least 0.02 | [7103363654] |
| Combination for English pages (volunteer test, stopped before any answer) | H_x/H_b = 0.0215, so G of 0.05 needs P_b at or below 0.43 | [7105288191] |
| Banded v2 (tilt and rule refit) replaces v1 | Gutter height inside marked boxes: v2 − v1 = +0.019 (+0.016 to +0.022); rule allowed +0.005 | [7131094183] |
| Combined segmenter for English pages | Gutter height outside marked boxes kept: 0.509 (0.494 to 0.525) of banded's; rule at least 0.85 | [7147408816] |
| Reading by column as the English re-recognition step (#2122) | Trigram C − W −0.018 (−0.046 to +0.014); rule at least +0.02. This rejection is reopened, see below. | [7106460936] |
| ALTO-only corpus audit of reading order (#2167) | Spearman with the image measure −0.04 (−0.24 to 0.15); rule at least 0.60 | [7206057060] |
| `lm_oov` as a detector of VLM losses at full resolution (#1922) | Qwen balanced accuracy 0.588 (0.525 to 0.659); Xiaomi inconclusive, 11 pairs with LoC better | [7219538393] |
| LoC-independent over-read checks A and B (#1922) | A-or-B recall 0.286 (Xiaomi) and 0.426 (Qwen); rule at least 0.60 | [7220509065] |

### Withdrawn, downgraded or reopened

| Item | What happened | Source |
| --- | --- | --- |
| "Whole-page Tesseract beats LoC on captions": W − L +0.054 trigram (+0.006 to +0.102) | Downgraded to inconclusive, then withdrawn | [7133172084], [7220974216], [7221308955] |
| "Whole-page Tesseract beats LoC" on 25 pages, descriptive: W − L +0.120 trigram | Withdrawn. On the 7 `en-asgold` pages, LoC is better on WER (+0.058) and CER (+0.065). | [7106460936], [7221308955] |
| Rejection of reading by column | Reopened: on WER and CER, by-column is level or slightly better than whole page | [7221308955] |
| Completeness-gate findings (PR #1915) | Text-only measurement, pending a human test | [7224737807], [7225127190] |

### Open

| Item | Closing | Source |
| --- | --- | --- |
| `en-read` pick task, English body text | 8 weeks from load on 2026-11-24 22:05Z, so 2027-01-19 | [7145789797] |
| `de-read` pick task, German (#2127) | 2027-01-19; needs at least 300 settled tasks | [7140264700] |
| `gate1915-v1` human test of blocked pages | 8 weeks from load on 2026-11-27 19:20Z, or 400 non-gold tasks settled, whichever is first | [7226424813], [7225127190] |

## Studies

Each section gives the question, the plan, the data, the measure and rule, the result, the decision, the deviations and the code. Studies from one issue appear in the order they were run.

### 1. Render against scan (#2103)

**Question.** Does the distance between a word drawn from its ALTO text and the scan crop at the same spot separate wrong words from right words better than the signals already held (`WC`, a dictionary flag)?

**Plan.** The issue body holds the proposal and the decision rule. The method was frozen in [7082093882] after two exploratory runs, and the held-out set was amended before scoring in [7082164041].

**Data.**
- Exploratory runs on the 18 Overproof dataset 3 pages: [7080812972] (Old Standard TT font) and [7081292004] (letter templates cut from each page's own ink).
- Held-out run: 7 of the 10 `en-asgold` pages. The Overproof copy `gold.py` reads has only 18 distinct pages, the same 18 already used, so the plan's held-out set was replaced before any scoring. Pages 004, 007 and 009 have no Chronicling America match and were dropped.
- 26,063 words after the filter, 4,178 wrong. The model used the 23,673 words with every signal, 3,073 of them wrong.

**Measure and rule.** AUC of logistic(existing + render) minus logistic(existing), 5-fold by page, 200 bootstrap draws. Go on to a trained model only if (1) the gain on all words is at least 0.05 with the interval above 0, and (2) the low-damage gain is above 0 and the combined model is not below the best single existing signal.

**Result** [7085802179].

| Test | Needed | Result | |
| --- | --- | --- | --- |
| 1. All words, gain | at least 0.05, interval above 0 | +0.113 (0.024 to 0.140) | pass |
| 2. Low damage, gain | above 0 | −0.044 (−0.073 to 0.039) | fail |
| 2. Low damage, existing + render against best single existing | not below | 0.743 against 0.726 (`lex_oov`) | pass |

Test 1 passes against a weak baseline: logistic(existing) scores 0.596, below the dictionary flag alone (0.697). Against the dictionary flag alone, the render signals add +0.013 (−0.032 to 0.094). The exploratory runs on the 18 Overproof pages are not evidence on their own, because each change came after seeing the previous result: [7080812972] gave +0.038 (all words) and +0.043 (real-word errors) over the combined existing signals, both under the 0.05 bar, and [7082093882] gave +0.049.

**Decision.** The idea is closed. No trained model. Letter templates did not help the swap check: AUC 0.631 (chamfer) against a bar of 0.75 [7081292004]. The reusable pieces stay in draft #2104.

**Deviations and caveats.** The held-out set was changed before scoring [7082164041]. Four of the 7 pages rendered entirely from the fallback font. The `template_usage` block reads zero, a reporting bug also present in the Overproof run. Ranking `WC` within a page, chosen to fix three constant-`WC` pages, hurt on this set (0.550 against 0.696 for raw `WC`). `gold.py` marks American Stories `scoring: False` because its dataset card was undocumented when the table was written.

**Code.** `study/2103-render-compare` (320c2af1). The frozen method is `53af36d07`, an ancestor of that tag.

### 2. Column finder, step 1, first attempt (#2107)

**Question.** Can a banded projection-profile column finder, independent of the OCR engine, serve as a reference for judging LoC's layout?

**Plan.** [7085973660], frozen before scoring, with the full-height profile of the NYTribune script as baseline. Reference gutters came from gaps between side-by-side human region boxes.

**Data.** Tuning: `de-rz`, 81 Reichsanzeiger pages. Test: `en-asgold` (7 pages, primary) and `de-cg`, Chronicling Germany's Test split (100 pages, secondary).

**Rule.** Go on to step 2 only if (1) `en-asgold` precision and recall are both at least 0.80, (2) `de-cg` precision and recall are each at least 0.70, and (3) banded F1 beats full-height on `en-asgold`.

**Result** [7086303955].

| Condition | Needed | Banded result | |
| --- | --- | --- | --- |
| 1. `en-asgold` | both at least 0.80 | P 0.836, R 0.400 | fail |
| 2. `de-cg` | both at least 0.70 | P 0.642, R 0.775 | fail |
| 3. Banded F1 beats full-height | strictly greater | 0.541 against 0.703 | fail |

**Decision.** Step 2 does not go ahead with this finder. The reference was judged a weak stand-in for column labels. Newspaper Navigator boxes mark pictures, not columns, so they could not serve as the fallback reference.

**Deviations.** None after the freeze. The comment gives a possible cause (`min_run` tuned on a gazette full of tables) and says it was not tested.

**Code.** Frozen at `7a303f45` (#2112). It has no tag of its own; it is an ancestor of `study/2107-step1-columns` (337fc0aa).

### 3. Column finder, step 1, second attempt (#2107)

**Question.** The same, with a reference built from five converted public layout datasets (#2113) instead of region gaps.

**Plan.** [7087220001], then three amendments before any test data: coverage matching in place of one-to-one matching [7087887965], an American Stories contender added [7088003727], and precision counted only where the reference can tell right from wrong, a / (a + b) [7088148178]. Frozen at `69cf3ccd` in [7088695280].

**Data.** Tuning on 500 La Liberté train pages, 623 finlam train pages, 888 Colorado train pages and the 81 `de-rz` pages. Primary test pool: Europeana (50 pages, printed separators), finlam test (48) and La Liberté test (433), weighted equally.

**Rule.** Go on to step 2 only if the primary pool has precision and recall at least 0.80, and each primary set has both at least 0.70.

**Result** [7088965425]. Banded: primary pool P 0.849 (0.802 to 0.888), R 0.892 (0.867 to 0.911), F1 0.870. Lowest set is Europeana, P 0.755 (0.632 to 0.845). Both conditions are met. American Stories zero-shot: pool P 0.990, R 0.664, F1 0.795.

| Set | Banded P / R / F1 | Unscored share (c), banded | American Stories P / R / F1 |
| --- | --- | --- | --- |
| Europeana | 0.755 / 0.896 / 0.820 | 0.25 | 0.999 / 0.506 / 0.672 |
| finlam test | 0.910 / 0.933 / 0.922 | 0.64 | 0.976 / 0.766 / 0.858 |
| La Liberté test | 0.881 / 0.847 / 0.863 | 0.68 | 0.994 / 0.719 / 0.834 |
| `en-asgold` (7 pages, descriptive) | 0.943 / 0.550 / 0.695 | 0.54 | 0.997 / 0.941 / 0.968 |

**Decision.** Step 2 goes ahead with the banded finder.

**Deviations.**
- Both metric amendments were made after seeing tuning results. The comment says so, and that under the first plan's one-to-one metric the same detections score F1 0.648 [7088965425].
- 62–69% of detected gutter height on the tuning sets falls in the unscored part (c) [7088695280]. On the test and validation sets the banded share is 0.64 to 0.69 for finlam and La Liberté [7088965425]. Precision rests on about a third of the output.
- One loader crash fix before any detection ran: the Europeana image column is `image` (`337fc0aa`).
- The tuning ran without a recorded `usnm-claim`.
- The primary test has no English pages.

**Code.** `study/2107-step1-columns` (337fc0aa); reference converters `study/2107-refs` (d59b8998).

### 4. ALTO-versus-columns layout signals, step 2 (#2107, #2116)

**Question.** Does LoC ALTO text that ignores the detected columns mark bad OCR?

**Plan.** [7089002391], frozen before scoring.

**Data.** Test A: the 25 gold pages already used in #2103 (18 Overproof, 7 `en-asgold`). Test B: Beyond Words pages joined to one LoC ALTO page, with a planted merge of each line with its neighbour across the nearest gutter.

**Rule.** Add the layout signals only if test A beats the baseline by at least 0.02 AUC with the interval above 0, and test B AUC is at least 0.90.

**Result** [7090158532].

| Test | Needed | Result | |
| --- | --- | --- | --- |
| B: planted line merges | AUC at least 0.90 | 0.9947 (106 pages) | pass |
| A: word errors | gain at least 0.02, interval above 0 | −0.0027 (−0.0056 to +0.0016) | fail |

Test A: 39,697 words with both baseline signals, 6,935 wrong; baseline AUC 0.7777, with layout 0.7750. A rerun of test B with an exact METS join reached only 33 pages: AUC 0.968, planted copy higher on 33 of 33 [7090888846]. With American Stories gutters it falls to 0.889 on 30 pages, below 0.90; this was secondary.

**Decision.** The layout signals are not added to the word-level quality signals. `cross_share` can flag pages with merged columns for re-segmentation. Whether flagged pages have worse reading order is untested, because there is no reading-order truth for Chronicling America.

**Deviations.** Test A ran before test B finished, and its result was held until B passed. Only 107 of 3,558 Beyond Words pages joined to exactly one ALTO page, against a target of 300. The first join crashed on one shard and was rerun in full. `missed_ink` averages 0.397 and is unvalidated, so it should not be read as missed text.

**Code.** `study/2107-alto-columns` (b684e2f1).

### 5. American Stories model as non-text mask (#2107, #2114)

**Question.** Can the American Stories layout model supply the corpus mask of pictures, ads and similar regions?

**Plan.** [7089945598], class mapping fixed in advance. **Data.** The 712 Beyond Words validation pages. **Rule.** Mask recall at least 0.80 and mask precision at least 0.70.

**Result** [7090168950]. Mask recall 0.749 (0.736 to 0.762), fail. Mask precision 0.891 (0.872 to 0.908), pass. Box AP at IoU 0.5 is 0.675. Photographs account for most of the missed area (mask recall 0.550).

**Decision.** The model's boxes are not used as the corpus mask. Mask F1 is 0.814, the baseline for the trained detector's rule 2. **Deviations.** None. Beyond Words labels are incomplete, so mask precision is a lower bound.

**Code.** `study/2107-nontext-mask` (1b6efaaf).

### 6. Trained layout detector (#2107, #2119)

**Question.** Does a YOLOv8m trained on finlam, La Liberté and Beyond Words pages, with American Stories as teacher, beat the American Stories model?

**Plan.** [7090026956] with class mapping [7090103753]. Checkpoint `best.pt` (sha256 `7d75048e…5b62f`, epoch 49 of 60) posted in [7100626610] before evaluation. Adopt only if all three hold: column pool F1 at least 0.825, Beyond Words validation mask F1 at least 0.844, Colorado validation column F1 no more than 0.02 below American Stories.

**Result** [7101133928].

| Rule | Needed | Result | |
| --- | --- | --- | --- |
| 1. Column pool F1 | at least 0.825 | 0.642 (P 0.997, R 0.474) against 0.795 for American Stories | fail |
| 2. Beyond Words mask F1 | at least 0.844 | 0.901 (P 0.857, R 0.950) | pass |
| 3. Colorado column F1 | within 0.02 | 0.799 against 0.802 | pass |

**Decision.** The American Stories model stays the detector.

**Deviations.** Training ran out of GPU memory in epoch 26 and was resumed from epoch 25 with the same settings; the log for the resumed epochs was lost, `results.csv` covers all 60. The trained model's gutters used boxes at confidence 0.25 while American Stories used every box down to 0.01. This departs from the plan, and its effect was not measured in that comment. [7106474139] measured it later (section 10).

**Code.** `study/2107-yolo-train` (501397f2).

### 7. Combination of banded and American Stories gutters, La Liberté (#2107)

**Question.** Does adding American Stories gutters where banding found none raise recall without costing precision?

**Plan.** [7102058528]. Descriptive diagnostics on early-stopping holdouts came first (banded alone 0.868 / 0.889 / 0.879 P / R / F1; banded plus American Stories 0.881 / 0.946 / 0.912). **Data.** 500 fresh La Liberté pages (id-list sha256 `eddf61be…b523`). **Rule.** ΔF1 at least 0.02, the interval above 0, and precision falling by no more than 0.02.

**Result** [7102798228].

| Method | P | R | F1 |
| --- | --- | --- | --- |
| Banded alone | 0.911 | 0.842 | 0.875 |
| Banded plus American Stories gutters | 0.914 | 0.932 | 0.923 |
| Difference | +0.003 | +0.090 | +0.048 (0.043 to 0.053) |

**Decision.** Adopted for "this kind of page, one French title from 1925–1928". For English and German it stays an option until tested.

**Deviations.** "At least one body-text reference gutter" was read as a non-empty scoring reference after the 3% cap (10 pages skipped). The first run crashed writing the results file on a numpy boolean; saved gutters were then scored with no new inference. For that reason `results.json` records the model hash as null.

**Code.** `combo_eval.py` at `02f74ea59`, `diag.py` at `31b624560`; tag `study/2107-diag` (b86e88a3).

### 8. The combination on German pages (#2107)

**Plan.** [7102857621], page list and adapter posted in [7102968397] (adapter `aa2c3e17f`, id-list sha256 `e67d4a61…1bda`). **Data.** 300 fresh Chronicling Germany pages (261 from the Kölnische Zeitung). **Rule.** The same three conditions as section 7.

**Result** [7103363654]. Banded alone P 0.970, R 0.971, F1 0.971; combination F1 0.972; difference +0.0017 (interval 0.0014 to 0.0020), against the 0.02 required. American Stories alone: P 1.000, R 0.639, F1 0.780. Banded does poorly on three titles: Der Bazar (3 pages, F1 0.135), the Stuttgarter Zeitung (1 page, 0.325) and the Vossische Zeitung (14 pages, 0.780).

**Decision.** For German pages the banded finder stays alone. No deviations. **Code.** `study/2107-diag` (b86e88a3), `cg_refs.py` at `aa2c3e17f`.

### 9. English volunteer test of the combination (#2107)

**Plan.** [7103801519], page list in [7104597711] (300 pages, id-list sha256 `90af1542…a41a882`). Volunteers on the check site would judge gutter lines drawn by each method.

**Result** [7105288191]. The test was stopped before any task was loaded. Banded drew 54,609 segments (H_b = 15,811,999 px); the extra American Stories gutters 970 segments (H_x = 340,511 px). Rule 1 needs G = P_x·H_x / (P_b·H_b) at least 0.05, but H_x/H_b = 0.0215. G can reach 0.05 only if P_b is at most 0.43. At banded precision 0.849 and P_x = 1, G would be 0.025. The comment calls 0.849 the `en-asgold` figure, but in [7088965425] it is the primary-pool precision and `en-asgold` is 0.943.

**Decision.** The combination is not adopted for English. English keeps the banded finder alone.

**Deviation.** The plan said tasks would be loaded once and analysed once. The stop uses only method output, no answers. Banded precision on English pages was not measured. Two build problems were found: gold positives were all short French pieces (median 29 px), and 17 of 600 sampled extra segments were under 20 px.

**Follow-on.** The crops were loaded as labelling sets, which decide nothing: `en2107-l1` [7106777752], replaced by `en2107-l2` (1,303 tasks) [7108565573] after 51 of 67 gold positives touched print. The refit cut lines touching print from 255 to 105 of 600 banded and from 309 to 81 of 583 extra. As of 24 November no label had settled [7131927738]. English gutter checks are now also served through `/en` [7230988543].

### 10. Threshold diagnostic for the trained detector (#2107)

**Question.** Does the confidence threshold explain the trained model's low recall? Diagnostic only; no adoption.

**Plan.** [7105573766]. **Data.** Unused La Liberté pages (300 dev, 500 test) and Chronicling Germany pages (100 dev, 215 test).

**Result** [7106474139]. On dev, recall falls as the threshold is lowered (pool recall 0.256 at c = 0.01 against 0.487 at 0.25), so c* = 0.25. On test: trained model pool F1 0.670 (0.653 to 0.686), American Stories 0.809 (0.793 to 0.823). The difference of 0.139 exceeds the plan's 0.02 margin, so reading 2 applies: the threshold does not explain the gap.

**Decision.** A second model would need columns as its training target, and German pages in training. The result comment lists no deviations. **Code.** `study/2107-threshold` (c9f43624).

### 11. Banded v2, tilt and rule refit (#2107)

**Question.** Does a refit of banded gutters for tilt and printed column rules keep column accuracy and avoid false gutters?

**Plan.** [7122420362]; code frozen at `54eb0e99` in [7122666185]. **Data.** Step 1: 500 fresh La Liberté pages. Step 2: 711 Beyond Words validation pages (one page, 3304, has no fetchable scan [7129266762]). **Rule.** v2 replaces v1 if (1) the ΔF1 interval's lower bound is above −0.01, and (2) the share of gutter height inside marked Beyond Words boxes for v2 is no more than 0.005 above v1's.

**Result** [7131094183].

| | Precision | Recall | F1 |
| --- | --- | --- | --- |
| v1 | 0.901 | 0.848 | 0.874 |
| v2 | 0.902 | 0.859 | 0.880 |

ΔF1 +0.0057 (+0.0039 to +0.0077): rule 1 passes. Share of gutter height inside a marked box:

| Method | Share (95% CI) |
| --- | --- |
| Banded v1 | 0.374 (0.354 to 0.394) |
| Banded v2 | 0.393 (0.373 to 0.413) |
| Combination | 0.359 (0.339 to 0.378) |
| American Stories at 0.01 | 0.033 (0.027 to 0.040) |
| Trained YOLOv8m at 0.25 | 0.014 (0.010 to 0.017) |

v2 − v1 = +0.019 (+0.016 to +0.022), above the +0.005 allowed: rule 2 fails.

**Decision.** v2 is not adopted. v1 stays everywhere. The comment gives a likely cause, rule snapping onto the borders of adverts and pictures, and says it was not tested. About 37% of banded gutter height runs through marked non-text boxes, a lower bound because Beyond Words marks no body text.

**Deviations.** The page ledger in the plan was wrong about the Chronicling Germany Test split; it is the spent `de-cg` set. The correction is [7123117847] and does not touch this test. The fetcher requested a 404 URL about 260 times over about three hours, never above one request a second.

**Code.** `study/2107-banded-v2` (54eb0e99).

### 12. Combined segmenter (#2107)

**Question.** Can a gradient-boosted classifier that pools candidate gutters from banded v1, banded v2, American Stories and the trained model beat the current baseline per page type?

**Plan.** [7131927738] with the training sample in [7131932511]. Model frozen in [7143082799] (code `4ff6b179`, model sha256 `034f8f39…ebf375a5`, threshold 0.30, Beyond Words weight factor 0.25); a missing-ALTO rule was set in [7143113421], all before any test page ran. **Rules.** La Liberté: ΔF1 at least 0.02, interval above 0, precision falling by no more than 0.02. English: (1) the false-gutter share falls with the interval below 0, and (3) the gutter height outside marked boxes is at least 0.85 of banded v1's, lower bound above 0.80. German: no decision.

**Result** [7147408816].

| La Liberté, 500 fresh pages | P | R | F1 (95% CI) |
| --- | --- | --- | --- |
| Combination | 0.903 | 0.932 | 0.917 (0.905 to 0.927) |
| Combined segmenter | 0.974 | 0.937 | 0.955 (0.950 to 0.960) |

ΔF1 +0.039 (0.031 to 0.047), ΔP +0.072 (0.057 to 0.086), ΔR +0.006 (0.002 to 0.010). English, 711 Beyond Words validation pages: rule 1 passes (false-gutter share 0.374 to 0.092, Δ −0.282); rule 3 fails (0.509, 0.494 to 0.525, against 0.85). German `de-cg` (spent, 66 of 100 pages with a reference, descriptive): banded F1 0.938, combined 0.969, ΔF1 +0.032 (0.009 to 0.056).

**Decision.** The combined segmenter replaces the combination for La Liberté-type pages. English stays on banded v1; the decision is labelled partial because no `en-gutter` label had settled. No German decision.

**Deviations.** 21 English pages had no ALTO and kept empty word features. Inputs were assembled and scored by two small wrapper scripts that call the frozen functions. One Beyond Words page was dropped for a missing scan at training. The comment notes that the combiner removes false gutters mostly by drawing about half as many gutters, and that some of what it drops may also be false.

**Code.** `study/2107-combo-seg` (4ff6b179).

### 13. Segment-first reading (#2122)

**Question.** Does Tesseract read English pages better one column at a time (arm C, PSM 4) than whole page (arm W, PSM 3)? LoC's ALTO text is arm L.

**Plan.** The issue body, frozen before scoring; code frozen at `01745f3a` in [7106160888]. **Data.** 25 pages (18 Overproof, 7 `en-asgold`), 446 truth articles. Both sets had been scored before. **Rule.** Reading by column becomes the English re-recognition step if trigram recall C − W is at least 0.02 with the interval above 0, and unigram recall C − W is at least −0.005.

**Result** [7106460936].

| Arm | Trigram recall | Unigram recall | Lines read across a gutter |
| --- | --- | --- | --- |
| L, LoC | 0.654 (0.537 to 0.750) | 0.896 (0.854 to 0.933) | 657 of 16,023 |
| W, whole page | 0.775 (0.665 to 0.862) | 0.943 (0.908 to 0.970) | 1,577 of 15,285 |
| C, by column | 0.757 (0.650 to 0.840) | 0.912 (0.833 to 0.962) | 12 of 15,598 |

C − W: trigram −0.018 (−0.046 to +0.014), unigram −0.031 (−0.082 to +0.002). Rule 1 not met. Article CER, pooled: L 0.082, W 0.168, C 0.131; median per article: L 0.0116, W 0.0058, C 0.0129. The comment warns the pooled figures are dominated by a few badly located articles.

**Decision at the time.** Reading by column is not the re-recognition step, and the comment states the descriptive finding W − L +0.120 trigram (+0.065 to +0.193). Both were later revised, see Corrections and withdrawals.

**Deviations.** Three Overproof articles share one `document_id` (`sn88074142-8-00280761485-1871122101-0816`); the loader was changed to keep them as three truth articles [7106419239]. This changed the loader only, not the arms.

**Code.** `study/2122-segfirst` (b045416f); arms at `01745f3a`.

### 14. Beyond Words captions (#2122)

**Question.** Does whole-page Tesseract beat LoC's text on Chronicling America photograph captions?

**Plan.** [7129161579]; code frozen at `97e7ce8f` in [7129510713]. **Data.** 574 pages, 803 Photograph captions of at least 20 characters, 96 titles, all 1917–1918, with at most 10 pages per title. The plan states the bias: volunteers corrected LoC's own OCR, so the truth leans toward LoC. **Rule.** Trigram recall W − L at least 0.02 with the interval above 0, and the unigram lower bound above −0.005. The measure is recall.

**Result** [7133172084].

| | Trigram recall | Unigram recall |
| --- | --- | --- |
| L, LoC | 0.574 (0.508 to 0.635) | 0.749 (0.694 to 0.799) |
| W, Tesseract whole page | 0.628 (0.577 to 0.676) | 0.791 (0.743 to 0.835) |
| W − L | +0.054 (+0.006 to +0.102) | +0.042 (−0.001 to +0.081) |

Both rules were met, narrowly: the trigram lower bound is +0.006 and the unigram bound clears its floor by 0.004. Article CER pointed the other way: pooled L 0.232, W 0.270; median per caption L 0.103, W 0.133. The comment noted that the caption box includes the picture.

**Decision at the time.** More evidence that English whole-page Tesseract beats LoC, not enough to replace LoC in search. Later downgraded and withdrawn, see Corrections and withdrawals.

**Deviations.** 574 pages rather than 600, because the 10-per-title cap ran out. Fetch helpers outside the frozen code were restarted once after a truncated download.

**Code.** `study/2122-bw-captions` (97e7ce8f).

### 15. `en-read` pick task (#2122, pending)

**Question.** On English body text, when whole-page Tesseract and LoC read the same printed word differently, which is right, and how often?

**Plan.** [7133847084], frozen before any sample page was built. Volunteers pick between two readings with the sides hidden. **Data.** 200 sample pages drawn from 643,021 held English pages; 41 pages crashed Tesseract on the JP2 and were read from LoC's JPEG; 48 pages (24%) fell below the 50% pairing rule, so 152 pages give tasks. Loaded 2026-11-24 22:05Z: `tess-loc-v1`, 905 tasks, and `tess-loc-gold-v1`, 120 gold tasks [7145789797].

**Rule.** All four must hold: (1) net win share p_T − p_L at least 0.10 with the lower bound above 0; (2) ΔWER at most −0.005 with the upper bound below 0; (3) gold verdict accuracy at least 0.85, and at least 0.80 on both T-right and L-right gold, otherwise the result is void; (4) at least 150 of the 200 pages give a settled task, and Neither, Can't tell and disputed together are at most 35% of settled tasks.

**Status.** Open, closing 2027-01-19. The analysis runs once then, or earlier if every task has settled or closed. With fewer than 150 pages with a settled task, the result is descriptive and decides nothing. Pilot figures for the pairing rule: 61.5% of LoC words pair, 12.7% disagree [7133847084].

**Deviations and changes during collection.** Two-line word crops fixed for 6 tasks, set renamed `tess-loc-v1b` [7148381795]. 13 tasks held for lookalike-only readings (`lookalike-display-v1`) [7148946357]. A Bad crop answer was added on 25 November [7181508554]; Bad crop tasks are reported as "left out: build defect". From 2026-11-28 00:25 UTC the words are also served through `/en`; the closing analysis will report estimates before and after that moment as a sensitivity check [7230988295].

**Code.** `study/2122-enpick` (7b8c1bf9).

### 16. ALTO-only reading-order audit (#2167)

**Question.** How much of the corpus has LoC text that reads across columns? Validate an ALTO-only version of `cross_share` before running it on the corpus.

**Plan.** The issue body, frozen before any measure was computed. **Data.** The 107 joined pages of #2116 (spent, so descriptive). **Rule.** Both: planted-merge AUC at least 0.90, and Spearman correlation with the image-based `cross_share` at least 0.60. If either fails, the corpus run does not go ahead.

**Result** [7206057060].

| Check | Threshold | Result | |
| --- | --- | --- | --- |
| Planted merges, AUC (106 pages) | at least 0.90 | 0.961 (0.930 to 0.987) | pass |
| Spearman with image `cross_share` (107 pages) | at least 0.60 | −0.04 (−0.24 to 0.15) | fail |

**Decision.** The corpus run does not go ahead. The comment's likely reason: where LoC read across columns, its ALTO geometry fuses the columns, so no gap is left for an ALTO-only pass to find (`split_columns` found a gutter on 35 of 107 pages). Finding broken reading order needs the page image.

**Deviations.** 76 of 107 pages use the pre-v2 ALTO namespace and `inch1200` coordinates, which the Rust reader refuses; the harness maps the namespace in memory.

**Code.** `study/2167-alto-audit` (d6109183).

### 17. Proxy blind spot and over-read checks (#1922)

**Question.** Does the `lm_oov` proxy still miss VLM errors at full resolution, and can image- or reader-based checks catch over-reading without using LoC as a reference?

**Plans.** [7219412999] (blind spot) and [7219725684] (checks A and B), both frozen before scoring. The adopted proxy evidence rule is [7219412526]; its text is in the quality-estimation report under "Evidence rule for #1922". **Data.** The 345 `en-op` crops (blind spot) and all 1,259 #968 benchmark crops (checks). Both are spent truth, so the results are descriptive.

**Result, blind spot** [7219538393]. Pairs where true CER differs by at least 1 point.

| LoC against | Pairs | LoC truly better | `lm_oov` balanced accuracy | Length rule | Combined | Verdict |
| --- | --- | --- | --- | --- | --- | --- |
| Xiaomi v5, full resolution | 321 | 11 | 0.578 (0.483 to 0.706) | 0.544 | 0.622 | Inconclusive (fewer than 15 pairs) |
| Qwen 8B, capped at 4.2 Mpx | 324 | 35 | 0.588 (0.525 to 0.659) | 0.784 (0.700 to 0.859) | 0.800 (0.717 to 0.873) | Cannot see VLM losses |

The length rule clears the bar for Qwen, but the comment says this is not evidence: it was found on these same crops in the quality-estimation study and needs fresh English truth.

**Result, checks** [7220509065].

| | Xiaomi, full resolution | Qwen 8B, capped |
| --- | --- | --- |
| True over-reads | 21 | 61 |
| Check A precision / recall | 0.016 / 0.238 | 0.050 / 0.262 |
| Check B precision / recall | 0.075 / 0.143 | 0.615 / 0.393 |
| A or B precision / recall | 0.017 / 0.286 | 0.076 / 0.426 |

Check A's pooled median character-count error is 14.8%, against a target of at most 10%. Calibration fell back to the set-level ratio on 1,073 of 1,259 crops, so the run mostly tested the fallback.

**Decision.** Both VLMs fail the checks. VLM faithfulness still needs human picks under step 6. The proxy can open human pick tasks only for Tesseract-type candidates.

**Deviations.** The blind-spot run held no `usnm-claim` on its container and copied lake slices over ssh. Balanced accuracy was taken as the mean of the recalls on the two pair types, ties scored 0.5, as `parity.py` does. The over-read run read the benchmark read-only over NFS inside a claimed container and ran the scorer from a local copy because it was slow on NFS.

**Code.** No `study/` tag. Code and outputs are held outside the repository; locations are in [7219538393].

### 18. Full-resolution VLM crops (#968)

**Question.** Production VLM crops were cut from half-size decodes. Does cutting them at full resolution lower CER?

**Plan and rule.** A pooled CER gain of at least 0.5 points. The rule was set in a coordination message before the A/B ran but not posted on the issue until the results were in [7220974702]. The comment says the gains are far above the threshold, so the timing does not change either decision. The audit adds that the rule has no confidence-interval clause. **Data.** All 1,259 benchmark crops, run through the production BOINC binary (`usnm_qwen` 1.20); measure is pooled folded CER.

**Results.**

| Run | Half resolution | Full resolution | Difference | Source |
| --- | --- | --- | --- | --- |
| Xiaomi v5 | 7.11% | 4.52% | −2.60 (−3.13 to −2.11) | [7201889239] |
| Qwen 8B, uncapped | 6.87% | 11.38% | +4.50 (−0.16 to +10.28) | [7204527727] |
| Qwen 8B, capped at 4.2 Mpx, old app | 6.87% | 12.57% | +5.70 (+0.81 to +11.79) | [7205861026] |
| Qwen 8B, capped at 4.2 Mpx, fixed app (#2168) | 6.87% | 4.27% | −2.60 (−4.14 to −1.52) | [7208268194] |

**Decision.** Full resolution is on for Xiaomi campaigns and, capped at 4.2 Mpx with the fixed app, for Qwen campaigns. The earlier Qwen failures came from an app limit: crops near 4.2 Mpx overflowed the 4,096-token context and failed the whole 24-crop unit (72 crops scored empty). The fix gives an 8,192-token context and fails a refused crop on its own. Production use needs #2168 merged and a new `usnm_qwen` release. Qwen work goes to cards with 6 GB or more, and the fixed app peaked at 7.5 GB VRAM, so the release must either give full resolution only to hosts with at least 8 GB or use a lower cap on 6 GB hosts.

**Deviations.** The capped run was added after the uncapped one failed; the cap is Xiaomi's effective limit, fixed in advance, and the comment discloses that it was judged on the same set by the same rule. The audit counts three Qwen runs on the same 1,259 crops (fail, fail, then pass after the app fix).

**Code.** No `study/` tag. The A/B outputs are held outside the repository; the location is in [7201889239].

### 19. Completeness gate (PR #1915)

**What it is.** Merged 2026-11-13 [PR #1915](https://github.com/tgoodyear/usnewsmap.com/pull/1915). A page that passes rule v1 also needs our characters over LoC's at least 0.85 (`short`) and at least 0.30 of LoC's words in ours (`cover`).

**Measurement** [7224737807]. On the live release `pages-v20261120-1`, 11,773 of the 19,124 English pages that reach the gate are blocked (61.6%): 8,366 as `short` and 3,407 on `cover`. Almost all are in LoC's worst damage decile: the share blocked is 20% for LoC damage 0.225–0.35, 66% for 0.35–0.5, 92% for 0.5–0.65 and 91% for 0.65–0.8. qe-v1 prefers our reading on 96.3% of blocked pages. The median blocked page has LoC damage 0.53 against 0.19 for ours. On `cover` blocks our text is longer than LoC's (median ratio 1.24), and qe-v1 prefers ours on all 3,407. For `short`, below a length ratio of 0.4 qe-v1 prefers LoC on 250 of 720 pages; between 0.4 and 0.85 on 189 of 7,646. The comment states the limits: these are text-only signals, no human judgement against the image yet, and no second-reader signal.

**Plan for a human test** [7225127190], frozen before any task was built. People judge our word against LoC's on pages the gate blocks. **Rules.** Drop the `cover` test if, over the cover strata, the net win share is at least +0.20 with the lower bound above 0 and no damage band below 0. Lower the `short` threshold from 0.85 to 0.4 if the 0.4–0.85 strata meet the same rule. Pages below 0.4 are descriptive. The measure is word-level accuracy where the readings differ, which is transcription accuracy; omissions are not measured by picks.

**Deviations, set before any task was loaded** [7226221506]. The pairing rule was lowered from 50% to 2%, because on blocked pages it removed 42 of 63 pages including every `short` stratum. One stratum is empty (no blocked `cover` page below LoC damage 0.35), so the sample is 8 strata and 64 pages. Our side was re-run with Tesseract 5.5.0 because stored text has no word boxes. One page crashed Tesseract on its JP2 and was skipped. The `lookalike-display-v1` filter removed 127 candidates. The plan said 72 pages. The deviations comment gives a sample of 64 pages and, separately, says the 50% rule removed 42 of 63 pages. The loaded set has 630 tasks on 63 pages [7226424813], one fewer than 64, and one page was skipped for the Tesseract crash.

**Status.** Loaded 2026-11-27 at 19:20Z as `gate1915-v1`. The set is not served first: the site cannot prioritise sets, so these tasks share the queue with the `en-read` sample [7226424813]. The gate is unchanged until the analysis; with fewer than 150 settled tasks the result is descriptive and the gate stays.

**Code.** No `study/` tag. Scripts and tables are held outside the repository; see [7224737807].

## Corrections and withdrawals

1. **The caption result was downgraded.** On 27 November the methods audit found that the caption result [7133172084] should not be read as "whole-page Tesseract beats LoC". Recall counts only how much of the truth an arm finds, and inserted or junk words cost nothing. CER on the same captions favours LoC (pooled 0.232 against 0.270; median 0.103 against 0.133). The unigram margin cleared its floor by 0.004, and the truth was corrected from LoC's own text. The result was downgraded to inconclusive [7220974216].
2. **"Tesseract beats LoC" on English was withdrawn after jiwer.** WER, CER and bag-of-words F1 were recomputed on the saved outputs of the 25-page test [7221308955]. The recomputation reproduces the original trigram, unigram and CER figures exactly. On the 7 `en-asgold` pages of independently keyed truth, the primary set:

| | Trigram recall | WER | CER |
| --- | --- | --- | --- |
| W − L | +0.044, W better, interval excludes 0 | L better (+0.058) | L better (+0.065; lower bound about 0) |
| C − W | −0.034, W better, excludes 0 | C better (−0.029; includes 0) | C better (−0.031; includes 0) |
| C − L | +0.011 (includes 0) | includes 0 | L better (+0.034; excludes 0) |

   Overproof, whose truth is anchored to LoC, shows the same pattern and is descriptive only. The comment's reason for the disagreement: recall pools a page's words and short runs regardless of article order, whereas WER and CER align each article as one stretch and whole-page Tesseract breaks article continuity more often (36 articles above WER 0.5, against 23 for LoC). It withdrew "whole-page Tesseract beats LoC" ([7106460936], descriptive, and the caption result) and says that on established accuracy measures LoC's text is as good or better on the only independent truth held, "though 7 pages decide little".
3. **The rejection of segment-first reading was reopened.** It rested on trigram recall. On WER and CER, reading by column is level or slightly better [7221308955]. The audit adds that the arms differ in more than segmentation (PSM 4 against PSM 3), so a clean re-test needs a new run.
4. **Recomputation of the caption outputs has not been posted.** [7220974216] said the caption outputs and the 25-page outputs would both be rescored. [7221308955] reports the 25-page test only. No source here reports a rescoring of the 803 captions.
5. **Completeness-gate findings are pending a human test.** The measurement [7224737807] uses text-only signals and says that text measures can mislead, citing the #2122 correction. The human test is `gate1915-v1` (section 19).
6. **The test page ledger was corrected.** [7122420362] said Chronicling Germany's Test split was unused and held for the final test. [7123117847] corrects this: it is `de-cg`, already scored in step 1 and excluded from the German test. No Chronicling Germany page with a reference is unused, so a German decision for any new method needs new German truth or must reuse a spent set and say so.
7. **Findings that were never evidence.** The length rule for Qwen (balanced accuracy 0.784) was found on the same crops it was scored on [7219538393]. The exploratory #2103 runs came after seeing earlier results [7082093882]. The #968 rule was set before the A/B but posted after it [7220974702].

### Standing rules adopted

- **LoC is never the reference.** "The maintainer's rule is that LoC is never the reference" [7224737807]. The same comment measures the completeness gate for that reason, because the gate compares our text with LoC's.
- **Name the property optimised.** "Recall approximates word and phrase findability in search. WER and CER approximate faithful transcription. Future plans must name which property they optimise, before freezing" [7221308955]. The gate test names its own: transcription accuracy, with omissions not measured by picks [7225127190].
- **Established measures.** The corrections were made by rescoring with jiwer (WER, CER, bag-of-words F1) [7221308955]. The audit's central finding is that most layout and English-text decisions rest on bespoke measures never checked against an established one (Methods caveats, below). No comment states "use established measures" as a rule in those words.
- **Proxy evidence rule, #1922.** Adopted by the maintainer on 27 November "as written" [7219412526]. The text, from the quality-estimation report: a proxy can establish that LoC's text is worse than a candidate's but cannot establish that the candidate's text is accurate. It counts when (1) the measure is `lm_oov` with the omission guard, from a lexicon and character model not built from either reader's output; (2) the language is Latin-script Antiqua in near-modern spelling; (3) at least 300 pages and 3,000 crop pairs from at least 10 titles; (4) the candidate is preferred on at least 75% of crop pairs with the lower 95% page-bootstrap bound at least 65%; (5) the candidate returns no word on at most 5% of crops and the median ratio of its in-lexicon words to LoC's is at least 0.9; (6) the #1896 blind pick, on at least 50 pages of the language, prefers the candidate. "Meeting 1–5 can open the human pick task for a language. Replacing LoC requires all of 1–6." The #1915 completeness gate still applies.
- **Data via NFS.** No comment in the cited record states this as a rule. The only mention in the cited record is a run condition: the over-read test "read the benchmark read-only over NFS" inside a claimed container, with the scorer run from a local copy because NFS was slow [7220509065]. Plans in #2107 state a placement rule: dev containers only, "Nothing runs on a bare host" [7087220001], and a `usnm-claim` on the containers; two runs are disclosed as lacking one ([7087887965], [7219538393]).

## Methods caveats

The independent audit of 27 November read the frozen plans and the code they name, methods first and results second, ran no experiments and posted nothing. Its bottom line: the CER-based decisions (#968 full resolution, and #2127's plan) rest on an established measure and look robust. Most layout and English-text decisions rest on bespoke measures never checked against an established one. Two adopted decisions could plausibly flip under a standard measure: the caption win and the rejection of banded v2.

| Decision | Risk the audit gives | At risk |
| --- | --- | --- |
| Caption win [7133172084] | An established measure disagrees in direction (CER favours LoC, pooled and per caption). Recall cannot see insertions. Truth comes from LoC's own text. Lower bound +0.006. Three titles dominate. | High; plausibly flips. Already downgraded, then withdrawn. |
| Banded v2 rejected [7131094183] | Rests on rule 2, planned as descriptive: +0.019 against a 0.005 tolerance. Hand-drawn Beyond Words boxes are loose, the text zones in the scorer are shrunk by 1% of page width and these boxes are not, and the boxes are incomplete. | Medium-high; could flip. Low cost if wrong, since v2 lines are pooled into the combined segmenter anyway. |
| Combined segmenter adopted for La Liberté-type pages [7147408816] | ΔF1 +0.039 comes almost entirely from precision (+0.072), measured on about a third of output height under a metric amended after tuning. Contender outputs 1-px lines, baseline keeps wide pieces. The (c) share and one-to-one metric were computed but not reported. Trained and tested on one title. "La Liberté-type" is never defined. | Medium; direction likely robust, size and scope are not. |
| English segmenter rule 1 and the English false-gutter measure | The share inside the union of loose boxes; the trained detectors learned from Beyond Words, so rule 1 (0.374 to 0.092) is partly a test against its own labelling convention. | Medium, but not adopted. |
| Segment-first rejected [7106460936] | PSM 3 and PSM 4 differ between arms; n = 25 spent pages; recall only; pooled CER favours C (0.131 against 0.168) while the median favours W. | Low as "not adopted"; medium for the inference "layout work not justified". Reopened. |
| Proxy evidence rule thresholds [7219412526] | 75/65 are asserted, not derived. The proxy prefers the VLM more often than the truth does on `en-op`, so a preference share is not the share where LoC is worse. Validated on pairs at least 1 CER point apart but applied to every crop. Step 6 has no numeric definition. | Medium for validity, low for consequence, because steps 1–5 only open a human task. |
| #968 full resolution [7201889239], [7208268194] | The rule's freeze is not on the issue and has no interval clause. Qwen was tested three times on the same crops. Pooling is driven by the German sets. | Low. Effects (−2.60 points each) are far above the 0.5 rule. Only the English Qwen result (`en-op` worse) deserves a caveat. |
| Proxy blind spot and over-read checks [7219538393], [7220509065] | Spent data; `en-op` crop boxes come from LoC's own ALTO lines, so ink outside LoC's line reads as an over-read; thresholds 1.15, 0.10 and 0.20 never justified; check A mostly tested its fallback. | Low; nothing adopted. |
| Gate #1915 | Not in the audit's table. The measurement is text-only [7224737807]. | Pending the human test. |

On the corrections above, two sources differ in wording. The audit proposes relabelling the caption result "mixed" if WER flips; the issue downgraded it to "inconclusive" [7220974216] and then withdrew it [7221308955]. On the unscored share (c) for the banded finder, the audit and the freeze comment give 62–69% (tuning sets, and finlam and La Liberté); the result comment gives 64–69% (test and validation sets).

The audit lists ten fixes, cheapest first, none of them run. Those that touch decisions in this report: report the (c) share and one-to-one scores for the combined segmenter; rescore the baseline in the contender's 1-px format; recompute the caption and segment-first scores with `jiwer`, `dinglehopper`, bag-of-words F1 and flexible character accuracy; recheck the Beyond Words false-gutter share with boxes shrunk by 0.5% and 1% of page width; tie the proxy thresholds to accuracy using the `en-op` confusion matrix; show the #968 rule's freeze trail; fix the `en-read` analysis before data arrives (describe the 48 unpaired pages, sample unpaired words, report gold accuracy separately for body and caption gold); re-run segment-first with arms that differ only in segmentation; validate the layout metrics against volunteer labels.

## Open items

| Item | Closes | Condition | What decides |
| --- | --- | --- | --- |
| `en-read` (`tess-loc-v1`, `tess-loc-gold-v1`) | 2027-01-19, 8 weeks after the 2026-11-24 22:05Z load, or earlier if every task has settled or closed [7145789797] | At least 150 of 200 pages with a settled task, otherwise descriptive | Net win share at least 0.10; ΔWER at most −0.005; gold accuracy at least 0.85; coverage rule [7133847084] |
| `de-read` (`b-loc-v1`, #2127) | 2027-01-19, 8 weeks after the 2026-11-24 load [7140264700] | At least 300 of 370 tasks settled; no German gold, so volunteer accuracy cannot be checked | B better than LoC on US German pages if the difference is above 0 with lower bound above 0. About 81% must settle. Now also served through `/de` from 2026-11-27 22:52 UTC (sensitivity split) [7229735401] |
| `gate1915-v1` | 8 weeks after the 2026-11-27 19:20Z load (22 January 2027 by my count), or when 400 non-gold tasks have settled [7225127190], [7226424813] | With fewer than 150 settled, descriptive and the gate is unchanged | Net win share at least +0.20 on the cover strata, none below 0, to drop the `cover` test; the same on the 0.4–0.85 strata to lower the `short` threshold |
| English `en-gutter` labels (`en2107-l2`) | No closing date in the record | No label had settled as of 24 November [7131927738]; now also served through `/en` [7230988543] | An English combined-segmenter decision stays partial until at least 300 settled non-gold lines in the test half |
| Rescoring with established measures | None set | Caption outputs not yet reported; layout measures not yet validated | Audit fixes 1 to 10 |
| Segment-first re-test | None set | Needs arms that differ only in segmentation and fresh truth; `en-read` cannot supply it | A new frozen plan |
| ALTO-and-image reading-order measure | None set | A measure using the page image, run on held scans only (about 945k pages), would need its own plan [7206057060] | A new frozen plan |
| VLM faithfulness for English | None set | Step 6 human pick still required to replace LoC [7220509065] | The blind pick (#1896) on at least 50 pages |

The `en-read` and `gate1915-v1` sets share one queue, so their settle rates depend on each other [7226424813]. Qwen full-resolution campaigns need #2168 merged and a new `usnm_qwen` release [7208268194].

[7131094183]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7131094183
[7201889239]: https://github.com/tgoodyear/usnewsmap.com/issues/968#issuecomment-7201889239
[7208268194]: https://github.com/tgoodyear/usnewsmap.com/issues/968#issuecomment-7208268194
[7220974702]: https://github.com/tgoodyear/usnewsmap.com/issues/968#issuecomment-7220974702
[7147408816]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7147408816
[7102798228]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7102798228
[7103363654]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7103363654
[7105288191]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7105288191
[7101133928]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7101133928
[7090158532]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7090158532
[7219412526]: https://github.com/tgoodyear/usnewsmap.com/issues/1922#issuecomment-7219412526
[7085802179]: https://github.com/tgoodyear/usnewsmap.com/issues/2103#issuecomment-7085802179
[7090168950]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7090168950
[7106460936]: https://github.com/tgoodyear/usnewsmap.com/issues/2122#issuecomment-7106460936
[7206057060]: https://github.com/tgoodyear/usnewsmap.com/issues/2167#issuecomment-7206057060
[7219538393]: https://github.com/tgoodyear/usnewsmap.com/issues/1922#issuecomment-7219538393
[7220509065]: https://github.com/tgoodyear/usnewsmap.com/issues/1922#issuecomment-7220509065
[7133172084]: https://github.com/tgoodyear/usnewsmap.com/issues/2122#issuecomment-7133172084
[7220974216]: https://github.com/tgoodyear/usnewsmap.com/issues/2122#issuecomment-7220974216
[7221308955]: https://github.com/tgoodyear/usnewsmap.com/issues/2122#issuecomment-7221308955
[7224737807]: https://github.com/tgoodyear/usnewsmap.com/issues/1915#issuecomment-7224737807
[7225127190]: https://github.com/tgoodyear/usnewsmap.com/issues/1915#issuecomment-7225127190
[7145789797]: https://github.com/tgoodyear/usnewsmap.com/issues/2122#issuecomment-7145789797
[7140264700]: https://github.com/tgoodyear/usnewsmap.com/issues/2127#issuecomment-7140264700
[7226424813]: https://github.com/tgoodyear/usnewsmap.com/issues/1915#issuecomment-7226424813
[7082093882]: https://github.com/tgoodyear/usnewsmap.com/issues/2103#issuecomment-7082093882
[7082164041]: https://github.com/tgoodyear/usnewsmap.com/issues/2103#issuecomment-7082164041
[7080812972]: https://github.com/tgoodyear/usnewsmap.com/issues/2103#issuecomment-7080812972
[7081292004]: https://github.com/tgoodyear/usnewsmap.com/issues/2103#issuecomment-7081292004
[7085973660]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7085973660
[7086303955]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7086303955
[7087220001]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7087220001
[7087887965]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7087887965
[7088003727]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7088003727
[7088148178]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7088148178
[7088695280]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7088695280
[7088965425]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7088965425
[7089002391]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7089002391
[7090888846]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7090888846
[7089945598]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7089945598
[7090026956]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7090026956
[7090103753]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7090103753
[7100626610]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7100626610
[7106474139]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7106474139
[7102058528]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7102058528
[7102857621]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7102857621
[7102968397]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7102968397
[7103801519]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7103801519
[7104597711]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7104597711
[7106777752]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7106777752
[7108565573]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7108565573
[7131927738]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7131927738
[7230988543]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7230988543
[7105573766]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7105573766
[7122420362]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7122420362
[7122666185]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7122666185
[7129266762]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7129266762
[7123117847]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7123117847
[7131932511]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7131932511
[7143082799]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7143082799
[7143113421]: https://github.com/tgoodyear/usnewsmap.com/issues/2107#issuecomment-7143113421
[7106160888]: https://github.com/tgoodyear/usnewsmap.com/issues/2122#issuecomment-7106160888
[7106419239]: https://github.com/tgoodyear/usnewsmap.com/issues/2122#issuecomment-7106419239
[7129161579]: https://github.com/tgoodyear/usnewsmap.com/issues/2122#issuecomment-7129161579
[7129510713]: https://github.com/tgoodyear/usnewsmap.com/issues/2122#issuecomment-7129510713
[7133847084]: https://github.com/tgoodyear/usnewsmap.com/issues/2122#issuecomment-7133847084
[7148381795]: https://github.com/tgoodyear/usnewsmap.com/issues/2122#issuecomment-7148381795
[7148946357]: https://github.com/tgoodyear/usnewsmap.com/issues/2122#issuecomment-7148946357
[7181508554]: https://github.com/tgoodyear/usnewsmap.com/issues/2122#issuecomment-7181508554
[7230988295]: https://github.com/tgoodyear/usnewsmap.com/issues/2122#issuecomment-7230988295
[7219412999]: https://github.com/tgoodyear/usnewsmap.com/issues/1922#issuecomment-7219412999
[7219725684]: https://github.com/tgoodyear/usnewsmap.com/issues/1922#issuecomment-7219725684
[7204527727]: https://github.com/tgoodyear/usnewsmap.com/issues/968#issuecomment-7204527727
[7205861026]: https://github.com/tgoodyear/usnewsmap.com/issues/968#issuecomment-7205861026
[7226221506]: https://github.com/tgoodyear/usnewsmap.com/issues/1915#issuecomment-7226221506
[7229735401]: https://github.com/tgoodyear/usnewsmap.com/issues/2127#issuecomment-7229735401
