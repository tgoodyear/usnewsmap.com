# OCR evaluation

How we measure OCR: the shared benchmark, the public ground truth we score against, and the corpus-wide comparisons that need no transcription.

- [Benchmark releases](bench.md): versioned archives of page images and reference text that any engine can be scored on, and how to fetch, score and add a release.
- [Benchmark annotation policy](bench-annotation-policy.md): the rules for admitting new reviewed transcriptions, illegibility masks and damage labels.
- [Evaluation measures v1](evaluation-measures-v1.md): the extended measures `scripts/ocr-bench/measures_v1.py` adds beside the benchmark scores (paired page scoring, full-page regions, search relevance, quality against cost).
- [Gold set](gold-set.md): our own crops, the public ground truth and its source policy, the CER scorer and the BOINC volunteer bundle.
- [Scoring guide](scoring-guide.md): scoring your own engine against the public ground truth, step by step.
- [Text agreement](text-agreement.md): LoC's OCR against American Stories' text for every page that has both.
- [LoC ALTO words](loc-alto-words.md): every word of LoC's OCR in Parquet, with its box and confidence.

Dated measurements behind these pages are in the [technical notes](../notes/README.md), among them the Gold calibrations of [16](../notes/2026-10-16-gold-bundle-80bf-measurements.md), [17](../notes/2026-10-17-gold-english-trust-calibration.md) and [18 October 2026](../notes/2026-10-18-german-fraktur-diagnostic.md).
