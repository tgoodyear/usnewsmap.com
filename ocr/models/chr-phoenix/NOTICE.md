# chr_phoenix.traineddata: notice

`chr_phoenix.traineddata` is a Tesseract 5 LSTM model for the Cherokee syllabary as printed in the Cherokee Phoenix (1828 to 1834). usnewsmap.com trained it for [issue 1621](https://github.com/tgoodyear/usnewsmap.com/issues/1621) by fine-tuning Tesseract's `chr` model on lines cut from the paper's pages and on synthetic lines set from its own glyphs.

## Licence

`chr_phoenix.traineddata` is licensed under the [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0); `LICENSE-Apache-2.0.txt` is a copy of tessdata_best's `LICENSE`.

It is a modification of this work:

- **`chr.traineddata`** from [tessdata_best](https://github.com/tesseract-ocr/tessdata_best), version string `4.00.00alpha:chr:synth20170629`, sha256 `b3a0da90e7744cfec534cdf2dd85860718f32b5595a07d4b7e49113a89bd0f98`, by the Tesseract OCR project, under Apache-2.0.

It was trained on:

- **Page scans and OCR of the Cherokee Phoenix** from the Library of Congress's Chronicling America, batches `dlc_misctopsn83020866_ver03` and `dlc_misctopsn83020874_ver02`. The paper is in the public domain.
- **The Cherokee New Testament (1860)**, as typeset at <https://todaysnative.org/wp-content/uploads/2019/01/cnts.pdf> (sha256 `54fd06850e09f4904b1b3f941e491789be39f9ad89f88c9b7e610ec4e4fb88ca`), CC0. Its text was used for the synthetic lines' wording and its word forms for the dictionary and for checking real lines.

## Changes made

- **Dictionary.** The start model's word list was extended with the New Testament's word forms (the `B_stock_nt` model of #1621's dictionary comparison).
- **Network.** The start model's LSTM network was trained further with Tesseract 5.5.0's `lstmtraining`, in two rounds of 20,000 iterations each, on about 14,400 real line images and 20,000 synthetic ones per round. `README.md` and `provenance.json` give the details.
- **Character set.** Unchanged.
- **Format.** The float model written by `lstmtraining --stop_training`.

## Disclaimer

The model is provided as is, without warranties or conditions of any kind, as set out in sections 7 and 8 of the Apache License 2.0. No endorsement by the Library of Congress, the publishers of the New Testament text or the Tesseract OCR project is implied.
