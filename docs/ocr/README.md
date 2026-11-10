# OCR evaluation

How we measure OCR: the shared benchmark, the public ground truth we score against, and the corpus-wide comparisons that need no transcription.

- [Benchmark releases](bench.md): versioned archives of page images and reference text that any engine can be scored on.
- [Benchmark annotation policy](bench-annotation-policy.md): the rules for admitting new reviewed transcriptions and damage labels.
- [Evaluation measures v1](evaluation-measures-v1.md): the extended measures `scripts/ocr-bench/measures_v1.py` adds beside the standard scores.
- [Gold set](gold-set.md): the public ground-truth sources, the source policy and the per-language decisions.
- [Scoring guide](scoring-guide.md): scoring your own engine against the gold set, step by step.
- [Text agreement](text-agreement.md): LoC's OCR against American Stories' text for every page that has both.
- [LoC ALTO words](loc-alto-words.md): every word of LoC's OCR in Parquet, with its box and confidence.
