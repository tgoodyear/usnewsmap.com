# Which gain measure tracks real CER? (20 Nov 2026)

The worst-first campaign (#1415) judges a re-read by the drop in LoC's damage rate, with a stop rule of 0.02. An earlier check of damage against human truth (closed PR #1889, folded in below) found it valid for English, French, Swedish and Finnish and not for German. The [quality-estimation report](https://github.com/tgoodyear/usnewsmap.com/pull/1925) (draft PR #1925) found qe-v1's `lm_oov` the best reference-free measure of a reading's quality. This report asks whether a drop in `lm_oov` tracks a real CER drop better than a drop in damage.

## Summary

- Over all 338 human-truth pages, `lm_oov` tracks CER drops better than damage: page-pair rho 0.65 against 0.48.
- Most of the difference is German, where damage barely tracks CER (0.28) and `lm_oov` does moderately (0.56). Even so, 15% of German pairs `lm_oov` counts as a gain are losses, so German stays held.
- In English, French, Swedish and Finnish both measures work. Damage at 0.02 is confirmed (93–99% of passing pairs really improved). Damage is better on Finnish.
- Truth volume is the limit: 20–28 pages per language outside German.

## Data and method

- Truth: the frozen #968 sets with human references (en-op, en-as, the new-newseye French, Finnish and Swedish sets, de-rz81 and de-nzz), 6,789 crop readings by LoC (English only), Tesseract PSM 3 and 6, Xiaomi-OCR-0 v5 and guarded Qwen 3-VL 8B. American Stories rows are left out, as in #1925.
- Measures per reading: the production damage rate and top-5,000 unknown share (`proxy-rows`, in `scripts/`), and qe-v1's `oov` and `lm_oov` (`charlm_bpc / 1.185 + oov / 0.117`), from the #1925 scripts with the character model shipped qe-v1 matches (`--lm-sources gutenberg+wikisource`, [ocr/qe/README.md](../../../ocr/qe/README.md)). That variant adds Wikisource only where Gutenberg has under 3 million characters, so it differs from the Gutenberg-only model for German and Finnish alone. With Gutenberg only, Finnish `lm_oov` is 0.80 (0.66–0.89) and German 0.57, and nothing else moves. CER is folded (long s, double hyphen, superscript e).
- Pages pool their crops as production does: CER over reference characters; damage as sum(near) / sum(head + near) (`usnm_core::damage`); the unknown share as 1 − sum(known) / sum(words); `oov` over words and `charlm_bpc` over characters, with `lm_oov` computed from the two pooled values. Each reader pair is pooled over the crops both readers read; on these sets every reader read every crop of its pages, so no crop or pair was left out. Page CER weights each crop's CER by its reference length as `metrics.py` reports it (`ref_chars`, the language-model text form), not by the CER text form that `cer()` divides by. The two forms differ only where case folding or Unicode composition changes a character count, mostly German combining marks, and a crop gets the same weight for both readers of a pair, so the weighting can't favour one reader. It can move a page's pooled CER slightly from an exact sum(edits) / sum(reference characters).
- Pairs: every two readers of the same page. For each measure, the drop between the two readings is compared with the CER drop.
- Statistics: Spearman's rho with 95% page-bootstrap limits (1,000 draws). For a like-for-like check, each other measure's threshold is set to pass the same share of pairs as damage at 0.02. "Fell" is the share of passing pairs whose CER really fell.

The script is [`scripts/gain_measures.py`](scripts/gain_measures.py). It writes numbers only.

## Results

| Language (pages) | damage rho       | `lm_oov` rho     | damage at 0.02: fell, mean CER drop | `lm_oov`: threshold, fell, mean CER drop |
| ---------------- | ---------------- | ---------------- | ----------------------------------- | ---------------------------------------- |
| English (28)     | 0.85 (0.72–0.93) | 0.88 (0.74–0.96) | 93%, 4.7 pt                         | 0.23, 94%, 5.0 pt                        |
| French (20)      | 0.80 (0.64–0.91) | 0.93 (0.88–0.96) | 98%, 8.0 pt                         | 0.39, 100%, 8.3 pt                       |
| Swedish (22)     | 0.88 (0.82–0.92) | 0.93 (0.84–0.97) | 94%, 6.6 pt                         | 0.41, 98%, 6.9 pt                        |
| Finnish (20)     | 0.94 (0.91–0.96) | 0.85 (0.70–0.92) | 99%, 5.8 pt                         | 0.22, 88%, 5.0 pt                        |
| German (248)     | 0.28 (0.20–0.36) | 0.56 (0.49–0.64) | 72%, 4.0 pt                         | 1.22, 85%, 8.8 pt                        |
| All (338)        | 0.48 (0.42–0.54) | 0.65 (0.60–0.70) | 83%, 5.0 pt                         | 0.86, 87%, 8.2 pt                        |

The full-lexicon `oov` alone scores 0.62 pooled, and the top-5,000 unknown share 0.57.

## Damage against CER, by language and unit

This part comes from the earlier gain-proxy check (closed PR #1889). It uses the same truth sets, with the 52-region French, Finnish and Swedish sets added and sampled pages of 2–3 crops, and tests the damage drop on its own. Each row is the damage drop against the CER drop, over every pair of readers on the same unit. "CER drop at 0.02" is the CER change a 0.02 damage drop implies, from a fitted slope, with 95% limits from a page bootstrap.

Page level, diplomatic CER:

| Language | rho (95%) | CER drop at 0.02 | Pairs with damage drop of 0.02 or more where CER fell |
| -------- | ---------------- | ------ | ---- |
| English  | 0.72 (0.54–0.86) | 1.2 pt | 93%  |
| French   | 0.60 (0.38–0.79) | 2.3 pt | 100% |
| Swedish  | 0.70 (0.49–0.82) | 0.9 pt | 100% |
| Finnish  | 0.69 (0.51–0.83) | 0.7 pt | 99%  |
| German   | 0.06 (−0.03–0.15) | 0.4 pt | 52% |

A 0.02 damage drop therefore means about 1 pt of CER in English, Swedish and Finnish, and more in French, where 0.02 is conservative. In Finnish it means under 1 pt.

The same rho at three units, diplomatic CER (crops: 3,066 German, 3,096 English and 332–416 for the other languages; pages: 1,205 German and 127–162 for the others; chunks of about 100 head-word tokens: 310 German, 423 English, 45–75 for the others):

| Language | crop rho | page rho | chunk rho |
| -------- | -------- | -------- | --------- |
| English  | 0.47     | 0.72     | 0.65      |
| French   | 0.44     | 0.60     | 0.61      |
| Swedish  | 0.62     | 0.70     | 0.69      |
| Finnish  | 0.54     | 0.69     | 0.57      |
| German   | 0.11     | 0.06     | 0.07      |

The CER drop at 0.02 is 0.8, 1.5, 0.8, 0.4 and 0.3 pt at crop level (English, French, Swedish, Finnish, German) and 1.2, 2.2, 1.1, 0.7 and 0.2 pt at chunk level. The tables for every unit, scoring and measure, including the share of unknown words, are in the closed PR's `data/tables.md`.

### Why German fails

- **Long s.** Qwen and Xiaomi write long s as s. The damage score rewards that, while diplomatic CER penalises it. With long s, the double hyphen and superscript e folded on both sides, their German CER is 2.1% (Qwen 8B) and 2.7% (Xiaomi), against 5.2% for Tesseract.
- **Folding doesn't repair it.** Folded German rises to page rho 0.31 (0.23–0.39), and CER falls on 80% of pages with a damage drop of 0.02 or more. That is still weak.
- **Head-word counts are not the cause.** German crops carry about 10.7 head-word tokens each, against about 14.8 in English and 17–18.7 in French, so a thin word sample doesn't explain the weak rho.

No threshold fixes German. The share of unknown words tracks page CER more closely than damage in every language: English 0.73, French 0.73, Swedish 0.82, Finnish 0.74, German 0.34 (0.49 folded).

The folded CER and head-word figures are quoted from the closed PR's report; its `results.json` was not published.

## Decision

1. `quality_gain.py` reports the mean `lm_oov` drop per cell beside the damage drop and the estimated CER drop, where a `usnm_quality` unit measured both texts with qe-v1. It decides nothing yet: the 0.02 damage rule keeps deciding English, French, Swedish and Finnish cells.
2. German stays held (#835). A German route needs its own gold-calibrated test.
3. Any other language printed in Fraktur or with modernised spelling is treated like German until it has human truth.
4. The `lm_oov` thresholds above are starting values for a later rule, not a rule. Before one is set, the truth needs more pages, above all held-out Chronicling America English pages.

## Production-scale snapshot (as of 12 Nov)

The first run of `quality_gain.py` (#1846, closed PR #1850) covered 23,986 pages, the newest scored output of each re-read page, read by Tesseract 5.5.0 (about 23,300 German Fraktur and about 770 English). Cells are reader × band × language × decade, with the band taken from LoC's damage rate.

- The worst band had a mean damage drop of 0.137 over 18,693 pages. Every cell of 200 or more pages was a go, at 0.08 to 0.17.
- The moderate band had a mean damage drop of -0.002 over 1,525 pages, and 6 of its 26 cells stopped.
- The German rho shows up in production. German 1890 had no estimated-CER gain (0.003 over 187 pairs), and German 1900 was worse (-0.054 over 1,167 pairs), while damage gained in both (0.147 and 0.130).
- The serving version `pages-v20261026-1` had replaced LoC's text on 21,290 of the 25,846 re-read pages it considered, 20,652 of them German.

## Limits

- Engines against engines: LoC's text, the production baseline, is in the English sets only.
- English is 28 pages from two collections; French, Finnish and Swedish are 20–22 pages each from European libraries.
- Page pairs, not 374-page cells: the rule's cell mean is not tested directly.
- The thresholds are matched to damage's selectivity on these sets, so they inherit the sets' mix of readers.

## Reproducing

The scripts are in [`scripts/`](scripts/). `proxy-rows` is a Rust crate outside the workspace (`cargo build --release --manifest-path scripts/proxy-rows/Cargo.toml`). It prints each reading's production damage counts and CER.

For the damage rows, with DIR the directory of frozen #968 inputs (`refs.jsonl` and `hyps/<reader>.jsonl`):

```sh
python3 scripts/prep_rows.py --dir DIR --out rows-in.jsonl
scripts/proxy-rows/target/release/proxy-rows < rows-in.jsonl > rows-out.jsonl
python3 scripts/analyze.py --dir DIR --rows rows-out.jsonl --out proxy-out
python3 scripts/tables.py --results proxy-out/results.json
```

In a work directory holding the #1925 inputs (`data/968-dev2-20261112`, `data/as-gold-20261113/en-as`, `data/qe-resources`, `data/yi.txt`) and the #1925 scripts, run the truth half of `run_all.sh` (`prep.py`, `confusions.py`, `metrics.py --kind truth --lm-sources gutenberg+wikisource --out-dir metrics-gw`, `damage.sh` with the `proxy-rows` binary above), then:

```sh
python3 scripts/gain_measures.py --metrics-dir metrics-gw --damage damage-truth.jsonl --out gain-measures.json --boot 1000
```

Run on 20 Nov 2026.
