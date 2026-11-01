# LoC ALTO words in Parquet

`usnm-ocr-worker loc-alto-words` writes every word of LoC's OCR from Chronicling America's bulk OCR archives to Parquet, one file per archive, with its place on the page, its confidence and its box ([#1424](https://github.com/tgoodyear/usnewsmap.com/issues/1424)). With it, a question about LoC's words reads a few columns instead of decompressing every archive.

`loc-alto-stats` is unchanged. It keeps a row a page for the damage and ranking work, and its loader reads every Parquet file in its directory, so the word files must go somewhere else.

## Running it

```sh
usnm-ocr-worker loc-alto-words --archives DIR --out DIR [--workers N] [--limit N] [--min-free-gib N]
```

| Flag             | Environment                   | Meaning                                                                      |
| ---------------- | ----------------------------- | ---------------------------------------------------------------------------- |
| `--archives`     | `USNM_LOC_WORDS_ARCHIVES`     | Directory of `<batch>_verNN.tar.bz2`                                         |
| `--out`          | `USNM_LOC_WORDS_OUT`          | Where the Parquet files go                                                   |
| `--workers`      | `USNM_LOC_WORDS_WORKERS`      | Archives read at once, one thread each. Default 1                            |
| `--limit`        | none                          | At most this many archives this run                                          |
| `--min-free-gib` | `USNM_LOC_WORDS_MIN_FREE_GIB` | Starts no archive while the output's filesystem has less free. Default 0     |

Each archive becomes `<out>/<batch>_verNN.parquet`, written to a hidden `.part` file, synced and renamed. A `<batch>_verNN.done` beside it records the archive's size and the schema version, so a rerun skips finished archives and reads one again if either changed. An interrupted run loses only the archives it was reading. A failed archive doesn't stop the others; the run exits non-zero at the end. A check of free space runs before each archive, so with N workers the disk can fall by up to N archives' output below the limit. The check reads what the filesystem reports, which on a dataset with a quota is the quota's room, not the pool's.

A worker holds its current page and one row group in memory. It streams the tar, and reads each page with a pull parser.

## Schema

A row is one ALTO `String`, in document order. A page's rows are consecutive. A page with no `String` still gets one row, with null `text` and `-1` for `block`, `line` and `word`, so every page read is in the file. A page whose XML can't be parsed is left out and logged. If an archive holds a page twice, the first is kept, as in `loc-alto-stats`.

| Column                | Type    | ALTO source                                                                                       |
| --------------------- | ------- | ------------------------------------------------------------------------------------------------- |
| `batch`               | string  | The archive's name                                                                                |
| `batch_version`       | int16   | The archive's name                                                                                |
| `doc_id`              | string  | The tar path: `<lccn>_<yyyy>-<mm>-<dd>_ed-<n>_seq-<m>`, as in `loc-alto-stats` and the page index |
| `block`               | int32   | Ordinal of the `TextBlock` on the page from 0, empty blocks and blocks in a `ComposedBlock` included |
| `line`                | int32   | Ordinal of the `TextLine` in its block from 0                                                     |
| `word`                | int32   | Ordinal of the `String` in its line from 0                                                        |
| `text`                | string  | `String/@CONTENT`, entities decoded                                                               |
| `wc`                  | float32 | `String/@WC`. Null if absent or outside 0 to 1, as `loc-alto-stats` counts it                     |
| `cc`                  | string  | `String/@CC` as written: per-character confidence, with or without spaces                         |
| `hyphen`              | bool    | A `HYP` follows this `String`                                                                     |
| `subs_content`        | string  | `String/@SUBS_CONTENT`                                                                            |
| `subs_type`           | string  | `String/@SUBS_TYPE`                                                                               |
| `alternatives`        | string  | The `String`'s `ALTERNATIVE` children, joined by U+001F                                           |
| `lang`                | string  | `TextBlock/@language`, or `@LANG`                                                                 |
| `x`, `y`, `w`, `h`    | int32   | `String/@HPOS`, `@VPOS`, `@WIDTH`, `@HEIGHT`, in `coord_unit`, rounded                            |
| `page_width`          | int32   | `Page/@WIDTH`, in `coord_unit`                                                                    |
| `page_height`         | int32   | `Page/@HEIGHT`, in `coord_unit`                                                                   |
| `coord_unit`          | string  | `pixel`, or the ALTO unit if the page gives no way to convert                                     |
| `alto_unit`           | string  | `Description/MeasurementUnit`                                                                     |
| `dpi`                 | float32 | The dpi used to convert. Null when `dpi_source` is `pixel` or `none`                              |
| `dpi_source`          | string  | Where `dpi` came from, below                                                                      |
| `alto_version`        | string  | The root's `xsi:schemaLocation` file (`1.4`), else the LoC namespace (`2`), else the namespace    |
| `software`            | string  | The first `softwareName` in the file: the same value as `loc-alto-stats`                          |
| `software_version`    | string  | The first `softwareVersion` in the file                                                           |
| `ocr_software`        | string  | The first `softwareName` under `ocrProcessingStep`                                                |
| `ocr_version`         | string  | The first `softwareVersion` under `ocrProcessingStep`                                             |
| `ocr_engine`          | string  | The first `<name>.version:` in `processingStepSettings`, such as `abbyy8 8.1.0.6184-12`           |
| `page_pc`             | float32 | `Page/@PC`                                                                                        |
| `page_accuracy`       | float32 | `Page/@ACCURACY`                                                                                  |

Columns that repeat for a page's rows cost almost nothing: Parquet stores them as a dictionary and run lengths.

**`software` and `ocr_software` can differ.** Where a batch's ALTO lists a preprocessing step first, `software` names that step's tool. PolishImage, Image Converter and the docWorks family are such tools on the pages sampled, so `software` alone doesn't always name the engine that read the page.

**Left out.** `ID` attributes, which can be rebuilt from order. `STYLE`, `STYLEREFS` and the `Styles` section. `SP` elements, whose gaps can be rebuilt from the boxes. `TextBlock` and `TextLine` boxes, which are the union of their words on most pages. `Illustration` and `GraphicalElement`. The processing dates and agencies.

### Pixels

Boxes are stored in pixels of the page image where the ALTO allows it. `dpi_source` says how:

| `dpi_source`  | Meaning                                                                                                                       |
| ------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| `pixel`       | `MeasurementUnit` is already `pixel`                                                                                          |
| `settings`    | `xdpi:` in `processingStepSettings`                                                                                           |
| `image_width` | `width:` in `processingStepSettings`, against `Page/@WIDTH`                                                                   |
| `grid`        | Inferred, `inch1200` only: every coordinate on the page is a multiple of 4 (300 dpi), 3 (400 dpi) or 2 (600 dpi). Needs 40 or more coordinates. A page that fits two of those names nothing |
| `none`        | No way to tell, or a unit we don't know. Boxes stay in the ALTO unit, and `coord_unit` says which                             |

To go back to ALTO units: `value / (dpi / 1200)` for `inch1200`, `value / (dpi / 254)` for `mm10`.
