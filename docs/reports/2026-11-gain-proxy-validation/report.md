# Does the damage-rate gain track real error? (13 Nov 2026)

The worst-first campaign (#1415) keeps or stops a cell (language × decade × damage band) on a proxy: the mean drop in damage rate between LoC's text and the re-read, with a stop rule of 0.02 (`quality_gain.py`, #1846). Damage rate is the share of a language's 20 commonest words misread by one edit. On German, the proxy and estimated CER disagreed (#1850). This report checks the proxy against human truth.

## Data and method

- Sets: the frozen #968 sets with human references: en-op, fr-new, fi-new, sv-new, de-rz81, de-nzz, and the 52-region fr, fi and sv sets.
- Readings: Tesseract 5.5.0 PSM 3 and PSM 6, Xiaomi-OCR-0 v5 and guarded Qwen 3-VL 8B. LoC text is used where it exists, which is en-op only.
- Scoring: each reading gets the production damage score (`score_forced`) and CER (`cer_wer`) against the reference. A folded CER also maps long s, double hyphen and superscript e to their plain forms on both sides.
- Comparison: for every pair of readers on the same unit, the damage drop is compared with the CER drop. Units are crops, sampled pages (2–3 crops) and chunks.
- Statistics: Spearman's rho, with 95% limits from page bootstrap. "CER drop at 0.02" is the CER change a 0.02 damage drop implies, from a fitted slope.

Tables for every unit, proxy and scoring are in `data/tables.md`. The numbers are in `data/results.json`. The code is in `scripts/`.

## Results, page level

| Language | rho (95%) | CER drop at 0.02 | Pages with damage drop ≥ 0.02 where CER also fell | Verdict |
|---|---|---|---|---|
| English | 0.72 (0.54 to 0.86) | 1.2 pt | 93% | valid |
| French | 0.60 (0.38 to 0.79) | 2.3 pt | 100% | valid; 0.02 is conservative |
| Swedish | 0.70 (0.49 to 0.82) | 0.9 pt | 100% | valid |
| Finnish | 0.69 (0.51 to 0.83) | 0.7 pt | 99% | valid; 0.02 means under 1 pt CER |
| German | 0.06 (−0.03 to 0.15) | 0.4 pt | 52% | not valid |

German, scored with long s folded, rises to rho 0.31 (0.23 to 0.39), and CER falls on 80% of pages with a damage drop of at least 0.02. That is still weak.

Why German fails:
- Long s. Qwen and Xiaomi write long s as s. The damage score rewards that, while diplomatic CER penalises it. Folded, their German CER is 2.1% (Qwen 8B) and 2.7% (Xiaomi), against 5.2% for Tesseract (`folded_cer` in `data/results.json`).
- Head-word counts are not the cause: German crops carry about 10.7 head-word tokens each, against about 14.8 in English and 17–18.7 in French (`head_per_crop` in `data/results.json`).

No threshold fixes German.

The share of unknown words tracks CER more closely than damage rate in every language. Page-level rho: English 0.73, French 0.73, Swedish 0.82, Finnish 0.74, German 0.34 (0.49 folded). German is still weak.

## Decision

1. Keep 0.02 as the worst-first stop rule for English, French, Swedish and Finnish. The English Xiaomi pilot (#1867) will be judged on it.
2. German: damage rate alone doesn't decide German cells. German stays held anyway: its frak2021 text is revoked at the next rebuild (#835).
3. Other Fraktur languages: any language printed in Fraktur or with modernised spelling is treated like German until it has human truth.
4. Next proxy work: test unknown-word share as a second gain measure, alongside damage rate, in `quality_gain.py`.

## Limits

- English is 18 pages from one collection, and it's a second use of the set. French, Finnish and Swedish are 26–28 pages each, from European libraries, not Chronicling America.
- The reader pairs are engines against engines. Only English has the production baseline, LoC's text.
- The data checks the proxy page by page. It can't test 0.02 as a mean over a 374-page cell directly.
