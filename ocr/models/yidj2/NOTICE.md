# yidj2.traineddata: notice

`yidj2.traineddata` is a Tesseract 5 LSTM model for printed Yiddish. usnewsmap.com trained it, in Study 10 of [issue 1479](https://github.com/tgoodyear/usnewsmap.com/issues/1479), by fine-tuning Tesseract's `yid` model on the Yiddish OCR Corpus.

## Licence

`yidj2.traineddata` is licensed under the [Creative Commons Attribution-NonCommercial-ShareAlike 4.0 International licence](https://creativecommons.org/licenses/by-nc-sa/4.0/) (CC BY-NC-SA 4.0).

- **NonCommercial.** It may be used and shared only for noncommercial purposes.
- **ShareAlike.** Anything adapted from it must be shared under the same licence.

It is Adapted Material of this work:

- **The Yiddish OCR Corpus** by Assaf Urieli and the Yiddish Book Center, licensed under [CC BY-NC-SA 4.0](https://creativecommons.org/licenses/by-nc-sa/4.0/).
  - Source: <https://gitlab.com/jochre/corpora/jochre-yiddish-corpus>, commit `bd69b412947e03cca18fdd5c72826207946d777f`.
  - Training split only.
  - Cite: Assaf Urieli, Amber Clooney, Michelle Sigiel and Grisha Leyfer, "Jochre 3 and the Yiddish OCR corpus", 2025, [arXiv:2501.08442](https://arxiv.org/abs/2501.08442).

It is a modification of this work:

- **`yid.traineddata`** from [tessdata_best](https://github.com/tesseract-ocr/tessdata_best), commit `e12c65a915945e4c28e237a9b52bc4a8f39a0cec`, by the Tesseract OCR project.
  - It's licensed under the Apache License 2.0; `LICENSE-Apache-2.0.txt` is a copy of that repository's `LICENSE`.
  - The start model is available under Apache-2.0 from that source. Section 4 of the Apache licence allows the modified model as a whole to carry the licence above.

Training used the unicharset properties of [langdata_lstm](https://github.com/tesseract-ocr/langdata_lstm), commit `07930fd9f246622c26eb5de794d9212ceac432d3`, under Apache-2.0. That data isn't redistributed here.

## Changes made

- **Network.** The LSTM network of `yid.traineddata` was trained further, using [tesstrain](https://github.com/tesseract-ocr/tesstrain) `405346a3a67d8e4e049341d1da6a4b752e0b8351` and Tesseract 5.5.0:
  - 52,300 iterations, at learning rate 0.0001 and `LANG_TYPE=RTL`;
  - on 20,509 text lines cut from the corpus's 554 training pages (18,458 for training, 2,051 for evaluation).
  - Lines containing any character of Unicode bidirectional class L (left to right) were left out.
- **Unicharset.** The unicharset was extended with the corpus's characters. The start model's characters keep their ids.
- **Dictionaries.** The word, punctuation and number dictionaries were rebuilt from the start model's own lists, in the right-to-left order Tesseract's decoder matches.
- **Format.** The model is the float model written by `lstmtraining --stop_training`.

`provenance.json` records every pin, the training settings and the recipe. `training-split.sha256.txt` lists the corpus pages trained on, by image and ALTO sha256.

## Disclaimer

The model is provided as is, without warranties or conditions of any kind, as set out in section 5 of CC BY-NC-SA 4.0 and sections 7 and 8 of the Apache License 2.0. The licensors aren't liable for any use of it. No endorsement by the Yiddish Book Center, the corpus authors or the Tesseract OCR project is implied.
