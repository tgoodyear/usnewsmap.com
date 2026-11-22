# Part A: Tesseract frak2021 on public German Fraktur truth

Scores `tesseract/5.5.0 frak2021` (PSM 3, 300 DPI; `deu_latf` as reference arm)
against public page ground truth, per the German Fraktur truth protocol (Part A). Python 3 stdlib
plus Pillow; `fetch.sh` needs bash, git, curl, unzip, python3.

## Run

```sh
export DATA_ROOT=/path/to/data            # each set goes in $DATA_ROOT/<set>/
./fetch.sh --list --set chronicling             # print the layout; download nothing big
./fetch.sh --i-am-a-person --set chronicling --set reichsanzeiger
./fetch.sh --set nzz                            # add --with-austrian (set AT_URLS)
```

**robots.txt.** `gitlab.uni-bonn.de` disallows `/*/*.git/*` (the Chronicling
Germany clone and its Git LFS images) and `digi.bib.uni-mannheim.de` disallows
`/reichsanzeiger.fcgi` (the Reichsanzeiger images). A person runs those two
sets: `fetch.sh` refuses (exit 2) to download `chronicling` or `reichsanzeiger`
without `--i-am-a-person` (`--list` is exempt). Zenodo `/api` is disallowed
too, so NZZ uses the direct zip URL after a 10 s crawl delay (`NZZ_DELAY`,
minimum 10); the Austrian set takes its file URLs from `AT_URLS`.

Layouts: Chronicling Germany `data/annotations/<Title>_<YYYY-MM-DD>_<NNNN>.xml`,
`data/images/<stem>.jpg` (Git LFS, pulled one image at a time with
`git lfs pull --include`), `work_in_progress/` and `models/` never used. All pages
inside the year range are selected (Tesseract never trained on this set, so
every split is unseen by it) and `fetch.sh` writes `chronicling/meta.csv`
(`page_id,title,year,stratum`; stratum `ood`, `test`, `validation` or `train` from `data/split.csv`), which
`score.py` reads automatically when present (`--meta` overrides it).
Reichsanzeiger `data/reichsanzeiger-1820-1939/GT-PAGE/<YYYY>_<issue>_<page>.xml`
(the `with-TableRegion` duplicate is ignored). NZZ truth is
`xml/NZZ_groundtruth/nzz_<YYYYMMDD>_0_0_a1_p1_1.xml` (the `ABBYY_*` dirs are
not truth); images `img/*.tif` named `<NNNN>_nzz_<YYYYMMDD>...` or
`nzz_<YYYYMMDD>...` are matched on that name or on `nzz_<YYYYMMDD>`. An
8-digit date gives its first 4 digits as the year.

Read the `--list` output and adjust `year_ok` (year filter) and the `*_select`
functions in `fetch.sh` once the layout is known. `year_of` takes the year from
the file name first (Reichsanzeiger `1891_1_0001.xml` is 1891), then from parent
directories nearest first (a range like `reichsanzeiger-1820-1939` is not a
year), then from a TSV `page_id<TAB>year` given as `CG_YEAR_MAP`. A candidate
the filter rejects is skipped, never fatal. Reichsanzeiger images come from
upstream `data/download_images.sh` (URL base) and `data/imageurls.list` (URL
suffix and image name per line): each selected XML stem `S` fetches the line
naming `S.jpg` into `images/S.jpg`. Downloads go to `<file>.part`, resume from
it, and are renamed only after curl succeeds; a failed image is reported and
retried on the next run. Each set gets its
LICENSE/README files and a `MANIFEST.tsv` (sha256, path). Requests are spaced by
`SLEEP` seconds (default 1.5, minimum 1.5); reruns resume.

```sh
python3 score.py --data-root $DATA_ROOT --out results.csv \
  --tessdata /path/to/tessdata --workers 4 --gpu-study-pages gpu_pages.txt
python3 analyze.py --csv results.csv      # --years 1850-1920 by default
python3 tests_score.py
```

- `--sets` (default `chronicling,reichsanzeiger,nzz`; add `austrian`), `--arms`
  (default `frak2021,deu_latf`), `--years 1850-1920`, `--limit N` for a smoke run.
- `--meta file.csv` (columns `page_id,title,year,stratum`) overrides what
  `score.py` guesses: year from the first 18xx/19xx in the id or path, title
  from the id before the year, stratum `ood` when a path component is `ood`
  (Chronicling Germany's 20 out-of-distribution pages), otherwise `test`.
  Pages without a year are skipped unless `--keep-unknown-year`. Page discovery
  skips `.clone`, `.git` and `.download` directories and lists each page id once
  per set. For `nzz` only XML under `NZZ_groundtruth` counts as truth (the
  archive also holds uncorrected ABBYY XML for the same pages).
- `--gpu-study-pages`: page ids (or `set/page_id`), one per line, `#` comments.
  The sorted ids and their sha256 go in the sidecar; a resume with a different
  list is refused (use a new `--out`).
- Year eligibility is decided first. If a year-eligible page has no image,
  scoring is refused (exit 2, the missing pages are listed). `--allow-missing-images`
  scores the rest and records the missing pages in the sidecar; `analyze.py` then
  reports INCOMPLETE.
- Engine: `--cmd` is a template with `{img} {imgdir} {imgname} {model} {tessdata}`.
  Image form (entrypoint `tesseract`, models baked in at `/opt/tessdata`):
  `--cmd 'podman run --rm --network=none --user 10001:10001 -v {imgdir}:/in:ro,Z IMAGE@sha256:DIGEST /in/{imgname} stdout -l {model} --psm 3 --dpi 300' --version-cmd 'podman run --rm IMAGE@sha256:DIGEST --version'`.
  `OMP_THREAD_LIMIT=1` is already set in that image; with the bare binary set it
  yourself for comparable CPU seconds.
- `results.csv.meta.json` records the model sha256 per arm (a warning is printed
  if frak2021 is not `1da23842...e299a`), the command and `tesseract --version`.
- Output is resumable: rerun with the same `--out` and finished (set, page, arm)
  rows are skipped. Failed pages are reported and retried next run. A resume is
  refused (exit 2, nothing overwritten) when the existing sidecar is missing or
  differs in model sha256, `tesseract --version` or `--cmd`; use a new `--out`.
- The sidecar also holds `expected`, the eligible page ids per arm and set, and
  `limit`, `missing_images` and the GPU-study list hash; `analyze.py` uses them to
  detect incomplete runs. On a resume the earlier `expected` ids are merged with
  the newly discovered ones, never replaced.
- CPU seconds are the `RUSAGE_CHILDREN` delta around the call (user+system). With
  `podman run` this counts the client only, not the container, so `cpu_s` is
  client-only there. `--cpu-cgroup PATH` (a cgroup directory holding `cpu.stat`,
  or that file) instead records the `usage_usec` delta around each page; it needs
  `--workers 1` and the engine running in that cgroup. A resume must use the
  same `--cpu-cgroup`.
- The CSV holds numbers only, never OCR or GT text. Temporary PNGs (for formats
  such as jp2) are removed after each page.

## Rules

Reference: PAGE-XML `TextLine/TextEquiv/Unicode` per line, lines joined by `\n`,
regions in `ReadingOrder` (then document order). Reading-order indexes are
local to their group: groups are traversed recursively and only the children
of one ordered group are sorted together; ALTO `String@CONTENT` joined by
spaces per line, a `HYP` attached to its word.

- **Raw**: NFC, CR dropped, whitespace collapsed to single spaces.
- **Folded**: ſ to s; ʒ (U+0292) to z; U+0364 to U+0308, then NFC again (so
  `a`+U+0364 equals `ä`); a hyphen mark (`-`,
  `¬`, `⸗` U+2E17) after a word character and before newline plus a word
  character is dehyphenated (mark and break removed, left to right, no
  overlap, as `qe::text::normalize`); any remaining `¬` or `⸗` becomes `-`; then
  whitespace collapse. Applied to reference and OCR alike.
- **CER** = Levenshtein edits / `ref_chars`. `ref_chars` is the raw normalised
  reference length, shared by raw and folded CER (folded edits use the same
  denominator, so pooling is sum edits / sum ref_chars for both).
- **Word F1**: letter runs (inner apostrophes allowed, case kept) of the folded
  texts, bag overlap, F1 = 2 overlap / (n_ref + n_hyp).
- **Decision rule A** (`analyze.py`): pooled folded CER over the decision pool
  (Chronicling Germany 1850-1920, ood stratum included, plus Reichsanzeiger
  pages not in the GPU study); page bootstrap, 10,000 resamples, seed
  20261121; lift iff upper 95% limit <= 0.179. Any set with a point estimate
  above 0.235 is flagged (hold and investigate); the decision line then reads
  HOLD, never LIFT. A
  title-clustered bootstrap is shown alongside; GPU-study pages are reported
  separately. The analyzer enforces the year window (`--years`, default
  1850-1920) on CSV rows and drops undated ones. It prints **INCOMPLETE** (exit 3) and never LIFT unless every expected in-window
  (set, page, arm) row from the sidecar is present for both arms, both pool sets
  have retained non-GPU-study pages after the year filter, no page lacks an
  image, no `--limit` run is involved, and (with several CSVs) the sidecars agree
  on model sha256s, tesseract version, `--cmd` and the GPU-study list; a `--limit` smoke run is always incomplete.
  Duplicate (set, page_id, arm) rows are an error.

## Licences

Reichsanzeiger-GT is CC0. Chronicling Germany, NZZ GT and Austrian Newspapers GT
are CC BY-NC: evaluation only. Nothing is redistributed, no derived page text or
images are published, and `fetch.sh` keeps each set's LICENSE next to its data.
Keep `$DATA_ROOT` out of any repository or release.
