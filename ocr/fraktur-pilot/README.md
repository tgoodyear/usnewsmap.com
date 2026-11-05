# Fraktur pilot sample

The frozen page sample of the Fraktur pilot ([#325](https://github.com/tgoodyear/usnewsmap.com/issues/325)): about 1,000 damaged German pages from 1850 to 1919, and separate Danish, Finnish and Lithuanian samples. The pilot asks which typeface those pages are printed in and how much a Fraktur model recovers, by reading each German page with Tesseract's Fraktur model (`deu_latf`) and its roman model (`deu`). This directory holds page IDs, audit membership and image hashes only: no OCR text and no scores. Nothing here has been run through OCR.

| File           | What                                                                                                                                                                                                                                                                                               |
| -------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `spec.json`    | The declared design, committed before the draw: the seed, the frame, each sample's language, decades, size and rationale, the allocation rule and the exclusions                                                                                                                                   |
| `pages.jsonl`  | One line per selected page, sorted by sample, decade, title (LCCN), date, edition and page                                                                                                                                                                                                         |
| `summary.json` | The inputs' names, sizes and sha256, and for each sample the frame, eligible and selected counts by decade, the exclusions by reason and decade, and every (decade, title) stratum's eligible and selected counts (a page's design weight is its stratum's eligible count over its selected count) |
| `SHA256SUMS`   | The sha256 of the three files above                                                                                                                                                                                                                                                                |

`scripts/fraktur-pilot/sample.py` draws the sample and checks it; `scripts/fraktur-pilot/plan.py` is the pilot run's dry run. Their tests (`test_sample.py`, `test_plan.py`) use synthetic fixtures, and `test_sample.py` also checks the committed files.

## The frame

The frame is the scan prefetch manifest `pages-v20261010-2`: every page of index `pages-v20261010-2` whose damage v2 rate is at least 0.25. German, Danish, Finnish and Lithuanian pages are in its keep set, so their full-resolution scans are already cached on the scan host and the pilot needs no new image request. A page is eligible when its page-language decision is `detected` (not `mixed`), it is dated within its sample's decades, and the scan fetcher's sidecar for it matches the manifest row, has a sha256 (and an md5 for a JP2), and sits at its keep-set path with the sidecar's size.

The frame is damaged pages only. A share of Fraktur measured on this sample is a share of the damaged, cached pages, not of all German pages.

## The samples

| Sample     | Decades         | Size  | In the frame | Eligible | Titles selected | JPEG scans | In the 10% audit |
| ---------- | --------------- | ----- | ------------ | -------- | --------------- | ---------- | ---------------- |
| German     | 1850s to 1910s  | 1,000 | 45,519       | 44,318   | 85              | 218        | 111              |
| Danish     | 1860s to 1910s  | 100   | 5,639        | 5,191    | 4               | 0          | 8                |
| Finnish    | 1900s and 1910s | 100   | 3,694        | 828      | 3               | 0          | 9                |
| Lithuanian | 1910s           | 100   | 1,560        | 942      | 3               | 0          | 11               |

Each sample's size is split equally over its decades, the remainder going to the earliest decades; a decade with fewer eligible pages than its share gives all of them and the rest is split among the others. Each decade's share is split over its titles the same way, and within a (decade, title) stratum the pages with the lowest selection key (the sha256 of the seed and the doc_id) are taken. So each stratum is a simple random sample.

The German pages by decade:

| Decade | Eligible pages | Titles | Selected | JPEG scans | In the 10% audit |
| ------ | -------------- | ------ | -------- | ---------- | ---------------- |
| 1850s  | 1,771          | 8      | 143      | 33         | 10               |
| 1860s  | 6,254          | 18     | 143      | 30         | 18               |
| 1870s  | 4,533          | 22     | 143      | 25         | 16               |
| 1880s  | 3,180          | 18     | 143      | 18         | 16               |
| 1890s  | 4,802          | 25     | 143      | 43         | 22               |
| 1900s  | 8,406          | 33     | 143      | 41         | 11               |
| 1910s  | 15,372         | 36     | 142      | 28         | 18               |

Every eligible title of every decade has at least one page in the sample. The Danish 1860s have 13 eligible pages, all taken, and the Finnish 1900s 10, all taken; the other decades share the rest (Danish 18, 18, 17, 17, 17 from the 1870s; Finnish 90 in the 1910s).

**Why these sizes and eras.** The German size and era are the issue's: about 1,000 pages over each decade from 1850 to 1919. Equal decades give each decade the same precision (143 pages measure a share near one half to about 8 points either way at 95%, before design effects), so a change of typeface over time can be seen; equal titles spread the pages over as many papers as possible, because a paper's printer chose its typeface. The Danish, Finnish and Lithuanian samples answer a screening question (is blackletter used on our pages, and does a Fraktur model help?), not a prevalence. With 100 pages, a reader who finds no blackletter bounds the share below about 3% at 95% (the rule of three), and a share near one half is measured to about 10 points either way; the three together stay under a third of the German reading. These are screening sizes, not representative minimums. Each takes the German question's period, ending in 1919, from its first decade with damaged pages in the frame. Most Finnish pages (2,866 of 3,694) and two thirds of the Lithuanian pages (613 of 1,560) are from the 1920s and later and are left out; sampling them would be a separate declared draw. The two 1940s Dirva pages named on the issue are outside the era and are not a substitute for it. `spec.json` has the full rationale.

**Exclusions.** A page is counted under the first reason that applies:

| Reason             | German                                     | Danish      | Finnish | Lithuanian |
| ------------------ | ------------------------------------------ | ----------- | ------- | ---------- |
| `outside_era`      | 464 (243 before 1850, 221 after 1919)      | 351 (1920s) | 2,866   | 613        |
| `mixed_language`   | 733                                        | 97          | 0       | 5          |
| `no_sidecar`       | 1 (1910s)                                  | 0           | 0       | 0          |
| `sidecar_mismatch` | 3 (1910s, the sidecar names another batch) | 0           | 0       | 0          |

`below_damage`, `scan_missing` and `duplicate_image` excluded no page. `spec.json` defines each reason.

## A page's record

`doc_id`, `sample`, `decade`, `date`, `lccn`, `edition`, `seq`, `batch`, `batch_version`; `image_id` (the page's IIIF base, an identifier only: the pilot reads the cached scan); `scan_path` (under the scans root), `scan_format` (`jp2` or `jpeg`), `scan_resolution`, `scan_bytes`, `scan_sha256` and `scan_md5` (null for JPEG, whose sidecar has none); `audit_10pct` (in the original 10% page audit by its hash rule) and `audit_scored` (in that audit's page list; the two agree on every frame page); `selection_key` and `stratum_rank` (the page's rank within its stratum).

## How it was made

The inputs are read-only copies, taken on 5 November 2026, of what the scan host and the curated store hold:

| Input            | Source                                                                                                                                                                    | sha256                                                             |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------ |
| `--manifest`     | `/tank/ocr/manifests/pages-v20261010-2/manifest.parquet` on usnm-scanfetch (1,922,826 rows)                                                                               | `ddc1fb2520cbea24ca9633e19da32fcce25f8ec010e473e77195f83462fc5140` |
| `--sidecars`     | `/tank/ocr/scans/index-full.jsonl` on usnm-scanfetch (417,820 pages)                                                                                                      | `e287669f8ccb5b225e9e2ec762a1e3138bfc3d49f62874cb9564ac06dffa420b` |
| `--audit-pages`  | curated `audit/ocr-quality-v2-pages-v20261003-1-10pct-pages.csv.gz` (2,359,875 pages)                                                                                     | `ca9890d21db07108f90454e1b16887a18ee4b962d54329ce7cdb409f8e373514` |
| `--scan-listing` | `find keep/<batch>/<lccn> -type f -printf '%p\t%s\n'` under `/tank/ocr/scans`, for the 171 batch and title directories of the four languages' frame pages (113,270 files) | `a006273ba0f91e597a8f0be690f06ce89e20eb0255502ae1c9e06415684f59e7` |

The fetcher's index grows as it fetches, so a later copy has a different digest; `summary.json` records the one used. To draw again from the same copies (Python 3 with `pyarrow` for a parquet manifest):

```sh
scripts/fraktur-pilot/sample.py --spec ocr/fraktur-pilot/spec.json \
  --manifest manifest.parquet --sidecars index-full.jsonl \
  --audit-pages ocr-quality-v2-pages-v20261003-1-10pct-pages.csv.gz \
  --scan-listing keep-listing.tsv --out ocr/fraktur-pilot
```

The same spec and inputs give the same bytes. To check the committed files without the inputs:

```sh
scripts/fraktur-pilot/sample.py --check --spec ocr/fraktur-pilot/spec.json --out ocr/fraktur-pilot
```

After the draw, the on-disk sidecars of all 1,300 selected pages were read on the scan host and matched their records, and 20 scans picked at random were hashed and matched their `scan_sha256`.

## The pilot run (dry run only)

`plan.py` turns the German sample into 2,000 paired jobs, `deu_latf` and `deu` for each page, and prints what the run would be. With `--commands-out` it writes the shell commands an OCR operator would run; it runs nothing, enqueues nothing and makes no network request:

```sh
scripts/fraktur-pilot/plan.py --pages ocr/fraktur-pilot/pages.jsonl --sample ger \
  --variants deu_latf,deu --tesseract-image sha256:<the production Tesseract image id> \
  --scans-root /tank/ocr/scans --work-root <run dir> \
  --verify-scans --commands-out run.sh --jobs-out jobs.jsonl
```

Each command copies a page's cached scan into its own work directory and reads it with the production Tesseract image the way the on-prem worker does (podman with no network, a read-only root and no capabilities; `-l <model> --psm 3 --dpi 300 txt alto`, a 600 s timeout), keeping stdout, stderr, the exit status and the start and end times beside `page.txt` and `page.xml`, and then deletes the copy. The script needs bash 5 and `timeout`. A page's two variants are adjacent, so a partial run stays paired. `--variants` must name at least two models pinned in `ocr/engines/tesseract/models.lock`; `--verify-scans` re-hashes each scan first. The script records the engine's version first.

## Not decided here

- Authorizing the compute for the paired run, and who runs it.
- A reader to label each page's typeface, and the about 20 pages to transcribe by hand for character error rate.
- The models for the Danish, Finnish and Lithuanian samples (for example `frak2021` against `dan`, `fin` or `lit`; all four are pinned).
- An undamaged comparison band, which would need scans that are not cached.
- Whether to draw Finnish and Lithuanian pages from the 1920s and later as a separate sample.
