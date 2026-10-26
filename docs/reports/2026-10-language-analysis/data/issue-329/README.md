# Issue #329: reproducible Slavic flag screening (15 October 2026)

**This is agent-prepared evidence, not the completed hand audit.** Access to
the OCR data worked. The evidence does not establish the true-error precision
requested in #329: most flags still need a fluent reader to compare them with
the scans. Do not close #329 or calibrate production re-OCR thresholds from
these numbers.

## Sample and denominators

The sampling frame is the original 7 October 10% audit, not today's index. For
Czech, Serbian and Russian, take detected pages with `damage_rate > 0.25`; for
the Croatian control, take all detected scored Croatian pages. The eligible
populations are 2,084, 2,272, 478 and 290 pages, in that order. Sort each
population by the hexadecimal SHA-256 of UTF-8 `issue-329-v1:` plus `doc_id`,
ascending, and take 20. This is the fixed seed; no substitutions were made
after seeing text. The control was deliberately not matched to the damage tail.
Mixed and undetermined pages are excluded. These samples cannot estimate
corpus-wide language precision or isolate script, date and title effects.

`sample.jsonl` records all 80 original CSV rows, eligibility counts, selection
hashes, curated part paths, OCR-text SHA-256, the exact head and near counts,
and scan links. There are 20 pages per language, but only 5 Czech titles, 3
Croatian titles, 1 Serbian title and 2 Russian titles. Czech dates span
1874–1925, Croatian 1906–1958, Serbian 1911–1953 and Russian 1926–1938.
Nineteen Russian pages are from `sn82015743`. The sampling units are pages, not
independent newspaper titles.

## What was counted

Every flagged **occurrence**, not just each distinct word, is preserved in
`flags.jsonl.gz` (7,631 rows). A flag is the detected-language damage metric's
near-miss, **not** its separate garbage rule. Each row has the original
case-folded word, the Serbian Latin transliteration where relevant, the
scorer's suggested common word, five neighboring word tokens on each side,
and a label, confidence and reason. `word_index` is zero-based within the
page's dominant-script word stream after the scorer's dehyphenation and
normalization. Contexts follow OCR order, not a reconstructed reading order;
punctuation and line breaks are not retained in these short concordances.
Full OCR was pulled into a local cache, not committed; the source part and
text hash let an authorized reader retrieve and check it.

The scorer is `ocr/ja-ocr/damage_v2.py`, with wordfreq 3.1.1. Its hash is in
`provenance.json`. On **all 80 pages**, the detected language, decision and
rounded damage rate reproduce the original audit. Flag occurrences also
reconcile to `detect().damage.near`. The Cyrillic path excludes the other
modeled catalog languages in that script, not English, so using `forced()`
instead would give a different denominator. The candidate replacement is an
algorithmic suggestion, **not an adjudicated correction**.

## Annotation and confidence

Copilot reviewed grouped word concordances and the occurrence contexts of the
explicit valid-word decisions. This was **not an exhaustive occurrence-by-
occurrence comparison with scans**. `labels.json` records 190 type-level
likely-error decisions and 311 occurrence-level valid-word decisions:

- `likely_ocr_error`, **medium confidence**: malformed forms in the concordance,
  such as Serbian `јс` → `је`, `сс` → `се`, `којн` → `који`, Czech `ae` near
  `se`, and Russian `нс` near `не`. These are provisional candidates, not
  scan-confirmed true errors. A type-level label is reused across its
  occurrences; a human must check for initials and exceptional contexts.
- `valid`, **high or medium confidence** per occurrence: plausible intact text
  that the metric falsely flags. Examples are Czech `ul.` (street), `taj.`
  (secretary), `pí.` (Mrs), and English address `Ave.`; Serbian `онт.`, `инд.`
  and `кан.` in addresses; Croatian `Karlo`, inflected `se kosi`, dialectal
  `kak`, archaic `nu` and `pako`; and Russian `дол.` (dollars), `Дж.` (initial
  or address abbreviation), `Жак` (name) and `сего` (archaic or genitive form).
  The same type is **not** assumed valid everywhere: for example, Russian `дол`
  in `дол ларов` and Croatian `bije` in `Sr bije` remain unresolved.
- `unresolved`, **low confidence**: everything not explicitly adjudicated.
  Many are apparent line-break fragments (`manžel ka`, `mo že`, `вре мя`),
  but printed hyphenation, lost hyphens, OCR-inserted spaces and parser and
  layout artifacts cannot be separated reliably from these concordances alone.
  A fragment resembling an inflection is not evidence of inflection bias.
  Unresolved flags are never silently counted as true errors or false flags.

A 25% whole-page derivative of one selected Czech, one Serbian and one Russian
scan was retrieved and viewed for orientation. This is not a transcription or
verification of all their flags. The requests for the Croatian derivative and
its ALTO both timed out at 30 seconds; the item metadata was available. The
Czech arbitrary-size derivative also timed out before the cached 25% derivative
succeeded. `scan-checks.json` records the exact representative pages and URLs,
and the hashes of the images that were retrieved. Item calls were spaced at
least 3.5 seconds and tile calls at least 1 second, matching
`damage_parity.py`; no tight retries or parallel LoC requests were used. All
page text came from the curated snapshot, with no live Quickwit search requests
and no writes to production.

## Results (provisional, occurrence-weighted)

| Language | Pages | Flagged | Likely OCR errors | Valid / false flags | Unresolved | Precision among classified | Conditional all-flag range |
|---|---:|---:|---:|---:|---:|---:|---:|
| Czech | 20 | 2,350 | 479 | 133 | 1,738 | 78.3% | 20.4–94.3% |
| Serbian | 20 | 2,863 | 1,332 | 69 | 1,462 | 95.1% | 46.5–97.6% |
| Russian | 20 | 1,475 | 86 | 81 | 1,308 | 51.5% | 5.8–94.5% |
| Croatian control | 20 | 943 | 64 | 28 | 851 | 69.6% | 6.8–97.0% |

“Precision among classified” is `likely / (likely + valid)`, a descriptive
ratio of the deliberately easy-to-classify subset, **not an unbiased estimate
of flag precision**. The conditional range is `likely / flagged` through
`(likely + unresolved) / flagged`, assuming the provisional likely and valid
labels are correct. These are not confidence intervals; if those labels are
wrong the true value can lie outside the range. Actual scan-verified true
error counts and precision are **not established for any language**.
`summary.json` preserves the unrounded arithmetic.

## Conclusion and human handoff

There are both recognizable OCR-like substitutions and real false flags; “all
high scores are inflection” is not supported. False flags include names,
archaic and dialectal forms and abbreviations, not just inflections. Serbian's
repeated Cyrillic substitutions warrant a **pilot**, not a claim that its
corpus-wide score is calibrated. Czech, Russian and the Croatian control
contain many apparent word-boundary fragments. Using the same word list does
not by itself control for differences in title, age, layout, script or
tokenization.

For #296, keep a measured pilot that checks errors against scans or
transcriptions; do not rank languages by these subset precisions, subtract the
false-flag fraction from the published damage rates, or use them as automatic
replacement gates. The all-flag uncertainty is too large to change language
targeting.

**Human check required for all four languages**, especially **Serbian and
Russian**, where Copilot's contextual reading confidence is low. Confidence in
the Czech and Croatian lexical labels is moderate; they are not a native
reader's adjudication. Readers should adjudicate every unresolved occurrence
and verify the likely-error types and valid-word examples against scans.
Distinguish character errors, segmentation and hyphenation artifacts, valid
inflections, names, abbreviations, dialectal or archaic spelling and dictionary
gaps. Count ambiguity separately. This work leaves #329 open; it supplies the
reproducible data pull and conservative screening, not the issue's missing
human ground truth.

## Reproduction (local writes only)

Use a scratch directory **inside your worktree**, e.g. `.audit-329`, not a
system temporary directory. Keep it uncommitted and remove it when finished.
With an existing Azure login authorized to read the snapshot:

```sh
mkdir -p .audit-329
az storage blob download --account-name stusnmdffapzy --auth-mode login \
  --container-name curated \
  --name audit/ocr-quality-v2-pages-v20261003-1-10pct-pages.csv.gz \
  --file .audit-329/pages.csv.gz --no-progress --only-show-errors -o none
for name in titles batches; do
  az storage blob download --account-name stusnmdffapzy --auth-mode login \
    --container-name reference --name "pages-v20261003-1/$name.json" \
    --file ".audit-329/$name.json" --no-progress --only-show-errors -o none
done
python3 docs/reports/2026-10-language-analysis/audit329.py select --source-dir .audit-329
# fetch/score additionally need wordfreq==3.1.1, pyarrow,
# azure-storage-blob and azure-identity in your existing audit environment.
python3 docs/reports/2026-10-language-analysis/audit329.py fetch --source-dir .audit-329
python3 docs/reports/2026-10-language-analysis/audit329.py score --source-dir .audit-329
python3 docs/reports/2026-10-language-analysis/audit329.py check
```

`select` checks all three snapshot hashes and prints the chosen IDs. `fetch`
checks the selection against the committed manifest and reads only `doc_id`
columns and the relevant text row groups, caching the whole text of the 80
selected pages in `texts.jsonl`. It can resume. `score` regenerates the
manifest, compressed flags and summary from those texts and the committed
decisions, and fails if a score or count disagrees with the historical one.
`check` uses only Python's standard library and validates the committed page
and flag counts, unique occurrence IDs, label application, label coverage,
allowed label and confidence values, the historical damage-rate arithmetic, and
the summary reconciliation. It does not validate linguistic correctness.
Reproducing the explicit decisions is not an independent annotation. For fresh
annotations, edit `labels.json`, regenerate, and record the reader and scan
evidence before claiming ground truth.
