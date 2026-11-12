# Worst-first re-reads: the first measured gain (12 November 2026)

This is the first measurement for #1415: what our re-reads gained over LoC's text, cell by cell, before the worst-first campaign starts. The threshold is 0.02, the smallest mean gain worth acting on, approved on #1415.

## Method

`quality_gain.py` (#1846) ran against the `usnm_quality` replica (CT 236) through the `quality_reader` DSN, with reader `ours-tesseract`. It took 23,986 pages, the newest scored output of each re-read page.

- **Damage drop:** LoC's damage rate (`quality.page_summary`) minus our text's (`quality.ocr_outputs`), page by page. The 0.02 threshold is set on this measure.
- **CER drop:** LoC's estimated CER minus ours, where one `usnm_quality` unit scored both texts. The unit is chosen as `quality.page_signals` chooses it, leaving out excluded campaigns.
- **Cells:** reader × band × language × decade. The band comes from LoC's damage rate and at least 50 word occurrences: worst is over 0.25, moderate is over 0.10, and low is everything else.
- **Decision:** `stop` when the 95% interval lies under 0.02 (from 30 pages on), or when 374 pages average under it. `go` the other way round. `measuring` until one of those holds.

Every page measured was read by Tesseract 5.5.0 between 12 and 23 October. About 23,300 of the outputs are the German Fraktur pilot (`frak2021`) and about 770 are English.

## Results

| Band | Pages | Mean damage drop | go / measuring / stop cells |
|---|---:|---:|---|
| worst | 18,693 | 0.137 | 8 / 12 / 0 |
| moderate | 1,525 | -0.002 | 2 / 18 / 6 |
| low | 3,768 | 0.007 | 1 / 2 / 5 |

Worst band, cells with at least 200 pages:

| Language, decade | Pages | Damage drop (95% interval) | CER pairs | CER drop |
|---|---:|---|---:|---:|
| German 1850 | 229 | 0.107 (0.088–0.125) | 90 | 0.021 |
| German 1860 | 476 | 0.078 (0.065–0.090) | 133 | 0.236 |
| German 1870 | 1,498 | 0.147 (0.141–0.152) | 412 | 0.152 |
| German 1880 | 2,387 | 0.168 (0.164–0.171) | 398 | 0.026 |
| German 1890 | 1,789 | 0.147 (0.143–0.151) | 187 | 0.003 |
| German 1900 | 3,241 | 0.130 (0.126–0.134) | 1,167 | -0.054 |
| German 1910 | 8,665 | 0.135 (0.133–0.138) | 762 | 0.068 |
| English 1790 | 262 | 0.083 (0.073–0.094) | 0 | n/a |

Stopped cells: moderate German 1870, 1880, 1890, 1900 and 1910, and moderate English 1800. The worst-first moderate samples leave them out.

## What it shows

- **Worst band:** re-reading gains on damage rate, and by a wide margin: every cell with 200 or more pages is a `go`, at 0.08 to 0.17.
- **Moderate band:** Tesseract gains nothing on this measure (-0.002 over 1,525 pages). This is mostly German, so the English moderate sample in the campaign is what settles the band for English.
- **The two measures disagree on German.** Damage rate gains in every worst-band German cell. Estimated CER gains in 1860, 1870 and 1910, barely in 1850 and 1880, not at all in 1890, and gets worse in 1900. Damage rate is a word-list measure, so it can reward text that looks like dictionary German without being right.
- Until reader-comparison's damage-band rule from human references is ready, the German cells where CER doesn't gain are the provisional revoke list on #835.
- **English:** the English evidence is thin. Only the 1790s cell has enough pages to decide.

## Already in search

The serving version `pages-v20261026-1` used rule v1 to replace LoC's text on 21,290 of the 25,846 re-read pages it considered: 20,652 German, 636 English and 2 French. The German pages are live while #835 holds German back until it's calibrated. The maintainer's decision is to keep German text only in the cells where it beats LoC and to revoke the rest at the next full rebuild. That rebuild comes after about a week of worst-first re-reads have passed this check, and it also carries Cherokee's 190 line-job pages.

## Reproducing

```sh
/opt/usnm-quality-feed/venv/bin/python /opt/usnm-quality-feed/quality_gain.py \
  --out-dir /var/tmp/usnm-gain-$(date -u +%Y%m%d) --dsn-file /etc/usnm/quality-reader/dsn
```

Run it in the `usnm-batch` container (CT 215). This run's output is `/srv/usnm-lake/wf1415-20261112/gain2/` there (`cells.csv`, `input.json`, `pages.csv`, `stop.json`, `summary.json`), with input SHA-256 `fc9f1e3f…` in `summary.json`. The output files aren't in the tree. The campaign lists built with its first run's stop list are in `/srv/usnm-lake/wf1415-20261112/` in the same container (`/tank/usnm-lake/wf1415-20261112` on pve-big), one `<lang>/wf1415-<lang>/` directory per language, with that run's `gain/` beside them.
