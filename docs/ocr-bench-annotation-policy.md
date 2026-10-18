# Benchmark transcription and damage policy v1

This is the admission policy for **new reviewed annotations**, not a claim that
legacy references have been re-transcribed. Existing public ground truth keeps
its source conventions and licence. Never normalize an old release in place.
A changed reference, mask, label or source is a new immutable release.

## Ground truth and diplomatic transcription (`diplomatic-v1`)

- Admit image/text pairs only with public provenance, redistribution rights for
  **both**, exact image registration and documented human transcription or
  verification. Engine consensus, published OCR and model-generated text alone
  are not ground truth. Keep source archive, metadata, licence and input hashes.
- Preserve visible spelling, capitalization, punctuation, accents, historical
  forms (including long s), script and printed hyphenation. Do not silently fold
  ligatures, modernize orthography, translate, spell-correct, case-fold, or apply
  NFC/NFKC. Keep UTF-8 source text unchanged; document source conventions that
  differ from this policy rather than claiming diplomatic compliance.
- For new transcriptions, separate printed lines by LF and regions by a blank
  line. Record the intended region/column sequence from the page, not OCR output.
  Vertical Japanese reads within each column top-to-bottom, columns right-to-left;
  Hebrew/Yiddish and Arabic follow logical Unicode text order, not visually
  reversed strings. Mixed-direction runs need a competent reader's review.
  Crops must not inherit text outside their exact image bounds.
- An identified contributor transcribes against the image; an independent
  competent-language reviewer resolves disagreements against it. Freeze evidence
  and a stable reviewer identifier in `transcription`, **before** examining engine
  outputs. Public GT may instead inherit its publisher's documented process;
  do not label it newly independently reviewed here.
- A new contiguous unreadable region may be represented by one U+FFFD replacement
  character, without asserting how many characters were lost. Never guess words.
  Existing public references stay unchanged. Masks refer to frozen reference
  positions, including an existing uncertain span if a reviewer confirms it.
  Uncertain reading order or unmatched images disqualify the unit from accuracy
  scoring; a mask cannot repair registration or order.
- Full-page GT must cover every included text region, including advertisements,
  headings and marginalia, with a reviewed reading-order receipt. A set of region
  transcriptions or page OCR is not automatically full-page GT. Explicitly record
  excluded non-text regions and missing text; never concatenate partial regions
  and advertise complete-page accuracy.

## Illegibility masks (`reference-wildcard-v1`)

Optional sample field `transcription` holds `policy`, nonempty `evidence`,
`reviewed_by`, and `illegible_spans`. Each span has integer `start`, `end` and a
nonempty `reason`. Offsets are **zero-based half-open Unicode code-point offsets
in the raw reference file**, not bytes, grapheme clusters, normalized text or
image coordinates. Spans must be nonempty, sorted, nonoverlapping and within the
reference. Keep the raw file and its hash unchanged. If no review happened, omit
`transcription`; an absent mask is not proof that every glyph was legible.

Only opt-in `bench-score --rule raw-masked-v1` applies these spans. Each span is
a wildcard matching **zero or more** hypothesis code points in minimum-edit
alignment; masked reference points leave the denominator. Unmasked characters
still incur ordinary insertion/deletion/substitution costs. Missing output is
empty text, with all visible reference points counted as errors. A fully masked
sample has zero reference characters and undefined CER (`null`), not a perfect
score. A sample's wildcard can absorb an arbitrary-length output run; this is an
optimistic, ambiguity-aware **lower bound**, not proof of correct recognition.
Report it beside `raw-v1`, the visible character denominator, mask counts and
missing outputs. Never compare masked and unmasked CER as the same metric.

`raw-v1` and `legacy-ws-v1` ignore masks and remain unchanged. The masked rule
performs no whitespace or Unicode normalization. There is deliberately no
whitespace-normalized mask rule: raw offsets must not drift after normalization.
New `raw-masked-v1` reports add `mask_policy` and per-row
`masked_reference_chars`; old score report shapes remain unchanged.

## Damage taxonomy (`reviewed` vs `unannotated`)

Unknown is not undamaged. The inherited object `{"status":"unannotated"}`
remains valid (including inherited narrative `observations`) and must never be included in clean/mild/moderate/severe strata.
Reviewed samples use `status: reviewed`, a unique list of `labels`, `severity`
and nonempty `evidence` (review receipt/source annotation, not an engine score).
Multiple labels describe co-occurring damage:

| Label | Visible phenomenon |
| --- | --- |
| `bleed-through` | Reverse-side print interferes with foreground strokes. |
| `faded-ink` | Weak or absent foreground ink; includes uneven impression. |
| `blur` | Defocus/motion/resampling prevents resolving strokes. |
| `stain` | Discoloration or spots obscure foreground text. |
| `tear` | Physical paper loss, holes or torn edges remove text. |
| `occlusion` | Bindings, folds or another object cover text. |
| `warping` | Curvature, folds or geometric deformation displace text. |
| `clipping` | Scan/crop boundary truncates printed text. |

Assess severity against the **text-bearing area of the sample** (not blank
margins), independently of OCR. `none` requires an empty label list and an
explicit clean review. All other severities require at least one label:

- `mild`: visible degradation, but a competent reader can recover all text.
- `moderate`: localized ambiguous/unreadable text, with most text recoverable.
- `severe`: extensive loss or distortion; substantial text cannot be recovered.

Keep borderline decisions and reviewer disagreements in the receipt; severity
is a human ordinal judgement, not a measured pixel percentage or CER threshold.
Do not infer damage from language, age, poor OCR or typeface. Fraktur/Antiqua,
layout difficulty and mixed-language status are separate labels. Masks and
damage are independent: blurred text may be fully readable; lost text need not
be blur. Freeze labels before engine selection. Synthetic corruptions, if added,
must be separate, with parent image hashes and deterministic transforms, never
reported as naturally damaged historical pages.

## Schema, subset and release requirements

[`annotation.schema.json`](../ocr/bench/annotation.schema.json) documents schema-2
sample fields. The Rust manifest reader/build/verify/scorer enforce span bounds,
review evidence, enumerations, uniqueness and severity consistency. Readers accept schemas 1 and 2. Recipes without imports/annotations continue
to emit schema 1 and its historical rule list, preserving legacy release bytes;
new imports/annotations emit schema 2 and advertise the opt-in masked rule.
Recipes may use `annotations: {"sample-id": {"damage": {...},
"transcription": {...}}}`; either annotation is optional. Unknown sample ids fail
the build. Imported published GT uses `imports` instead of invented Iris jobs:
each inventory-pinned `PATH/samples.json` lists `id`, relative `image` and
`reference`, `source_page_id`, and provenance with `for_scoring: true` and
`source_dataset` matching the recipe's source. `PATH/LICENSES/` must travel too.
`legacy: null` means no historical campaign is asserted.

Per-item `source` resolves to image/reference licences and attribution in the
manifest and `LICENSES/`. An aggregate release does not erase NC/ND conditions.
No ND derivatives or unverified rights enter new freely redistributable tracks.

Count unique sample ids, not overlapping subset memberships; disclose distinct
source pages and source issues. Never split a page across train/dev/test, or
call previously published data unseen. A multilingual pooled subset is **not** a
mixed-language sample. Count observed language × damage × unit cells, with
unannotated damage distinct and absent languages/tier targets explicit. Per-track
`standard-1000`/`extended-10000` and full-page `pages-10/100/1000` targets remain
unmet unless real eligible units reach those counts; do not duplicate, augment
or rename an unbalanced 1,000-unit pool to meet them.
