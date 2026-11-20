# OCR experiments: protocol and registry

Every OCR experiment that could change what reaches search follows this template, and gets one row in the registry below before it runs. The point is that results compare across studies, and that scaling a winner up is routine rather than a new design each time.

## The template

Copy this block into the experiment's issue (or `protocol.md` in its report directory) and fill it in **before** any run.

```markdown
### Protocol: <short name> (#<issue>)

- **Question:** one sentence. What decision does the answer feed?
- **Population:** the pages the decision applies to (e.g. "English pages in the worst damage decile, 1870-1899"),
  and how many exist.
- **Arms:** each reading compared, pinned: engine, version, model sha256, settings, resource-file sha256.
  The baseline is the text search serves today (usually LoC's).
- **Truth tier:** gold (human, held out, test only) / silver (agreed or check-site settled) /
  reference-free (qe-v1 `lm_oov`, damage). One tier decides; others are supporting.
- **Sample:** size, how drawn (stratified by language, decade, title, damage decile), seed.
  Unit of analysis is the page; lines and crops on one page are not independent.
- **Measure:** primary (e.g. folded CER, page-weighted), secondary (word F1, Cherokee letter error,
  reading-order error), cost (CPU or GPU seconds per page).
- **Decision rule:** written now, e.g. "adopt if n >= 300 pages and the upper 95% page-bootstrap limit
  of the CER change is below -1 pp". Also the stop rule for a live run (e.g. halt on any line changed
  that the job didn't read).
- **Exclusions:** lists every analysis must anti-join (e.g. the Cherokee reference-mismatch TSV,
  pages excluded on #1919), and why.
- **Leakage check:** how test pages are kept out of training and out of threshold tuning.
- **Placement:** left to Compute; science states only the gates (gold trust, spot checks, canary size).
```

## Rules that apply to every experiment

1. **Truth tiers are never mixed.** A result names its tier. Gold is test-only: never training, never threshold tuning. The expert Phoenix transcriptions are gold, private, and only their error rates are published.
2. **Page-grouped everything.** Splits, bootstraps and paired comparisons group by page (and by title where titles repeat).
3. **Sample from the decision's population.** A worst-first decision is tested on worst-first pages, not on all pages.
4. **Pilot, canary, scale.** Production runs go through BOINC on gold-trusted hosts, with engine and resource hashes pinned and checked.
5. **Shadow before serving.** New text is stored beside the old (`text_loc`) and reaches search only after its gate passes. A revocation is a hold.
6. **Report paired differences and cost.** Per-page paired differences with intervals, and CER gained per compute hour.
7. **Negative results get a row too,** so later sessions don't repeat them.

## Registry

One row per experiment, newest first. Status: planned, running, decided, abandoned. "Decision" is what changed (or didn't) because of it.

| Date           | Experiment                                                                         | Issue / PR                                                | Tier                   | Status  | Result                                                                                                                   | Decision                                                                                |
| -------------- | ---------------------------------------------------------------------------------- | --------------------------------------------------------- | ---------------------- | ------- | ------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------- |
| 20 Nov 2026    | Gain measures against CER: does qe-v1 `lm_oov` track CER drops better than damage? | [report](../reports/2026-11-gain-measures/report.md)      | gold (#968 sets)       | decided | Pooled page-pair rho: `lm_oov` 0.65, damage 0.48. German 0.56 vs 0.28; fi damage better (0.94 vs 0.85); en, fr, sv level | Proposed: `lm_oov` as second gain measure; German stays held; truth volume is the limit |
| 15 Nov 2026    | Cherokee letter similarity, round 1 (pixels+HOG, DINOv2)                           | #2006 (draft)                                             | silver                 | decided | Fails its decision rule in 3 of 4 cells                                                                                  | Round 2 with 1860 NT scans (#2007), deferred                                            |
| 15 Nov 2026    | Xiaomi-OCR-0 q8 on BOINC (x08)                                                     | #1922, #1998                                              | gold (#968 sets)       | decided | Matches the full model; about 30 GPU-min per page                                                                        | No corpus-wide Xiaomi; pilot only                                                       |
| 13 Nov 2026    | Reference-free OCR quality (qe-v1)                                                 | #1925 (draft), #1927                                      | gold (#968 sets)       | decided | `lm_oov` rho 0.58 per crop, 0.60 per page; paired accuracy 0.77-0.97; fails transfer to German                           | qe-v1 added to `usnm_quality`; damage stays for worst-first                             |
| 13 Nov 2026    | Does the damage-rate gain track real CER?                                          | #1889 (draft)                                             | gold (#968 sets)       | decided | Valid for en, fr, sv, fi (page rho 0.60-0.72); not German (0.06)                                                         | Keep 0.02 stop rule; German held                                                        |
| 12 Nov 2026    | Per-language reader comparison                                                     | #968, #1853 (draft)                                       | gold                   | running | Interim: Xiaomi best en/fr, Qwen 8B best fi/sv                                                                           | Routing in #968 comment; final after BOINC replicate                                    |
| 11-12 Nov 2026 | `chr_phoenix` production re-read of 191 LoC pages                                  | #1621, #1622                                              | silver + damage        | decided | 190 accepted, 94.7% of lines took the model's reading                                                                    | Waits for the rebuild to reach search                                                   |
| 9 Nov 2026     | `chr_phoenix` fine-tune                                                            | #1621, #1673                                              | silver + 70 gold lines | decided | 10.2% silver CER vs LoC 18.0%; gold lines too few to settle                                                              | Passed step 3; deploy via re-read                                                       |
| Nov 2026       | `yidj2` Yiddish fine-tune                                                          | #1479, #1428                                              | gold (book corpus)     | decided | Halves stock `yid` error on books                                                                                        | Opt-in only; untested on newspapers                                                     |
| Nov 2026       | GPU studies (Studies 1-11)                                                         | [report](../reports/2026-11-gpu-studies/report.md)        | gold                   | decided | See report                                                                                                               | Backfill one row per study                                                              |
| Oct 2026       | OCR engine survey, round 1 (80 pages)                                              | [report](../reports/2026-10-ocr-engine-survey/report.md)  | gold                   | decided | See report                                                                                                               | Backfill                                                                                |
| Oct 2026       | JP2 against IIIF JPEG                                                              | [report](../reports/2026-10-jp2-vs-jpeg-ocr/report.md)    | gold                   | decided | See report                                                                                                               | Backfill                                                                                |
| Oct 2026       | Corpus damage ranking                                                              | [report](../reports/2026-10-ocr-damage-ranking/report.md) | reference-free         | decided | See report                                                                                                               | Worst-first campaign (#1415)                                                            |
| Oct 2026       | Surya 2 on BOINC volunteers                                                        | [report](../reports/2026-10-surya-volunteer/report.md)    | gold                   | decided | See report                                                                                                               | Backfill                                                                                |
