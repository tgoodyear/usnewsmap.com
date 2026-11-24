# Plan (draft for freezing): does our Tesseract read English Chronicling America body text better than LoC's OCR? (#2122, route 2)

Route 1, the Beyond Words caption test (comment 7129161579), passed. A pass there tests captions only. Its plan said body-text evidence must come from a pick-between-readings queue on the check site. This is that queue. Human truth comes only from picks: nobody types.

## Question

On English body text, when our whole-page Tesseract and LoC's OCR read the same printed word differently, which one is right, and how often?

## Pages

- **Universe:**
  - every held English Chronicling America page: a confirmed scan, and damage audit v2 `lang == eng` with `decision == detected`, as #2107's page list has it;
  - its LoC bulk ALTO archive is present.
  - This gives 643,021 pages after the exclusions below.
- **Excluded:** every page in the #2107 and #2122 ledgers, 373 pages in the universe:
  - the en2107 300 pages and their 50 reserves;
  - en-asgold (7);
  - Overproof (18);
  - Beyond Words pages, matched by batch path, plus every page of an issue that has a Beyond Words page.
- **Ranking:** `sha256("2122:pick:"+doc_id)`.
- **Draws, in order:**
  - pilot: 20 pages;
  - sample: 200 pages;
  - reserves: 50 pages.
- **Per title:** at most 5 pages, counted over all three draws together.
- **Spread by decade:**
  - each decade gets 3 pages, or all it has;
  - the rest follows each decade's share of the universe.
  - Equal decade quotas would put about 40% of the sample before 1840, against 0.6% of the corpus.
- **Code and lists:** `scripts/ocr-survey/enpick/select_pages.py`, sha256 of the lists:
  - pilot `06504e38…`;
  - sample `623140f2…`;
  - reserves `057393eb…`.
- **The pilot's role:** its pages were used to set the build rules below. They are not in the sample.
- **Reserves:** used only to replace a sample page the build cannot read (no scan, not in locstats, Tesseract fails). They are used in rank order, and each use is reported.

## Readings and tasks

- **Readings:**
  - **L:** LoC's ALTO words. A line-end HYP stays on its word.
  - **T:** Tesseract 5.5.0 `eng`, PSM 3, `--dpi 300`, one thread, on the scan file. These are the worker's settings, as in #2122 arm W.
- **Pairing:** by box overlap.
  - The two words must be each other's best IoU match, with IoU ≥ 0.5.
  - No other word may cover 20% or more of either box. If one does, one engine split or joined the word.
  - A page is left out whole if fewer than 50% of LoC's words pair.
- **Normalisation:** as `score2122.py` at b045416f. It decides only whether two readings differ.
- **What volunteers see:** the raw readings.
- **Left out, and counted:**
  - words cut by a line-end hyphen (both halves);
  - words on merged lines (#1978 rule);
  - pairs whose difference is punctuation or case only (an agreement);
  - pairs with no letter or digit after normalisation;
  - pairs that normalise to more than one word.
- **Tasks:**
  - up to 6 per page, a simple random sample of the page's disagreements by `sha256(seed:id)`;
  - each task stores its page's candidate count, for weights;
  - display order a/b is set by a secret side key.
  - `private` (hidden until settled) holds: the side, page, word indexes, IoU, cover, normalised forms, Tesseract confidence and strata.
- **Language and code:**
  - language `en-read` (`languages/en-read.json`); `en` stays with #1867;
  - set `tess-loc-v1`;
  - code `edge/check/tools/tess_pick.py`, with input from `english_pages.py`.

## Gold items (volunteer quality)

- **Same queue, separate set** (`tess-loc-gold-v1`), so volunteers cannot tell gold tasks apart. Never seeded.
- **Sources:** pages with existing human text.
  - Overproof dataset 3: article text.
  - en-asgold: gold regions.
  - Beyond Words: verified captions, as `regions`.
- **Which words:** only disagreements where exactly one reading equals the normalised truth word, plus a few where neither does.
- **Balance:** about 45% T-right, 45% L-right, 10% neither.
  - Beyond Words captions favour LoC: volunteers corrected LoC's text, so LoC-right items are easy to find.
  - So they are capped at a third of the gold set.
- **Size:** 120 gold tasks, about 10% of the queue.

## Settle rule

The site's rule: at least 3 answers, and one choice holds two thirds of them; a word with 7 answers and no such choice closes as disputed.

## Estimand

- **Disagreements:** D is the set of paired words whose normalised readings differ.
- **Primary, p_T − p_L, the net win share:**
  - p_T is the share of D where T's reading is the settled answer;
  - p_L is the share of D where L's reading is the settled answer.
  - Neither, Can't tell, Not text and disputed outcomes count in the denominator as no win. They are also reported apart.
  - Weighting: within a page, tasks are weighted by candidates / sampled; pages are post-stratified to the universe's decade shares.
- **Word error rate difference:** ΔWER = WER_T − WER_L, over paired words.
  - It equals the disagreement rate d (D / paired words, per page, from `pages.jsonl`) times (p_L − p_T).
  - Agreeing pairs add nothing to ΔWER, because both readings are right or both are wrong in the same way.
  - So the agreements fix the denominator but cannot show either engine's absolute WER.
  - Unpaired words (splits, joins, words one engine dropped) and the left-out classes are reported as shares of LoC's words, as the part ΔWER does not cover.
- **Intervals:** cluster bootstrap, resampling titles and then pages within title; 1,000 draws, seed 2122.
- **Descriptive:**
  - by decade and by damage band;
  - verdict accuracy on gold, overall and by which reading was right;
  - the share of Neither and Can't tell.

## Decision rule (proposed thresholds)

English whole-page Tesseract counts as reading body text better than LoC only if all four hold:

1. Net win share p_T − p_L ≥ 0.10, with its 95% interval lower bound above 0.
2. ΔWER ≤ −0.005 (half a point fewer word errors), with its 95% interval upper bound below 0.
3. Gold verdict accuracy ≥ 0.85, and at least 0.80 on both T-right and L-right gold. If not, the queue's verdicts are not trusted and the result is void, not a fail.
4. Coverage: at least 150 of the 200 sample pages give at least one settled task. Neither plus Can't tell plus disputed is at most 35% of settled tasks.

What the result means:
- **A fail of rule 1 or 2 is reported as such.**
- **Inconclusive:** an interval spans 0 but the point estimate meets the threshold.
- **No per-decade rule.** Decades are described, not decided.

## What it can and cannot support

- **What a pass is:** the body-text evidence the per-language evidence rule asks for, alongside route 1's caption pass.
- **What it is not:**
  - It does not by itself replace LoC's text in search. The #1915 completeness gate still applies page by page; this test measures word accuracy where both engines read the same word, not completeness.
  - It says nothing about reading order or column fusing. #2122's first test covers reading order.
- **ΔWER covers paired words only.** If T drops or splits many words, a ΔWER gain can coexist with losses elsewhere; the unpaired shares are reported for that reason.
- **The universe:** held pages only, English by our detector, decade-weighted to our holdings, not to all of Chronicling America.

## Size and volunteer supply

- **Tasks:** about 200 pages × up to 6 = about 1,100 tasks, plus 120 gold.
- **Answers:** at about 3.3 answers per settled task, about 4,000 answers.
- **Supply is the constraint:** current check-site volume is tens of answers a week.
- **Check:**
  - after 300 settled tasks, report progress, but not the win share;
  - if under 600 tasks have settled 8 weeks after loading, the sample is cut to the first-ranked 100 pages, and that is reported.

## Pilot (20 pages, CT 107, 24 Nov 2026)

Run as `nobody`. Scans were read in place from `/data/ocr-scans-cold`. ALTO came from the bulk archives after the locstats check. Reports are in `sel/pilot-*` and 12 renders in `samples/`; `key.tsv` says which reading is which. The rules here were fixed after the pilot and will not change once the plan is frozen.

**Pages:**
- All 20 pages had a scan, a locstats row and an ALTO member.
- **6 pages:** Tesseract 5.5.0 died with SIGFPE (exit 136) on the scan. Their dates are 1759, 1804, 1826, 1832, 1858 and 1919. The fault reproduces with a single thread.
- **4 pages:** fewer than 50% of LoC's words paired, so these pages were left out whole. Their dates are 1877, 1941, 1948 and 1956. One of them, a 1848 page, shows Tesseract with 15,100 words against LoC's 8,261. The likely causes are a scale or rotation mismatch, or a page with mostly non-text.
- **10 pages gave tasks.**

**Words, on the 14 pages that Tesseract read (67,280 LoC words):**

| Outcome | Share of LoC words |
|---|---:|
| Paired | 61.5% |
| Agree exactly | 34.5% |
| Agree after normalisation | 2.8% |
| Disagree (candidates) | 12.7% (8,538) |
| Unpaired: split or joined | 9.0% |
| Unpaired: IoU < 0.5 | 2.6% |
| Unpaired: no Tesseract word | 1.4% |
| Left out: merged line | 2.1% |
| Left out: hyphen cut | 2.0% |
| Left out: several words after normalisation | 0.7% |
| Left out: punctuation only | 0.3% |

- 37.7% of the paired words that are not agreements are left out (`aligned_words_left_out_share` in the report).
- Disagreements per page, over the 10 pages that gave tasks:
  - minimum 5, median 462, maximum 3,050;
  - the 1864 and 1883 pages have over 2,000 each, which shows how damaged those pages are.
- 59 tasks were sampled, at most 6 per page.

**Samples:** 12 renders. Each shows the line crop with the word boxed, the word crop enlarged, and the raw readings as Reading 1 and Reading 2.
- They include clear OCR errors on one side (carefrl, propeilv, otficer, livesfock), long s (himfelf / hTmfetf), and capitals (But / Hut, toox / Cook).
- Sample 2 is LoC's `beating` against Tesseract's `ating`. Tesseract's box covers only part of the printed word, yet IoU ≥ 0.5. The task is still valid, since the print decides it.

**Decisions to take before freezing:**
1. **SIGFPE pages.** In the main run, a page where Tesseract fails is a Tesseract failure. It counts against T in a coverage measure and is reported. It is not replaced by a reserve, since replacing it would hide a real worker fault. Proposal: debug the SIGFPE separately; it may be the scans' JP2 resolution header combined with `--dpi 300`. That is an issue for the worker, not for this test.
2. **Page alignment failures (4 of 14).** Proposal: keep the 50% gate. Report the failed pages and their causes. A failed page counts as "no evidence", not as a loss for either engine.
3. **Splits and joins (9% of LoC words).** These are outside ΔWER and reported as a bound, as above.
