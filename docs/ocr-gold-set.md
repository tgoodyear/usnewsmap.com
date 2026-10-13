# OCR gold set

Part of [#296](https://github.com/tgoodyear/usnewsmap.com/issues/296). The [engine survey](reports/2026-10-ocr-engine-survey/report.md) has no ground truth, so it can't report a character error rate (CER) or word error rate (WER); it relies on the damage metric. The gold set is a fixed set of crops with a person's transcription, so any engine, profile or route change can be scored by CER and WER on the same text. The plan in [13 §13.9](design/13-ocr-engines.md#139-how-the-survey-and-production-share-engine-images) already names `score --truth <crops dir>`; this is what goes in that directory.

The transcriptions are not written yet. Every `transcription` field is empty, and none may be filled from an engine's output.

## Selection

- **Source.** The survey's 80 scored pages (`data/scores.jsonl`; the sample itself is on pve-big). One crop per page.
- **Target.** 60 crops. The target is shared equally between the survey's groups; a group with fewer pages gives all of them and the others share the rest. The first selection is 10 each for German, worst-English, Serbian, Czech and control, and 5 each for Polish and Russian (all their pages).
- **Strata.** Group (which stands for language and print type: the survey groups were chosen as the worst-read cases per language, and the control is well-read English dailies), then era. Inside a group the picks cycle through decades in a seeded order, so a group spans as many decades as it has pages in. The era runs from the 1850s to the 1960s.
- **Seeded.** `gold.py select` with seed 296 reproduces `fixtures/ocr-gold/selection.jsonl`.
- **Not stratified by typeface.** The sample records no typeface (Fraktur or Antiqua, for German). Record it in `notes` when transcribing; if a stratum turns out to hold one typeface only, add pages.
- **Crop box.** One text region per page, chosen as the survey's `crops.py` does (a line of LoC's ALTO with 4 or more words in a block at least 600 pixels wide, and the region below it, up to 1,600 pixels wide and 6% of the page high), seeded per page. The box is fixed in the manifest.

```sh
scripts/ocr-survey/gold.py select --scores docs/reports/2026-10-ocr-engine-survey/data/scores.jsonl \
  --out fixtures/ocr-gold/selection.jsonl --target 60 --seed 296
# on a node, with the survey's sample and work directories:
scripts/ocr-survey/gold.py cut --selection fixtures/ocr-gold/selection.jsonl --sample SAMPLE --work WORK --out OUT
```

`select` with `--manifest SAMPLE/manifest-final.jsonl` gives the same choice and carries the image and ALTO URLs and checksums.

## Japanese crops

The 24 Japanese crops of the [#128](https://github.com/tgoodyear/usnewsmap.com/issues/128) comparison are not in the 60. `~/code/usnm-ja-ocr/crops/refs/` holds 23 text files for them, and `refs-excluded/` one more. Their provenance, from the files and logs on the Mac (read only):

- No script, log or note in that directory or in the repository records how the refs were made. `ja-ocr/crops.py` says "transcribe each crop into refs/<page_id>.txt" and nothing more; the commit that added it has no message beyond its title.
- None is identical to any engine's text for the crop (best similarity per file 0.36 to 0.93 against NDLOCR-Lite, the prototype NDLOCR run, or Azure), so they are not copies of one engine's output.
- The files were written between 10:35:20 and 10:50:46 on 4 October, 20 to 60 seconds apart, some of 1,400 to 1,700 characters, and two in the same second. That pace is not typing by a person. It fits transcription by a program or an AI model reading the images, but nothing on disk says which.
- Fifteen lines contain the geta mark 〓 for an unreadable character. No engine's output contains it.

Verdict: not verified as hand-made, and most likely machine-made. They are not ground truth. The survey report is right that no crops were hand-transcribed. Until a person transcribes them from the images, they are a hint, like an engine's text. The images are only on the Mac, so they are not in the manifest yet.

Needs transcription (crop from `sample.csv`, all unverified):

| page                                | group   | file on the Mac                         |
| ----------------------------------- | ------- | --------------------------------------- |
| `sn83025518_1945-06-16_ed-1_seq-2`  | typeset | ref                                     |
| `sn83025518_1945-08-14_ed-1_seq-3`  | typeset | ref                                     |
| `sn83025518_1945-10-27_ed-1_seq-3`  | typeset | ref                                     |
| `sn83025517_1944-11-08_ed-1_seq-2`  | typeset | ref                                     |
| `sn83025517_1944-08-23_ed-1_seq-2`  | typeset | ref                                     |
| `sn83025518_1945-10-27_ed-1_seq-2`  | typeset | ref                                     |
| `sn83025518_1945-06-07_ed-1_seq-3`  | typeset | ref                                     |
| `sn83025517_1945-10-29_ed-1_seq-2`  | typeset | ref                                     |
| `sn83025517_1944-11-08_ed-1_seq-3`  | typeset | ref                                     |
| `sn83025517_1945-10-29_ed-1_seq-3`  | typeset | ref                                     |
| `sn83025517_1944-09-25_ed-1_seq-3`  | typeset | ref                                     |
| `sn83025517_1945-06-20_ed-1_seq-3`  | typeset | ref                                     |
| `sn85040043_1943-09-30_ed-1_seq-3`  | camp    | ref                                     |
| `sn94050512_1945-03-03_ed-1_seq-2`  | camp    | ref                                     |
| `sn84024049_1943-08-21_ed-1_seq-9`  | camp    | ref                                     |
| `sn83025352_1945-09-08_ed-1_seq-4`  | camp    | ref                                     |
| `sn83025521_1942-10-21_ed-1_seq-8`  | camp    | excluded (4 lines; why is not recorded) |
| `sn84025952_1944-04-20_ed-1_seq-8`  | camp    | ref                                     |
| `sn83025353_1943-11-20_ed-1_seq-8`  | camp    | ref                                     |
| `sn85040043_1943-09-21_ed-1_seq-5`  | camp    | ref                                     |
| `sn83025353_1942-10-14_ed-1_seq-8`  | camp    | ref                                     |
| `sn83025522_1943-09-01_ed-1_seq-10` | camp    | ref                                     |
| `sn83025333_1943-04-24_ed-1_seq-9`  | camp    | ref                                     |
| `sn84025952_1944-04-20_ed-1_seq-7`  | camp    | ref                                     |

The scorer for these is `scripts/ja-ocr/score.py`, which folds old character forms and also reports bigram F1. Japanese text has no spaces, so `gold.py`'s WER doesn't apply to it; once transcribed, the crops can be added to `crops.jsonl` with `lang` `jpn` and scored by CER.

## Crop format

- `manifest.jsonl`, one row per crop: `crop_id` (`<group>-<nn>`), `page_id`, `group`, `lang`, `era`, `stratum`, `lccn`, `date`, `edition`, `seq`, `title`, `box` (x0, y0, x1, y1 in the page image's pixels), `image`, `crop_sha256`, `encoder`, `page_url` (the loc.gov page), `page_image_url` (the IIIF full-size JPEG the survey used), `page_image_sha256`, `alto_url`.
- `images/<crop_id>.jpg`: the box cut from the page, grayscale, JPEG quality 85.
- `crops.jsonl`, the transcription template, one row per crop: `crop_id`, `group`, `lang`, `date`, `title`, `image`, `hints`, `transcription` (null until a person fills it in), `transcriber`, `reviewer`, `hint_copy_reviewed` (false), `notes`.
- `hints.texts` holds what LoC's ALTO and each engine read in the box, in visual line order. `hints.is_truth` is false and `hints.note` says so. A hint is there to find the passage on the page, not to be corrected. The CER of a hint against the transcription is a result, so a transcription that copies a hint spoils it.

## Where the images live

The page checksum pins the pixels. `page_image_sha256` is the checksum of the page JPEG the survey fetched, and `gold.py fetch` downloads each page (paced as the survey's `locfetch.py` does), checks it, and cuts the box. The crop JPEGs themselves are committed under `fixtures/ocr-gold/` only if the total is under about 10 MB; `cut` prints the total. Otherwise the repository holds the manifest, and `fetch` rebuilds the images on a node. JPEG bytes can differ between Pillow versions, so a differing `crop_sha256` with a matching page checksum is a warning, not an error. 60 pages are about 60 full-resolution downloads from loc.gov, so `fetch` belongs on a node, not on a laptop.

## Transcription rules

1. **Verbatim.** Type what is printed, including errors, old spellings, long s (ſ), ligatures and printed punctuation. Don't correct, modernize or normalize. Serbian stays in the script printed (Cyrillic or Latin).
2. **Lines.** One line of the transcription per printed line in the crop, in reading order for the crop's column. If the crop cuts through a line at its edge, transcribe only the characters wholly inside the crop and note it in `notes`.
3. **Hyphenation.** Keep the hyphen at the end of a line as printed and don't join the word. The scorer joins it (see below).
4. **Uncertain characters.** `[?]` stands for one character that can't be read. `[?x]` stands for characters read as `x` with doubt. More than 2% of a crop's characters marked this way fails validation; drop the crop and add another page instead.
5. **Out of scope.** Leave out ornaments, rules and the text of images. Mark a gap with a line `[...]` only if it falls inside a line of text that was skipped.
6. **Who.** The maintainer transcribes, or a person they name. Set `transcriber` to that person. An engine's text, or a model's reading of the image, is never the transcription.
7. **Second pass.** A different person reads every transcription against the image, fixes it, and sets `reviewer`. The validator rejects a reviewer equal to the transcriber. If only one person is available, run validate with `--allow-unreviewed`, and say so wherever scores are quoted.
8. **Hint copies.** A transcription equal to a hint after normalization fails validation. If the hint really is right, check the image and set `hint_copy_reviewed` to true.

```sh
scripts/ocr-survey/gold.py validate --crops OUT/crops.jsonl --manifest OUT/manifest.jsonl --images OUT/images
```

## CER and WER

`gold.py score` compares each hypothesis text with the transcription. This is the CER the plan describes in 13 §13.9, minus the part that finds the crop's span in a whole page's text; that part needs the engine runs' word boxes, and belongs in `usnm-ocr-survey score`, which doesn't exist yet (the crates have no `usnm-ocr-survey`, so there is nothing to extend). Until then the Python scorer is the reference for the arithmetic: edit distance over the normalized transcription's length, summed over crops (a crop's weight is its length), reported for all crops and per group. WER is the same over whitespace-separated words.

Normalization, on both sides:

1. Unicode NFC. Case, punctuation, diacritics, long s and ligatures are not folded.
2. A hyphen at the end of a line, followed by the line break, is removed and the line joined. The survey's text does the same.
3. Line breaks and runs of whitespace become one space.

In the transcription only, `[?x]` counts as `x` and `[?]` is dropped.

Hypotheses are the hint texts in `crops.jsonl` (`--hints`), or per-engine files of `{"crop_id", "text"}` lines (`--hyp tesseract-5.5=FILE.jsonl`). Crops that fail validation are left out and counted.

```sh
scripts/ocr-survey/gold.py score --crops OUT/crops.jsonl --hints --out OUT/scores
```

## Licensing

The library believes the newspapers in Chronicling America are in the public domain or have no known copyright restrictions, and that those published more than 95 years ago are in the public domain in their entirety; newer ones may hold copyrighted third-party material ([Rights and Access](https://www.loc.gov/collections/chronicling-america/about-this-collection/rights-and-access/)). Pages in the sample run to 1963, so crops from pages after 1931 carry that caveat. Transcriptions are original work and go in under the repository's licence.

## Human-corrected text from LoC

Checked in public documentation only, nobody contacted.

- **Confirmed.** LoC's OCR text and ALTO for Chronicling America pages are machine-made. The survey's own LoC text is that.
- **Partly confirmed.** The Newspaper Navigator paper ([arXiv:2005.01583](https://arxiv.org/abs/2005.01583)) says its training annotations were collected in LoC's Beyond Words crowdsourcing initiative. A search summary of the paper describes volunteers drawing boxes around photographs, illustrations, comics, cartoons and maps in World War 1-era pages and correcting the OCR of their captions, about 10,000 corrected annotations. That covers captions on illustrations in one era, not running article text, and the abstract page I fetched did not carry those details. UNVERIFIED: the count, the exact era, and whether the corrected captions can be matched to the sample's pages.
- **UNVERIFIED.** Whether LoC publishes corrected full-page text for any Chronicling America page. I found none in the documentation I could reach; the About page returned HTTP 403 to the fetch tool.
- **Not used.** None of this is in the gold set. Even if it were, volunteer corrections would need the same second-pass review before counting as truth.

## Open

- A person transcribes the 60 crops and the Japanese crops.
- Decide whether to commit the images once `cut` has run on a node and the total is known.
- `usnm-ocr-survey score` takes over the span-finding for whole-page runs and reads this format as `--truth`.
