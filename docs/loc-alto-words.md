# LoC ALTO words in Parquet

`usnm-ocr-worker loc-alto-words` writes every word of LoC's OCR from Chronicling America's bulk OCR archives to Parquet, one file per archive, with each word's place on the page, confidence and box ([#1424](https://github.com/tgoodyear/usnewsmap.com/issues/1424)). With it, answering a question about LoC's words means reading a few columns instead of decompressing every archive.

`loc-alto-stats` is unchanged. It keeps a row a page for the damage and ranking work, and its loader reads every Parquet file in its directory, so the word files must go in a different directory.

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
| `--min-free-gib` | `USNM_LOC_WORDS_MIN_FREE_GIB` | Starts no archive while the output's filesystem has less than this many GiB free. Default 0     |

Each archive becomes `<out>/<batch>_verNN.parquet`, written to a hidden `.part` file, synced and renamed. A `<batch>_verNN.done` beside it records the archive's size and the schema version, so a rerun skips finished archives and reads one again if either changed. An interrupted run loses only the archives it was reading. A failed archive doesn't stop the others; the run exits non-zero at the end. The job checks free space before each archive, so with N workers free space can end up as much as N archives' output below the limit. The check reads what the filesystem reports, which on a dataset with a quota is the space left in the quota, not in the pool.

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

| `dpi_source`  | Meaning                                                                                                                                                                                       |
| ------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `pixel`       | `MeasurementUnit` is already `pixel`                                                                                                                                                          |
| `settings`    | `xdpi:` in `processingStepSettings`                                                                                                                                                           |
| `image_width` | `width:` in `processingStepSettings`, against `Page/@WIDTH`                                                                                                                                   |
| `grid`        | Inferred, `inch1200` only: every coordinate on the page is a multiple of 4 (300 dpi), 3 (400 dpi) or 2 (600 dpi). Needs 40 or more coordinates. A page that fits both 4 and 3 gets no dpi |
| `none`        | No way to tell, or a unit we don't know. Boxes stay in the ALTO unit, and `coord_unit` says which                                                                                             |

To go back to ALTO units: `value / (dpi / 1200)` for `inch1200`, `value / (dpi / 254)` for `mm10`.

**`none` is common.** On 19 of the archives in [the proof](#proof), 26,686 of 59,086 pages have no dpi. Their ALTO says `inch1200` but gives no image size, and their coordinates fit no grid. The archives hold no images, so nothing in them gives the dpi. For those pages, map a box onto a scan by proportion: `x / page_width * image_width`. That works for every page, whatever its `coord_unit`. At least one batch's `inch1200` values look like pixels already (`fu_cofrin_ver01`, pages 1,420 units wide), so don't assume the unit is true.

## Why these columns

The columns follow from a check of LoC's confidence against the damage score ([#1424](https://github.com/tgoodyear/usnewsmap.com/issues/1424)).

- **`ocr_software`, `ocr_version` and `ocr_engine`.** The first `softwareName`, which `loc-alto-stats` keeps, is often a preprocessing tool. `iArchives OCR Framework` reports its version as "Multiple", yet its pages sit on two confidence scales: a median `page_pc` of 0.37 to 0.39 under ABBYY 8 and 0.81 to 0.93 under ABBYY 9, on the archives sampled. `ocr_engine` separates them.
- **`page_pc`.** On 94.9% of iArchives pages with 50 or more scored words, mean and median confidence are equal: each word carries the page's value. There, `WC` is a page score, so the page's own value needs keeping beside it.
- **`wc`.** Normalized within software, low confidence goes with high damage, a Spearman rank correlation of 0.49 over 20.2M pages. It also picks out pages the damage score can't rate.
- **`cc`.** At 1.2 bytes a word it's the one costly column kept without a measured use. It's the only signal finer than a word, and getting it later would mean reading every archive again.

## Proof

Twenty-one archives covering all 15 `softwareName` values `loc-alto-stats` has seen, ALTO 1.2, 1.4, 2 and 3 and the CCS dialect: 70,035 pages and 221,213,678 words, no page left out. Page and word counts match `loc-alto-stats` on all 21. An independent reader (Python's ElementTree, sharing no code with this job) compared 6,717,132 words on 2,357 pages, every field against the XML, with no mismatches. Small archives were read whole, large ones one page in 20 to 50.

The files came to 2.28 GB, 10.3 bytes a word, against 4.9 GB of archives. Archives run from 6.8 to 13.5 bytes a word, except the smallest, of a few pages, which reach 20. On the first 19, text takes 3.0 bytes, the four box columns 4.4, `cc` 1.2 and `wc` 0.7.

The corpus has about 107 billion words, so the whole store comes to about 1.1 TB.

## Speed

The job costs 9.7 CPU minutes per GB of archive. About two thirds of that is bzip2, which no setting here changes. The 2.5 TB of archives come to about 400 CPU hours. With 4 workers it peaked at 595 MB.

Wall time depends on what else the host runs. At `nice 19` and idle I/O priority under a load of about 20 on eight threads, four workers got 0.6 of a core and read 3.7 GB an hour. At `nice 10` under about the same load they read 10 GB an hour. At `nice 19` under a load of about 50 they got almost nothing.
