# OCR quality without human truth (13 Nov 2026)

We want to know, language by language, whether a reader (Qwen 3-VL 8B, Xiaomi-OCR-0, Tesseract) beats LoC's OCR where no human transcription exists: Serbian, Czech, Polish, Russian, Spanish, Yiddish and others. Typed transcription is ruled out. This report tests reference-free measures against true CER on the truth sets we have, then applies the best one to the unlabelled Qwen and LoC crops. The second part is proxy evidence, not measured accuracy.

## Summary

- **Best measure:** `lm_oov`, a character 5-gram model's bits per character plus the out-of-lexicon rate against a full lexicon (wordfreq plus Hunspell), each scaled by its truth-set standard deviation. Over all truth sets its Spearman rho with CER is 0.58 per crop and 0.60 per page. The production damage rate gets 0.41 and 0.33, and the top-5,000 unknown share 0.36 and 0.36.
- **Paired decisions:** given two readings of the same crop whose CER differs by at least 1 point, `lm_oov` picks the better one 85% of the time in English, 97% in French, 87% in Finnish, 92% in Swedish and 77% in German.
- **Per-language verdicts on truth:** in 31 of 34 language and reader-pair cases, the reader `lm_oov` prefers on most crops is the one that truly wins most crops. All three misses have 2–4 decisive crops. The measure exaggerates the winner's share (German, Qwen against Tesseract PSM 3: true 58%, proxy 78%).
- **VLM against LoC:** in en-op, LoC beats Qwen by 1 point or more on 35 of 324 crops. Every measure misses most of those (balanced accuracy 0.57–0.60). Qwen's losing readings are fluent and a median 14% longer than the reference: it adds text, probably from just outside the crop. A rate-based measure can show that LoC's text is bad. It can't show that a VLM's text is faithful.
- **Transfer fails for German and is marginal for Finnish.** No decision margin reaches 90% in German. A margin chosen on the other languages gives 81% in German, 88% in Finnish, and 93–99% in English, French and Swedish.
- **Confusion weighting doesn't help.** Counting only near-misses through common OCR confusions correlates worse with CER per crop than counting any near-miss, in every language.
- **Unlabelled crops (proxy):** `lm_oov` prefers Qwen on 93–97% of crops in Czech, Slovene, French, Norwegian, Danish and Serbian (both scripts), on 77% in Spanish and 73% in Polish, where Qwen returns no word on 10–12% of crops, and on 22% in Yiddish, where 37% of Qwen's readings are only the illegible marker `[?]`. No language meets the proposed evidence rule yet. The binding limit is breadth: the Qwen crops come from 1 Serbian title, 2 Slovene, 5 Czech and 9 French. Nothing can replace LoC on proxy evidence alone.
- **Wikisource:** as character-model training text it changes no verdict and doesn't fix German (paired accuracy 0.77 with every training text). Used alone it raises the character model's paired accuracy by 5–6 points in English, Finnish and Swedish. As truth it is not usable: none of the seven gap-language wikis has validated newspaper pages in 1880–1930.
- **Recommendation:** add `oov` (full lexicon), `charlm_bpc`, `lm_oov`, the omission guard's word count and a no-words flag to `usnm_quality`. Drop the confusion-weighted near-miss, garbage and script-mix ideas. XLM-R pseudo-perplexity matched `lm_oov` on the same sample (paired accuracy 0.88 against 0.87) at about 2.5 s per reading on two CPU threads, so it isn't worth a BOINC app.

## Literature

What was tested elsewhere, and what it suggests here. Full references are at the end.

- **Lexicality.** The share of tokens found in a dictionary tracks human ratings of OCR quality (Alex and Burns 2014) and true accuracy on historical print, once historical spellings are allowed for (Springmann, Fink and Schulz 2016). The evidence is mostly English and German, and every language needs its own lexicon.
- **Garbage strings.** Taghva et al.'s rules (very long tokens, runs of one character, punctuation inside a word, mixed case) catch non-text noise cheaply (Taghva, Borsack and Condit 1996; Taghva et al. 2001). They don't separate a plausible misreading from a correct word.
- **Language models.** A language model trained on text of the same genre and period ranks OCR transcriptions without a reference (Booth, Shoemaker and Gaizauskas 2022). Supervised classifiers on reference-free features reach useful F1 for German, French and Luxembourgish (Schneider and Maurer 2021). Absolute CER needs calibration; rankings within a corpus need none.
- **Post-OCR detection competitions.** ICDAR 2017 and 2019 measured error detection against ground truth in up to 10 languages, Czech, Polish, Spanish and French among them (Chiron et al. 2017; Rigaud et al. 2019). The 2019 winner fine-tuned multilingual BERT, with token-level F1 from 0.67 (French) to 0.95. A BERT-based detector later did slightly better (Nguyen et al. 2020).
- **Masked-LM scores.** Pseudo-log-likelihood scores a sentence with a masked language model (Salazar et al. 2020). On historical German and Latin handwriting, pseudo-perplexity ranked models about as well as lexicon hit rate and better than an n-gram model; it picked the best model 60–70% of the time on two test sets and 30% on a third, where CERs were close (Ströbel et al. 2022).
- **Confusion-weighted matching.** Noisy-channel error models (Brill and Moore 2000; Kolak and Resnik 2005) and corpus-wide character-confusion statistics (Reynaert 2011, TICCL) tell an OCR near-miss from a new word for correction. No source tests them as quality estimators.
- **Historical orthography.** Spelling variation makes a modern lexicon count real period words as errors (Piotrowski 2012; Bollmann 2019). A reader that modernises spelling is rewarded by any modern-lexicon or modern-LM proxy. That is an inference, and this study measures it.
- **Validation against CER.** Few no-reference measures have been checked against true CER across languages. Ströbel et al. (2022) is the closest. van Strien et al. (2020) measure what OCR error does to downstream NLP rather than estimating it.

What that left to test: lexicon hit rate with full lexica (not a top-5,000 list), a period character n-gram model, XLM-R pseudo-perplexity, garbage rules, script mixing, and confusion-weighted near-misses, against the existing damage rate and unknown-word share. One more limit follows from the definitions, not from any paper: every rate-based measure is blind to text a reader leaves out.

## Data

Truth sets (human references; the frozen copies on the data lake, `pve-big:/tank/usnm-lake`):

| Set | Language | Crops | Pages | Readers |
|---|---|---|---|---|
| en-op | English | 345 | 18 | LoC, Tesseract PSM 3 and 6, Xiaomi-OCR-0 v5, guarded Qwen 3-VL 8B |
| en-as | English | 507 | 10 | LoC, Tesseract PSM 3 and 6 |
| new-newseye-bnf | French | 60 | 20 | Tesseract PSM 3 and 6, Xiaomi, Qwen 8B |
| new-newseye-nlf-fi | Finnish | 60 | 20 | the same |
| new-newseye-nlf-sv | Swedish | 60 | 22 | the same |
| de-rz81, de-nzz | German | 682 | 248 | the same |

fr-52, fi-52 and sv-52 are subsets of the new-newseye sets (the same crops) and are left out. de-rz81 holds pairs of crops with the same reference; both are kept, as in #968. American Stories text exists for 95 en-op crops but is article text, mostly not the crop's; it is left out.

Unlabelled target: the crops of the four Qwen campaigns (`qwen-bundle-v3-20261111`, `qwen-guard-v3-20261102`, `qwen-guard-v4-20261102`, `qwen-nonenglish-v1`). The Qwen 8B text is `vlm_text` in each campaign's `crops.parquet`. LoC's crop text is not in the quality database or in `crops.parquet`: it is the `loc_text` of each crop in the campaign's staged `qwen.json`, LoC's ALTO words inside the crop box. Both were copied to the lake (`qe-resources/unlabelled/`). Looped and unread crops are left out: 124,226 crop pairs over 16 languages. Up to 6,000 crops per language were scored, as whole pages drawn at random (seed 1). Serbian is split by the script of the Qwen reading.

## Measures

Each is computed per reading and oriented so that higher means worse. Words are letter runs after joining line-end hyphenation, lower-cased and folded: long s to s; pre-1918 Russian letters to modern ones; Serbian Cyrillic transliterated to Latin, so one lexicon and one model serve both scripts; Yiddish points dropped and ligatures split.

- `oov`: the share of words in neither the language's full wordfreq 3.1.1 list nor its Hunspell dictionary. This is the existing unknown-word share with a full lexicon instead of the top 5,000. Yiddish has only the repository's 5,000-word list.
- `nearmiss_any`: the share of words out of lexicon but within one edit of a lexicon word (two edits for words of 7+ letters), found with a one-deletion index over the 150,000 commonest words.
- `nearmiss_conf30/50/80`: the same, counting only edits among the commonest OCR confusions that cover 30%, 50% or 80% of all edits. The confusions are measured by aligning readings to references on the other languages' truth sets of the same script, so each language is scored without its own truth.
- `novel`: out of lexicon and not a near-miss.
- `charlm_bpc`: cross-entropy in bits per character under a character 5-gram model (interpolated Witten-Bell). It is trained on up to 3 million characters of Project Gutenberg books in the language plus a frequency-weighted wordfreq sample, and never on a reader's output. Two variants replace or top up the books with Wikisource text ([below](#character-model-training-text-gutenberg-and-wikisource)).
- `garbage`: the share of whitespace tokens breaking a Taghva-style rule: longer than 25 characters, four identical characters in a row, more than two punctuation marks inside, more symbols than letters, case changes inside a word, digits between letters, no vowel, or six consonants in a row. The last two are skipped for Czech, Serbian, Slovene and Polish.
- `script_mix`: the share of words with letters from more than one script.
- `mlm_pppl`: XLM-R base pseudo-perplexity, as mean negative log-likelihood per subword token over the first 128 tokens. Tokens are masked in six interleaved groups rather than one at a time, to fit CPU time. It was run on a sample: 50 crops per truth language with all their readings, and 30 crop pairs per target language.
- `damage` and `unknown`: the production damage rate and top-5,000 unknown share (`usnm_core::damage::score_forced`, through PR #1889's `proxy-rows`).
- `lm_oov`: `charlm_bpc / 1.185 + oov / 0.117`, each scaled by its standard deviation over the pooled truth sets. No fitting. (In the leave-one-language-out checks the deviations come from the other languages.)
- `combo_lolo`: a least-squares linear fit of CER on `oov`, `nearmiss_conf50`, `garbage`, `charlm_bpc` and `unknown`, fitted with the scored language left out.

True CER uses whitespace runs collapsed and long s, the double hyphen and superscript e folded on both sides, as `gpu-study score --fold` does. The diplomatic CER is in the data too.

## Validation

- **Correlation:** Spearman's rho with CER per crop and per page (pages pool their crops), with 95% page-bootstrap limits.
- **Paired decisions:** for every two readers of the same crop or page, the measure picks the reading with the lower value, and the pick is checked against CER. Accuracy is reported over all pairs and over pairs whose CER gap is at least 1 point. Balanced accuracy is reported too: the mean of the accuracies on pairs the first reader truly wins and pairs it truly loses, because "always pick the VLM" scores high whenever the VLM usually wins.
- **Omission guard:** a rate cannot see missing text. With the guard, a reading with under 80% of the other's in-lexicon words of 3+ letters loses. A character-count guard was tried first, and a guard counting all in-lexicon words second. Both fail on LoC crops, which often carry neighbouring-column fragments: they make LoC longer, and their one-letter pieces ("a", "y") are in the lexicon.
- **Transfer:** leave one language out. The combined fit and the confusion sets never see the held-out language. A decision margin is chosen on the other languages as the smallest measure gap at which every one of them reaches 90% accuracy on 1-point pairs, with at least 5% of those pairs decided. It is then applied to the held-out language.

Every script is in `scripts/` (`run_all.sh` runs them in order). The numbers are in `data/` and summarised in `data/tables.md`.

## Resources

On the lake under `qe-resources/`, with source URL and SHA-256 per file in `SOURCES.tsv` (copied to `data/resources.tsv`):

| Resource | Languages | Licence |
|---|---|---|
| LibreOffice Hunspell dictionaries (commit `c0865a85`) | cs, pl, ru, sr (Cyrillic and Latin), es, fr, de, en, sv, sl, nb, da | cs: GPL; pl: GPL 2 / LGPL 2.1 / MPL 1.1 / Apache 2.0 / CC BY 4.0; ru: BSD-style (Lebedev); sr: LGPL 3 / MPL 2 / GPL; es: GPL 3 / LGPL 3 / MPL 1.1; fr: MPL 2.0; de: GPL 2 or 3; en: SCOWL (permissive); sv: LGPL 3; sl: LGPL / GPL; nb: CC BY 4.0 and CLARIN PUB+BY; da: GPL 2 / LGPL 2.1 / MPL 1.1 |
| wordfreq 3.1.1 wheel | all but Yiddish | code Apache-2.0, data CC BY-SA 4.0 |
| Repository Yiddish list (`ocr/ja-ocr/wordlists/yi.txt`) | yi | CC BY-SA 4.0 |
| Project Gutenberg books, from the `aleph.pglaf.org` mirror (`gutenberg/books.tsv`, 84 books) | cs, pl, ru, sr, sl, es, fr, de, fi, sv, en, da, no | public domain in the US; PG header stripped before use |
| Wikisource, main namespace of the 1 November 2026 `pages-articles` dumps (`dumps.wikimedia.org`), read once from the head of each dump until 3 million characters were kept (`wikisource/wikisource.tsv`: URL, dump date, the dump's published SHA-1, SHA-256 of the bytes read and of the text kept) | ru, sr, sl, yi, cs, pl, de, fr, es, sv, fi, da, no, en | transcriptions CC BY-SA 4.0; underlying texts public domain |
| XLM-RoBERTa base (`FacebookAI/xlm-roberta-base`) | multilingual | MIT |
| Qwen campaign `crops.parquet`, `pages.parquet` and `qwen.json` archives | target set | LoC page text (public domain) and our readings |

The GPL and MPL dictionaries are used as data through the Hunspell command-line tool and are not redistributed here. The Gutenberg languages short of text are Russian (5 books, 0.6 MB), Serbian (3 books, 0.8 MB) and Slovene (5 books, 1.2 MB). There is no Yiddish book; in the main results the Yiddish character model is trained on the 5,000-word list alone, and in the Wikisource variants on yi.wikisource text. No historical lexicon was added. Pre-1918 Russian is handled by folding the old letters, and German historical spelling is left to Hunspell and the Gutenberg model.

## Results on the truth sets

### Correlation with CER

Spearman's rho, per crop / per page, all readers pooled (LoC is in English only).

| Measure | English | French | Finnish | Swedish | German | All |
|---|---|---|---|---|---|---|
| `lm_oov` | 0.54 / 0.50 | 0.61 / 0.74 | 0.79 / 0.81 | 0.78 / 0.85 | 0.60 / 0.61 | 0.58 / 0.60 |
| `charlm_bpc` | 0.48 / 0.44 | 0.59 / 0.73 | 0.74 / 0.65 | 0.65 / 0.74 | 0.47 / 0.46 | 0.50 / 0.51 |
| `oov` | 0.48 / 0.51 | 0.48 / 0.63 | 0.82 / 0.84 | 0.85 / 0.88 | 0.52 / 0.56 | 0.50 / 0.53 |
| `unknown` (top 5,000) | 0.34 / 0.39 | 0.29 / 0.48 | 0.57 / 0.68 | 0.52 / 0.70 | 0.36 / 0.37 | 0.36 / 0.36 |
| `damage` | 0.48 / 0.58 | 0.37 / 0.51 | 0.69 / 0.87 | 0.61 / 0.70 | 0.25 / 0.20 | 0.41 / 0.33 |
| `nearmiss_any` | 0.43 / 0.49 | 0.43 / 0.64 | 0.69 / 0.74 | 0.81 / 0.87 | 0.44 / 0.47 | 0.46 / 0.48 |
| `nearmiss_conf50` | 0.33 / 0.44 | 0.33 / 0.45 | 0.60 / 0.77 | 0.78 / 0.86 | 0.31 / 0.36 | 0.37 / 0.41 |
| `garbage` | 0.30 / 0.38 | 0.21 / 0.28 | 0.27 / 0.48 | 0.35 / 0.53 | 0.12 / 0.12 | 0.22 / 0.18 |
| `script_mix` | 0.00 | 0.00 | −0.07 | 0.00 | 0.00 | 0.00 |
| `combo_lolo` | 0.52 / 0.50 | 0.62 / 0.74 | 0.77 / 0.81 | 0.77 / 0.85 | 0.50 / 0.52 | 0.52 / 0.56 |
| `mlm_pppl` (sample) | 0.53 / 0.62 | 0.73 / 0.74 | 0.86 / 0.93 | 0.88 / 0.89 | 0.42 / 0.34 | 0.61 / 0.55 |

The 95% intervals are in `data/tables.md`. For `lm_oov` per crop they run from 0.45–0.63 (English) to 0.71–0.86 (Finnish). The fitted combination is no better than the unfitted `lm_oov`. Most of its weight is on `garbage`, which is weak alone (`combo_weights` in `data/validation.json`).

### Paired decisions

Accuracy on pairs of readings of the same crop (or page) whose CER differs by at least 1 point, with the omission guard:

| Measure | English | French | Finnish | Swedish | German | All |
|---|---|---|---|---|---|---|
| crop pairs | 2,910 | 214 | 238 | 224 | 2,433 | 6,019 |
| `lm_oov`, crops | 0.85 | 0.97 | 0.87 | 0.92 | 0.77 | 0.83 |
| `lm_oov`, pages | 0.93 | 0.97 | 0.81 | 0.96 | 0.76 | 0.80 |
| `charlm_bpc`, crops | 0.84 | 0.98 | 0.86 | 0.92 | 0.74 | 0.81 |
| `oov`, crops | 0.77 | 0.81 | 0.86 | 0.93 | 0.74 | 0.77 |
| `unknown`, crops | 0.83 | 0.84 | 0.87 | 0.91 | 0.71 | 0.79 |
| `damage`, crops | 0.72 | 0.73 | 0.85 | 0.87 | 0.63 | 0.70 |
| `nearmiss_any`, crops | 0.74 | 0.76 | 0.83 | 0.87 | 0.69 | 0.73 |
| `nearmiss_conf30`, crops | 0.61 | 0.60 | 0.81 | 0.88 | 0.61 | 0.63 |
| `garbage`, crops | 0.60 | 0.60 | 0.78 | 0.78 | 0.56 | 0.59 |
| `mlm_pppl`, crops (sample) | 0.92 | 0.98 | 0.95 | 0.94 | 0.71 | 0.88 |

The masked-LM row covers only its sample (160, 129, 129, 157, 204 and 779 pairs). On exactly those pairs `lm_oov` scores 0.95, 0.98, 0.83, 0.89, 0.75 and 0.87 (`data/mlm-same-sample.json`). XLM-R is better on the Finnish and Swedish Fraktur sets, worse on English and German, and level overall. Its en-op sample holds one crop where LoC beats Qwen, and both measures miss it.

The guard changes little on the truth sets, where readers rarely leave text out.

### Per reader pair

The decisive pairs are VLM against LoC (en-op only) and VLM against Tesseract. Accuracy / balanced accuracy on crops with a gap of 1 point or more:

| Language | Pair | Pairs | First reader truly better | `lm_oov` | `charlm_bpc` | `unknown` |
|---|---|---|---|---|---|---|
| English | LoC vs Qwen 8B | 324 | 0.11 | 0.89 / 0.57 | 0.88 / 0.60 | 0.89 / 0.57 |
| English | LoC vs Xiaomi | 321 | 0.03 | 0.95 / 0.58 | 0.93 / 0.57 | 0.95 / 0.58 |
| English | LoC vs Tesseract PSM 6 | 594 | 0.25 | 0.85 / 0.80 | 0.84 / 0.79 | 0.80 / 0.76 |
| English | Qwen vs Xiaomi | 73 | 0.18 | 0.62 / 0.59 | 0.59 / 0.54 | 0.49 / 0.54 |
| German | Qwen vs Tesseract PSM 6 | 441 | 0.61 | 0.73 / 0.68 | 0.71 / 0.65 | 0.68 / 0.64 |
| German | Qwen vs Xiaomi | 427 | 0.79 | 0.83 / 0.66 | 0.79 / 0.65 | 0.74 / 0.60 |
| Finnish | Qwen vs Tesseract PSM 6 | 52 | 0.98 | 0.96 / 0.98 | 0.96 / 0.98 | 0.97 / 0.74 |
| Swedish | Qwen vs Xiaomi | 18 | 0.78 | 0.78 / 0.59 | 0.67 / 0.43 | 0.69 / 0.49 |

Against Tesseract, whose errors are visible garbage, the measures are balanced (0.80 for LoC against Tesseract in English). Against a VLM they find the VLM's wins and miss its losses. On the 35 en-op crops where Qwen loses to LoC, Qwen's reading has a median 1.14 times the reference's characters, mostly with no out-of-lexicon word; it adds fluent text, probably from just outside the crop. An exploratory rule lifts balanced accuracy for Qwen against LoC from 0.57 to 0.80: prefer LoC when its crop is clean (out-of-lexicon rate at most 0.05) and Qwen's reading is over 10% longer (`data/overread-enop.json`). It was found and scored on the same crops, and barely helps Xiaomi (0.54 to 0.58).

### Transfer to a language without truth

A decision margin chosen on the other languages, then applied to the held-out one (`lm_oov`, 1-point crop pairs). With German in the training pool no margin meets the bar, so German is left out of the pools:

| Held out | Margin | Accuracy | Share decided |
|---|---|---|---|
| English | 0.274 | 0.93 | 0.73 |
| French | 0.237 | 0.99 | 0.84 |
| Finnish | 0.125 | 0.88 | 0.96 |
| Swedish | 0.250 | 0.98 | 0.92 |
| German | 0.237 | 0.81 | 0.79 |

Finnish and German fall short of 90%. A margin can't certify a single crop in a language without truth.

The per-language verdict uses the aggregate over crops, which holds up better than single-crop decisions. For each language and reader pair: the share of 1-point crops on which the first reader truly wins, and the share on which `lm_oov` prefers it (`data/aggregate-check.json`). Selected rows:

| Language | Pair | Decisive crops | True | Proxy |
|---|---|---|---|---|
| English | LoC vs Qwen 8B | 324 | 0.11 | 0.04 |
| English | LoC vs Xiaomi | 321 | 0.03 | 0.02 |
| English | LoC vs Tesseract PSM 3 | 575 | 0.23 | 0.22 |
| English | Qwen vs Xiaomi | 73 | 0.18 | 0.40 |
| German | Qwen vs Tesseract PSM 3 | 409 | 0.58 | 0.78 |
| German | Qwen vs Xiaomi | 426 | 0.79 | 0.88 |
| German | Tesseract PSM 6 vs Xiaomi | 511 | 0.57 | 0.58 |
| Finnish | Tesseract PSM 6 vs Xiaomi | 49 | 0.02 | 0.27 |
| Swedish | Qwen vs Xiaomi | 18 | 0.78 | 0.89 |
| French | Qwen vs Xiaomi | 3 | 0.33 | 0.67 |

The proxy majority is on the truth's side in 31 of 34 cases, and in every case with at least 18 decisive crops. The three misses (French Qwen against Xiaomi, and Tesseract PSM 3 against PSM 6 in Finnish and Swedish) have 2–4 decisive crops.

### Character-model training text: Gutenberg and Wikisource

The main results use Gutenberg books. Two variants were rerun end to end: Gutenberg topped up to 3 million characters from Wikisource, and Wikisource alone (3 million characters per language from the main namespace of the 1 November 2026 dumps; Norwegian 2.2 million). `charlm_bpc` and `lm_oov` per crop, Spearman's rho / paired accuracy with the guard (`data/lm-sources.json`):

| Language | `lm_oov`, Gutenberg | `lm_oov`, + Wikisource | `lm_oov`, Wikisource | `charlm_bpc`, Gutenberg | `charlm_bpc`, + Wikisource | `charlm_bpc`, Wikisource |
|---|---|---|---|---|---|---|
| English | 0.54 / 0.85 | 0.53 / 0.85 | 0.53 / 0.89 | 0.48 / 0.84 | 0.48 / 0.84 | 0.48 / 0.90 |
| French | 0.61 / 0.97 | 0.61 / 0.97 | 0.59 / 0.96 | 0.59 / 0.98 | 0.59 / 0.98 | 0.58 / 0.97 |
| Finnish | 0.79 / 0.87 | 0.81 / 0.88 | 0.81 / 0.88 | 0.74 / 0.86 | 0.76 / 0.92 | 0.75 / 0.92 |
| Swedish | 0.78 / 0.92 | 0.78 / 0.92 | 0.82 / 0.97 | 0.65 / 0.92 | 0.65 / 0.92 | 0.74 / 0.97 |
| German | 0.60 / 0.77 | 0.60 / 0.77 | 0.59 / 0.77 | 0.47 / 0.74 | 0.49 / 0.74 | 0.50 / 0.75 |
| All | 0.58 / 0.83 | 0.58 / 0.83 | 0.58 / 0.85 | 0.50 / 0.81 | 0.50 / 0.81 | 0.52 / 0.85 |

The top-up changes almost nothing where Gutenberg already had 2–3 million characters, which is every truth language. Wikisource alone helps the character model in English, Finnish and Swedish (paired accuracy up 5–6 points) and leaves French and German where they were. German stays at 0.77 with every training text, so the German failure isn't the training text. Held out, German still gets 80–81% and Finnish 88–90%. The aggregate check agrees in 31, 31 and 32 of 34 cases.

On the unlabelled crops no verdict moves by more than 0.01 for `lm_oov`. The gap languages change only in the size of the character-model difference: Serbian, Slovene and Russian by under 0.3 bits per character, Polish from −2.1 to −3.0 (Qwen lower). In Yiddish, Qwen's readings score worse than LoC's by 2.2 bits per character with the word-list model and by 1.2 with Wikisource text, and `lm_oov` still prefers Qwen on 22–23% of crops. Wikisource is worth keeping as training text, mainly because it gives Yiddish a real character model; it does not change any conclusion here.

### Confusion weighting

The commonest edits differ by reader. LoC in English reads e as o (1,736 times in 289k reference characters). Tesseract and Xiaomi on Finnish and Swedish Fraktur read s as f and k as f. Qwen writes v for Finnish w. The counts are in `data/confusions-top.json`. Restricting near-misses to the commonest confusions (30%, 50% or 80% of edit mass, from the other languages) lowered the crop-level correlation in every language. The confusions don't carry over between readers and languages well enough to sharpen the count.

## Qwen against LoC on unlabelled crops (proxy evidence)

`lm_oov` with the omission guard, per language. Shares are of crops (or pages) on which Qwen's reading is preferred, with 95% page-bootstrap intervals. "No words" is the share of crops where Qwen returned no word at all. "Known words" is the median ratio of Qwen's to LoC's in-lexicon words of 3+ letters. Full table: `data/tables.md`.

| Language | Titles | Pages | Crops | Qwen no words | Known words, Qwen/LoC | Qwen preferred, crops | Qwen preferred, pages | Without guard |
|---|---|---|---|---|---|---|---|---|
| Czech | 5 | 660 | 6,003 | 0.01 | 1.97 | 0.95 (0.94–0.95) | 0.98 (0.97–0.99) | 0.97 |
| Slovene | 2 | 571 | 6,008 | 0.00 | 3.71 | 0.97 (0.97–0.98) | 0.99 (0.99–1.00) | 0.98 |
| French | 9 | 684 | 6,003 | 0.02 | 1.88 | 0.93 (0.92–0.94) | 0.99 (0.98–0.99) | 0.98 |
| Serbian, Cyrillic | 1 | 613 | 5,038 | 0.00 | 1.67 | 0.97 (0.96–0.97) | 1.00 (0.99–1.00) | 0.97 |
| Serbian, Latin | 1 | 361 | 962 | 0.03 | 3.67 | 0.96 (0.94–0.97) | 0.95 (0.92–0.97) | 0.97 |
| Norwegian | 3 | 218 | 2,413 | 0.00 | 1.91 | 0.93 (0.91–0.94) | 0.98 (0.96–1.00) | 0.99 |
| Danish | 3 | 121 | 1,225 | 0.00 | 2.35 | 0.97 (0.96–0.98) | 0.99 (0.98–1.00) | 0.99 |
| Spanish | 18 | 598 | 6,010 | 0.12 | 1.36 | 0.77 (0.74–0.79) | 0.82 (0.79–0.85) | 0.97 |
| Polish | 10 | 354 | 3,803 | 0.10 | 1.09 | 0.73 (0.70–0.75) | 0.76 (0.72–0.81) | 0.92 |
| German | 19 | 36 | 319 | 0.05 | 1.28 | 0.88 (0.81–0.93) | 0.99 (0.96–1.00) | 0.97 |
| Russian | 2 | 11 | 126 | 0.03 | 1.23 | 0.85 (0.75–0.94) | 0.91 (0.73–1.00) | 0.89 |
| Yiddish | 6 | 338 | 3,007 | 0.38 | 0.00 | 0.22 (0.19–0.24) | 0.24 (0.19–0.28) | 0.54 |

Per language:

- **Czech, Slovene, French, Norwegian, Danish:** Qwen is preferred on 93–97% of crops, and LoC's crops have about half as many real words or fewer. These five meet the strength and omission conditions of the rule below, but none has 10 titles. Norwegian and Danish also fall short on pages. Whether their titles are in Antiqua and near-modern spelling (rule item 2) was not checked.
- **Serbian:** the same strength in both scripts, but from one title. Cyrillic is scored through transliteration, which has no truth behind it.
- **Spanish and Polish:** without the guard Qwen is preferred on 92–97% of crops. With it, 73–77%, because Qwen returns no word on 10–12% of crops where LoC has some. Those crops need a look: they may be blank or illegible regions, or Qwen may be skipping text.
- **Yiddish:** no evidence for Qwen. 38% of Qwen's readings have no word: 1,120 of 3,007 (37%) are only `[?]`. Another 557 are mostly Latin script. Where both have words, the measure is split (0.54 without the guard, interval of the mean difference spanning zero). The lexicon is 5,000 words and the character model has no running text, so this verdict is weak either way.
- **German and Russian:** too few pages. German also failed transfer on truth.

The exploratory over-read rule flags 0–4% of crops in every language but Russian (11%), and changes no verdict.

XLM-R pseudo-perplexity on a small sample (30 crop pairs per language, of which 7–30 fall inside the scored sample) points the same way, without the guard: Qwen is preferred on 1.00 of crops in Czech (8), French (10), Slovene (30) and Serbian Cyrillic (7), 0.94 in Spanish (16), 0.80 in Polish (30), 0.77 in Russian (30) and 0.73 in Yiddish (30). In Russian and Yiddish the interval of the mean difference reaches zero. Masked-LM scores favour fluent text, and Qwen's Yiddish readings are often short Latin-script phrases, so the Yiddish figure says little.

## Wikisource as truth

Wikisource's ProofreadPage keeps a page text beside each page scan. Validated (green, quality 4) pages have been proofread and then checked by a second contributor, so validated newspaper pages could serve as truth sets. We checked sr, cs, pl, sl, yi, es and ru with listing queries to the MediaWiki API (Index namespaces, newspaper categories, `proofreadpagesinindex` quality counts). Nothing was downloaded.

| Wiki | Newspaper scans (Index pages) in 1880–1930 | Validated newspaper pages |
|---|---|---|
| sr | none (44 Index pages in all, none a newspaper) | 0 |
| cs | 1 stub (Litovelské noviny 1891, no page created) | 0 |
| pl | no daily papers; weeklies *Życie* 1897–98 and *Świat* 1906 | 0 from newspapers; 318 from *Życie*, a Kraków literary weekly (314 of 1898, 4 of 1897) |
| sl | none (38 Index pages, all books and almanacs) | 0 |
| yi | none (4 Index pages in all) | 0 |
| es | *El Noticioso* (La Paz, 1899), 11 issues, about 44 pages | 0 (proofread at most once) |
| ru | about 20 newspaper issues 1892–1916 (*Архангельск*, *Русский Инвалид*, diocesan gazettes) | 0 in the 5 indexes checked (all at quality 1 or not created) |

Scans are PDF or DjVu files on Wikimedia Commons, mostly public domain; the transcriptions are CC BY-SA 4.0. None of the titles with scans is a US paper. sl.wikisource's categories name US Slovene papers (*Proletarec* and *Glas svobode*, both in Chronicling America; *Amerikanski Slovenec*, *Glas naroda*, *Prosveta* unconfirmed), but they hold mainspace article extracts with no scans behind them.

Wikisource is not a usable source of validated newspaper truth in these languages. The only validated periodical block is *Życie* (1898): a magazine-style weekly, not a multi-column daily, and not a Chronicling America title. ru.wikisource's category "Индекс — Периодика" lists about 609 magazine indexes (*Нива*, *Вестник Европы*). Their validated pages were not counted; they are the one place left to look.

## Evidence rule for #1922

Proposed wording for the per-language gate:

> **Proxy evidence.** In a language without human truth, proxy evidence can establish that LoC's text is worse than a candidate reader's. It cannot establish that the candidate's text is accurate. It counts when all of these hold:
>
> 1. **Measure.** The measure is `lm_oov` as validated in `docs/reports/2026-11-ocr-quality-estimation/`, with the omission guard (in-lexicon words of 3+ letters). It is computed from a lexicon and a character model that were not built from either reader's output.
> 2. **Script and orthography.** The language is printed in Latin-script Antiqua in a spelling close to the modern one. That is the condition under which the truth sets transferred. Fraktur, historical orthography, Cyrillic and Hebrew script don't qualify until a truth set of that kind exists. Transliterated Serbian Cyrillic doesn't qualify.
> 3. **Size.** At least 300 pages and 3,000 crop pairs of the language, from at least 10 titles.
> 4. **Strength.** The candidate is preferred on at least 75% of crop pairs, and the lower 95% page-bootstrap bound is at least 65%. On the truth sets, every language and reader pair with at least 18 decisive crops had its proxy majority on the same side as the truth.
> 5. **No omission.** The candidate returns no word on at most 5% of crops, and the median ratio of its in-lexicon words to LoC's is at least 0.9.
> 6. **Faithfulness checked by people.** The #1896 blind pick task, on at least 50 pages of the language, prefers the candidate. The proxy cannot see fluent errors: on en-op it missed most of the crops where Qwen lost to LoC.
>
> Meeting 1–5 can open the human pick task for a language. Replacing LoC requires all of 1–6.

## Recommendation for usnm_quality (Phase 2)

Add, per reader text, crop and page:

1. **`oov`**: the out-of-lexicon rate against the full wordfreq list plus Hunspell, with the folding above. It replaces the top-5,000 unknown share as the lexical measure: it tracks CER better in every truth language. The Hunspell check can be shipped as expanded word lists (unmunched at build time), so the app needs no Hunspell.
2. **`charlm_bpc`**: a character 5-gram model per language, trained on public-domain period text, never on any reader's output. Wikisource text is at least as good as Gutenberg's here and covers Yiddish, so use it where Gutenberg is short. A pruned model per language is a few megabytes. With Wikisource, every language here has 2–3 million characters.
3. **`lm_oov`**: the two above, scaled by the pooled truth-set standard deviations (1.185 bits per character and 0.117), summed. This is the decision measure.
4. **The omission guard**: each reader's count of in-lexicon words of 3+ letters, so that a pair decision can make the reading with under 80% of the other's count lose.
5. **`no_words`**: whether a reading has no word at all. Yiddish Qwen readings that are only the illegible marker `[?]` are the case that matters.

Don't add confusion-weighted near-misses (worse than plain near-misses in every language), the garbage rules (rho 0.12–0.35 at crop level), or script mixing (zero on every truth set; keep it as a flag for Cyrillic and Hebrew pages only). Keep the damage rate for the existing worst-first campaign, but it is the weakest measure here and is not valid for German. XLM-R pseudo-perplexity ranks about as well as `lm_oov` (0.88 against 0.87 on the same pairs) for about 2.5 s of CPU per reading and a 1.1 GB model. That is not worth a BOINC app. It could break ties on a small sample where `lm_oov` is close.

## Limits

- **One LoC truth set.** The only test of the decision that matters, a VLM against LoC, is en-op: 345 crops from 18 pages of one collection, a set the readers may have seen in training (Qwen's en-op readings are close to the reference). en-as has LoC but no VLM reading.
- **Pairs are mostly engine against engine.** French, Finnish, Swedish and German compare Qwen, Xiaomi and Tesseract. Their accuracies say how well the measures rank readers. They don't show how the measures treat LoC's error profile.
- **No truth in Cyrillic or Hebrew script.** Serbian Cyrillic is scored after transliteration with Latin-script resources, and Russian and Yiddish not at all. The Yiddish lexicon is 5,000 modern words. Wikisource gives Yiddish, Russian, Serbian and Slovene 3 million characters of text, but much of it is modern (Soviet documents lead the Russian dump; Yiddish Wikisource is mostly modern spelling).
- **German is the transfer failure.** No margin gives 90% in German, and the margin chosen elsewhere gives 81% there. Any language printed in Fraktur or in historical spelling is in the same position until it has truth.
- **Modernisation.** A reader that modernises spelling scores better on every modern-lexicon and modern-text measure. The Finnish `w→v` and German long-s edits in the confusion counts are this. Folded CER hides part of it. Diplomatic CER would count it as error.
- **Crop sample, not pages.** Unlabelled verdicts cover the quality campaigns' sampled crops, up to 6,000 per language, not whole pages.
- **The over-read rule is in-sample.** It was found and scored on the same 35 en-op crops.

## References

1. Alex, B. and Burns, J. (2014). Estimating and rating the quality of optically character recognised text. DATeCH 2014. https://www.research.ed.ac.uk/en/publications/estimating-and-rating-the-quality-of-optically-character-recognis/
2. Springmann, U., Fink, F. and Schulz, K. U. (2016). Automatic quality evaluation and (semi-)automatic improvement of OCR models for historical printings. arXiv:1606.05157. https://arxiv.org/abs/1606.05157
3. Taghva, K., Borsack, J. and Condit, A. (1996). Evaluation of model-based retrieval effectiveness with OCR text. ACM TOIS 14(1), 64–93. https://doi.org/10.1145/214174.214180
4. Taghva, K., Nartker, T., Condit, A. and Borsack, J. (2001). Automatic removal of "garbage strings" in OCR text: an implementation. WMSCI 2001. (Rules as implemented in rmgarbage: https://rdrr.io/github/benmarwick/rmgarbage)
5. Booth, C., Shoemaker, R. and Gaizauskas, R. (2022). A language modelling approach to quality assessment of OCR'ed historical text. LREC 2022, 5859–5864. https://aclanthology.org/2022.lrec-1.630
6. Schneider, P. and Maurer, Y. (2021). Rerunning OCR: a machine learning approach to quality assessment and enhancement prediction. arXiv:2110.01661. https://arxiv.org/abs/2110.01661
7. Chiron, G., Doucet, A., Coustaty, M. and Moreux, J.-P. (2017). ICDAR2017 competition on post-OCR text correction. ICDAR 2017, 1423–1428. https://hal.archives-ouvertes.fr/hal-03025499
8. Rigaud, C., Doucet, A., Coustaty, M. and Moreux, J.-P. (2019). ICDAR 2019 competition on post-OCR text correction. ICDAR 2019. https://hal.archives-ouvertes.fr/hal-02304334
9. Nguyen, T. T. H., Jatowt, A., Nguyen, N.-V., Coustaty, M. and Doucet, A. (2020). Neural machine translation with BERT for post-OCR error detection and correction. JCDL 2020. https://doi.org/10.1145/3383583.3398605
10. Salazar, J., Liang, D., Nguyen, T. Q. and Kirchhoff, K. (2020). Masked language model scoring. ACL 2020, 2699–2712. https://aclanthology.org/2020.acl-main.240
11. Ströbel, P. B., Volk, M., Clematide, S., Schwitter, R., Hodel, T. and Schoch, D. (2022). Evaluation of HTR models without ground truth material. LREC 2022, 4395–4404. https://aclanthology.org/2022.lrec-1.467
12. Brill, E. and Moore, R. C. (2000). An improved error model for noisy channel spelling correction. ACL 2000, 286–293. https://aclanthology.org/P00-1037/
13. Kolak, O. and Resnik, P. (2005). OCR post-processing for low density languages. HLT-EMNLP 2005. https://aclanthology.org/H05-1109/
14. Reynaert, M. W. C. (2011). Character confusion versus focus word-based correction of spelling and OCR variants in corpora. IJDAR 14, 173–187. https://link.springer.com/article/10.1007/s10032-010-0133-5
15. Piotrowski, M. (2012). Natural Language Processing for Historical Texts. Morgan & Claypool. https://doi.org/10.2200/S00436ED1V01Y201207HLT017
16. Bollmann, M. (2019). A large-scale comparison of historical text normalization systems. NAACL 2019, 3885–3898. https://aclanthology.org/N19-1389/
17. van Strien, D., Beelen, K., Coll Ardanuy, M., Hosseini, K., McGillivray, B. and Colavizza, G. (2020). Assessing the impact of OCR quality on downstream NLP tasks. ICAART 2020, 484–496. https://doi.org/10.5220/0009169004840496
18. Speer, R. (2022). rspeer/wordfreq: v3.0. Zenodo. https://doi.org/10.5281/zenodo.7199437
