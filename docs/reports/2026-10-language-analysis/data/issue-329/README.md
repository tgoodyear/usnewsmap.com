# Issue #329: reproducible Slavic flag screening (15 October 2026)

**This is agent-prepared evidence, not the completed hand audit.** OCR data access
worked. It does not establish the true-error precision requested in #329: most
flags still need a fluent reader to compare them with the scans. Do not close
#329 or calibrate production re-OCR thresholds from these numbers.

## Sample and denominators

The sampling frame is the original 7 October 10% audit, not today's index.
For Czech, Serbian and Russian, take detected pages with `damage_rate > 0.25`;
for the Croatian control, take all detected scored Croatian pages. The eligible
populations are respectively 2,084, 2,272, 478 and 290 pages. Sort each population
by the hexadecimal SHA-256 of UTF-8 `issue-329-v1:` plus `doc_id`, ascending, and
take 20. This is the fixed seed; no substitutions were made after seeing text.
The control is intentionally not damage-tail matched. Mixed/undetermined pages
are excluded. These samples cannot estimate corpus-wide language precision or
isolate script, date and title effects.

`sample.jsonl` records all 80 original CSV rows, eligibility counts, selection
hashes, curated part paths, OCR-text SHA-256, exact head/near counts, and scan
links. There are 20 pages per language, but only 5 Czech titles, 3 Croatian
titles, 1 Serbian title and 2 Russian titles. Czech dates span 1874–1925,
Croatian 1906–1958, Serbian 1911–1953 and Russian 1926–1938. Nineteen Russian
pages are from `sn82015743`. Pages, not independent newspaper titles, are the
sampling units.

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

The scorer is `ja-ocr/damage_v2.py`, with wordfreq 3.1.1. Its hash is in
`provenance.json`. On **all 80 pages**, the detected language, decision and
rounded damage rate reproduce the original audit. Flag occurrences also
reconcile to `detect().damage.near`. In particular, the Cyrillic path excludes
other modeled catalog languages in that script, not English; blindly using
`forced()` would reproduce a different denominator. The candidate replacement
is an algorithmic suggestion, **not an adjudicated correction**.

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
  `kak`, archaic `nu`/`pako`; and Russian `дол.` (dollars), `Дж.` (initial or
  address abbreviation), `Жак` (name) and `сего` (archaic/genitive form).
  The same type is **not** assumed valid everywhere: e.g. Russian `дол` in
  `дол ларов` and Croatian `bije` in `Sr bije` remain unresolved.
- `unresolved`, **low confidence**: everything not explicitly adjudicated.
  Many are apparent line-break fragments (`manžel ka`, `mo že`, `вре мя`),
  but printed hyphenation, lost hyphens, OCR-inserted spaces and parser/layout
  artifacts cannot be separated reliably from these concordances alone.
  A fragment resembling an inflection is not evidence of inflection bias.
  Unresolved flags are never silently counted as true errors or false flags.

Whole-page 25% derivatives of one selected Czech, Serbian and Russian scan
were retrieved and viewed for orientation. This is not a transcription or
verification of all their flags. The Croatian derivative and ALTO requests
both timed out at 30 seconds; its item metadata was available. The Czech
arbitrary-size derivative also timed out before the cached 25% derivative
succeeded. `scan-checks.json` records the exact representative pages/URLs and
successful image hashes. Item calls were spaced at least 3.5 seconds and tile
calls at least 1 second, matching `damage_parity.py`; no tight retries or
parallel LoC requests were used. All page text came from the curated snapshot,
without live Quickwit/search requests or writes to production.

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
`(likely + unresolved) / flagged`, assuming the provisional likely/valid
labels are correct. These are not confidence intervals; if those labels are
wrong the true value can lie outside the range. Actual scan-verified true
error counts and precision are **not established for any language**.
`summary.json` preserves the unrounded arithmetic.

## Conclusion and human handoff

There are both recognizable OCR-like substitutions and real false flags;
“all high scores are inflection” is not supported. False flags include names,
archaic/dialectal forms and, importantly, abbreviations—not just inflections.
Serbian's repeated Cyrillic substitutions warrant a **pilot**, not a claim
that its corpus-wide score is calibrated. Czech/Russian and the Croatian
control contain many apparent word-boundary fragments. The same word list
alone does not control title, age, layout, script or tokenization differences.

For #296, retain a measured pilot with scan/transcription error checks; do not
rank languages by these subset precisions, subtract the false-flag fraction
from the published damage rates, or use them as automatic replacement gates.
The all-flag uncertainty is too large to change language targeting.

**Human check required for all four languages**, especially **Serbian and
Russian**, where Copilot's contextual reading confidence is low. Czech and
Croatian lexical confidence is moderate, not native-reader adjudication.
Readers should adjudicate every unresolved occurrence and verify the likely-
error types and valid-word examples against scans. Distinguish character
errors, segmentation/hyphenation artifacts, valid inflections, names,
abbreviations, dialect/archaic spelling and dictionary gaps. Count ambiguity
separately. This work leaves #329 open; it supplies the reproducible data pull
and conservative screening, not the issue's missing human ground truth.

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
checks selection against the committed manifest and reads only `doc_id` columns
and relevant text row groups, caching whole text for the 80 selected pages in
`texts.jsonl`. It can resume. `score` regenerates the manifest, compressed flags
and summary from those texts and the committed decisions, failing on historical
score/count disagreement. `check` uses only Python's standard library and
validates committed page/flag counts, unique occurrence IDs, label application,
label coverage, allowed label/confidence values, historical damage-rate arithmetic,
and summary reconciliation.
It does not validate linguistic correctness. Reproducing the explicit decisions
is not an independent annotation. For fresh annotations, edit `labels.json`,
regenerate, and record the reader and scan evidence before claiming ground truth.
