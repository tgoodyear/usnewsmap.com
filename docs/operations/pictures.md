# Pictures

Part of the [operations guide](README.md).

## Newspaper Navigator's picture boxes

`usnm-ingest pictures` ([04 §4.11](../design/04-data-sources-and-ingestion.md#411-pictures-1102), #1102) writes Newspaper Navigator's boxes of photographs, illustrations, maps, comics and cartoons to the curated store, one row per box, keyed by the scan the box was found on and with its crop URL on tile.loc.gov. It is the first part of the picture layer. Nothing reads the table yet: the join to the curated pages, the search field and filter, and the crops in the hit list come later (#1102).

1. **The revision.** `--revision` is required and is the full 40-character commit SHA of [`biglam/newspaper-navigator`](https://huggingface.co/datasets/biglam/newspaper-navigator) (CC0), in lower case; a branch or tag is refused, because it would not pin the data. The one #1102 was triaged against is `e2e585824cdd28d52755c481dca69ed0bfa992a1`. A later revision is a separate run with its own path.
2. **Where.** On an on-prem node, as a detached unit. It downloads about 1.4 GB of Parquet (the cartoons file alone is 71 MB for 206,054 rows) one file at a time into `$USNM_WORK_DIR/pictures` (default: `usnm-ingest` in the system temp directory), each under a name of its own (revision, file and a random part) so runs can share the directory, hashes and reads the file it opened, deletes each once read, and holds about one part of 100,000 rows in memory. The command takes the top-level options every `usnm-ingest` command takes; it reads nothing from `--reference` and keeps no state, but the binary wants both an empty directory and a state file:

   ```sh
   systemd-run --unit usnm-pictures --nice 19 \
     /usr/local/bin/usnm-ingest --state-file /var/lib/usnm-pictures/state.json \
     --curated <curated directory or container URL> --reference <empty directory> \
     pictures --revision e2e585824cdd28d52755c481dca69ed0bfa992a1
   journalctl -u usnm-pictures
   ```

   Files already on the node are read with `--from-dir <directory>`, which holds one directory per config (`cartoons`, `comics`, `illustrations`, `maps`, `photos`) with the config's `*.parquet` files in it, as `huggingface-cli download` lays them out; a link to a file, as in a Hugging Face cache snapshot, counts as the file, and one that points at nothing fails the run. Without it the run lists each config's files at the revision (`/api/datasets/biglam/newspaper-navigator/tree/{revision}/{config}`) and fetches them from `resolve/{revision}/…`, checking each against the SHA-256 and size the Hub lists for it. `--hub-url` points that at another host, for a copy of its responses.

   A curated directory on the node needs no grant. The Azure `curated` container takes the writes only from an identity with a create-only grant on `pictures/`; nothing has one yet, and adding it is an infrastructure change of its own, so the first run writes to a directory.

3. **What it writes.** Under `pictures/newspaper-navigator/{revision}/{attempt}/` in the curated store, all create-only:
   - `{kind}-{nnnn}.parquet`, `kind` being `cartoon`, `comic`, `illustration`, `map` or `photo`: at most 100,000 rows each, numbered consecutively within a kind, and never spanning two input files. The columns are `kind`, `lccn` (lower case), `date`, `edition`, `batch`, `reel`, `frame`, `score`, `box` (four numbers, as the dataset gives them), `caption_words`, `predicted_type`, `page_url` and `crop_url`.
   - `provenance.json`, last. It marks the attempt finished and holds the revision, the licence, the tool's version and commit, every input file with its size, SHA-256 and rows, each kind's parts with their SHA-256, bytes and rows, and the counts below, per kind and in total.

   Attempts are numbered from `0001`. A run that stops leaves its parts without a `provenance.json`; they are ignored, and the next run starts the next attempt. They can be deleted by hand; nothing reads them. A revision that already has a finished attempt of the current schema version is not read again: the run says so and exits, and it writes a new attempt only after a schema change. The report is JSON on stdout (revision, attempt, rows by kind, and the path of the provenance).

4. **What to check.** In `provenance.json`:
   - `rows` by kind against the dataset's counts: photos 1,543,525, illustrations 798,475, comics 526,319, maps 200,188, cartoons 206,054, 3,274,561 in all. A shortfall means a file was missed.
   - `quality`, whose counts say how much of the data the rules fit. Expect small numbers, and none for `frame_differs_from_page_seq_num`, which would mean `page_seq_num` is not the frame after all:

     | Count                             | Rows that                                                                                                                                |
     | --------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
     | `unplaced_rows`                   | have no complete scan identity (no scan in `page_url`, or no `batch`) or no `crop_url`, whatever the reason (what the stop below counts) |
     | `batch_missing`                   | have an empty `batch`: an incomplete identity, so also in `unplaced_rows`                                                                |
     | `page_url_unplaced`               | have a `page_url` that is not `…/{lccn}/{reel}/{issue}/{frame}.jp2`: no identity                                                         |
     | `crop_url_missing`                | have no crop URL                                                                                                                         |
     | `crop_url_unplaced`               | have a crop URL of a shape the rewrite does not place, such as an image id with no frame: no `crop_url`                                  |
     | `crop_url_mismatch`               | have a crop URL of another scan or batch than the row's `page_url` and `batch`: no `crop_url`                                            |
     | `box_unusable`                    | have a `box` that is not four finite numbers                                                                                             |
     | `score_missing`                   | have no finite `score`                                                                                                                   |
     | `page_seq_num_missing`            | have no `page_seq_num`                                                                                                                   |
     | `frame_differs_from_page_seq_num` | have a `page_seq_num` other than the frame in `page_url`                                                                                 |
     | `without_crop_by_batch`           | are in `crop_url_unplaced` or `crop_url_mismatch`, by batch (`(none)` for a row without one), to see which batches                       |

5. **When it stops.** If more than 1% of a kind's rows have no complete scan identity or no crop URL, the run stops with an error naming the counts, logs the 20 batches with the most rows without a crop URL, and writes no `provenance.json`. That means the shapes of `page_url` and `prediction_section_iiif_url` read here do not fit this data. Look at the batches, fix the reader, and run again; `--max-unplaced-pct` raises the limit to write the attempt anyway and read the counts in its provenance. A file that is not Parquet, lacks one of the columns read (`batch`, `box`, `ocr`, `page_seq_num`, `page_url`, `predicted_type`, `prediction_section_iiif_url`, `score`) or has a column of a type the reader does not know fails the run with the file named.

Licence and credit: the dataset is CC0, and the paper places the dataset, the model and the code in the public domain. The credits page needs a section for data sources (#372 built it for OCR engines and models) before the picture layer is shown on the site; that is part of #1102 and not done here.

## Verified Beyond Words baseline

`scripts/pictures/beyond_words.py` evaluates the retained Newspaper Navigator predictions, not an OCR engine, against the verified classes of the canonical Beyond Words validation split. It does not fetch anything, train or tune a model, join frames to page sequence numbers, or change a store.

Use Python with `scripts/pictures/requirements.txt` installed (validated with PyArrow 20). Retain the two metadata files `data/train_80_percent.json` and `data/val_20_percent.json` from [`biglam/loc_beyond_words` at `6c7f5fb3c60f02d9fe925cfc14aa7008f6c89099`](https://huggingface.co/datasets/biglam/loc_beyond_words/tree/6c7f5fb3c60f02d9fe925cfc14aa7008f6c89099/data). Only those JSON files are needed, about 7 MB; do not download the image archive or image Parquet. Their pinned SHA256s are checked before scoring; their Git blobs also match the canonical LibraryOfCongress/newspaper-navigator files. Independently hash and pin the finished picture provenance before running:

```sh
python3 scripts/pictures/beyond_words.py \
  --curated /var/lib/usnm-pictures/curated \
  --provenance /var/lib/usnm-pictures/curated/pictures/newspaper-navigator/e2e585824cdd28d52755c481dca69ed0bfa992a1/0001/provenance.json \
  --provenance-sha256 <independently-recorded-SHA256> \
  --annotations /var/lib/usnm-pictures/beyond-words \
  --out /var/lib/usnm-pictures/beyond-words-baseline.json
```

The output is create-only. Every curated part's bytes, hash and row count are checked, as are per-kind and total counts; the exact hashed bytes are passed to Arrow. Only local files are read, about one part at a time; the actual one-CPU run used a 3 GiB cap. Ordinary unit tests need no Arrow installation and run automatically through `scripts/ci/run-unit-tests.py`.

The split stays at 2,846 training and 712 validation images. Training/validation image-id overlap is an error; physical scan overlap is excluded even when the ids differ. Drop pages added after 1 December 2019, unverified headline/ad classes, and crowd annotations. Identity includes batch version, LCCN, date, edition, reel and frame. Newspaper Navigator boxes are normalized XYXY; COCO boxes are pixel XYWH. Coordinates are clipped to the page. A predeclared 0.5 prediction threshold and confidence-greedy one-to-one IoU at least 0.5 are used, both class-aware and geometry-only.

Report the results as **annotation agreement, not AP**. Unmatched predictions are not proven false; a scan without a retained prediction has unknown detector coverage, not proven absence of pictures. Verified-annotation recall includes those coverage-unknown scans. This baseline does not fulfill the engine `illustration`/text-loss evaluation, full curated-page join, ads, credits, rebuild or UI acceptance gates of #1102.

### Actual source-only baseline, 29 October 2026

The [retained result](../reports/2026-10-29-beyond-words-baseline.md) comes from an actual one-CPU run over all 3,274,561 curated rows and 36 hash-verified parts, not a fixture or a model rerun. The portable evaluator completed in 36.402 seconds and reproduced every metric, exclusion and hash of the independently run baseline.

The split checks leave 692 eligible validation scans with 1,358 verified picture boxes: 19 post-cutoff scans and one physical training overlap are excluded. There are retained predictions on 456 scans; the other 236 have unknown coverage and are not dropped from scoring. At the fixed threshold there are 726 predictions, with 564 class-aware matches (annotation precision 77.69%, verified-annotation recall 41.53%) or 590 geometry-only matches (81.27% and 43.45%). These are not the original detector's published AP, and none of them measures an OCR engine's layout or lost text.
