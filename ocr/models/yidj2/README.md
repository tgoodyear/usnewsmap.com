# yidj2: Tesseract `yid` trained on the Yiddish OCR Corpus

`yidj2.traineddata` is stock Tesseract `yid` fine-tuned on the training split of the [Yiddish OCR Corpus](https://gitlab.com/jochre/corpora/jochre-yiddish-corpus) (Study 10 of the November 2026 GPU studies, draft PR #1428, issue #1479). On the corpus's held-out book pages it halves stock `yid`'s error. Whether it reads our newspapers better is unknown, so **it is opt-in and not deployed**: stock `yid` stays the default.

| File                                                                                  | What it is                                                                                                                                      |
| ------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| `NOTICE.md`                                                                           | The licence, attribution, changes made and disclaimer. Ships with the model                                                                     |
| `provenance.json`                                                                     | Every pin: the model's sha256, the start model, the corpus commit and split, the tools, training settings, recipe, scores and the release asset |
| `SHA256SUMS`                                                                          | The sha256 of each release asset but itself                                                                                                     |
| [`scripts/ocr-models/package_yidj2.py`](../../../scripts/ocr-models/package_yidj2.py) | Builds the release assets from their inputs and checks a downloaded release ([below](#build-and-verify-the-release))                            |

## Release

The model is the [`model-tesseract-yidj2-20261105`](https://github.com/tgoodyear/usnewsmap.com/releases/tag/model-tesseract-yidj2-20261105) release's assets, not a file in the repository:

| Asset                       | What it is                                                                                                                      |
| --------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `yidj2.traineddata`         | The model, sha256 `e12d3b57191df9915179b4c779a218775f010670e980a72dc8e938bf5c4d4578`, 3,323,403 bytes                           |
| `NOTICE.md`                 | As committed here                                                                                                               |
| `LICENSE-Apache-2.0.txt`    | tessdata_best's `LICENSE` at the pinned commit, which covers the start model                                                    |
| `provenance.json`           | As committed here                                                                                                               |
| `training-split.sha256.txt` | The 554 training pages: each page image's and ALTO file's sha256 and path in the corpus                                         |
| `recipe-study10.tar`        | Study 10's recipe from #1428 (commit `4ca78f01`): `study10.sh`, `cut-lines.py`, `tesstrain.Containerfile` and `langdata.sha256` |
| `SHA256SUMS`                | As committed here                                                                                                               |

To fetch and check it:

```sh
gh release download model-tesseract-yidj2-20261105 --repo tgoodyear/usnewsmap.com --dir yidj2
scripts/ocr-models/package_yidj2.py verify --dir yidj2
```

To read with it, put it beside Tesseract's own models and name it: `tesseract page.jpg out -l yidj2 --tessdata-dir DIR`.

## Licence

**CC BY-NC-SA 4.0.** The model is Adapted Material of the corpus, which is licensed under [CC BY-NC-SA 4.0](https://creativecommons.org/licenses/by-nc-sa/4.0/), as cleared on 2026-11-03 in [design 13 §13.7.2](../../../docs/design/13-ocr-engines.md) (#1480). The start model is Apache-2.0. `NOTICE.md` gives the attribution and the changes made.

- **Credit.** Anyone who shares the model credits the corpus as `NOTICE.md` does. The credits page uses `engines.toml`'s `credit` line for pages the model read.
- **NonCommercial.** The clearance rests on usnewsmap.com being non-commercial and doesn't pass to commercial reuse of the model.
- **ShareAlike.** Anyone we give it to, BOINC volunteers included, gets it under the same licence with no added restrictions.
- **Release.** Each release ships `NOTICE.md` and the Apache licence with the model.

## Scores

From Study 10, on the corpus's 104 held-out test pages, with Study 8's references, scorer and page bootstrap. Mean page CER, with 95% limits:

| Reader      | Whole page (PSM 3)    | ALTO line crops (PSM 7) |
| ----------- | --------------------- | ----------------------- |
| Stock `yid` | 8.72% (7.14 to 10.47) | 13.57% (11.41 to 16.28) |
| `yidj2`     | 4.03% (2.84 to 5.52)  | 8.21% (6.11 to 10.75)   |
| Jochre 3    | 1.52% (1.17 to 1.90)  |                         |

- **Better on book pages.** `yidj2` minus stock `yid` is -4.69 points on whole pages (-5.43 to -4.04). It read 103 of 104 pages better; its median page CER is 1.71% against 5.71%.
- **Still behind Jochre 3.** `yidj2` minus Jochre 3 is +2.51 points (+1.31 to +3.94).
- **The test pages are from books it trained on.** The splits share all 60 books, so these scores probably overstate the gain on other books.
- **Not preregistered.** The second training run, the dictionary comparison and the snapshots compared were chosen after seeing the first run's results. The model is the newest best checkpoint on training error, not one picked by test score.
- **Newspapers are unscored.** On 100 newspaper crops, its readings are as far from Jochre 3's as stock `yid`'s are (symmetric edit distance 68.4% against 69.7%). That is agreement, not accuracy.

## Training

- **Start model.** `tessdata_best` `yid`, as pinned in `ocr/engines/tesseract/models.lock`.
- **Data.** The corpus's training split at commit `bd69b412`: 554 pages from 60 books. Each ALTO text line was cut from its page, its text in NFC and logical order. The 512 lines with a left-to-right (bidi class L) character were left out, so Tesseract still lays pages out right to left. That left 20,509 lines: 18,458 to train on and 2,051 for tesstrain's evaluation.
- **Tools.** tesstrain `405346a3`, in Study 8's Tesseract 5.5.0 image with make and Python added, and langdata_lstm `07930fd9`.
- **Settings.** `LANG_TYPE=RTL`, 60,000 iterations, learning rate 0.0001, `RATIO_TRAIN=0.90`, `RANDOM_SEED=0`. One CPU thread at the lowest priority for 1 hour 24 minutes.
- **Result.** The newest best checkpoint, at 52,300 iterations with a training BCER of 1.470%, converted to a float model. Its character set is stock `yid`'s 73 entries, unchanged, and 29 new ones. Its dictionaries are stock `yid`'s 20,582 words, rebuilt in the visual order Tesseract matches.

`provenance.json` has every pin and `recipe-study10.tar` the commands. Study 10 checked the recipe against the commands as run: it selected the same lines, and rebuilt the stock-network control and run 1's model from its checkpoint byte for byte. `provenance.json`'s `reproducibility` records the repeat of training below.

To repeat run 2, fix the order of the unicharset first. tesstrain joins the line files into `all-gt` in `find` order, and the 29 characters that stock `yid` lacks enter the unicharset in the order they first appear. A fresh copy of the same lines gave the same 102 characters in another order, and so a different network. So have tesstrain build `$DATA_DIR/yidj2/all-gt` (its own make target), put those characters first, then run `make ... unicharset proto-model`:

```sh
combine_tessdata -u yidj2.traineddata x.
{ sed -n '75,103p' x.lstm-unicharset | cut -d' ' -f1 | paste -sd' '; cat data/yidj2/all-gt; } > all-gt.new
mv all-gt.new data/yidj2/all-gt
```

Entries 73–101 of the model's unicharset are lines 75–103 of the file. With them first, tesstrain rebuilt Study 10's `unicharset`, `yidj2.unicharset` and proto `yidj2.traineddata` byte for byte. Training from there, with two threads where Study 10 used one, repeated run 2 exactly: the same 600 progress lines and checkpoints, and the 52,300-iteration checkpoint converted to this `yidj2.traineddata` byte for byte.

## Use in the pipeline

`ocr/engines/engines.toml` catalogs the model as `tessdata/yidj2.traineddata` and the profile `yidj2` for `yid`. Nothing selects it:

- **The worker.** The model isn't in `ocr/engines/tesseract/models.lock`, so no image carries it and `profiles.lock` has no entry. The worker picks the first installed profile for a language in catalog order, which is `yid`, ahead of `heb` and then `yidj2`.
- **The quality router.** `usnm_ocr_quality::router::route_page_with` with `RouteOptions { yiddish_jochre: true }` puts `yidj2` first for Yiddish. `route_page` doesn't.

Making it the default is #1479's step 4. It waits on the newspaper score: a qualified reader transcribes [`ocr/bench/yiddish-newspaper/`](../../bench/yiddish-newspaper/README.md), a second checks it, and `yid`, `yidj2` and Jochre 3 are scored against it. If `yidj2` wins, the maintainer decides; the change is then:

1. pin the release asset's URL and sha256 in `ocr/engines/tesseract/models.lock`, and add `yidj2`'s `profiles.lock` entry with `usnm-ocr-worker routes check --update-lock`;
2. move the `yidj2` profile ahead of `yid` in `engines.toml`, or set the router option;
3. ship `NOTICE.md` and `LICENSE-Apache-2.0.txt` in any image or BOINC app that carries the model.

## Build and verify the release

The inputs aren't in the repository: the model and the training-split list are kept with Study 10's runs, the Apache licence is tessdata_best's `LICENSE` at commit `e12c65a9`, and the recipe is #1428's `docs/reports/2026-11-gpu-studies/scripts/yiddish/` at commit `4ca78f01`. Pass that folder as it is: `build` takes only the four files `provenance.json` pins and ignores the other studies' scripts. It refuses any input whose sha256 differs from `provenance.json`, packs the recipe into a tar that depends only on the files' names and bytes, and fails unless the result matches the committed `SHA256SUMS`:

```sh
scripts/ocr-models/package_yidj2.py build --model yidj2.traineddata --base-license LICENSE \
  --training-split training-split.sha256.txt --recipe-dir yiddish --out release
```

After a deliberate change to `NOTICE.md` or `provenance.json`, `--update-sums` rewrites `SHA256SUMS`; the result is a new release, not a replaced asset. `scripts/ocr-models/test_package_yidj2.py` checks the build on made-up inputs, and that `SHA256SUMS`, `provenance.json`, `NOTICE.md` and the catalog entry agree.
