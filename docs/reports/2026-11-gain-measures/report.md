# Which gain measure tracks real CER? (20 Nov 2026)

The worst-first campaign (#1415) judges a re-read by the drop in LoC's damage rate, with a stop rule of 0.02. The [gain-proxy report](https://github.com/tgoodyear/usnewsmap.com/pull/1889) (draft PR #1889) found damage valid for English, French, Swedish and Finnish and not for German. The [quality-estimation report](https://github.com/tgoodyear/usnewsmap.com/pull/1925) (draft PR #1925) found qe-v1's `lm_oov` the best reference-free measure of a reading's quality. This report asks whether a drop in `lm_oov` tracks a real CER drop better than a drop in damage.

## Summary

- Over all 338 human-truth pages, `lm_oov` tracks CER drops better than damage: page-pair rho 0.66 against 0.48.
- Most of the difference is German, where damage barely tracks CER (0.27) and `lm_oov` does moderately (0.57). Even so, 16% of German pairs `lm_oov` counts as a gain are losses, so German stays held.
- In English, French, Swedish and Finnish both measures work. Damage at 0.02 is confirmed (93–99% of passing pairs really improved). Damage is better on Finnish.
- Truth volume is the limit: 20–28 pages per language outside German.

## Data and method

- Truth: the frozen #968 sets with human references (en-op, en-as, the new-newseye French, Finnish and Swedish sets, de-rz81 and de-nzz), 6,789 crop readings by LoC (English only), Tesseract PSM 3 and 6, Xiaomi-OCR-0 v5 and guarded Qwen 3-VL 8B. American Stories rows are left out, as in #1925.
- Measures per reading: the production damage rate and top-5,000 unknown share (`proxy-rows`, PR #1889), and qe-v1's `oov` and `lm_oov` (`charlm_bpc / 1.185 + oov / 0.117`), from the #1925 scripts. CER is folded (long s, double hyphen, superscript e).
- Pages pool their crops: CER weighted by reference characters, measures by words.
- Pairs: every two readers of the same page. For each measure, the drop between the two readings is compared with the CER drop.
- Statistics: Spearman's rho with 95% page-bootstrap limits (1,000 draws). For a like-for-like check, each other measure's threshold is set to pass the same share of pairs as damage at 0.02. "Fell" is the share of passing pairs whose CER really fell.

The script is [`scripts/gain_measures.py`](scripts/gain_measures.py). It writes numbers only.

## Results

| Language (pages) | damage rho       | `lm_oov` rho     | damage at 0.02: fell, mean CER drop | `lm_oov`: threshold, fell, mean CER drop |
| ---------------- | ---------------- | ---------------- | ----------------------------------- | ---------------------------------------- |
| English (28)     | 0.84 (0.71–0.93) | 0.88 (0.74–0.96) | 93%, 4.8 pt                         | 0.25, 94%, 5.2 pt                        |
| French (20)      | 0.80 (0.65–0.91) | 0.93 (0.88–0.96) | 98%, 8.2 pt                         | 0.40, 100%, 8.4 pt                       |
| Swedish (22)     | 0.87 (0.81–0.91) | 0.93 (0.86–0.97) | 94%, 6.7 pt                         | 0.42, 98%, 7.0 pt                        |
| Finnish (20)     | 0.94 (0.92–0.96) | 0.80 (0.66–0.89) | 99%, 5.8 pt                         | 0.21, 88%, 4.9 pt                        |
| German (248)     | 0.27 (0.19–0.35) | 0.57 (0.50–0.64) | 72%, 3.9 pt                         | 1.20, 84%, 9.3 pt                        |
| All (338)        | 0.48 (0.41–0.53) | 0.66 (0.60–0.71) | 83%, 4.9 pt                         | 0.86, 87%, 8.2 pt                        |

The full-lexicon `oov` alone scores 0.62 pooled, and the top-5,000 unknown share 0.57.

## Decision

1. `quality_gain.py` reports the mean `lm_oov` drop per cell beside the damage drop and the estimated CER drop, where a `usnm_quality` unit measured both texts with qe-v1. It decides nothing yet: the 0.02 damage rule keeps deciding English, French, Swedish and Finnish cells.
2. German stays held (#835). A German route needs its own gold-calibrated test.
3. The `lm_oov` thresholds above are starting values for a later rule, not a rule. Before one is set, the truth needs more pages, above all held-out Chronicling America English pages.

## Limits

- Engines against engines: LoC's text, the production baseline, is in the English sets only.
- English is 28 pages from two collections; French, Finnish and Swedish are 20–22 pages each from European libraries.
- Page pairs, not 374-page cells: the rule's cell mean is not tested directly.
- The thresholds are matched to damage's selectivity on these sets, so they inherit the sets' mix of readers.

## Reproducing

In a work directory holding the #1925 inputs (`data/968-dev2-20261112`, `data/as-gold-20261113/en-as`, `data/qe-resources`, `data/yi.txt`) and the #1925 scripts, run the truth half of `run_all.sh` (`prep.py`, `confusions.py`, `metrics.py --kind truth`, `damage.sh` with `proxy-rows` from PR #1889 at e4a34a38), then:

```sh
python3 scripts/gain_measures.py --metrics-dir metrics --damage damage-truth.jsonl --out gain-measures.json --boot 1000
```

Run on 20 Nov 2026 in CT 245 (usnm-dev3).
