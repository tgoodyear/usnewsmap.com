# Pictures

Part of the [operations guide](README.md).

## Newspaper Navigator's picture boxes

`usnm-ingest pictures` ([04 §4.11](../design/04-data-sources-and-ingestion.md#411-pictures-1102), #1102) writes Newspaper Navigator's boxes of photographs, illustrations, maps, comics and cartoons to the curated store, one row per box, keyed by the scan the box was found on and with its crop URL on tile.loc.gov. It is the first part of the picture layer. Nothing reads the table yet: the join to the curated pages, the search field and filter, and the crops in the hit list come later (#1102).

1. **The revision.** `--revision` is required and is the full 40-character commit SHA of [`biglam/newspaper-navigator`](https://huggingface.co/datasets/biglam/newspaper-navigator) (CC0), in lower case; a branch or tag is refused, because it would not pin the data. The one #1102 was triaged against is `e2e585824cdd28d52755c481dca69ed0bfa992a1`. A later revision is a separate run with its own path.
2. **Where.** On an on-prem node, as a detached unit. It downloads about 1.4 GB of Parquet (the cartoons file alone is 71 MB for 206,054 rows) one file at a time into `$USNM_WORK_DIR/pictures` (default: `usnm-ingest` in the system temp directory), deletes each once read, and holds about one part of 100,000 rows in memory. The command takes the top-level options every `usnm-ingest` command takes; it reads nothing from `--reference` and keeps no state, but the binary wants both an empty directory and a state file:

   ```sh
   systemd-run --unit usnm-pictures --nice 19 \
     /usr/local/bin/usnm-ingest --state-file /var/lib/usnm-pictures/state.json \
     --curated <curated directory or container URL> --reference <empty directory> \
     pictures --revision e2e585824cdd28d52755c481dca69ed0bfa992a1
   journalctl -u usnm-pictures
   ```

   Files already on the node are read with `--from-dir <directory>`, which holds one directory per config (`cartoons`, `comics`, `illustrations`, `maps`, `photos`) with the config's `*.parquet` files in it, as `huggingface-cli download` lays them out. Without it the run lists each config's files at the revision (`/api/datasets/biglam/newspaper-navigator/tree/{revision}/{config}`) and fetches them from `resolve/{revision}/…`, checking each against the SHA-256 and size the Hub lists for it. `--hub-url` points that at another host, for a copy of its responses.

   A curated directory on the node needs no grant. The Azure `curated` container takes the writes only from an identity with a create-only grant on `pictures/`; nothing has one yet, and adding it is an infrastructure change of its own, so the first run writes to a directory.

3. **What it writes.** Under `pictures/newspaper-navigator/{revision}/{attempt}/` in the curated store, all create-only:
   - `{kind}-{nnnn}.parquet`, `kind` being `cartoon`, `comic`, `illustration`, `map` or `photo`: at most 100,000 rows each, numbered consecutively within a kind, and never spanning two input files. The columns are `kind`, `lccn` (lower case), `date`, `edition`, `batch`, `reel`, `frame`, `score`, `box` (four numbers, as the dataset gives them), `caption_words`, `predicted_type`, `page_url` and `crop_url`.
   - `provenance.json`, last. It marks the attempt finished and holds the revision, the licence, the tool's version and commit, every input file with its size, SHA-256 and rows, each kind's parts with their SHA-256, bytes and rows, and the counts below, per kind and in total.

   Attempts are numbered from `0001`. A run that stops leaves its parts without a `provenance.json`; they are ignored, and the next run starts the next attempt. They can be deleted by hand; nothing reads them. A revision that already has a finished attempt of the current schema version is not read again: the run says so and exits, and it writes a new attempt only after a schema change. The report is JSON on stdout (revision, attempt, rows by kind, and the path of the provenance).

4. **What to check.** In `provenance.json`:
   - `rows` by kind against the dataset's counts: photos 1,543,525, illustrations 798,475, comics 526,319, maps 200,188, cartoons 206,054, 3,274,561 in all. A shortfall means a file was missed.
   - `quality`, whose counts say how much of the data the rules fit. Expect small numbers, and none for `frame_differs_from_page_seq_num`, which would mean `page_seq_num` is not the frame after all:

     | Count                             | Rows that                                                                                                          |
     | --------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
     | `unplaced_rows`                   | have no scan identity or no `crop_url`, whatever the reason (what the stop below counts)                           |
     | `batch_missing`                   | have an empty `batch`                                                                                              |
     | `page_url_unplaced`               | have a `page_url` that is not `…/{lccn}/{reel}/{issue}/{frame}.jp2`: no identity                                   |
     | `crop_url_missing`                | have no crop URL                                                                                                   |
     | `crop_url_unplaced`               | have a crop URL of a shape the rewrite does not place, such as an image id with no frame: no `crop_url`            |
     | `crop_url_mismatch`               | have a crop URL of another scan or batch than the row's `page_url` and `batch`: no `crop_url`                      |
     | `box_unusable`                    | have a `box` that is not four finite numbers                                                                       |
     | `score_missing`                   | have no finite `score`                                                                                             |
     | `page_seq_num_missing`            | have no `page_seq_num`                                                                                             |
     | `frame_differs_from_page_seq_num` | have a `page_seq_num` other than the frame in `page_url`                                                           |
     | `without_crop_by_batch`           | are in `crop_url_unplaced` or `crop_url_mismatch`, by batch (`(none)` for a row without one), to see which batches |

5. **When it stops.** If more than 1% of a kind's rows have no scan identity or no crop URL, the run stops with an error naming the counts, logs the 20 batches with the most rows without a crop URL, and writes no `provenance.json`. That means the shapes of `page_url` and `prediction_section_iiif_url` read here do not fit this data. Look at the batches, fix the reader, and run again; `--max-unplaced-pct` raises the limit to write the attempt anyway and read the counts in its provenance. A file that is not Parquet, lacks one of the columns read (`batch`, `box`, `ocr`, `page_seq_num`, `page_url`, `predicted_type`, `prediction_section_iiif_url`, `score`) or has a column of a type the reader does not know fails the run with the file named.

Licence and credit: the dataset is CC0, and the paper places the dataset, the model and the code in the public domain. The credits page needs a section for data sources (#372 built it for OCR engines and models) before the picture layer is shown on the site; that is part of #1102 and not done here.
