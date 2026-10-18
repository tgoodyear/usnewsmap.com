# Extended OCR evaluation measures v1

`python3 scripts/ocr-bench/measures_v1.py` implements `usnm-ocr-evaluation-measures/1`. It is an
additive evaluation module: it does not change the `usnm-ocr-survey bench-score` implementation,
its `legacy-ws-v1`/`raw-v1` outputs, any `ocr-bench` release, or sample membership. Its inputs are
generic reference/output pairs keyed by stable IDs, so a release adapter can prepare them without
binding this scorer to a particular OCR engine or LoC artifact.

## Run

Prepare UTF-8 JSONL reference rows (`id`, `reference`, `page_id`, `title_id`; optional
`language`, `script`, `damage`, `layout`, and verified page `regions`) and one output JSONL per
system (`id`, `text`, optional `status` and `regions`). Outputs not present in a reference set are
rejected; missing rows remain in the denominator and are treated as empty text. For example:

```sh
python3 scripts/ocr-bench/measures_v1.py \
  --references references.jsonl \
  --outputs legacy-loc=legacy.jsonl \
  --outputs refreshed-loc=refreshed.jsonl \
  --rule diplomatic-v1 --report comparison.json
```

Each reference row has a unique `id` and text. `page_id` and `title_id` identify paired
uncertainty clusters; if absent, the scorer falls back to the sample/page ID. Strata are scalar
strings or lists of strings, e.g. `"language":["de"]`, `"script":"Fraktur"`,
`"damage":["fading","skew"]`, and `"layout":"columns"`. For full-page evaluation, attach
`regions:[{"box":[x,y,width,height],"order":0,"text":"..."}]` from verified reference
geometry/order. Output regions use the same shape and identify the engine's detected blocks.
Coordinates are on the same page canvas. Do not use unverified or estimated geometry as truth.

Output status may be `ok`, `truncated`, `missing`, or `error`; absent output rows also mean
`missing`. `missing` and `error` contribute empty text to CER and remain explicitly counted;
`truncated` text is scored as emitted and separately counted. Empty successful strings have their
own count. Keep raw engine outputs, adapter logs, model/runtime hashes and retrieval/index settings
as immutable inputs alongside the report; the scorer never rewrites them.

## Measures and fixed policies

- Transcription rules are `diplomatic-v1` (exact Unicode code points, no normalization) and
  `legacy-ws-v1` (Python whitespace splitting, runs collapsed to one ASCII space, ends trimmed).
  Neither changes case, punctuation, diacritics, historical spelling, hyphenation or combining
  sequences. CER uses exact Levenshtein distance with linear-space alignment for page-length strings, is pooled by reference code points, and reports substitutions,
  insertions, deletions/omissions, empty outputs, missing outputs, errors and truncations. An
  empty-reference CER is `null` when the aggregate denominator is zero. No WER is claimed because
  tokenization is language/script dependent. Run both rules separately if both views are needed;
  preserve their separately versioned reports.
- Historical modernization and glyph confusions are **human-adjudicated labels**, not guessed by
  the scorer. The optional adjudication JSONL contains `id`, `decision:"human-adjudicated"`, at
  least two pseudonymous `qualified_readers` records (`id`, `qualification`), and `categories`
  chosen from `historical-modernization`, `historical-glyph`, `cross-region-contamination`, and
  `other`. The report gives counts/rates and error-review coverage. The tool validates the record
  shape, not a reader's credentials; keep the qualification basis and adjudication notes with the
  review artifact. Do not use the OCR system or another model as its own judge. Freeze the review
  IDs before comparing systems; `deterministic_review_sample(ids, count, seed)` makes a reproducible
  output-independent sample.
- Full-page results keep three measures distinct: `page_regions.transcription` is text edit
  accuracy over reference regions (unmatched reference regions count as omissions); `segmentation`
  reports region recall/coverage, precision and F1; `reconstruction` reports pairwise reading-order
  accuracy plus a cross-region contamination indicator. Geometry matching is deterministic
  maximum-cardinality one-to-one matching at IoU >= 0.5, with overlap and IDs as stable tie-breaks.
  The contamination indicator counts a geometrically matched region whose text is closer by
  normalized edit distance to another reference region than to its assigned region. It is a
  screening measure, not ground truth; inspect and human-adjudicate a fixed sample. Full-page text
  CER, region geometry and reading order must come from independent verified annotations.
- Per-system CER and missing/error counts are reported overall and by the available language,
  script, damage and layout labels. Paired candidate-minus-baseline CER uncertainty is a seeded
  percentile bootstrap (2,000 replicates by default) over page clusters and separately title
  clusters. The comparison also reports the rate of pages whose baseline CER is <= 2% and whose
  candidate page CER is worse; pass an explicitly preregistered threshold to the library API when
  another “already-good” cutoff is appropriate. Small cluster counts yield wide/coarse intervals;
  report denominators, not only point estimates.

## Fixed search-utility evaluation

Create a small, independently adjudicated query JSONL with unique `id`, `kind` (`name`, `place`,
`date`, etc.), `query`, and a `relevance` map from document/page ID to graded relevance (0 is
non-relevant; positive integers are graded relevant). Search-system JSONL documents have the same
`id` set across systems and `text` containing exactly the fields under comparison. Example:

```sh
python3 scripts/ocr-bench/measures_v1.py \
  --references references.jsonl --outputs loc=loc.jsonl --outputs candidate=candidate.jsonl \
  --queries adjudicated-queries.jsonl \
  --search-documents legacy-loc=legacy-index-text.jsonl \
  --search-documents refreshed-loc=refreshed-index-text.jsonl \
  --search-documents candidate=candidate-index-text.jsonl \
  --report comparison.json
```

`search-v1` pins one analyzer (`NFKC`, case-fold, Unicode `\\w+` tokenization; no stemming or
query expansion) and BM25 (`k1=1.2`, `b=0.75`), with document-ID tie-breaking. It reports
macro query recall@k, reciprocal rank and graded nDCG@k, overall and by query kind, plus per-query
results. The same relevance judgments, document IDs/fields, analyzer, ranking parameters and `k`
are used for every text source. A production search engine's tokenizer, filters, boosts, metadata,
and ranker may differ; evaluate it only through a separately pinned adapter and do not silently
compare differing index settings. Search relevance measures retrieval usefulness, not faithful
historical transcription. Retain the adjudication protocol and query set version with the report.

## Quality/cost decision

Measure cost (e.g. actual dollars, joules, or wall time per **usable page**) independently; never
assign published OCR zero cost or substitute GPU utilization for quality. Before a run, declare
non-inferiority margins for CER, missing/error rate and regression rate on already-good pages.
`quality_cost_decision(...)` applies those margins first and calls a candidate cost-better only if it
passes every quality gate and has lower measured cost per usable page. Compare like units, hardware
scope and page definitions, and also inspect strata, truncation and retrieval results before
selecting an engine. The margins are project policy, not universal defaults.

All result fields and rules above are versioned; any policy change that affects scores requires a
new measure version rather than silently altering an existing report.
