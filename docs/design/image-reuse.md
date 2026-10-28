# Image reuse across newspapers

**Status:** Design for review, not a rollout authorization · **Date:** 26 October 2026 · **Issue:** [#1220](https://github.com/tgoodyear/usnewsmap.com/issues/1220), Phase 3 of [#1102](https://github.com/tgoodyear/usnewsmap.com/issues/1102)

The result should distinguish reuse of the same illustration from pictures of
the same subject. A cluster's map and timeline show observed appearances in the
digitized corpus, not the direction of copying or its first historical printing.
Phase 1 caption search and Phase 2 layout evaluation remain prerequisites, not
work this design claims to have delivered.

## Models and reproducibility

The initial embedding candidate is
[`google/siglip2-base-patch16-224`](https://huggingface.co/google/siglip2-base-patch16-224).
Its publisher's model card declares Apache-2.0 and describes the vision encoder
and WebLI pretraining. This is a candidate for evaluation, not evidence of
newspaper near-duplicate accuracy or proof that no evaluation image appeared in
pretraining.

Before executing an evaluation, retain the model repository's immutable revision,
weight and processor hashes, licence text and notices, dependency versions, crop
manifest hash, preprocessing settings and embedding dimension. Normalize image
embeddings to unit length and use cosine similarity. Never resolve mutable
`main` weights during a campaign. The model licence does not grant rights to
source newspaper images: retain the source's rights statement and attribution
with every page and crop; exclude sources without an acceptable reuse basis.

The two Beyond Words detector candidates named in #1220 are not approved
baselines. The
[conditional DETR card](https://huggingface.co/davanstrien/conditional-detr-resnet-50_fine_tuned_beyond_words)
declares Apache-2.0, but says "More information needed" for training/evaluation
data. A dataset name and reported validation loss do not establish unseen-page
evaluation. Obtain immutable training/validation page IDs and annotation versions
from the training recipe or author, or exclude that checkpoint from claims of
leakage-free performance and train a detector with recorded splits instead.

## Leakage-free measurement

Start from Phase 2's versioned human-labelled gold manifest, not the candidate
model's predictions. Keep all regions of one page, all pages of an issue, and
known reprint/near-duplicate families in one split. Group and split before model
fitting, threshold selection or augmentation; freeze training, development and
held-out test manifests and their hashes. Audit intersections by source page ID
and exact/perceptual image hashes. Independently label ambiguous reprint families
before placing them; never move a hard test pair into development.

Evaluate detection and reuse separately. Report detector precision/recall and
AP at stated IoU thresholds, by region class and newspaper/scan quality. Evaluate
reuse first on gold crops, then end-to-end on detector crops, so missed boxes
cannot disappear from the denominator. Include hard negatives: different
portraits of one person, similar maps, advertisements, ornamental borders and
common column layouts. Include degraded positives with cropping, scale,
halftone, rotation and print damage, without splitting synthetic variants across
evaluation sets.

Unknown pretraining overlap is disclosed separately from verified fine-tuning
split disjointness. Do not call the entire evaluation uncontaminated unless both
claims can be supported.

## Retrieval, verification and thresholds

Retrieve the top 50 nearest neighbours per crop within the evaluation corpus.
Record candidate-recall before applying any threshold; a threshold cannot recover
a missed neighbour. Add a second-stage local-feature/geometric verification
check, with minimum matched area and inlier requirements tuned only on the
development split. Semantic embedding similarity alone must not assert reuse.

There is deliberately no production cosine cutoff selected from an unmeasured
model. On development data, sweep thresholds and geometric-verification settings
and choose the highest-recall combination meeting the following evidence gate.
On each of development and held-out test, require at least 300 independent
evaluation families and sample one asserted pair per family using a preregistered
uniform-within-family sampler. Pairs sharing either a source page or a known
reprint family count as one evidence unit, not multiple independent observations.
The exact one-sided 95% Clopper-Pearson lower bound on this **family-balanced pair
precision** must be at least 99%. With 300 correct independent observations and
zero errors the lower bound is approximately 99.006%, not 100%; fewer independent
observations cannot pass the minimum-evidence gate.

Freeze the selected thresholds, sampling scheme and analysis code before
measuring the held-out test set, which must independently pass the same gate
before publishing asserted reuse clusters. A failed test gate requires a new
campaign with a fresh holdout, not retuning on that test set. Also report
ordinary pair-weighted precision/recall, family counts, confidence intervals,
class/quality strata and inspected false positives; the family-balanced gate is
not a claim of 99% pair-weighted precision. Family-block bootstrap intervals may
describe these secondary metrics, but an empirical bootstrap with zero observed
errors is never sufficient acceptance evidence. If independent evidence is
insufficient, keep the result as exploratory retrieval.

Connected components of pair matches can chain unrelated pictures together.
Require a new member to pass verification against the cluster's representative,
retain the match evidence and representative revision, and audit a sample of
whole clusters for contamination. Preserve uncertain matches as candidates.
Keep crop IDs stable from source-qualified page ID, page checksum, detector
revision and rectangle; retain page dates/places and links to the source.

## Verified source budget and rollout boundary

The budget for this design and its proposed first offline evaluation is
**zero new LoC, GHN or other source HTTP requests; zero BOINC work dispatches**.
Use only already-retained, rights-checked scans/crops. A missing local input is
an explicit exclusion in the coverage report, never a network fallback. Run any
future offline evaluation with source egress disabled and verify zero source
requests against its request counters. No source quota or existing 429 hold is
assumed to have been relaxed.

Before any source-backed extension, attach a separate reviewed budget receipt:
exact required pages, archive/cache hits, remaining bytes/requests by endpoint,
current shared limiter and hold state, observed retry/error counts, operator
window and hard stop/expiry. Respect existing global pacing and Retry-After;
do not add independent clients to multiply a per-host allowance. If a source
budget cannot be verified, its allowance remains zero.

Closing #1220 accepts a reviewed design only. Actual evaluation and any later
GPU rollout require their own issues, measured acceptance, model/data artefacts,
resource budget and explicit authorization. This document creates no queue,
fetch job, model download or volunteer workload.
