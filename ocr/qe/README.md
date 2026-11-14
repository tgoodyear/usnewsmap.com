# Reference-free OCR quality measure, version 1 (`qe-v1`)

The measure the quality-estimation study recommends ([report](../../docs/reports/2026-11-ocr-quality-estimation/report.md), PR #1925), as `usnm_core::qe` (feature `qe`). Per reading:

| Column          | What                                                                                                                         |
| --------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `qe_oov`        | share of words in neither the language's full wordfreq list nor its Hunspell dictionary                                      |
| `qe_charlm_bpc` | bits per character under a character 5-gram model, interpolated Witten-Bell                                                  |
| `qe_lm_oov`     | `charlm_bpc / 1.185 + oov / 0.117`, the pooled truth-set standard deviations; the study's decision measure                   |
| `qe_known3`     | in-lexicon words of 3 or more letters: the omission guard's count (a reading under 80% of the other's loses a pair decision) |
| `qe_no_words`   | the reading has no word at all (Yiddish Qwen readings that are only `[?]`)                                                   |
| `qe_words`      | words; `qe_chars` the characters the model scored                                                                            |
| `qe_version`    | `qe-v1`; `qe_resources` is the sha256 of the resource file used                                                              |

Words are letter runs after joining line-end hyphenation, lower-cased, long s folded, pre-1918 Russian letters modernised, Serbian Cyrillic transliterated to Latin, Yiddish points dropped. `usnm_core::qe::prefer` is the study's pair decision (guard, then lower `lm_oov`); nothing calls it yet. No feed, gate or ranking reads these columns: the worst-first stop rule stays on the damage rate.

## Where it runs

- `usnm_quality` units whose job carries the language's resource file (`qe.bin`, the sixth input of `usnm_quality_qe_in`, sticky) measure every reader's page text into `ReaderReport::qe`. It is part of the canonical result, so two hosts must agree on it; `log2` is `libm`'s, so every platform gives the same bits. The `boinc-quality-native` workflow checks one result's digest on Linux, macOS and Windows.
- `usnm-boinc quality stage --qe-dir DIR` gives units of a language with a file their `qe.bin`; without the flag units are staged as before.
- `usnm-boinc quality collect --qe-dir DIR` measures every crop text (each reader's words in the box, the vision-language reading, Surya's) into `crops.parquet`. `pages.parquet` carries the unit's own measure.
- `scripts/onprem/quality/load.py results` loads the page columns into `quality.reader_scores`.
- Other tools score text with the same code: `cargo run --release -p usnm-core --features qe --example qe -- --command score --resources DIR --rows FILE --out FILE`.

Cost on a host: loading a file and checking its two `fst` checksums takes about 10 ms; a 40,000-character page takes about 27 ms per reader (CT 245). A 20-page bundle with 8 readers adds about 5 s.

## Resource files

One file per language, `<lang>.qe`, pinned by [`resources.lock.json`](resources.lock.json). The files are on the lake at `pve-big:/tank/usnm-lake/qe-resources/qe-v1/`, each with `<lang>.NOTICE.txt` beside it; the notice is also inside the file, after the magic bytes. A file is immutable: a change is a new file, a new sha256 and a new lock entry.

Each file holds the lexicon as an `fst` set and the character model as an `fst` map from each n-gram (and each history) to its packed counts. 4- and 5-grams seen fewer than 3 times are dropped; the truth-set check below is on the pruned files.

| Language | Bytes     | Lexicon words | Model entries | Hunspell       | Licence of the file                     |
| -------- | --------- | ------------- | ------------- | -------------- | --------------------------------------- |
| ces      | 4,744,052 | 600,568       | 242,312       | none (GPL)     | CC-BY-SA-4.0                            |
| dan      | 3,722,537 | 836,118       | 129,511       | MPL 1.1        | CC-BY-SA-4.0 AND MPL-1.1                |
| deu      | 4,749,592 | 628,530       | 156,810       | none (GPL)     | CC-BY-SA-4.0                            |
| eng      | 2,558,531 | 345,920       | 107,230       | SCOWL          | CC-BY-SA-4.0 AND LicenseRef-SCOWL       |
| fin      | 4,979,354 | 726,404       | 168,851       | none available | CC-BY-SA-4.0                            |
| fra      | 3,713,688 | 839,480       | 140,362       | MPL 2.0        | CC-BY-SA-4.0 AND MPL-2.0                |
| nor      | 4,663,810 | 846,466       | 196,637       | CC BY 4.0      | CC-BY-SA-4.0 AND CC-BY-4.0              |
| pol      | 5,686,345 | 4,559,913     | 216,312       | Apache 2.0     | CC-BY-SA-4.0 AND Apache-2.0             |
| rus      | 5,367,430 | 1,564,534     | 194,047       | Lebedev (BSD)  | CC-BY-SA-4.0 AND LicenseRef-Lebedev-BSD |
| slv      | 2,560,727 | 1,159,596     | 176,028       | LGPL 3         | CC-BY-SA-4.0 AND LGPL-3.0-only          |
| spa      | 3,009,210 | 1,123,723     | 138,811       | MPL 1.1        | CC-BY-SA-4.0 AND MPL-1.1                |
| srp      | 4,694,006 | 3,176,079     | 191,051       | MPL 2.0        | CC-BY-SA-4.0 AND MPL-2.0                |
| swe      | 4,366,529 | 1,095,002     | 132,059       | LGPL 3         | CC-BY-SA-4.0 AND LGPL-3.0-only          |
| yid      | 2,046,302 | 5,000         | 179,503       | none available | CC-BY-SA-4.0                            |

Every file is CC BY-SA 4.0 because the lexicon is wordfreq 3.1.1's data and the model's text includes a wordfreq sample and, where Gutenberg fell short of 3 million characters, Wikisource text (both CC BY-SA 4.0). The Hunspell part is used under its non-GPL option where it has one; each notice names the dictionary, its source commit in LibreOffice/dictionaries and carries the licence text the licence asks for. Slovene's README offers "GNU/LGPL and GNU/GPL" and links the current LGPL; the file uses LGPL 3 on that reading.

**Czech and German ship with wordfreq alone.** Their only Hunspell licence is the GPL, so their measure was run both ways (pruned files, `scripts/qe/parity.py`). Against the study's wordfreq plus Hunspell, wordfreq alone changes paired accuracy on the German truth set by −0.25 points (0.766 to 0.763 on 2,433 crop pairs). On the unlabelled crops it changes the decision on 0.88% of Czech crop pairs and 0.31% of German ones, and the share preferring Qwen from 94.05% to 94.17% (Czech) and 85.58% to 85.27% (German). All are under 1 point.

## Parity with the study

`scripts/qe/parity.py` compares the shipped files' measures with the study's Python (`metrics.py --lm-sources gutenberg+wikisource`) on every reading the study scored. Unpruned, the character model's bits per character match the Python to 4e-15 on every reading; the lexicon is a static list where the study asked Hunspell about each word it met, so `oov` is a little higher (unmunched forms plus the period text's accepted words cannot cover every compound).

Paired accuracy on truth-set crop pairs whose CER differs by at least 1 point, with the omission guard (study / shipped files):

| Language | Pairs | `lm_oov`              | `charlm_bpc`          | `oov`                 |
| -------- | ----- | --------------------- | --------------------- | --------------------- |
| English  | 2,910 | 0.855 / 0.861 (+0.58) | 0.841 / 0.842 (+0.10) | 0.774 / 0.778 (+0.36) |
| French   | 214   | 0.972 / 0.972 (0.00)  | 0.981 / 0.981 (0.00)  | 0.811 / 0.811 (0.00)  |
| Finnish  | 238   | 0.887 / 0.882 (−0.42) | 0.924 / 0.920 (−0.42) | 0.863 / 0.863 (0.00)  |
| Swedish  | 224   | 0.920 / 0.924 (+0.45) | 0.920 / 0.920 (0.00)  | 0.926 / 0.926 (0.00)  |
| German   | 2,433 | 0.766 / 0.763 (−0.25) | 0.743 / 0.742 (−0.08) | 0.738 / 0.732 (−0.53) |

Every difference is under 1 point. Pruning harder (n-grams of 3 or more characters seen fewer than 5 times) saved a further 0.2 to 0.7 MB per language and cost Finnish 1.3 points, so it was not used.

On the unlabelled crops (Qwen against LoC, `lm_oov` with the guard), the share of crops preferring Qwen, study / shipped, and the share of crop pairs decided the other way:

| Language  | Crop pairs | Qwen preferred | Decisions that differ |
| --------- | ---------- | -------------- | --------------------- |
| Czech     | 6,003      | 0.941 / 0.942  | 0.9%                  |
| Danish    | 1,225      | 0.972 / 0.972  | 0.2%                  |
| French    | 6,003      | 0.928 / 0.928  | 0.4%                  |
| German    | 319        | 0.856 / 0.853  | 0.3%                  |
| Norwegian | 2,413      | 0.925 / 0.927  | 0.5%                  |
| Polish    | 3,803      | 0.723 / 0.726  | 1.1%                  |
| Russian   | 126        | 0.825 / 0.825  | 0.0%                  |
| Slovene   | 6,008      | 0.970 / 0.971  | 0.1%                  |
| Spanish   | 6,010      | 0.755 / 0.759  | 1.0%                  |
| Serbian   | 6,000      | 0.960 / 0.961  | 0.8%                  |
| Yiddish   | 3,007      | 0.184 / 0.184  | 0.2%                  |

The crate's own tests check the port against `scripts/qe/golden.py`, which runs the study's `qe_lib` on a small model and a set of texts (normalisation, folding, word splitting, counts and log probabilities).

## Building the files

On a host with the lake's `qe-resources/` (Hunspell dictionaries and READMEs, Gutenberg and Wikisource text, `licences/`), wordfreq 3.1.1, Hunspell and `unmunch` (Debian `hunspell-tools`):

```sh
scripts/qe/build_resources.py --res qe-resources --yi-list ocr/ja-ocr/wordlists/yi.txt \
  --langs eng,fra,deu,fin,swe,ces,pol,slv,spa,dan,nor,srp,rus,yid --out build
cargo build --release -p usnm-core --features qe --example qe
scripts/qe/release.py --res qe-resources --build build --qe target/release/examples/qe \
  --out qe-v1 --lock ocr/qe/resources.lock.json
scripts/qe/parity.py --study-metrics metrics-gw --scores truth.jsonl --scores unlabelled.jsonl --out parity.json
```

`release.py` holds the licence decision per language. The same inputs give the same files, byte for byte.
